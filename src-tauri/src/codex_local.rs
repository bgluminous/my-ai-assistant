//! 本地 ChatGPT（Codex Desktop/CLI）集成：读写登录文件 auth.json，实现免登录切换本机账号、
//! 导入本机登录，以及账户续期后向本机回同步新凭据。同时支持 Windows 与 macOS。
//!
//! 切号流程与 Cursor 对齐：先关闭正在运行的客户端，再写 auth.json，再拉起客户端。
//! 跨平台策略：路径/进程判断用 `cfg!(...)` 运行时分支，便于在 Windows 上也能检查 macOS 分支。

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tauri::AppHandle;

use crate::{accounts, audit, http, paths, process, settings};

// ---------------------------------------------------------------------------
// 可执行文件路径配置（写入统一 settings.json）
// ---------------------------------------------------------------------------

/// 用户手动指定的 ChatGPT / Codex Desktop 路径；空串 = 未配置（走自动搜索）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexClientConfig {
    #[serde(default)]
    pub exe_path: String,
}

fn current() -> CodexClientConfig {
    settings::read(|s| s.codex_client.clone()).unwrap_or_default()
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

// ---------------------------------------------------------------------------
// auth.json 读写
// ---------------------------------------------------------------------------

/// Codex 登录文件路径：$CODEX_HOME/auth.json（环境变量非空优先），否则 ~/.codex/auth.json。
fn auth_json_path() -> Option<PathBuf> {
    let root = std::env::var("CODEX_HOME")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| paths::home_dir().map(|h| h.join(".codex")))?;
    Some(root.join("auth.json"))
}

/// 写 auth.json：先写临时文件再 rename 原子替换，写入中途崩溃不会损坏原文件。
/// Unix 下以 0600 权限落盘（先建空临时文件设好权限再写内容，与 Codex CLI 一致）。
fn write_auth_json(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("codex_auth_write_failed: {e}"))?;
    }
    let text =
        serde_json::to_string_pretty(value).map_err(|e| format!("codex_auth_write_failed: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&tmp, "").map_err(|e| format!("codex_auth_write_failed: {e}"))?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("codex_auth_write_failed: {e}"))?;
    }
    std::fs::write(&tmp, text).map_err(|e| format!("codex_auth_write_failed: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("codex_auth_write_failed: {e}"))?;
    Ok(())
}

/// 从新 access_token 的 JWT 里解析 chatgpt_account_id（可能缺失）。
fn account_id_from_token(access_token: &str) -> Option<String> {
    http::decode_jwt_payload(access_token)
        .and_then(|c| {
            c.get("https://api.openai.com/auth")
                .and_then(|a| a.get("chatgpt_account_id"))
                .and_then(|x| x.as_str())
                .map(str::to_string)
        })
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// 导入 / 回同步
// ---------------------------------------------------------------------------

/// 读取本机 ChatGPT / Codex 登录凭据。文件不存在视为从未登录；
/// 存在但 tokens.access_token 缺失或解析失败视为 invalid。refresh_token 可选。
pub fn read_local_login() -> accounts::LocalLoginRead {
    let Some(path) = auth_json_path() else {
        return accounts::LocalLoginRead::Missing;
    };
    if !path.exists() {
        return accounts::LocalLoginRead::Missing;
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        return accounts::LocalLoginRead::Invalid;
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return accounts::LocalLoginRead::Invalid;
    };
    let tokens = v.get("tokens");
    let Some(access_token) = tokens
        .and_then(|t| t.get("access_token"))
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        return accounts::LocalLoginRead::Invalid;
    };
    let refresh_token = tokens
        .and_then(|t| t.get("refresh_token"))
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    // 备注提示：access_token JWT 里的邮箱，拿不到给空串
    let note_hint = http::decode_jwt_payload(&access_token)
        .and_then(|c| {
            c.get("https://api.openai.com/profile")
                .and_then(|p| p.get("email"))
                .and_then(|x| x.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    accounts::LocalLoginRead::Found(accounts::LocalLogin {
        kind: "codex".into(),
        token: access_token,
        refresh_token,
        note_hint,
    })
}

/// 账户续期成功后，best-effort 把新凭据回同步进本机 auth.json——仅当本机登录与
/// 续期账户是同一账号（account_id 均非空且一致）时写入，否则静默跳过。
/// 任何失败只记审计日志，绝不向调用方报错，不影响刷新主流程。
pub fn sync_auth_json(
    app: &AppHandle,
    access_token: &str,
    refresh_token: &Option<String>,
    id_token: Option<&str>,
) {
    let Some(path) = auth_json_path() else { return };
    if !path.exists() {
        return;
    }
    let mut value = match std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
    {
        Ok(v) => v,
        Err(e) => {
            audit::log(
                app,
                "codex_sync_failed",
                format!("同步本机 ChatGPT 登录失败：auth.json 读取/解析失败（{e}）"),
                None,
            );
            return;
        }
    };
    let local_account_id = value
        .pointer("/tokens/account_id")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    match (local_account_id, account_id_from_token(access_token)) {
        (Some(local), Some(new)) if local == new => {}
        _ => return,
    }
    let Some(tokens) = value.get_mut("tokens").and_then(|t| t.as_object_mut()) else {
        return;
    };
    tokens.insert("access_token".into(), json!(access_token));
    if let Some(rt) = refresh_token {
        tokens.insert("refresh_token".into(), json!(rt));
    }
    if let Some(idt) = id_token {
        tokens.insert("id_token".into(), json!(idt));
    }
    if let Some(obj) = value.as_object_mut() {
        obj.insert("last_refresh".into(), json!(Utc::now().to_rfc3339()));
    }
    if let Err(e) = write_auth_json(&path, &value) {
        audit::log(
            app,
            "codex_sync_failed",
            format!("同步本机 ChatGPT 登录失败：{e}"),
            None,
        );
        return;
    }
    audit::log(app, "codex_sync", "已同步本机 ChatGPT 登录凭据".to_string(), None);
}

// ---------------------------------------------------------------------------
// 客户端探测 / 进程 / 关闭 / 启动
// ---------------------------------------------------------------------------

const CODEX_BUNDLE_ID: &str = "com.openai.codex";

#[cfg(windows)]
fn powershell_trim(script: &str) -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(not(windows))]
fn powershell_trim(_script: &str) -> Option<String> {
    None
}

/// Windows Store 包的 AppID（`OpenAI.Codex_*!App`），用于 shell:AppsFolder 启动。
fn windows_store_app_id() -> Option<String> {
    if !cfg!(target_os = "windows") {
        return None;
    }
    powershell_trim(
        "Get-AppxPackage -Name OpenAI.Codex | Select-Object -First 1 -ExpandProperty PackageFamilyName",
    )
    .map(|family| format!("{family}!App"))
}

fn windows_store_install_location() -> Option<PathBuf> {
    if !cfg!(target_os = "windows") {
        return None;
    }
    powershell_trim(
        "Get-AppxPackage -Name OpenAI.Codex | Select-Object -First 1 -ExpandProperty InstallLocation",
    )
    .map(PathBuf::from)
}

fn macos_bundle_id(app: &Path) -> Option<String> {
    let plist = app.join("Contents").join("Info.plist");
    let output = Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-"])
        .arg(&plist)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn path_starts_with_ci(path: &Path, prefix: &Path) -> bool {
    let a = path.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
    let b = prefix
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    let b = b.trim_end_matches('\\');
    a == b || a.starts_with(&format!("{b}\\"))
}

/// 常见安装位置候选（按优先级，不检查存在性；macOS 的 ChatGPT.app 仅在 bundle id 匹配时加入）。
fn detect_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "windows") {
        if let Some(loc) = windows_store_install_location() {
            let app = loc.join("app");
            out.push(app.join("ChatGPT.exe"));
            out.push(app.join("Codex.exe"));
        }
        if let Some(local) = env_path("LOCALAPPDATA").or_else(paths::data_local_dir) {
            let programs = local.join("Programs").join("OpenAI").join("Codex");
            out.push(programs.join("ChatGPT.exe"));
            out.push(programs.join("Codex.exe"));
        }
        if let Some(pf) = env_path("ProgramFiles") {
            out.push(pf.join("OpenAI").join("Codex").join("ChatGPT.exe"));
            out.push(pf.join("OpenAI").join("Codex").join("Codex.exe"));
        }
    } else if cfg!(target_os = "macos") {
        let mut dirs = vec![PathBuf::from("/Applications")];
        if let Some(home) = paths::home_dir() {
            dirs.push(home.join("Applications"));
        }
        for dir in dirs {
            out.push(dir.join("Codex.app"));
            let chatgpt = dir.join("ChatGPT.app");
            if chatgpt.exists() {
                if macos_bundle_id(&chatgpt).as_deref() == Some(CODEX_BUNDLE_ID) {
                    out.push(chatgpt);
                }
            } else {
                out.push(chatgpt);
            }
        }
    }
    out
}

fn resolve_exe_path() -> Result<PathBuf, String> {
    let configured = current().exe_path.trim().to_string();
    if !configured.is_empty() {
        let p = PathBuf::from(&configured);
        if p.exists() {
            return Ok(p);
        }
    }
    detect_candidates()
        .into_iter()
        .find(|p| p.exists())
        .ok_or_else(|| "codex_exe_not_found".to_string())
}

fn can_launch() -> bool {
    if cfg!(target_os = "windows") && windows_store_app_id().is_some() {
        return true;
    }
    resolve_exe_path().is_ok()
}

/// 关闭/检测时匹配哪些进程：Store 包、桌面运行时缓存、用户配置路径；不含独立 CLI（Programs\OpenAI\Codex）。
struct DesktopMatcher {
    macos_needles: Vec<String>,
}

impl DesktopMatcher {
    fn new() -> Self {
        let mut macos_needles = Vec::new();
        if cfg!(target_os = "macos") {
            macos_needles.push("/codex.app/".into());
            for c in detect_candidates() {
                if c.exists() {
                    let mut s = c.to_string_lossy().to_ascii_lowercase();
                    if !s.ends_with('/') {
                        s.push('/');
                    }
                    macos_needles.push(s);
                }
            }
            let configured = current().exe_path.trim().to_string();
            if !configured.is_empty() {
                let p = PathBuf::from(&configured);
                let app = p.ancestors().find(|a| {
                    a.extension()
                        .map(|e| e.eq_ignore_ascii_case("app"))
                        .unwrap_or(false)
                });
                let base = app.unwrap_or(p.as_path());
                let mut s = base.to_string_lossy().to_ascii_lowercase();
                if !s.ends_with('/') {
                    s.push('/');
                }
                macos_needles.push(s);
            }
        }
        DesktopMatcher { macos_needles }
    }

    fn matches(&self, p: &process::ProcessInfo) -> bool {
        let Some(exe) = p.exe.as_ref() else {
            return false;
        };
        if cfg!(target_os = "windows") {
            is_windows_desktop_exe(exe)
        } else if cfg!(target_os = "macos") {
            let s = exe.to_string_lossy().to_ascii_lowercase();
            self.macos_needles.iter().any(|n| s.contains(n))
        } else {
            false
        }
    }

    fn pids(&self) -> Vec<u32> {
        process::list_processes()
            .into_iter()
            .filter(|p| self.matches(p))
            .map(|p| p.pid)
            .collect()
    }

    fn is_running(&self) -> bool {
        !self.pids().is_empty()
    }
}

fn is_windows_desktop_exe(exe: &Path) -> bool {
    let lower = exe
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    if lower.contains(r"\windowsapps\openai.codex_") {
        return true;
    }
    if lower.contains(r"\packages\openai.codex_") {
        return true;
    }
    // 桌面应用运行时缓存：%LOCALAPPDATA%\OpenAI\Codex\...（不含 Programs\OpenAI\Codex 独立 CLI）
    if let Some(local) = paths::data_local_dir() {
        let runtime = local.join("OpenAI").join("Codex");
        if path_starts_with_ci(exe, &runtime) {
            return true;
        }
    }
    let configured = current().exe_path.trim().to_string();
    if !configured.is_empty() {
        let cfg = PathBuf::from(&configured);
        if cfg.exists() && (exe == cfg.as_path() || path_starts_with_ci(exe, &cfg)) {
            return true;
        }
        if let Some(parent) = cfg.parent() {
            if path_starts_with_ci(exe, parent) {
                return true;
            }
        }
    }
    false
}

fn is_windowsapps_path(path: &Path) -> bool {
    path.to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase()
        .contains(r"\windowsapps\")
}

#[cfg(windows)]
fn run_hidden(cmd: &mut Command) -> std::io::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW).output()
}

#[cfg(not(windows))]
fn run_hidden(cmd: &mut Command) -> std::io::Result<std::process::Output> {
    cmd.output()
}

fn wait_exit(matcher: &DesktopMatcher, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if !matcher.is_running() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn kill_pids(pids: &[u32], force: bool) {
    if cfg!(target_os = "windows") {
        for pid in pids {
            let pid_s = pid.to_string();
            let mut cmd = Command::new("taskkill");
            cmd.args(["/PID", &pid_s]);
            if force {
                cmd.args(["/F", "/T"]);
            }
            let _ = run_hidden(&mut cmd);
        }
    } else if cfg!(target_os = "macos") {
        let sig = if force { "-9" } else { "-TERM" };
        for pid in pids {
            let _ = Command::new("kill")
                .args([sig, &pid.to_string()])
                .output();
        }
    }
}

/// 关闭本地 ChatGPT / Codex Desktop：先优雅退出，约 8 秒超时后强制结束，再等约 3 秒确认。
fn close_desktop() -> bool {
    let matcher = DesktopMatcher::new();
    if !matcher.is_running() {
        return true;
    }
    if cfg!(target_os = "windows") {
        let pids = matcher.pids();
        kill_pids(&pids, false);
        if !wait_exit(&matcher, Duration::from_secs(8)) {
            kill_pids(&matcher.pids(), true);
            let _ = wait_exit(&matcher, Duration::from_secs(3));
        }
    } else if cfg!(target_os = "macos") {
        // bundle id 在 Codex / ChatGPT 品牌包上相同，避免误关经典 ChatGPT（com.openai.chat）。
        let _ = Command::new("osascript")
            .args(["-e", &format!("tell application id \"{CODEX_BUNDLE_ID}\" to quit")])
            .output();
        if !wait_exit(&matcher, Duration::from_secs(8)) {
            kill_pids(&matcher.pids(), true);
            let _ = wait_exit(&matcher, Duration::from_secs(3));
        }
    }
    !matcher.is_running()
}

fn launch_store_app(app_id: &str) -> bool {
    Command::new("explorer.exe")
        .arg(format!("shell:AppsFolder\\{app_id}"))
        .spawn()
        .is_ok()
}

/// Windows：脱离本进程 Job 启动，避免本应用退出时把客户端一并杀掉。
#[cfg(windows)]
fn launch_detached_windows(exe: &Path) -> bool {
    use std::os::windows::process::CommandExt;
    const DETACH_FLAGS: u32 = 0x0100_0000 | 0x0000_0200 | 0x0000_0008;
    if Command::new(exe).creation_flags(DETACH_FLAGS).spawn().is_ok() {
        return true;
    }
    Command::new("explorer.exe").arg(exe).spawn().is_ok()
}

#[cfg(not(windows))]
fn launch_detached_windows(_exe: &Path) -> bool {
    false
}

fn launch_path(exe: &Path) -> bool {
    if cfg!(target_os = "windows") {
        if is_windowsapps_path(exe) {
            if let Some(id) = windows_store_app_id() {
                return launch_store_app(&id);
            }
        }
        launch_detached_windows(exe)
    } else if cfg!(target_os = "macos") {
        let is_app_bundle = exe
            .extension()
            .map(|e| e.eq_ignore_ascii_case("app"))
            .unwrap_or(false);
        if is_app_bundle {
            Command::new("open").arg(exe).spawn().is_ok()
        } else {
            let name = if exe.to_string_lossy().contains("ChatGPT") {
                "ChatGPT"
            } else {
                "Codex"
            };
            Command::new("open").args(["-a", name]).spawn().is_ok()
        }
    } else {
        false
    }
}

fn launch_desktop() -> bool {
    let configured = current().exe_path.trim().to_string();
    if !configured.is_empty() {
        let p = PathBuf::from(&configured);
        if p.exists() && !(cfg!(target_os = "windows") && is_windowsapps_path(&p)) {
            return launch_path(&p);
        }
    }
    if cfg!(target_os = "windows") {
        if let Some(id) = windows_store_app_id() {
            return launch_store_app(&id);
        }
    }
    if let Ok(exe) = resolve_exe_path() {
        return launch_path(&exe);
    }
    false
}

fn is_desktop_running() -> bool {
    DesktopMatcher::new().is_running()
}

// ---------------------------------------------------------------------------
// 命令
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexClientView {
    pub exe_path: String,
    pub path: String,
}

fn view(cfg: &CodexClientConfig) -> CodexClientView {
    CodexClientView {
        exe_path: cfg.exe_path.clone(),
        path: settings::path_display(),
    }
}

#[tauri::command]
pub fn codex_client_get(_app: AppHandle) -> Result<CodexClientView, String> {
    settings::ensure_loaded()?;
    Ok(view(&current()))
}

#[tauri::command]
pub fn codex_client_set(_app: AppHandle, exe_path: String) -> Result<CodexClientView, String> {
    let exe_path = exe_path.trim().to_string();
    if !exe_path.is_empty() && !Path::new(&exe_path).exists() {
        return Err("codex_exe_invalid".into());
    }
    let cfg = CodexClientConfig { exe_path };
    settings::mutate(|s| {
        s.codex_client = cfg.clone();
        Ok(())
    })?;
    Ok(view(&cfg))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectResult {
    pub exe_path: Option<String>,
}

/// 探测常见安装位置（含 Windows Store 包），不写配置。
#[tauri::command]
pub fn codex_client_detect() -> DetectResult {
    DetectResult {
        exe_path: resolve_exe_path()
            .ok()
            .map(|p| p.to_string_lossy().to_string()),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientStatus {
    pub running: bool,
    pub exe_configured: bool,
    pub exe_path: Option<String>,
}

#[tauri::command]
pub async fn codex_client_status(id: String) -> Result<ClientStatus, String> {
    let _ = id;
    let exe_path = resolve_exe_path()
        .ok()
        .map(|p| p.to_string_lossy().to_string());
    Ok(ClientStatus {
        running: is_desktop_running(),
        exe_configured: can_launch(),
        exe_path,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseResult {
    pub closed: bool,
}

#[tauri::command]
pub async fn codex_client_close() -> Result<CloseResult, String> {
    let closed = tauri::async_runtime::spawn_blocking(close_desktop)
        .await
        .map_err(|e| e.to_string())?;
    Ok(CloseResult { closed })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchResult {
    pub launched: bool,
}

#[tauri::command]
pub fn codex_client_launch() -> Result<LaunchResult, String> {
    Ok(LaunchResult {
        launched: launch_desktop(),
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchResult {
    pub switched: bool,
    pub message: String,
}

/// 审计显示名：有备注用备注，否则用 token 前 8 字符打码。绝不落全量 token。
fn display_name(acc: &accounts::Account) -> String {
    let note = acc.note.trim();
    if !note.is_empty() {
        note.to_string()
    } else {
        let head: String = acc.token.trim().chars().take(8).collect();
        format!("{head}…")
    }
}

/// 切换本机 ChatGPT 登录：用账户的 refresh_token 换取全新凭据组后写入 auth.json。
/// 写文件要求客户端已关闭（关闭由 codex_client_close 单独负责），与 Cursor 切号一致。
#[tauri::command]
pub async fn codex_switch_local(app: AppHandle, id: String) -> Result<SwitchResult, String> {
    if !cfg!(target_os = "windows") && !cfg!(target_os = "macos") {
        return Err("unsupported_platform".into());
    }

    // 与账户刷新共用同一全局队列：切换同样会消费并轮换 refresh_token。
    let _queued = accounts::refresh_queue().lock().await;

    let acc = accounts::account_snapshot(&id)?;
    if acc.kind != "codex" {
        return Err("not_codex_account".into());
    }

    let alive = acc
        .status
        .as_ref()
        .and_then(|s| s.get("alive"))
        .and_then(|x| x.as_bool());
    if alive != Some(true) {
        return Err("account_not_verified".into());
    }

    let rt = match acc.refresh_token.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(rt) => rt.to_string(),
        None => return Err("codex_no_refresh_token".into()),
    };

    let name = display_name(&acc);
    let (token, new_rt, id_token) = match accounts::request_codex_refresh(&rt).await? {
        accounts::RefreshOutcome::Denied { body } => {
            audit::log(
                &app,
                "codex_renew_failed",
                format!("ChatGPT 续期被拒绝：{name}（{body}）"),
                Some(json!({ "id": id })),
            );
            return Err("codex_refresh_denied".into());
        }
        accounts::RefreshOutcome::Success {
            access_token,
            refresh_token,
            id_token,
        } => (access_token, refresh_token, id_token),
    };

    // 轮换后旧 refresh_token 已作废，无论后续成败先把新凭据写回账户。
    let refresh_token = new_rt.or(Some(rt));
    accounts::persist_tokens(&app, &id, &token, &refresh_token)?;
    let Some(id_token) = id_token else {
        return Err("codex_id_token_missing".into());
    };

    let account_id = account_id_from_token(&token);

    if is_desktop_running() {
        return Err("codex_running".into());
    }

    let Some(path) = auth_json_path() else {
        return Err("codex_auth_write_failed: home_dir_unavailable".into());
    };
    let mut api_key = Value::Null;
    if path.exists() {
        let backup = path.with_extension("json.bak");
        std::fs::copy(&path, &backup)
            .map_err(|e| format!("codex_auth_write_failed: backup_failed: {e}"))?;
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                if let Some(k) = v.get("OPENAI_API_KEY") {
                    api_key = k.clone();
                }
            }
        }
    }

    let value = json!({
        "OPENAI_API_KEY": api_key,
        "tokens": {
            "id_token": id_token,
            "access_token": token,
            "refresh_token": refresh_token,
            "account_id": account_id,
        },
        "last_refresh": Utc::now().to_rfc3339(),
    });
    write_auth_json(&path, &value)?;

    audit::log(
        &app,
        "codex_switch_local",
        format!("切换本机 ChatGPT 登录：{name}"),
        Some(json!({ "id": id })),
    );

    Ok(SwitchResult {
        switched: true,
        message: "已写入本地登录。".into(),
    })
}

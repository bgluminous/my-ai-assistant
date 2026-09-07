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
use std::time::Duration;
use tauri::AppHandle;

use crate::local_client::{
    kill_pids, launch_detached_windows, path_starts_with_ci, run_hidden, wait_exit, ClientStatus,
    CloseResult, DetectResult, LaunchResult, SwitchResult,
};
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

/// 从 access_token 的 JWT 里解析 chatgpt_account_id（可能缺失）。
pub(crate) fn account_id_from_token(access_token: &str) -> Option<String> {
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
        raw: Some(v),
    })
}

/// 本机 auth.json 里的登录若与给定 access_token 属同一账号（account_id 均非空且一致），返回其凭据；
/// 未登录、文件无法解析或不是同一账号都返回 None。供账户刷新 / 切换时把客户端自行续期后的
/// 新凭据同步回账户（与 sync_auth_json 相反的方向）。
pub fn local_login_of_same_account(account_token: &str) -> Option<accounts::LocalLogin> {
    let accounts::LocalLoginRead::Found(local) = read_local_login() else {
        return None;
    };
    match (account_id_from_token(&local.token), account_id_from_token(account_token)) {
        (Some(local_id), Some(account_id)) if local_id == account_id => Some(local),
        _ => None,
    }
}

/// 账户续期成功后，best-effort 把新凭据回同步进本机 auth.json——仅当本机登录与
/// 续期账户是同一账号（account_id 均非空且一致）时写入，否则静默跳过。
/// 任何失败只记审计日志，绝不向调用方报错，不影响刷新主流程。
pub fn sync_auth_json(access_token: &str, refresh_token: &Option<String>, id_token: Option<&str>) {
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
            "codex_sync_failed",
            format!("同步本机 ChatGPT 登录失败：{e}"),
            None,
        );
        return;
    }
    audit::log("codex_sync", "已同步本机 ChatGPT 登录凭据".to_string(), None);
}

// ---------------------------------------------------------------------------
// 客户端探测 / 进程 / 关闭 / 启动
// ---------------------------------------------------------------------------

const CODEX_BUNDLE_ID: &str = "com.openai.codex";

/// Windows：静默执行一段 PowerShell 并返回去空白的 stdout；失败 / 空输出 / 非 Windows 为 None。
fn powershell_trim(script: &str) -> Option<String> {
    if !cfg!(target_os = "windows") {
        return None;
    }
    let output = run_hidden(
        Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", script]),
    )
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

/// 常见安装位置候选（按优先级，不检查存在性；macOS 的 ChatGPT.app 仅在 bundle id 匹配时加入）。
fn detect_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "windows") {
        if let Some(loc) = windows_store_install_location() {
            let app = loc.join("app");
            out.push(app.join("ChatGPT.exe"));
            out.push(app.join("Codex.exe"));
        }
        if let Some(local) = paths::env_nonempty("LOCALAPPDATA").or_else(paths::data_local_dir) {
            let programs = local.join("Programs").join("OpenAI").join("Codex");
            out.push(programs.join("ChatGPT.exe"));
            out.push(programs.join("Codex.exe"));
        }
        if let Some(pf) = paths::env_nonempty("ProgramFiles") {
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

/// 关闭本地 ChatGPT / Codex Desktop：先优雅退出，约 8 秒超时后强制结束，再等约 3 秒确认。
fn close_desktop() -> bool {
    let matcher = DesktopMatcher::new();
    if !matcher.is_running() {
        return true;
    }
    let running = || matcher.is_running();
    if cfg!(target_os = "windows") {
        let pids = matcher.pids();
        kill_pids(&pids, false);
        if !wait_exit(running, Duration::from_secs(8)) {
            kill_pids(&matcher.pids(), true);
            let _ = wait_exit(running, Duration::from_secs(3));
        }
    } else if cfg!(target_os = "macos") {
        // bundle id 在 Codex / ChatGPT 品牌包上相同，避免误关经典 ChatGPT（com.openai.chat）。
        let _ = Command::new("osascript")
            .args(["-e", &format!("tell application id \"{CODEX_BUNDLE_ID}\" to quit")])
            .output();
        if !wait_exit(running, Duration::from_secs(8)) {
            kill_pids(&matcher.pids(), true);
            let _ = wait_exit(running, Duration::from_secs(3));
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
pub fn codex_client_get() -> Result<CodexClientView, String> {
    settings::ensure_loaded()?;
    Ok(view(&current()))
}

#[tauri::command]
pub fn codex_client_set(exe_path: String) -> Result<CodexClientView, String> {
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

/// 探测常见安装位置（含 Windows Store 包），不写配置。
#[tauri::command]
pub fn codex_client_detect() -> DetectResult {
    DetectResult {
        exe_path: resolve_exe_path()
            .ok()
            .map(|p| p.to_string_lossy().to_string()),
    }
}

#[tauri::command]
pub async fn codex_client_status() -> Result<ClientStatus, String> {
    let exe_path = resolve_exe_path()
        .ok()
        .map(|p| p.to_string_lossy().to_string());
    Ok(ClientStatus {
        running: is_desktop_running(),
        exe_configured: can_launch(),
        exe_path,
    })
}

#[tauri::command]
pub async fn codex_client_close() -> Result<CloseResult, String> {
    let closed = tauri::async_runtime::spawn_blocking(close_desktop)
        .await
        .map_err(|e| e.to_string())?;
    Ok(CloseResult { closed })
}

#[tauri::command]
pub fn codex_client_launch() -> Result<LaunchResult, String> {
    Ok(LaunchResult {
        launched: launch_desktop(),
    })
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

    let name = accounts::short_display_name(&acc);
    // 本机若正登录着同一账号且客户端已自行续期，先换上它的凭据，避免拿已作废的 refresh_token 去换
    let mut current_token = acc.token.clone();
    let mut current_rt = acc.refresh_token.clone();
    accounts::adopt_newer_local_codex(&app, &id, &name, &mut current_token, &mut current_rt)?;

    // 副本完整且 access_token 还没进续期窗口时直接写入，不换票：每次换票都会轮换 refresh_token，
    // 能少一次就少一次与本机客户端脱节的机会
    let fresh_copy = accounts::account_snapshot(&id).ok().and_then(|a| {
        let copy = accounts::codex_auth_json(&a)?;
        let renew =
            accounts::codex_token_needs_renewal(&current_token, accounts::codex_last_refresh(&a));
        (copy_is_complete(&copy) && !renew).then_some(copy)
    });
    if let Some(copy) = fresh_copy {
        if is_desktop_running() {
            return Err("codex_running".into());
        }
        write_local_auth(copy)?;
        audit::log(
            "codex_switch_local",
            format!("切换本机 ChatGPT 登录：{name}（写入保存的凭据副本，未换票）"),
            Some(json!({ "id": id, "exchanged": false })),
        );
        return Ok(SwitchResult {
            switched: true,
            message: "已写入保存的凭据副本（未换票）。".into(),
            exchanged: Some(false),
        });
    }

    let rt = match current_rt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(rt) => rt.to_string(),
        None => return Err("codex_no_refresh_token".into()),
    };

    let (token, new_rt, id_token) = match accounts::request_codex_refresh(&rt).await? {
        accounts::RefreshOutcome::Denied { reason, detail } => {
            audit::log(
                "codex_renew_failed",
                format!("ChatGPT 续期被拒绝：{name}（{}：{detail}）", reason.label()),
                Some(json!({ "id": id, "reason": reason.code() })),
            );
            // 错误码带归类后缀（expired / reused / revoked / invalid），前端据此给出针对性提示
            return Err(format!("codex_refresh_denied:{}", reason.code()));
        }
        accounts::RefreshOutcome::Success {
            access_token,
            refresh_token,
            id_token,
        } => (access_token, refresh_token, id_token),
    };

    // 轮换后旧 refresh_token 已作废，无论后续成败先把新凭据组（含 id_token）写进账户副本。
    let refresh_token = new_rt.or(Some(rt));
    {
        let (t, r, i) = (token.clone(), refresh_token.clone(), id_token.clone());
        accounts::update_codex_auth(&app, &id, move |v| {
            accounts::apply_codex_tokens(v, &t, r.as_deref(), i.as_deref())
        })?;
    }
    let Some(id_token) = id_token else {
        return Err("codex_id_token_missing".into());
    };

    if is_desktop_running() {
        return Err("codex_running".into());
    }

    let value = json!({
        "OPENAI_API_KEY": Value::Null,
        "tokens": {
            "id_token": id_token,
            "access_token": token,
            "refresh_token": refresh_token,
            "account_id": account_id_from_token(&token),
        },
        "last_refresh": Utc::now().to_rfc3339(),
    });
    write_local_auth(value)?;

    audit::log(
        "codex_switch_local",
        format!("切换本机 ChatGPT 登录：{name}（已换取新凭据）"),
        Some(json!({ "id": id, "exchanged": true })),
    );

    Ok(SwitchResult {
        switched: true,
        message: "已换取新凭据并写入本地登录。".into(),
        exchanged: Some(true),
    })
}

/// 副本是否具备写成本机 auth.json 的三要素（id_token / access_token / refresh_token 均非空）。
fn copy_is_complete(v: &Value) -> bool {
    ["id_token", "access_token", "refresh_token"].iter().all(|key| {
        v.pointer(&format!("/tokens/{key}"))
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    })
}

/// 本机 auth.json 的修改时间（文件不存在为 None），供定时同步判断是否有变化。
fn auth_json_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(auth_json_path()?).ok()?.modified().ok()
}

/// 后台定时同步：ChatGPT Desktop / Codex CLI 运行时会自行续期并改写 auth.json，这里按设置的间隔
///（默认 5 分钟）看一次文件修改时间，变了就把同账号账户的凭据副本更新过来（不联网，与定时刷新
/// 是否开启无关）。每 30 秒醒一次判断是否到点，改间隔后无需重启即生效。
/// 与刷新 / 切换共用同一把队列锁，不会与正在进行的换票交错。
pub fn start_local_sync(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut seen = auth_json_mtime();
        let mut last_check = std::time::Instant::now();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let interval = std::time::Duration::from_secs(u64::from(settings::local_sync_minutes()) * 60);
            if last_check.elapsed() < interval {
                continue;
            }
            last_check = std::time::Instant::now();
            let now = auth_json_mtime();
            if now == seen {
                continue;
            }
            seen = now;
            if now.is_none() {
                continue;
            }
            let _queued = accounts::refresh_queue().lock().await;
            if let Err(e) = accounts::sync_codex_from_local(&app) {
                audit::log(
                    "codex_sync_failed",
                    format!("从本机 auth.json 同步 ChatGPT 凭据失败：{e}"),
                    None,
                );
            }
        }
    });
}

/// 读取本机同步间隔（分钟）。
#[tauri::command]
pub fn local_sync_get() -> Result<u32, String> {
    settings::ensure_loaded()?;
    Ok(settings::local_sync_minutes())
}

/// 设置本机同步间隔（分钟），只接受设置弹窗提供的几档。
#[tauri::command]
pub fn local_sync_set(minutes: u32) -> Result<u32, String> {
    if !settings::LOCAL_SYNC_CHOICES.contains(&minutes) {
        return Err("invalid_interval".into());
    }
    settings::mutate(|s| {
        s.codex_local_sync_minutes = minutes;
        Ok(())
    })?;
    let text = if minutes >= 60 && minutes % 60 == 0 {
        format!("{} 小时", minutes / 60)
    } else {
        format!("{minutes} 分钟")
    };
    audit::log(
        "local_sync_interval_set",
        format!("本机 ChatGPT 凭据同步间隔设为每 {text}"),
        None,
    );
    Ok(minutes)
}

/// 把一组完整凭据写成本机 auth.json：先备份原文件为 auth.json.bak；副本里没有 OPENAI_API_KEY 时
/// 保留原文件里的值，避免覆盖用户配置的 API Key。
fn write_local_auth(mut value: Value) -> Result<(), String> {
    let Some(path) = auth_json_path() else {
        return Err("codex_auth_write_failed: home_dir_unavailable".into());
    };
    let mut existing_api_key = Value::Null;
    if path.exists() {
        let backup = path.with_extension("json.bak");
        std::fs::copy(&path, &backup)
            .map_err(|e| format!("codex_auth_write_failed: backup_failed: {e}"))?;
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                if let Some(k) = v.get("OPENAI_API_KEY") {
                    existing_api_key = k.clone();
                }
            }
        }
    }
    if let Some(obj) = value.as_object_mut() {
        let keep_existing = obj
            .get("OPENAI_API_KEY")
            .is_none_or(|k| k.is_null() || k.as_str().is_some_and(|s| s.trim().is_empty()));
        if keep_existing {
            obj.insert("OPENAI_API_KEY".into(), existing_api_key);
        }
    }
    write_auth_json(&path, &value)
}

/// 「强制写入并启动」的写入步骤：不向 OpenAI 换新凭据，直接用账户里保存的 auth.json 副本覆盖
/// 本机文件。供切换时 refresh_token 已失效、拿不到新 access_token 的场景兜底；副本不完整
///（缺 id_token / access_token / refresh_token）时报 codex_auth_incomplete。
#[tauri::command]
pub async fn codex_force_write_local(id: String) -> Result<SwitchResult, String> {
    if !cfg!(target_os = "windows") && !cfg!(target_os = "macos") {
        return Err("unsupported_platform".into());
    }
    let acc = accounts::account_snapshot(&id)?;
    if acc.kind != "codex" {
        return Err("not_codex_account".into());
    }
    let Some(mut value) = accounts::codex_auth_json(&acc) else {
        return Err("codex_auth_missing".into());
    };
    if !copy_is_complete(&value) {
        return Err("codex_auth_incomplete".into());
    }
    // 副本缺 account_id 时由 access_token 补上（Codex 反序列化需要该字段）
    if let Some(tokens) = value.get_mut("tokens").and_then(Value::as_object_mut) {
        let missing = tokens
            .get("account_id")
            .and_then(Value::as_str)
            .is_none_or(|s| s.trim().is_empty());
        if missing {
            if let Some(account_id) = tokens
                .get("access_token")
                .and_then(Value::as_str)
                .and_then(account_id_from_token)
            {
                tokens.insert("account_id".into(), json!(account_id));
            }
        }
    }
    if is_desktop_running() {
        return Err("codex_running".into());
    }
    write_local_auth(value)?;

    let name = accounts::short_display_name(&acc);
    audit::log(
        "codex_force_write",
        format!("强制写入本机 ChatGPT 登录：{name}（未换新凭据，直接写入保存的副本）"),
        Some(json!({ "id": id })),
    );
    Ok(SwitchResult {
        switched: true,
        message: "已写入本地登录（未换新凭据）。".into(),
        exchanged: Some(false),
    })
}

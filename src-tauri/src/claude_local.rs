//! 本地 Claude 集成：读写 Claude Code 登录凭据（Windows/Linux 为
//! ~/.claude/.credentials.json，macOS 为 Keychain），实现本机登录导入与切号；
//! 并提供 Claude Desktop 客户端的探测 / 运行检测 / 关闭 / 启动。
//!
//! 切号只写 Claude Code 凭据（Claude Desktop 聊天应用的登录态在其内部加密存储，
//! 无法安全写入）；客户端关闭 / 启动与 Cursor / Codex 的切号流程保持同一交互。

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tauri::AppHandle;

use crate::local_client::{
    kill_pids, launch_detached_windows, path_starts_with_ci, wait_exit, ClientStatus, CloseResult,
    DetectResult, LaunchResult, SwitchResult,
};
use crate::{accounts, audit, claude_oauth, paths, process, settings};

// ---------------------------------------------------------------------------
// 可执行文件路径配置（写入统一 settings.json）
// ---------------------------------------------------------------------------

/// 用户手动指定的 Claude Desktop 路径；空串 = 未配置（走自动搜索）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeClientConfig {
    #[serde(default)]
    pub exe_path: String,
}

fn current() -> ClaudeClientConfig {
    settings::read(|s| s.claude_client.clone()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 凭据读写（.credentials.json / macOS Keychain）
// ---------------------------------------------------------------------------

/// Keychain 条目名（Claude Code 官方使用的 service 名）。
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// 凭据文件路径：$CLAUDE_CONFIG_DIR（取第一个逗号段）/.credentials.json，
/// 否则 ~/.claude/.credentials.json。
fn credentials_path() -> Option<PathBuf> {
    let dir = std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .and_then(|s| {
            s.split(',')
                .map(str::trim)
                .find(|p| !p.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| paths::home_dir().map(|h| h.join(".claude")))?;
    Some(dir.join(".credentials.json"))
}

/// macOS：从 Keychain 读取凭据 JSON。先按当前用户账户名查，再退回任意账户。
fn keychain_read() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let mut attempts: Vec<Vec<String>> = Vec::new();
    if let Ok(user) = std::env::var("USER") {
        if !user.trim().is_empty() {
            attempts.push(vec![
                "find-generic-password".into(),
                "-a".into(),
                user.trim().to_string(),
                "-s".into(),
                KEYCHAIN_SERVICE.into(),
                "-w".into(),
            ]);
        }
    }
    attempts.push(vec![
        "find-generic-password".into(),
        "-s".into(),
        KEYCHAIN_SERVICE.into(),
        "-w".into(),
    ]);
    for args in attempts {
        let output = Command::new("security").args(&args).output().ok()?;
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

/// macOS：把凭据 JSON 写入 Keychain（-U 存在则更新）。
fn keychain_write(json_text: &str) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    let user = std::env::var("USER").unwrap_or_default();
    let account = if user.trim().is_empty() { "claude" } else { user.trim() };
    Command::new("security")
        .args([
            "add-generic-password",
            "-U",
            "-a",
            account,
            "-s",
            KEYCHAIN_SERVICE,
            "-w",
            json_text,
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 写凭据文件：先写临时文件再 rename 原子替换；Unix 下 0600 权限。
fn write_credentials_file(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("claude_creds_write_failed: {e}"))?;
    }
    let tmp = path.with_extension("json.tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&tmp, "").map_err(|e| format!("claude_creds_write_failed: {e}"))?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("claude_creds_write_failed: {e}"))?;
    }
    std::fs::write(&tmp, text).map_err(|e| format!("claude_creds_write_failed: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("claude_creds_write_failed: {e}"))?;
    Ok(())
}

/// 从 credentials JSON 文本解析 claudeAiOauth 段。
fn parse_credentials(text: &str) -> Option<(String, Option<String>)> {
    let v: Value = serde_json::from_str(text).ok()?;
    let oauth = v.get("claudeAiOauth")?;
    let access = oauth
        .get("accessToken")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let refresh = oauth
        .get("refreshToken")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Some((access, refresh))
}

/// 读取本机 Claude Code 登录凭据。macOS 先查 Keychain 再退回文件；
/// 文件与 Keychain 均无视为从未登录；有内容但解析不出关键字段视为 invalid。
pub fn read_local_login() -> accounts::LocalLoginRead {
    let mut found_source = false;
    if cfg!(target_os = "macos") {
        if let Some(text) = keychain_read() {
            found_source = true;
            if let Some((access, refresh)) = parse_credentials(&text) {
                return accounts::LocalLoginRead::Found(accounts::LocalLogin {
                    kind: "claude".into(),
                    token: access,
                    refresh_token: refresh,
                    note_hint: String::new(),
                });
            }
        }
    }
    if let Some(path) = credentials_path() {
        if path.exists() {
            found_source = true;
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Some((access, refresh)) = parse_credentials(&text) {
                    return accounts::LocalLoginRead::Found(accounts::LocalLogin {
                        kind: "claude".into(),
                        token: access,
                        refresh_token: refresh,
                        note_hint: String::new(),
                    });
                }
            }
        }
    }
    if found_source {
        accounts::LocalLoginRead::Invalid
    } else {
        accounts::LocalLoginRead::Missing
    }
}

/// 组装并写入本机凭据（macOS 写 Keychain，文件存在也同步更新；其它平台写文件）。
fn write_local_credentials(
    access_token: &str,
    refresh_token: &str,
    expires_at_ms: Option<i64>,
    subscription_type: Option<&str>,
) -> Result<(), String> {
    let mut oauth = Map::new();
    oauth.insert("accessToken".into(), json!(access_token));
    oauth.insert("refreshToken".into(), json!(refresh_token));
    if let Some(ms) = expires_at_ms {
        oauth.insert("expiresAt".into(), json!(ms));
    }
    let scopes: Vec<&str> = claude_oauth::SCOPE.split(' ').collect();
    oauth.insert("scopes".into(), json!(scopes));
    if let Some(sub) = subscription_type.map(str::trim).filter(|s| !s.is_empty()) {
        oauth.insert("subscriptionType".into(), json!(sub));
    }
    let value = json!({ "claudeAiOauth": Value::Object(oauth) });
    let text = serde_json::to_string_pretty(&value)
        .map_err(|e| format!("claude_creds_write_failed: {e}"))?;

    let path = credentials_path();
    if cfg!(target_os = "macos") {
        let keychain_ok = keychain_write(&text);
        // 本机若同时存在凭据文件（Keychain 被拒时 Claude Code 会退回文件），保持两处一致
        let mut file_ok = false;
        if let Some(p) = &path {
            if p.exists() || !keychain_ok {
                if let Some(pp) = p.exists().then_some(p) {
                    let backup = pp.with_extension("json.bak");
                    let _ = std::fs::copy(pp, backup);
                }
                file_ok = write_credentials_file(p, &text).is_ok();
            }
        }
        if !keychain_ok && !file_ok {
            return Err("claude_creds_write_failed: keychain_and_file_failed".into());
        }
        return Ok(());
    }
    let Some(path) = path else {
        return Err("claude_creds_write_failed: home_dir_unavailable".into());
    };
    if path.exists() {
        let backup = path.with_extension("json.bak");
        std::fs::copy(&path, &backup)
            .map_err(|e| format!("claude_creds_write_failed: backup_failed: {e}"))?;
    }
    write_credentials_file(&path, &text)
}

// ---------------------------------------------------------------------------
// Claude Desktop 客户端探测 / 进程 / 关闭 / 启动
// ---------------------------------------------------------------------------

const CLAUDE_BUNDLE_ID: &str = "com.anthropic.claudefordesktop";

/// Windows：AnthropicClaude 安装目录下最新的 app-x.y.z 版本目录。
fn windows_latest_app_dir(base: &Path) -> Option<PathBuf> {
    let mut best: Option<(String, PathBuf)> = None;
    for entry in std::fs::read_dir(base).ok()?.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("app-") || !entry.path().is_dir() {
            continue;
        }
        match &best {
            Some((bn, _)) if bn.as_str() >= name.as_str() => {}
            _ => best = Some((name, entry.path())),
        }
    }
    best.map(|(_, p)| p)
}

/// 常见安装位置候选（按优先级，不检查存在性）。
fn detect_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "windows") {
        if let Some(local) = paths::env_nonempty("LOCALAPPDATA").or_else(paths::data_local_dir) {
            let base = local.join("AnthropicClaude");
            out.push(base.join("claude.exe"));
            if let Some(app) = windows_latest_app_dir(&base) {
                out.push(app.join("claude.exe"));
                out.push(app.join("Claude.exe"));
            }
        }
    } else if cfg!(target_os = "macos") {
        out.push(PathBuf::from("/Applications/Claude.app"));
        if let Some(home) = paths::home_dir() {
            out.push(home.join("Applications").join("Claude.app"));
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
        .ok_or_else(|| "claude_exe_not_found".to_string())
}

fn matches_desktop(p: &process::ProcessInfo) -> bool {
    let Some(exe) = p.exe.as_ref() else {
        return false;
    };
    if cfg!(target_os = "windows") {
        let lower = exe.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
        if lower.contains(r"\anthropicclaude\") {
            return true;
        }
        let configured = current().exe_path.trim().to_string();
        if !configured.is_empty() {
            let cfg_path = PathBuf::from(&configured);
            if cfg_path.exists() {
                if exe == cfg_path.as_path() || path_starts_with_ci(exe, &cfg_path) {
                    return true;
                }
                if let Some(parent) = cfg_path.parent() {
                    if path_starts_with_ci(exe, parent) {
                        return true;
                    }
                }
            }
        }
        false
    } else if cfg!(target_os = "macos") {
        exe.to_string_lossy()
            .to_ascii_lowercase()
            .contains("/claude.app/")
    } else {
        false
    }
}

fn desktop_pids() -> Vec<u32> {
    process::list_processes()
        .into_iter()
        .filter(matches_desktop)
        .map(|p| p.pid)
        .collect()
}

fn is_desktop_running() -> bool {
    !desktop_pids().is_empty()
}

/// 关闭 Claude Desktop：先优雅退出，约 8 秒超时后强制结束，再等约 3 秒确认。
fn close_desktop() -> bool {
    if !is_desktop_running() {
        return true;
    }
    if cfg!(target_os = "windows") {
        kill_pids(&desktop_pids(), false);
        if !wait_exit(is_desktop_running, Duration::from_secs(8)) {
            kill_pids(&desktop_pids(), true);
            let _ = wait_exit(is_desktop_running, Duration::from_secs(3));
        }
    } else if cfg!(target_os = "macos") {
        let _ = Command::new("osascript")
            .args(["-e", &format!("tell application id \"{CLAUDE_BUNDLE_ID}\" to quit")])
            .output();
        if !wait_exit(is_desktop_running, Duration::from_secs(8)) {
            kill_pids(&desktop_pids(), true);
            let _ = wait_exit(is_desktop_running, Duration::from_secs(3));
        }
    }
    !is_desktop_running()
}

fn launch_desktop() -> bool {
    let Ok(exe) = resolve_exe_path() else {
        return false;
    };
    if cfg!(target_os = "windows") {
        launch_detached_windows(&exe)
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg(&exe).spawn().is_ok()
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// 命令
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeClientView {
    pub exe_path: String,
    pub path: String,
}

fn view(cfg: &ClaudeClientConfig) -> ClaudeClientView {
    ClaudeClientView {
        exe_path: cfg.exe_path.clone(),
        path: settings::path_display(),
    }
}

#[tauri::command]
pub fn claude_client_get() -> Result<ClaudeClientView, String> {
    settings::ensure_loaded()?;
    Ok(view(&current()))
}

#[tauri::command]
pub fn claude_client_set(exe_path: String) -> Result<ClaudeClientView, String> {
    let exe_path = exe_path.trim().to_string();
    if !exe_path.is_empty() && !Path::new(&exe_path).exists() {
        return Err("claude_exe_invalid".into());
    }
    let cfg = ClaudeClientConfig { exe_path };
    settings::mutate(|s| {
        s.claude_client = cfg.clone();
        Ok(())
    })?;
    Ok(view(&cfg))
}

/// 探测常见安装位置，不写配置。
#[tauri::command]
pub fn claude_client_detect() -> DetectResult {
    DetectResult {
        exe_path: resolve_exe_path()
            .ok()
            .map(|p| p.to_string_lossy().to_string()),
    }
}

#[tauri::command]
pub async fn claude_client_status() -> Result<ClientStatus, String> {
    let exe_path = resolve_exe_path()
        .ok()
        .map(|p| p.to_string_lossy().to_string());
    Ok(ClientStatus {
        running: is_desktop_running(),
        exe_configured: exe_path.is_some(),
        exe_path,
    })
}

#[tauri::command]
pub async fn claude_client_close() -> Result<CloseResult, String> {
    let closed = tauri::async_runtime::spawn_blocking(close_desktop)
        .await
        .map_err(|e| e.to_string())?;
    Ok(CloseResult { closed })
}

#[tauri::command]
pub fn claude_client_launch() -> Result<LaunchResult, String> {
    Ok(LaunchResult {
        launched: launch_desktop(),
    })
}

/// 切换本机 Claude Code 登录：用账户 refresh_token 换取全新凭据组后写入本机
/// （macOS Keychain / 其它平台 .credentials.json）。写入前要求 Claude Desktop
/// 已关闭（关闭由 claude_client_close 单独负责），与 Cursor / Codex 切号一致。
#[tauri::command]
pub async fn claude_switch_local(app: AppHandle, id: String) -> Result<SwitchResult, String> {
    if !cfg!(target_os = "windows") && !cfg!(target_os = "macos") {
        return Err("unsupported_platform".into());
    }

    // 与账户刷新共用同一全局队列：切换同样会消费并轮换 refresh_token。
    let _queued = accounts::refresh_queue().lock().await;

    let acc = accounts::account_snapshot(&id)?;
    if acc.kind != "claude" {
        return Err("not_claude_account".into());
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
        None => return Err("claude_no_refresh_token".into()),
    };

    let name = accounts::short_display_name(&acc);
    let subscription = acc
        .status
        .as_ref()
        .and_then(|s| s.get("plan"))
        .and_then(|x| x.as_str())
        .map(str::to_string);

    let (token, new_rt, expires_at_ms) = match claude_oauth::request_claude_refresh(&rt).await? {
        claude_oauth::ClaudeRefreshOutcome::Denied { body } => {
            audit::log(
                "claude_renew_failed",
                format!("Claude 续期被拒绝：{name}（{body}）"),
                Some(json!({ "id": id })),
            );
            return Err("claude_refresh_denied".into());
        }
        claude_oauth::ClaudeRefreshOutcome::Success {
            access_token,
            refresh_token,
            expires_at_ms,
            ..
        } => (access_token, refresh_token, expires_at_ms),
    };

    // 轮换后旧 refresh_token 已作废，无论后续成败先把新凭据写回账户。
    let refresh_token = new_rt.unwrap_or(rt);
    accounts::persist_tokens(&app, &id, &token, &Some(refresh_token.clone()))?;

    if is_desktop_running() {
        return Err("claude_running".into());
    }

    write_local_credentials(&token, &refresh_token, expires_at_ms, subscription.as_deref())?;

    audit::log(
        "claude_switch_local",
        format!("切换本机 Claude Code 登录：{name}"),
        Some(json!({ "id": id })),
    );

    Ok(SwitchResult {
        switched: true,
        message: "已写入本地登录。".into(),
    })
}

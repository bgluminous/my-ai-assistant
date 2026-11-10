//! V2 本机集成：只读身份导入，写操作全部交给官方 CLI 的凭据 API。
use crate::{accounts, opencode, paths, settings};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeClientConfig {
    #[serde(default)]
    pub exe_path: String,
    #[serde(default)]
    pub db_path: String,
}

fn config() -> OpenCodeClientConfig {
    settings::read(|s| s.opencode_client.clone()).unwrap_or_default()
}

fn detect() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = paths::home_dir() {
        candidates.push(home.join(".opencode/bin").join(if cfg!(windows) {
            "opencode.exe"
        } else {
            "opencode"
        }));
    }
    if cfg!(target_os = "macos") {
        candidates.extend(
            [
                "/opt/homebrew/bin/opencode",
                "/usr/local/bin/opencode",
                "/Applications/OpenCode.app/Contents/Resources/opencode",
            ]
            .map(PathBuf::from),
        );
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            candidates.push(dir.join(if cfg!(windows) {
                "opencode.exe"
            } else {
                "opencode"
            }));
        }
    }
    if let Some(dir) = paths::env_nonempty("LOCALAPPDATA") {
        for relative in [
            "Programs/OpenCode/resources/opencode.exe",
            "OpenCode/opencode.exe",
            "Microsoft/WinGet/Links/opencode.exe",
        ] {
            candidates.push(dir.join(relative));
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn executable() -> Result<PathBuf, String> {
    let configured = config().exe_path;
    let path = if configured.trim().is_empty() {
        detect().ok_or("opencode_exe_not_found")?
    } else {
        PathBuf::from(configured.trim())
    };
    if !path.is_file() {
        return Err("opencode_exe_invalid".into());
    }
    if cfg!(windows)
        && !path
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("exe"))
    {
        return Err("opencode_exe_invalid".into());
    }
    Ok(path)
}

/// 有界命令执行。并行排空输出，避免凭据列表填满管道；输出/错误不进入日志或错误文案。
fn run(args: &[&str]) -> Result<String, String> {
    #[cfg(test)]
    if let Some(result) = tests::run_fixture(args) {
        return result;
    }
    let mut command = Command::new(executable()?);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(home) = paths::home_dir() {
        command.current_dir(home);
    }
    let cfg = config();
    if !cfg.db_path.trim().is_empty() {
        command.env("OPENCODE_DB", cfg.db_path.trim());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().map_err(|_| "opencode_cli_failed")?;
    let stdout = child.stdout.take().ok_or("opencode_cli_failed")?;
    let stderr = child.stderr.take().ok_or("opencode_cli_failed")?;
    let out = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stdout.take(16 * 1024 * 1024 + 1).read_to_end(&mut b);
        b
    });
    let err = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stderr.take(1024 * 1024).read_to_end(&mut b);
    });
    let deadline = Instant::now() + Duration::from_secs(45);
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                break if status.success() {
                    Ok(())
                } else {
                    Err("opencode_cli_failed")
                }
            }
            Err(_) => break Err("opencode_cli_failed"),
            _ if Instant::now() >= deadline => break Err("opencode_cli_timeout"),
            _ => std::thread::sleep(Duration::from_millis(30)),
        }
    };
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let bytes = out.join().unwrap_or_default();
    let _ = err.join();
    result?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("opencode_cli_output_too_large".into());
    }
    String::from_utf8(bytes).map_err(|_| "opencode_cli_invalid_response".into())
}

fn require_v2() -> Result<(), String> {
    let version = run(&["--version"])?;
    let version = version.trim().strip_prefix('v').unwrap_or(version.trim());
    if version
        .split('.')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        != Some(2)
    {
        return Err("opencode_v2_required".into());
    }
    Ok(())
}

fn api(method: &str, path: &str, data: Option<Value>) -> Result<Value, String> {
    let body = data.map(|d| d.to_string());
    let mut args = vec!["api", method, path];
    if let Some(body) = body.as_deref() {
        args.extend(["--data", body]);
    }
    let text = run(&args)?;
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(text.trim()).map_err(|_| "opencode_cli_invalid_response".into())
}

fn database() -> Result<PathBuf, String> {
    let cfg = config();
    if !cfg.db_path.trim().is_empty() {
        return Ok(PathBuf::from(cfg.db_path.trim()));
    }
    if executable().is_ok() {
        require_v2()?;
        let path = run(&["debug", "paths", "db"])?;
        if path.trim() == ":memory:" {
            return Err("opencode_database_unavailable".into());
        }
        return Ok(PathBuf::from(path.trim()));
    }
    let root = paths::env_nonempty("XDG_DATA_HOME")
        .or_else(|| paths::home_dir().map(|p| p.join(".local/share")))
        .ok_or("home_dir_unavailable")?
        .join("opencode");
    match paths::env_nonempty("OPENCODE_DB") {
        Some(path) if path.is_absolute() => Ok(path),
        Some(path) if path.to_string_lossy() == ":memory:" => {
            Err("opencode_database_unavailable".into())
        }
        Some(path) => Ok(root.join(path)),
        None => Ok(root.join("opencode.db")),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalIdentity {
    pub credential_id: String,
    pub workspace_id: String,
    pub name: String,
    pub email: String,
}

pub fn identity(id: &str, value: &Value) -> Option<LocalIdentity> {
    if opencode::text(value, "type") != "oauth" || !opencode::valid_workspace(id) {
        return None;
    }
    let metadata = value.get("metadata")?;
    let server = opencode::text(metadata, "server").trim_end_matches('/');
    if !matches!(
        server,
        "https://opencode.ai/console" | "https://console.opencode.ai"
    ) {
        return None;
    }
    let workspace = opencode::text(metadata, "orgID");
    if !opencode::valid_workspace(workspace) {
        return None;
    }
    Some(LocalIdentity {
        credential_id: id.into(),
        workspace_id: workspace.into(),
        name: opencode::text(metadata, "orgName").into(),
        email: opencode::text(metadata, "email").into(),
    })
}

pub fn read_identities() -> Result<Vec<LocalIdentity>, String> {
    let path = database()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| "opencode_database_unavailable")?;
    db.busy_timeout(Duration::from_secs(3))
        .map_err(|_| "opencode_database_unavailable")?;
    let mut statement = db.prepare("SELECT id,value FROM credential WHERE integration_id='opencode' ORDER BY active DESC,time_updated DESC")
        .map_err(|_| "opencode_v2_required")?;
    let values = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|_| "opencode_database_unavailable")?;
    let mut found = Vec::new();
    for value in values {
        let (id, raw) = value.map_err(|_| "opencode_database_unavailable")?;
        if let Ok(value) = serde_json::from_str::<Value>(&raw) {
            if let Some(identity) = identity(&id, &value) {
                if !found
                    .iter()
                    .any(|i: &LocalIdentity| i.workspace_id == identity.workspace_id)
                {
                    found.push(identity);
                }
            }
        }
    }
    Ok(found)
}

#[tauri::command]
pub async fn opencode_local_accounts() -> Result<Vec<LocalIdentity>, String> {
    let _operation = crate::operations::begin()?;
    settings::ensure_loaded()?;
    tauri::async_runtime::spawn_blocking(read_identities)
        .await
        .map_err(|_| "opencode_database_unavailable")?
}

#[tauri::command]
pub fn opencode_client_get() -> Result<Value, String> {
    settings::ensure_loaded()?;
    Ok(serde_json::to_value(config()).map_err(|_| "opencode_invalid_config")?)
}
#[tauri::command]
pub fn opencode_client_detect() -> Value {
    json!({"exePath":detect().map(|p|p.to_string_lossy().to_string())})
}

/// 所有客户端路径写入共用此入口；检查与持久化都在原生操作锁内，避免授权启动时换路径。
/// 待完成的授权（包括取消/回滚失败）必须先清理，备份也不能越过此检查。
pub(crate) fn with_client_config_update<T>(update: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let _guard = native_lock().try_lock().map_err(|_| "opencode_client_auth_pending")?;
    if !attempts()
        .lock()
        .map_err(|_| "state_lock_poisoned")?
        .is_empty()
    {
        return Err("opencode_client_auth_pending".into());
    }
    update()
}

#[tauri::command]
pub fn opencode_client_set(exe_path: String, db_path: Option<String>) -> Result<Value, String> {
    let _operation = crate::operations::begin()?;
    with_client_config_update(|| set_client_config(exe_path, db_path))
}

fn set_client_config(exe_path: String, db_path: Option<String>) -> Result<Value, String> {
    let exe_path = exe_path.trim().to_string();
    if !exe_path.is_empty()
        && (!std::path::Path::new(&exe_path).is_file()
            || cfg!(windows) && !exe_path.to_ascii_lowercase().ends_with(".exe"))
    {
        return Err("opencode_exe_invalid".into());
    }
    if db_path.as_deref().is_some_and(|p| {
        !p.trim().is_empty()
            && (!std::path::Path::new(p.trim()).is_absolute()
                || !std::path::Path::new(p.trim()).is_file())
    }) {
        return Err("opencode_database_unavailable".into());
    }
    settings::mutate(|s| {
        s.opencode_client.exe_path = exe_path;
        if let Some(path) = db_path {
            s.opencode_client.db_path = path.trim().into();
        }
        Ok(())
    })?;
    opencode_client_get()
}

fn credentials() -> Result<Vec<LocalIdentity>, String> {
    let body = api("get", "/api/credential", None)?;
    let rows = body.as_array().ok_or("opencode_cli_invalid_response")?;
    Ok(rows
        .iter()
        .filter(|v| opencode::text(v, "integrationID") == "opencode")
        .filter_map(|v| identity(opencode::text(v, "id"), &v["value"]))
        .collect())
}

fn activate(id: &str) -> Result<(), String> {
    if !opencode::valid_workspace(id) {
        return Err("opencode_cli_invalid_response".into());
    }
    api("post", &format!("/api/credential/{id}/activate"), None)?;
    // SQLite active 字段由官方 API 写入，回读仅用于确认，不直接修改该库。
    let db = Connection::open_with_flags(database()?, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| "opencode_database_unavailable")?;
    let active: Option<String> = db.query_row(
        "SELECT id FROM credential WHERE integration_id='opencode' AND active=1 ORDER BY time_created DESC LIMIT 1",
        [], |row|row.get(0)).ok();
    if active.as_deref() != Some(id) {
        return Err("opencode_switch_not_confirmed".into());
    }
    Ok(())
}

#[derive(Clone)]
struct NativeAttempt {
    workspace: String,
    previous: Option<String>,
    deadline: i64,
}
fn attempts() -> &'static Mutex<HashMap<String, NativeAttempt>> {
    static ATTEMPTS: OnceLock<Mutex<HashMap<String, NativeAttempt>>> = OnceLock::new();
    ATTEMPTS.get_or_init(|| Mutex::new(HashMap::new()))
}
fn native_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}
fn active_id() -> Result<Option<String>, String> {
    use rusqlite::OptionalExtension;
    let db = Connection::open_with_flags(database()?, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| "opencode_database_unavailable")?;
    db.query_row("SELECT id FROM credential WHERE integration_id='opencode' AND active=1 ORDER BY time_created DESC LIMIT 1",[],|r|r.get(0)).optional().map_err(|_|"opencode_database_unavailable".into())
}

#[tauri::command]
pub async fn opencode_switch_local(id: String) -> Result<Value, String> {
    let _operation = crate::operations::begin()?;
    let _guard = native_lock().lock().await;
    let account = accounts::account_snapshot(&id)?;
    if account.kind != "opencode" {
        return Err("not_opencode_account".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        require_v2()?;
        if !attempts().lock().map_err(|_| "state_lock_poisoned")?.is_empty() { return Err("opencode_client_auth_pending".into()); }
        if let Some(c) = credentials()?.into_iter().find(|c| c.workspace_id == account.token) {
            activate(&c.credential_id)?;
            return Ok(json!({"state":"complete"}));
        }
        let previous = active_id()?;
        let result = api("post","/api/integration/opencode/connect/oauth",Some(json!({"methodID":"device"})))?;
        let attempt = opencode::text(&result,"attemptID");
        let url = opencode::authorization_url(opencode::text(&result,"url"))?;
        if !opencode::valid_workspace(attempt) { return Err("opencode_cli_invalid_response".into()); }
        attempts().lock().map_err(|_| "state_lock_poisoned")?.insert(attempt.into(),NativeAttempt{workspace:account.token,previous,deadline:result.pointer("/time/expires").and_then(Value::as_i64).unwrap_or_else(||chrono::Utc::now().timestamp_millis()+15*60_000)});
        let opened = opencode::open_browser(&url);
        Ok(json!({"state":"pending","id":attempt,"url":url,"instructions":result["instructions"],"opened":opened}))
    }).await.map_err(|_| "opencode_cli_failed")?
}

#[tauri::command]
pub async fn opencode_client_oauth_poll(id: String, cancel: bool) -> Result<Value, String> {
    let _operation = crate::operations::begin()?;
    let _guard = native_lock().lock().await;
    tauri::async_runtime::spawn_blocking(move || {
        let pending = attempts()
            .lock()
            .map_err(|_| "state_lock_poisoned")?
            .get(&id)
            .cloned();
        let Some(pending) = pending else {
            return if cancel {
                Ok(json!({"state":"cancelled"}))
            } else {
                Err("opencode_oauth_expired".into())
            };
        };
        let expired = chrono::Utc::now().timestamp_millis() >= pending.deadline;
        let path = format!("/api/integration/opencode/connect/oauth/{id}");
        let result = api(
            if cancel || expired { "delete" } else { "get" },
            &path,
            None,
        )?;
        let state = opencode::text(&result, "status");
        if !cancel && !expired && state == "pending" {
            return Ok(json!({"state":"pending"}));
        }
        let completion = if !cancel && !expired && state == "complete" {
            credentials().and_then(|all| {
                let login = all
                    .iter()
                    .find(|c| c.workspace_id == pending.workspace)
                    .ok_or("opencode_workspace_mismatch")?;
                activate(&login.credential_id)
            })
        } else {
            Err(if expired {
                "opencode_oauth_expired"
            } else {
                "opencode_client_auth_failed"
            }
            .into())
        };
        if completion.is_err() {
            // 恢复成功前保留 attempt，关闭时可以重试；不能提前丢弃回滚所需的原登录。
            if let Some(previous) = pending.previous {
                activate(&previous)?;
            }
        }
        attempts()
            .lock()
            .map_err(|_| "state_lock_poisoned")?
            .remove(&id);
        if cancel {
            Ok(json!({"state":"cancelled"}))
        } else {
            completion.map(|_| json!({"state":"complete"}))
        }
    })
    .await
    .map_err(|_| "opencode_cli_failed")?
}

#[cfg(test)]
#[path = "tests/opencode_local.rs"]
pub(crate) mod tests;

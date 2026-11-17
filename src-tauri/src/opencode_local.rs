//! V2 本机集成：只读身份导入，写操作全部交给官方 CLI 的凭据 API。
use crate::{accounts, opencode, paths, settings};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    sync::OnceLock,
    time::{Duration, Instant},
};
#[cfg(test)]
use std::sync::Mutex;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeClientConfig {
    #[serde(default)]
    pub exe_path: String,
    #[serde(default)]
    pub db_path: String,
    #[serde(default)]
    pub desktop_exe_path: String,
    #[serde(default)]
    pub desktop_db_path: String,
}

pub(crate) fn config() -> OpenCodeClientConfig {
    settings::read(|s| s.opencode_client.clone()).unwrap_or_default()
}

pub(crate) fn standalone_cli() -> Result<Option<PathBuf>, String> {
    let cfg = config();
    let desktop=desktop_executable().ok().flatten();
    let known_desktops=desktop_candidates();
    let bundled=desktop.as_ref().and_then(|app|bundled_cli(app));
    for path in crate::local_client::cli_candidates("opencode", &cfg.exe_path) {
        if desktop.as_ref().is_some_and(|app|crate::local_client::same_path(app,&path))
            || known_desktops.iter().any(|app|crate::local_client::same_path(app,&path)) {
            if !cfg.exe_path.is_empty() {return Err("opencode_gui_is_not_cli".into());}
            continue;
        }
        if cfg.exe_path.is_empty() && bundled.as_ref().is_some_and(|exe|crate::local_client::same_path(exe,&path)) {continue;}
        if cfg!(windows) && !path.extension().is_some_and(|s|s.eq_ignore_ascii_case("exe")) {continue;}
        return Ok(Some(path));
    }
    if !cfg.exe_path.is_empty() {return Err("opencode_exe_invalid".into());}
    Ok(None)
}
pub(crate) fn desktop_executable() -> Result<Option<PathBuf>, String> {
    let cfg = config();
    if !cfg.desktop_exe_path.is_empty() {
        let path = PathBuf::from(cfg.desktop_exe_path);
        if !crate::local_client::valid_desktop_path(&path) { return Err("opencode_desktop_exe_invalid".into()); }
        return Ok(Some(path));
    }
    Ok(desktop_candidates().into_iter().find(|p|crate::local_client::valid_desktop_path(p)))
}
fn desktop_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if cfg!(windows) {
        if let Some(local) = paths::env_nonempty("LOCALAPPDATA") {
            for relative in ["Programs/ai.opencode.desktop/OpenCode.exe", "Programs/OpenCode/OpenCode.exe", "OpenCode/OpenCode.exe"] {
                candidates.push(local.join(relative));
            }
        }
    } else if cfg!(target_os="macos") {
        candidates.push(PathBuf::from("/Applications/OpenCode.app"));
        if let Some(home)=paths::home_dir(){candidates.push(home.join("Applications/OpenCode.app"));}
    }
    candidates
}
pub(crate) fn bundled_cli(app: &std::path::Path) -> Option<PathBuf> {
    let base = if app.extension().is_some_and(|s|s.eq_ignore_ascii_case("app")) {app.join("Contents/Resources")}
        else {app.parent()?.join("resources")};
    ["opencode-cli.exe", "opencode-cli", "opencode.exe", "opencode"].iter().map(|name|base.join(name)).find(|p|p.is_file())
}
fn detect() -> Option<PathBuf> {
    standalone_cli().ok().flatten().or_else(||desktop_executable().ok().flatten().and_then(|app|bundled_cli(&app)))
}
fn executable() -> Result<PathBuf, String> { detect().ok_or_else(||"opencode_exe_not_found".into()) }

/// 有界命令执行。并行排空输出，避免凭据列表填满管道；输出/错误不进入日志或错误文案。
fn run(args: &[&str]) -> Result<String, String> {
    #[cfg(test)] if let Some(result) = tests::run_fixture(args) { return result; }
    run_executable(&executable()?, &config().db_path, args)
}
pub(crate) fn run_executable(exe: &std::path::Path, db: &str, args: &[&str]) -> Result<String, String> {
    #[cfg(test)]
    if let Some(result) = tests::run_fixture(args) {
        return result;
    }
    let mut command = Command::new(exe);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(home) = paths::home_dir() {
        command.current_dir(home);
    }
    if !db.trim().is_empty() { command.env("OPENCODE_DB", db.trim()); }
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
    #[cfg(test)]
    if tests::run_fixture(&["--version"]).is_some() {
        return read_identity_databases(&[database()?]);
    }
    let cfg=config();
    let mut databases=Vec::new();
    if let Some(cli)=standalone_cli()? {
        databases.push(database_for(&cli,&cfg.db_path)?);
    } else if !cfg.db_path.trim().is_empty() {
        databases.push(PathBuf::from(cfg.db_path.trim()));
    }
    if let Some(app)=desktop_executable()? {
        let cli=bundled_cli(&app).ok_or("opencode_desktop_cli_not_found")?;
        let db=database_for(&cli,&cfg.desktop_db_path)?;
        if !databases.iter().any(|p|crate::local_client::same_path(p,&db)) {databases.push(db);}
    }
    if databases.is_empty() {databases.push(database()?);}
    read_identity_databases(&databases)
}
fn read_identity_databases(paths: &[PathBuf]) -> Result<Vec<LocalIdentity>, String> {
    let mut found=Vec::new();
    for path in paths {
        for identity in read_identities_at(path)? {
            if !found.iter().any(|i: &LocalIdentity|i.workspace_id==identity.workspace_id) {found.push(identity);}
        }
    }
    Ok(found)
}
fn read_identities_at(path: &std::path::Path) -> Result<Vec<LocalIdentity>, String> {
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
    json!({"exePath":standalone_cli().ok().flatten().map(|p|p.to_string_lossy().to_string()),
        "desktopExePath":desktop_executable().ok().flatten().map(|p|p.to_string_lossy().to_string())})
}

/// 所有客户端路径写入共用此入口；检查与持久化都在原生操作锁内，避免授权启动时换路径。
/// 待完成的授权（包括取消/回滚失败）必须先清理，备份也不能越过此检查。
pub(crate) fn with_client_config_update<T>(update: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let _guard = native_lock().try_lock().map_err(|_| "opencode_client_auth_pending")?;
    let _lease = crate::linked_clients::lease("opencode")?;
    crate::opencode_clients::ensure_idle()?;
    update()
}

#[tauri::command]
pub fn opencode_client_set(exe_path: String, db_path: Option<String>, desktop_exe_path: Option<String>, desktop_db_path: Option<String>) -> Result<Value, String> {
    let _operation = crate::operations::begin()?;
    with_client_config_update(|| set_client_config(exe_path, db_path, desktop_exe_path, desktop_db_path))
}

fn set_client_config(exe_path: String, db_path: Option<String>, desktop_exe_path: Option<String>, desktop_db_path: Option<String>) -> Result<Value, String> {
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
    if let Some(path) = &desktop_exe_path {
        if !path.trim().is_empty() && !crate::local_client::valid_desktop_path(std::path::Path::new(path.trim())) {
            return Err("opencode_desktop_exe_invalid".into());
        }
    }
    if let Some(path) = &desktop_db_path {
        if !path.trim().is_empty() && (!std::path::Path::new(path.trim()).is_absolute() || !std::path::Path::new(path.trim()).is_file()) {
            return Err("opencode_database_unavailable".into());
        }
    }
    settings::mutate(|s| {
        s.opencode_client.exe_path = exe_path;
        if let Some(path) = desktop_exe_path { s.opencode_client.desktop_exe_path = path.trim().into(); }
        if let Some(path) = desktop_db_path { s.opencode_client.desktop_db_path = path.trim().into(); }
        if let Some(path) = db_path {
            s.opencode_client.db_path = path.trim().into();
        }
        Ok(())
    })?;
    opencode_client_get()
}

fn native_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(||tokio::sync::Mutex::new(()))
}
pub(crate) fn database_for(exe: &std::path::Path, configured: &str) -> Result<PathBuf, String> {
    let version=run_executable(exe,configured,&["--version"])?;
    let version=version.trim().strip_prefix("opencode ").unwrap_or(version.trim()).trim_start_matches('v');
    if version.split('.').next()!=Some("2"){return Err("opencode_v2_required".into());}
    let path=if configured.trim().is_empty(){run_executable(exe,"",&["debug","paths","db"])?}else{configured.trim().into()};
    let path=PathBuf::from(path.trim());
    if !path.is_absolute(){return Err("opencode_database_unavailable".into());}
    Ok(path)
}
pub(crate) fn desktop_running(app: &std::path::Path) -> bool { !desktop_pids(app).is_empty() }
fn desktop_pids(app: &std::path::Path) -> Vec<u32> {
    crate::process::list_processes().into_iter().filter(|p|p.exe.as_ref().is_some_and(|exe|
        if app.is_dir(){crate::local_client::path_starts_with_ci(exe,app) && exe.to_string_lossy().to_ascii_lowercase().contains("/contents/macos/")}else{crate::local_client::same_path(exe,app)})).map(|p|p.pid).collect()
}
pub(crate) fn close_desktop(app: &std::path::Path) -> bool {
    let roots=desktop_pids(app);
    crate::local_client::close_family(&roots,||crate::local_client::kill_pids(&roots,false))
}

pub(crate) fn launch_desktop(app: &std::path::Path) -> bool {
    #[cfg(test)] if let Some(result)=tests::launch_fixture() { return result; }
    let launched=if cfg!(windows){crate::local_client::launch_detached_windows(app)}
        else if cfg!(target_os="macos"){Command::new("open").arg(app).spawn().is_ok()}else{false};
    launched && crate::local_client::wait_exit(||!desktop_running(app),Duration::from_secs(15))
}
pub(crate) fn linked_status() -> Result<Value,String> {
    let cli_result=standalone_cli();let desktop_result=desktop_executable();
    let cli=cli_result.as_ref().ok().cloned().flatten();let desktop=desktop_result.as_ref().ok().cloned().flatten();
    let running=desktop.as_ref().is_some_and(|p|desktop_running(p));
    let mut result=crate::local_client::linked_status(cli,desktop.clone(),running);
    for (endpoint,error) in [("cli",cli_result.err()),("desktop",desktop_result.err())] {
        if let Some(error)=error {result[endpoint]["switchable"]=json!(false);result[endpoint]["error"]=json!(error);}
    }
    if let Some(app)=desktop {
        if bundled_cli(&app).is_none(){result["desktop"]["switchable"]=json!(false);result["desktop"]["error"]=json!("opencode_desktop_cli_not_found");}
    }
    Ok(result)
}
pub(crate) async fn switch_linked(id:String,close_desktop:bool,target:Option<crate::local_client::SwitchTarget>)->Result<Value,String>{
    let _guard=native_lock().lock().await;
    crate::opencode_clients::start(id,close_desktop,target).await
}
#[tauri::command]
pub async fn opencode_switch_local(id:String)->Result<Value,String>{
    let _operation=crate::operations::begin()?;
    let _lease=crate::linked_clients::lease("opencode")?;
    let _queue=accounts::refresh_queue("opencode")?.lock().await;
    switch_linked(id,false,None).await
}
#[tauri::command]
pub async fn opencode_client_oauth_poll(id:String,cancel:bool)->Result<Value,String>{
    let _operation=crate::operations::begin()?;
    let _lease=crate::linked_clients::lease("opencode")?;
    let _queue=accounts::refresh_queue("opencode")?.lock().await;
    let _guard=native_lock().lock().await;
    crate::opencode_clients::poll(id,cancel).await
}

#[cfg(test)]
#[path = "tests/opencode_local.rs"]
pub(crate) mod tests;

//! 本机客户端（Cursor / ChatGPT / Claude Desktop）集成的共享部分：探测、关闭、启动用到的
//! 平台工具，以及三个客户端模块向前端返回的同构结果类型。
//!
//! 跨平台策略与各客户端模块一致：路径 / 进程判断用 `cfg!(...)` 运行时分支，两个平台的分支
//! 都参与编译；只有必须调用 Windows 专用 API 的地方才用 `#[cfg(windows)]` 条件编译。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// 执行命令并收集输出，Windows 下不弹出控制台窗口（taskkill / powershell 等会闪黑窗）。
#[cfg(windows)]
pub fn run_hidden(cmd: &mut Command) -> std::io::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW).output()
}

#[cfg(not(windows))]
pub fn run_hidden(cmd: &mut Command) -> std::io::Result<std::process::Output> {
    cmd.output()
}

/// Windows：以脱离本进程 Job / 进程组的方式启动客户端。
/// dev 及部分启动环境会把本应用连同子进程圈进 kill-on-close 的 Job Object，
/// 普通 spawn 出的客户端会在本应用退出时被连带结束。
/// 先带 CREATE_BREAKAWAY_FROM_JOB 启动脱离 Job；Job 禁止 breakaway 时该调用直接失败，
/// 回退用 explorer.exe 代理启动（新进程父为 explorer，同样不在本应用 Job 内）。
#[cfg(windows)]
pub fn launch_detached_windows(exe: &Path) -> bool {
    use std::os::windows::process::CommandExt;
    // CREATE_BREAKAWAY_FROM_JOB | CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS
    const DETACH_FLAGS: u32 = 0x0100_0000 | 0x0000_0200 | 0x0000_0008;
    if Command::new(exe).creation_flags(DETACH_FLAGS).spawn().is_ok() {
        return true;
    }
    Command::new("explorer.exe").arg(exe).spawn().is_ok()
}

/// 非 Windows 目标不编译上面的 Windows 专用 API；运行时分支也不会走到这里。
#[cfg(not(windows))]
pub fn launch_detached_windows(_exe: &Path) -> bool {
    false
}

/// 路径前缀匹配（不分大小写，`/` 与 `\` 等价）：path 等于 prefix，或位于 prefix 目录之下。
pub fn path_starts_with_ci(path: &Path, prefix: &Path) -> bool {
    let a = path.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
    let b = prefix
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    let b = b.trim_end_matches('\\');
    a == b || a.starts_with(&format!("{b}\\"))
}

/// 按 pid 结束进程：Windows 走 taskkill（force 时 /F /T 连整棵进程树），
/// macOS 走 kill -TERM / -9。命令退出码一律忽略，以进程是否消失为准。
pub fn kill_pids(pids: &[u32], force: bool) {
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
            let _ = Command::new("kill").args([sig, &pid.to_string()]).output();
        }
    }
}

/// 轮询等待客户端退出：每 300ms 调一次 `running`，直到返回 false 或超时。返回是否已退出。
pub fn wait_exit(running: impl Fn() -> bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if !running() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// `*_client_status`：客户端是否在运行、可执行文件是否可用及其解析出的路径。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientStatus {
    pub running: bool,
    pub exe_configured: bool,
    pub exe_path: Option<String>,
}

/// `*_client_close`：客户端是否已完全关闭。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseResult {
    pub closed: bool,
}

/// `*_client_launch`：是否成功拉起客户端（找不到可执行文件时为 false，不报错）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchResult {
    pub launched: bool,
}

/// `*_switch_local`：本机登录已写入。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchResult {
    pub switched: bool,
    pub message: String,
    /// ChatGPT：本次是否向 OpenAI 换取了新凭据（true），还是直接写入账户保存的 auth.json 副本（false）。
    /// Cursor / Claude 不区分，为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exchanged: Option<bool>,
}

/// `*_logout_local`：本机登录凭据已清除。was_logged_in=false 表示本机原本就没有登录，
/// 什么都没改；托管的账户不受影响。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogoutResult {
    pub was_logged_in: bool,
    pub message: String,
}

/// CLI detection never executes a shell shim, and never treats a GUI binary as a CLI.
pub fn cli_candidates(name: &str, configured: &str) -> Vec<PathBuf> {
    if !configured.trim().is_empty() {
        return PathBuf::from(configured.trim()).is_file().then(|| PathBuf::from(configured.trim())).into_iter().collect();
    }
    let mut dirs = Vec::new();
    if let Some(home) = crate::paths::home_dir() {
        dirs.extend([home.join(".local/bin"), home.join(".cargo/bin"), home.join(".opencode/bin")]);
    }
    if let Some(appdata) = crate::paths::config_dir() { dirs.push(appdata.join("npm")); }
    if let Some(path) = std::env::var_os("PATH") { dirs.extend(std::env::split_paths(&path)); }
    if cfg!(target_os = "macos") { dirs.extend([PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")]); }
    let mut found: Vec<PathBuf>=Vec::new();
    for dir in dirs {
        for suffix in if cfg!(windows) { vec![".exe", ".cmd", ".bat"] } else { vec![""] } {
            let path = dir.join(format!("{name}{suffix}"));
            if path.is_file() && !found.iter().any(|p|same_path(p,&path)) { found.push(path); }
        }
    }
    found
}

pub fn independent_cli(name: &str, configured: &str, desktop: Option<&Path>) -> Option<PathBuf> {
    cli_candidates(name,configured).into_iter().find(|cli|desktop.is_none_or(|app| {
        if same_path(cli,app) { return false; }
        let root=if app.is_dir() {Some(app)} else {app.parent()};
        !configured.trim().is_empty() || !root.is_some_and(|root|path_starts_with_ci(cli,root))
    }))
}

pub fn validate_optional_path(path: &str, directory: bool) -> Result<(), String> {
    let path = path.trim();
    if path.is_empty() { return Ok(()); }
    let p = Path::new(path);
    if !p.is_absolute() || (if directory { !p.is_dir() } else { !p.is_file() && !p.is_dir() }) {
        return Err("client_path_invalid".into());
    }
    Ok(())
}

pub fn valid_desktop_path(path: &Path) -> bool {
    if cfg!(windows) {
        path.is_file() && path.extension().is_some_and(|s|s.eq_ignore_ascii_case("exe"))
    } else if cfg!(target_os="macos") {
        path.is_dir() && path.extension().is_some_and(|s|s.eq_ignore_ascii_case("app"))
            && path.join("Contents/Info.plist").is_file()
    } else { false }
}

pub fn endpoint(path: Option<PathBuf>, running: bool) -> serde_json::Value {
    serde_json::json!({"installed":path.is_some(), "running":running, "switchable":path.is_some(),
        "exePath":path.map(|p| p.to_string_lossy().to_string())})
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SwitchTarget { Cli, Desktop, Both }

/// Omission preserves older callers; explicit choices cannot silently expand or
/// fall back to another endpoint when installation state changes while queued.
pub fn selected_clients(target: Option<SwitchTarget>, cli: bool, desktop: bool) -> Result<(bool, bool), String> {
    let selected = match target {
        None => (cli, desktop),
        Some(SwitchTarget::Cli) if cli => (true, false),
        Some(SwitchTarget::Desktop) if desktop => (false, true),
        Some(SwitchTarget::Both) if cli && desktop => (true, true),
        Some(_) => return Err("client_target_unavailable".into()),
    };
    if selected == (false, false) { return Err("client_not_installed".into()); }
    Ok(selected)
}

pub fn linked_status(cli: Option<PathBuf>, desktop: Option<PathBuf>, running: bool) -> serde_json::Value {
    let cli_running = cli.as_ref().is_some_and(|exe| crate::process::list_processes().iter()
        .any(|p| p.exe.as_ref().is_some_and(|p| same_path(p, exe))));
    serde_json::json!({"running":running, "exeConfigured":desktop.is_some(),
        "exePath":desktop.as_ref().map(|p|p.to_string_lossy().to_string()),
        "cli":endpoint(cli, cli_running), "desktop":endpoint(desktop, running)})
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    let a = a.canonicalize().unwrap_or_else(|_|a.to_path_buf());
    let b = b.canonicalize().unwrap_or_else(|_|b.to_path_buf());
    if cfg!(windows) { a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()) } else { a == b }
}

pub fn client_result(target: &str, result: Result<(), String>) -> serde_json::Value {
    match result {
        Ok(()) => serde_json::json!({"target":target,"status":"success"}),
        Err(error) => serde_json::json!({"target":target,"status":"failed","error":error}),
    }
}

pub fn switch_result(clients: Vec<serde_json::Value>, exchanged: Option<bool>) -> serde_json::Value {
    let switched = !clients.is_empty() && clients.iter().all(|c| c["status"] == "success");
    let any_success = clients.iter().any(|c| c["status"] == "success");
    serde_json::json!({"state":"complete", "switched":switched, "clients":clients, "exchanged":exchanged,
        "message":if switched {"已切换所有检测到的客户端。"} else if any_success {"部分客户端未完成切换。"} else {"客户端切换失败。"}})
}

/// Remember the GUI process family before graceful exit, so an orphaned app-server
/// is still stopped while an independent terminal session remains untouched.
pub fn close_family(roots: &[u32], quit: impl FnOnce()) -> bool {
    let owned=crate::process::family(&crate::process::list_processes(),roots);
    let remaining=||crate::process::list_processes().into_iter().filter(|p|
        owned.iter().any(|old|old.pid==p.pid&&old.exe==p.exe)).map(|p|p.pid).collect::<Vec<_>>();
    quit();
    if !wait_exit(||!remaining().is_empty(),Duration::from_secs(8)) {
        kill_pids(&remaining(),true);
    }
    wait_exit(||!remaining().is_empty(),Duration::from_secs(3))
}

#[cfg(test)]
mod target_tests {
    use super::*;
    #[test]
    fn selection_never_falls_back_to_the_other_endpoint() {
        assert_eq!(selected_clients(Some(SwitchTarget::Cli),true,true).unwrap(),(true,false));
        assert_eq!(selected_clients(Some(SwitchTarget::Desktop),true,true).unwrap(),(false,true));
        assert_eq!(selected_clients(Some(SwitchTarget::Both),true,true).unwrap(),(true,true));
        assert!(selected_clients(Some(SwitchTarget::Cli),false,true).is_err());
        assert!(selected_clients(Some(SwitchTarget::Both),true,false).is_err());
        assert_eq!(selected_clients(None,false,true).unwrap(),(false,true));
        assert!(selected_clients(None,false,false).is_err());
    }
}

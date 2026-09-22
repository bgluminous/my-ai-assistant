//! 本机客户端（Cursor / ChatGPT / Claude Desktop）集成的共享部分：探测、关闭、启动用到的
//! 平台工具，以及三个客户端模块向前端返回的同构结果类型。
//!
//! 跨平台策略与各客户端模块一致：路径 / 进程判断用 `cfg!(...)` 运行时分支，两个平台的分支
//! 都参与编译；只有必须调用 Windows 专用 API 的地方才用 `#[cfg(windows)]` 条件编译。

use serde::Serialize;
use std::path::Path;
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

/// `*_client_detect`：探测到的可执行文件路径（不写配置）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectResult {
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

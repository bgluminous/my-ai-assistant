//! 本地 Cursor 客户端集成：可执行文件路径配置与自动检测、进程检测、优雅关闭、启动，
//! 以及把某个已保存的 Cursor 账户 token 写入本地 Cursor 认证库（state.vscdb），
//! 实现免浏览器切换登录。同时支持 Windows 与 macOS。
//!
//! 跨平台策略：全部用 `cfg!(...)` 运行时布尔判断分支，而非 `#[cfg(...)]` 条件编译，
//! 这样两个平台的分支都参与编译，在 Windows 上 `cargo check` 也能检查 macOS 分支。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::{accounts, audit, cursor, http, paths, process, settings};

// ---------------------------------------------------------------------------
// 可执行文件路径配置（写入统一 settings.json）
// ---------------------------------------------------------------------------

/// 用户手动指定的 Cursor 可执行文件路径；空串 = 未配置（走自动搜索）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorClientConfig {
    #[serde(default)]
    pub exe_path: String,
}

fn current() -> CursorClientConfig {
    settings::read(|s| s.cursor_client.clone()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 路径解析
// ---------------------------------------------------------------------------

/// 本地 Cursor 认证库路径：{config_dir}/Cursor/User/globalStorage/state.vscdb。
/// Windows 上 config_dir() = %APPDATA%(Roaming)，macOS 上 = ~/Library/Application Support，
/// 两者都正好命中 Cursor 的存储位置。
fn auth_db_path() -> Result<PathBuf, String> {
    let base = paths::config_dir().ok_or("config_dir_unavailable")?;
    Ok(base
        .join("Cursor")
        .join("User")
        .join("globalStorage")
        .join("state.vscdb"))
}

/// 读环境变量为非空路径。
fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// 常见安装位置候选（按优先级排列，不检查存在性）。
fn detect_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "windows") {
        // 每用户安装（官方默认）：%LOCALAPPDATA%\Programs\cursor\Cursor.exe
        if let Some(local) = env_path("LOCALAPPDATA").or_else(paths::data_local_dir) {
            out.push(local.join("Programs").join("cursor").join("Cursor.exe"));
        }
        // 全机安装：Program Files（目录大小写变体）与 Program Files (x86)
        if let Some(pf) = env_path("ProgramFiles") {
            out.push(pf.join("cursor").join("Cursor.exe"));
            out.push(pf.join("Cursor").join("Cursor.exe"));
        }
        if let Some(pf86) = env_path("ProgramFiles(x86)") {
            out.push(pf86.join("cursor").join("Cursor.exe"));
        }
    } else if cfg!(target_os = "macos") {
        out.push(PathBuf::from("/Applications/Cursor.app"));
        if let Some(home) = paths::home_dir() {
            out.push(home.join("Applications").join("Cursor.app"));
        }
    }
    out
}

/// 解析实际使用的 Cursor 可执行文件：手动配置优先（存在才算数），否则自动搜索候选路径。
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
        .ok_or_else(|| "cursor_exe_not_found".to_string())
}

// ---------------------------------------------------------------------------
// 进程检测 / 关闭 / 启动
// ---------------------------------------------------------------------------

/// 判断某进程是否本地 Cursor 客户端进程（分平台运行时判断）。
fn is_cursor_process(p: &process::ProcessInfo) -> bool {
    if cfg!(target_os = "windows") {
        // Windows：进程名精确匹配 Cursor.exe（忽略大小写）。
        p.name.eq_ignore_ascii_case("Cursor.exe")
    } else if cfg!(target_os = "macos") {
        // macOS：可执行路径包含 /Cursor.app/ 即视为 Cursor（含各 Helper 子进程）。
        p.exe
            .as_ref()
            .map(|e| e.to_string_lossy().contains("/Cursor.app/"))
            .unwrap_or(false)
    } else {
        false
    }
}

/// 刷新进程列表后判断是否存在正在运行的 Cursor 进程。
fn is_cursor_running() -> bool {
    process::list_processes().iter().any(is_cursor_process)
}

/// 轮询等待 Cursor 完全退出：每 300ms 检查一次，直到退出或超时。返回是否已退出。
fn wait_cursor_exit(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if !is_cursor_running() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// 关闭所有 Cursor 进程：先请求整体优雅退出（给主进程有序收尾的机会，避免下次启动
/// 弹"意外终止"崩溃提示），约 8 秒超时后强制结束兜底，再等约 3 秒确认。
/// 命令退出码一律忽略，以进程是否消失为准。返回最终是否已完全关闭。
fn close_cursor() -> bool {
    if !is_cursor_running() {
        return true;
    }
    if cfg!(target_os = "windows") {
        // 不带 /F：发送正常关闭请求（等价于点窗口关闭按钮），由主进程带子进程有序退出。
        let _ = Command::new("taskkill").args(["/IM", "Cursor.exe"]).output();
        if !wait_cursor_exit(Duration::from_secs(8)) {
            // 兜底：/F /T 强制结束整棵进程树。
            let _ = Command::new("taskkill")
                .args(["/F", "/T", "/IM", "Cursor.exe"])
                .output();
            let _ = wait_cursor_exit(Duration::from_secs(3));
        }
    } else if cfg!(target_os = "macos") {
        // AppleScript quit：等价于 Cmd+Q 的正常退出。
        let _ = Command::new("osascript")
            .args(["-e", "tell application \"Cursor\" to quit"])
            .output();
        if !wait_cursor_exit(Duration::from_secs(8)) {
            // 兜底：按可执行路径匹配强杀所有 Cursor.app 相关进程。
            let _ = Command::new("pkill")
                .args(["-9", "-f", "/Cursor.app/"])
                .output();
            let _ = wait_cursor_exit(Duration::from_secs(3));
        }
    }
    !is_cursor_running()
}

/// 拉起本地 Cursor 客户端（spawn 独立进程，不等待退出）。找不到可执行文件返回 false。
fn launch_cursor() -> bool {
    let Ok(exe) = resolve_exe_path() else {
        return false;
    };
    if cfg!(target_os = "windows") {
        launch_detached_windows(&exe)
    } else if cfg!(target_os = "macos") {
        // .app 是目录，需经 open 启动；若配置的是包内二进制等其它路径则按应用名打开。
        // open 经 LaunchServices 拉起，新进程父为 launchd，天然独立于本应用、不随本应用退出。
        let is_app_bundle = exe
            .extension()
            .map(|e| e.eq_ignore_ascii_case("app"))
            .unwrap_or(false);
        if is_app_bundle {
            Command::new("open").arg(&exe).spawn().is_ok()
        } else {
            Command::new("open").args(["-a", "Cursor"]).spawn().is_ok()
        }
    } else {
        false
    }
}

/// Windows：以脱离本进程 Job / 进程组的方式启动 Cursor。
/// dev 及部分启动环境会把本应用连同子进程圈进 kill-on-close 的 Job Object，
/// 普通 spawn 出的 Cursor 会在本应用退出时被连带结束。
/// 先带 CREATE_BREAKAWAY_FROM_JOB 启动脱离 Job；Job 禁止 breakaway 时该调用直接失败，
/// 回退用 explorer.exe 代理启动（新进程父为 explorer，同样不在本应用 Job 内）。
#[cfg(windows)]
fn launch_detached_windows(exe: &Path) -> bool {
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
fn launch_detached_windows(_exe: &Path) -> bool {
    false
}

// ---------------------------------------------------------------------------
// 写认证库
// ---------------------------------------------------------------------------

/// 向 ItemTable 写入（存在则替换）一条键值。
fn upsert_item(conn: &rusqlite::Connection, key: &str, value: &str) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO ItemTable(key, value) VALUES (?1, ?2)",
        rusqlite::params![key, value],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 从 ItemTable 删除一条键（不存在则无操作）。
fn delete_item(conn: &rusqlite::Connection, key: &str) -> Result<(), String> {
    conn.execute("DELETE FROM ItemTable WHERE key=?1", rusqlite::params![key])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 把 session 认证信息写入本地 Cursor 认证库。写前先整库备份到 *.vscdb.bak。
/// 写入 session access/refresh token 与身份缓存字段，并删除上一个账号残留的旧身份缓存键。
fn write_auth(
    db: &Path,
    tokens: &cursor::SessionTokens,
    email: Option<&str>,
) -> Result<(), String> {
    if !db.exists() {
        return Err("state_db_not_found".into());
    }
    // 备份：state.vscdb -> state.vscdb.bak（失败即报错，避免无备份地改库）。
    let backup = db.with_extension("vscdb.bak");
    std::fs::copy(db, &backup).map_err(|e| format!("backup_failed: {e}"))?;

    let conn = rusqlite::Connection::open(db).map_err(|e| e.to_string())?;
    upsert_item(&conn, "cursorAuth/accessToken", &tokens.access_token)?;
    upsert_item(&conn, "cursorAuth/refreshToken", &tokens.refresh_token)?;
    upsert_item(&conn, "cursor.accessToken", &tokens.access_token)?;
    if let Some(email) = email {
        upsert_item(&conn, "cursor.email", email)?;
        upsert_item(&conn, "cursorAuth/cachedEmail", email)?;
    }
    if let Some(auth_id) = tokens.auth_id.as_deref() {
        upsert_item(&conn, "adminSettings.cachedAuthId", auth_id)?;
        upsert_item(&conn, "glass.lastSignedInAuthId", auth_id)?;
        upsert_item(&conn, "cursorAuth/stripeMembershipAuthId", auth_id)?;
    }
    // 删除上一个账号残留的身份/团队/订阅缓存，避免与新账号混淆。
    for key in [
        "cursorAuth/cachedSignUpType",
        "cursorAuth/cachedScopedProfile",
        "cursorAuth/cachedTeam",
        "cursorAuth/stripeCustomerId",
    ] {
        delete_item(&conn, key)?;
    }
    // 将 WAL 合并回主库并截断，确保客户端下次启动读到最新值。
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 读认证库（导入本机登录）
// ---------------------------------------------------------------------------

/// 从 ItemTable 读一个键的值；键不存在、查询失败或值为空白返回 None。
fn read_item(conn: &rusqlite::Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM ItemTable WHERE key=?1",
        rusqlite::params![key],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

/// 读取本机 Cursor 登录凭据。认证库不存在视为从未登录；打不开、取不到
/// accessToken 或解析不出 user_id 视为 invalid。只读打开：Cursor 运行中也能读，
/// 且绝不改动用户的库。
pub fn read_local_login() -> accounts::LocalLoginRead {
    let Ok(db) = auth_db_path() else {
        return accounts::LocalLoginRead::Missing;
    };
    if !db.exists() {
        return accounts::LocalLoginRead::Missing;
    }
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) else {
        return accounts::LocalLoginRead::Invalid;
    };
    let Some(jwt) = read_item(&conn, "cursorAuth/accessToken") else {
        return accounts::LocalLoginRead::Invalid;
    };
    // JWT sub 形如 "auth0|user_01XXX"（provider 前缀还有 github| 等），取 '|' 后段为 user_id
    let Some(user_id) = http::decode_jwt_payload(&jwt)
        .and_then(|c| c.get("sub").and_then(|x| x.as_str()).map(str::to_string))
        .map(|sub| sub.rsplit('|').next().unwrap_or(&sub).to_string())
        .filter(|s| !s.is_empty())
    else {
        return accounts::LocalLoginRead::Invalid;
    };
    let note_hint = read_item(&conn, "cursor.email")
        .or_else(|| read_item(&conn, "cursorAuth/cachedEmail"))
        .unwrap_or_default();
    // `user_id::<jwt>` 即为可用的 WorkosCursorSessionToken（本项目 Cursor 账户存的就是这种）
    accounts::LocalLoginRead::Found(accounts::LocalLogin {
        kind: "cursor".into(),
        token: format!("{user_id}::{jwt}"),
        refresh_token: None,
        note_hint,
    })
}

// ---------------------------------------------------------------------------
// 命令
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorClientView {
    /// 已保存的手动路径（空串 = 自动搜索）。
    pub exe_path: String,
    /// 统一设置文件 settings.json 的完整路径。
    pub path: String,
}

fn view(cfg: &CursorClientConfig) -> CursorClientView {
    CursorClientView {
        exe_path: cfg.exe_path.clone(),
        path: settings::path_display(),
    }
}

#[tauri::command]
pub fn cursor_client_get(_app: AppHandle) -> Result<CursorClientView, String> {
    settings::ensure_loaded()?;
    Ok(view(&current()))
}

/// 保存手动指定的 Cursor 可执行文件路径；空串 = 清除配置（回到自动搜索）。
#[tauri::command]
pub fn cursor_client_set(_app: AppHandle, exe_path: String) -> Result<CursorClientView, String> {
    let exe_path = exe_path.trim().to_string();
    if !exe_path.is_empty() && !Path::new(&exe_path).exists() {
        return Err("cursor_exe_invalid".into());
    }
    let cfg = CursorClientConfig { exe_path };
    settings::mutate(|s| {
        s.cursor_client = cfg.clone();
        Ok(())
    })?;
    Ok(view(&cfg))
}

// ---------------------------------------------------------------------------
// 真实文件系统扫描（逐深度 BFS + 黑名单剪枝 + 进度事件 + 可取消）
// ---------------------------------------------------------------------------

/// 逐深度遍历的最大深度（从盘符根算起）：限制深目录树耗时，并避免 junction/软链成环。
const SCAN_MAX_DEPTH: usize = 8;

/// 扫描取消标志：cursor_client_scan_cancel 置真，扫描循环每处理一个目录检查一次。
fn scan_cancel_flag() -> &'static AtomicBool {
    static F: OnceLock<AtomicBool> = OnceLock::new();
    F.get_or_init(|| AtomicBool::new(false))
}

/// 目录名黑名单（不分大小写）：系统/回收站/临时/依赖等明显不含 Cursor 安装的目录，剪枝提速。
fn is_blacklisted(name: &str) -> bool {
    const COMMON: &[&str] = &["node_modules", ".git", ".cache"];
    const WIN: &[&str] = &[
        "Windows",
        "$Recycle.Bin",
        "System Volume Information",
        "Windows.old",
        "Package Cache",
        "Temp",
    ];
    const MAC: &[&str] = &["System", "private", ".Trashes", ".Spotlight-V100"];
    let extra = if cfg!(target_os = "macos") { MAC } else { WIN };
    COMMON
        .iter()
        .chain(extra.iter())
        .any(|b| b.eq_ignore_ascii_case(name))
}

/// 扫描起点：Windows 为所有存在的盘符根；macOS 为应用目录、用户主目录与已挂载卷。
fn scan_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "windows") {
        for c in b'A'..=b'Z' {
            let p = PathBuf::from(format!("{}:\\", c as char));
            if p.exists() {
                roots.push(p);
            }
        }
    } else if cfg!(target_os = "macos") {
        roots.push(PathBuf::from("/Applications"));
        if let Some(home) = paths::home_dir() {
            roots.push(home.join("Applications"));
            roots.push(home);
        }
        if let Ok(rd) = std::fs::read_dir("/Volumes") {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    roots.push(p);
                }
            }
        }
    }
    roots
}

/// 追加命中路径并去重（不分大小写）；新加入返回 true。
fn push_unique(found: &mut Vec<String>, path: &Path) -> bool {
    let s = path.to_string_lossy().to_string();
    if found.iter().any(|f| f.eq_ignore_ascii_case(&s)) {
        false
    } else {
        found.push(s);
        true
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanProgress {
    scanned: usize,
    depth: usize,
    current: String,
    found: Vec<String>,
}

/// 可暂停/续扫的扫描会话：命中一个即返回并保留状态，下次从原处继续。
struct ScanState {
    level: VecDeque<PathBuf>,
    next: VecDeque<PathBuf>,
    depth: usize,
    scanned: usize,
    found: Vec<String>,
    want_app: bool,
}

impl ScanState {
    fn new() -> Self {
        ScanState {
            level: scan_roots().into_iter().collect(),
            next: VecDeque::new(),
            depth: 0,
            scanned: 0,
            found: Vec::new(),
            want_app: cfg!(target_os = "macos"),
        }
    }
    fn progress(&self, current: String) -> ScanProgress {
        ScanProgress {
            scanned: self.scanned,
            depth: self.depth,
            current,
            found: self.found.clone(),
        }
    }
}

/// 跨命令调用保留的扫描会话（None = 无进行中的会话）。
fn scan_state() -> &'static Mutex<Option<ScanState>> {
    static S: OnceLock<Mutex<Option<ScanState>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepResult {
    /// 本次新命中的路径；None 表示本步没有新命中（通常伴随 done 或 cancelled）。
    pub found: Option<String>,
    /// 扫描是否已结束（无更多目录、命中的是最后一个、或被取消）。
    pub done: bool,
    /// 是否因取消而结束。
    pub cancelled: bool,
    pub scanned: usize,
    pub depth: usize,
}

/// 推进扫描直到命中下一个 Cursor 或扫描结束。restart=true 丢弃旧会话从头开始；
/// 命中即暂停并保留会话供下次继续，扫到底/取消则清除会话。
fn step_blocking(app: AppHandle, restart: bool) -> StepResult {
    let mut guard = scan_state().lock().unwrap_or_else(|e| e.into_inner());
    if restart {
        *guard = None;
    }
    let mut state = match guard.take() {
        Some(s) => s,
        None => {
            scan_cancel_flag().store(false, Ordering::SeqCst);
            ScanState::new()
        }
    };
    let mut last_emit = Instant::now();

    loop {
        if scan_cancel_flag().load(Ordering::SeqCst) {
            let _ = app.emit("cursor-scan-progress", state.progress(String::new()));
            return StepResult {
                found: None,
                done: true,
                cancelled: true,
                scanned: state.scanned,
                depth: state.depth,
            };
        }
        let dir = match state.level.pop_front() {
            Some(d) => d,
            None => {
                // 本层扫完：进入下一层；下一层为空或已达深度上限则结束。
                if state.next.is_empty() || state.depth >= SCAN_MAX_DEPTH {
                    let _ = app.emit("cursor-scan-progress", state.progress(String::new()));
                    return StepResult {
                        found: None,
                        done: true,
                        cancelled: false,
                        scanned: state.scanned,
                        depth: state.depth,
                    };
                }
                state.level = std::mem::take(&mut state.next);
                state.depth += 1;
                continue;
            }
        };
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue; // 权限不足/瞬时错误：静默跳过该目录
        };
        state.scanned += 1;
        let mut hit: Option<String> = None;
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if state.want_app {
                if ft.is_dir() && name.eq_ignore_ascii_case("Cursor.app") {
                    if push_unique(&mut state.found, &entry.path()) {
                        hit = Some(entry.path().to_string_lossy().to_string());
                    }
                    continue; // 命中 .app，不深入其内部
                }
            } else if ft.is_file() && name.eq_ignore_ascii_case("Cursor.exe") {
                if push_unique(&mut state.found, &entry.path()) {
                    hit = Some(entry.path().to_string_lossy().to_string());
                }
                continue;
            }
            if ft.is_dir() && !is_blacklisted(&name) {
                state.next.push_back(entry.path());
            }
        }
        if hit.is_some() || last_emit.elapsed() >= Duration::from_millis(200) {
            last_emit = Instant::now();
            let _ = app.emit(
                "cursor-scan-progress",
                state.progress(dir.to_string_lossy().to_string()),
            );
        }
        if let Some(path) = hit {
            // 命中即暂停：仍有待扫目录则保留会话，否则本次即最后一个。
            let done = state.level.is_empty() && state.next.is_empty();
            let scanned = state.scanned;
            let depth = state.depth;
            if !done {
                *guard = Some(state);
            }
            return StepResult {
                found: Some(path),
                done,
                cancelled: false,
                scanned,
                depth,
            };
        }
    }
}

/// 推进一步扫描：restart=true 从头开始，false 从上次命中处继续；命中即返回并暂停。
/// 进度经 "cursor-scan-progress" 事件上报。重活放阻塞线程池执行。
#[tauri::command]
pub async fn cursor_client_scan_step(
    app: AppHandle,
    restart: bool,
) -> Result<StepResult, String> {
    tauri::async_runtime::spawn_blocking(move || step_blocking(app, restart))
        .await
        .map_err(|e| e.to_string())
}

/// 请求取消正在进行的扫描（下一次目录检查时生效）。
#[tauri::command]
pub fn cursor_client_scan_cancel() {
    scan_cancel_flag().store(true, Ordering::SeqCst);
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientStatus {
    pub running: bool,
    pub exe_configured: bool,
    pub exe_path: Option<String>,
}

/// 查询本地 Cursor 客户端状态。id 仅为前端契约保留，不校验账户（不因账户问题报错）。
#[tauri::command]
pub async fn cursor_client_status(id: String) -> Result<ClientStatus, String> {
    let _ = id;
    let exe_path = resolve_exe_path()
        .ok()
        .map(|p| p.to_string_lossy().to_string());
    Ok(ClientStatus {
        running: is_cursor_running(),
        exe_configured: exe_path.is_some(),
        exe_path,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseResult {
    pub closed: bool,
}

/// 关闭本地 Cursor（优雅退出 + 强制兜底 + 轮询确认），最长约 11 秒。
#[tauri::command]
pub async fn cursor_client_close() -> Result<CloseResult, String> {
    // 轮询会阻塞较久，放到阻塞线程池执行，避免占用异步运行时工作线程。
    let closed = tauri::async_runtime::spawn_blocking(close_cursor)
        .await
        .map_err(|e| e.to_string())?;
    Ok(CloseResult { closed })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchResult {
    pub launched: bool,
}

/// 启动本地 Cursor。找不到可执行文件时 launched=false（不报错）。
#[tauri::command]
pub fn cursor_client_launch() -> Result<LaunchResult, String> {
    Ok(LaunchResult {
        launched: launch_cursor(),
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

/// 从账户解析邮箱：优先 status.email（字符串），否则若 note 形似邮箱（含 '@'）用 note。
fn resolve_email(acc: &accounts::Account) -> Option<String> {
    let from_status = acc
        .status
        .as_ref()
        .and_then(|s| s.get("email"))
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    from_status.or_else(|| {
        let note = acc.note.trim();
        if note.contains('@') {
            Some(note.to_string())
        } else {
            None
        }
    })
}

/// 切换本地 Cursor 客户端登录账户：把账户 token 写入本地认证库。
/// 写库要求 Cursor 已关闭（关闭动作由 cursor_client_close 单独负责）。
#[tauri::command]
pub async fn cursor_switch_local(
    app: tauri::AppHandle,
    id: String,
) -> Result<SwitchResult, String> {
    // 1) 仅支持 Windows / macOS。
    if !cfg!(target_os = "windows") && !cfg!(target_os = "macos") {
        return Err("unsupported_platform".into());
    }

    // 2) 取账户快照并校验类型。
    let acc = accounts::account_snapshot(&id)?;
    if acc.kind != "cursor" {
        return Err("not_cursor_account".into());
    }

    // 3) 有效性校验：仅允许已验证有效（status.alive == true）的账户写入本地登录。
    let alive = acc
        .status
        .as_ref()
        .and_then(|s| s.get("alive"))
        .and_then(|x| x.as_bool());
    if alive != Some(true) {
        return Err("account_not_verified".into());
    }

    // 4) 归一化 token（换取时必须用完整 `user_xxx::<jwt>` 作 Cookie）。
    let token = http::normalize_cursor_token(&acc.token);
    if token.is_empty() {
        return Err("empty_token".into());
    }

    // 5) 先换取 session token（联网，与 Cursor 是否运行无关）。
    let tokens = cursor::exchange_web_to_session(&token).await?;

    // 6) 运行中不写库（SQLite 文件被占用，且改到一半会被客户端覆盖）。
    if is_cursor_running() {
        return Err("cursor_running".into());
    }

    // 7) 邮箱 + 写认证库。
    let email = resolve_email(&acc);
    let db = auth_db_path()?;
    write_auth(&db, &tokens, email.as_deref())?;

    // 8) 审计（绝不写入完整 token）。
    audit::log(
        &app,
        "cursor_switch_local",
        format!("切换本地 Cursor 登录：{}", display_name(&acc)),
        Some(json!({ "id": id })),
    );

    Ok(SwitchResult {
        switched: true,
        message: "已写入本地登录。".into(),
    })
}

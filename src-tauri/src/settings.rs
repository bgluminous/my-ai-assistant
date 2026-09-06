//! 统一用户设置：单一 `settings.json` + 内存态 + 写锁。
//! 所有会改盘的模块必须走 `mutate`，避免分字段写盘互相覆盖。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use crate::accounts::{Account, AccountsFile};
use crate::audit;
use crate::claude_local::ClaudeClientConfig;
use crate::codex_local::CodexClientConfig;
use crate::cursor_local::CursorClientConfig;
use crate::paths;
use crate::pricing::{PricingRemote, PricingTable};
use crate::proxy::{self, ProxyConfig};

/// 用户设置文件结构（camelCase）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub accounts: Vec<Account>,
    #[serde(default)]
    pub interval_minutes: u32,
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub pricing: PricingTable,
    #[serde(default)]
    pub pricing_remote: PricingRemote,
    #[serde(default)]
    pub cursor_client: CursorClientConfig,
    #[serde(default)]
    pub codex_client: CodexClientConfig,
    #[serde(default)]
    pub claude_client: ClaudeClientConfig,
    /// 开机自启动拉起时是否静默启动（不弹主窗口，仅托盘运行）。
    /// 自启动开关本身注册在系统里（Windows 注册表 / macOS LaunchAgent），不落本文件。
    #[serde(default)]
    pub autostart_silent: bool,
    /// 后台检查本机 Codex `auth.json` 是否被客户端改写的间隔（分钟），默认 5。
    #[serde(default = "default_local_sync_minutes")]
    pub codex_local_sync_minutes: u32,
}

pub const DEFAULT_LOCAL_SYNC_MINUTES: u32 = 5;
/// 本机同步间隔的可选值（分钟），与设置弹窗的下拉项一致。
pub const LOCAL_SYNC_CHOICES: [u32; 7] = [1, 5, 10, 30, 60, 360, 1440];

fn default_local_sync_minutes() -> u32 {
    DEFAULT_LOCAL_SYNC_MINUTES
}

/// 当前生效的本机同步间隔：全新安装（`Settings::default()` 为 0）或落盘值不在可选范围时按默认值。
pub fn local_sync_minutes() -> u32 {
    let v = read(|s| s.codex_local_sync_minutes).unwrap_or(DEFAULT_LOCAL_SYNC_MINUTES);
    if LOCAL_SYNC_CHOICES.contains(&v) {
        v
    } else {
        DEFAULT_LOCAL_SYNC_MINUTES
    }
}

impl Settings {
    pub fn accounts_file(&self) -> AccountsFile {
        AccountsFile {
            accounts: self.accounts.clone(),
            interval_minutes: self.interval_minutes,
        }
    }

    pub fn apply_accounts(&mut self, data: AccountsFile) {
        self.accounts = data.accounts;
        self.interval_minutes = data.interval_minutes;
    }
}

fn cell() -> &'static RwLock<Settings> {
    static SETTINGS: OnceLock<RwLock<Settings>> = OnceLock::new();
    SETTINGS.get_or_init(|| RwLock::new(Settings::default()))
}

/// 磁盘数据是否已成功载入内存（文件不存在的全新安装也算成功）。
/// 载入成功前禁止任何写盘操作，避免把默认空数据覆盖到用户文件上。
static LOADED: AtomicBool = AtomicBool::new(false);

pub fn file_path() -> Result<PathBuf, String> {
    let dir = paths::app_dir().ok_or_else(|| "home_dir_unavailable".to_string())?;
    Ok(dir.join("settings.json"))
}

pub fn path_display() -> String {
    file_path()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// 归一化外部来源的设置数据（磁盘载入 / 全量备份导入共用）。
/// ChatGPT 账户由加密副本解出 token / refresh_token；只有明文凭据的旧数据组成副本。
pub(crate) fn sanitize_loaded(mut data: Settings) -> Settings {
    data.proxy = proxy::sanitize(data.proxy);
    data.pricing = data.pricing.lowercased();
    data.pricing_remote.table = data.pricing_remote.table.lowercased();
    crate::accounts::hydrate_accounts(&mut data.accounts);
    data
}

/// 落盘 / 备份导出的形态：有副本的 ChatGPT 账户不再单独写出 token / refresh_token。
pub(crate) fn persisted_form(data: &Settings) -> Settings {
    let mut copy = data.clone();
    crate::accounts::dehydrate_accounts(&mut copy.accounts);
    copy
}

/// 旧版本把 ChatGPT 凭据以明文存放；载入时若有这类账户，组好副本后立刻重写一次文件。
fn needs_codex_auth_migration(data: &Settings) -> bool {
    data.accounts
        .iter()
        .any(|a| a.kind == "codex" && a.codex_auth.is_none() && !a.token.trim().is_empty())
}

/// 尝试从磁盘载入一次。失败时内存保持原样并返回错误：
/// - 读取失败（如被安全软件或并发写入方短暂占用）→ settings_read_failed
/// - 解析失败（文件损坏）→ 先备份原文件为 settings.json.bad 再报 settings_parse_failed
fn attempt_load() -> Result<(), String> {
    let path = file_path()?;
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            LOADED.store(true, Ordering::SeqCst);
            return Ok(());
        }
        Err(e) => return Err(format!("settings_read_failed: {e}")),
    };
    let (data, migrate) = match serde_json::from_str::<Settings>(&text) {
        Ok(d) => {
            let migrate = needs_codex_auth_migration(&d);
            (sanitize_loaded(d), migrate)
        }
        Err(e) => {
            let _ = std::fs::copy(&path, path.with_extension("json.bad"));
            return Err(format!("settings_parse_failed: {e}"));
        }
    };
    {
        let mut g = cell()
            .write()
            .map_err(|_| "state_lock_poisoned".to_string())?;
        *g = data;
    }
    LOADED.store(true, Ordering::SeqCst);
    if migrate {
        // 明文凭据已换成加密副本，立刻重写文件；失败不影响载入（下次任何写盘会再落）
        let result = cell()
            .read()
            .map_err(|_| "state_lock_poisoned".to_string())
            .and_then(|g| save_to_disk(&g));
        match result {
            Ok(()) => audit::log(
                "codex_auth_migrated",
                "ChatGPT 账户凭据已改为加密副本存储，settings.json 中不再保留明文".to_string(),
                None,
            ),
            Err(e) => audit::log(
                "codex_auth_migrated",
                format!("ChatGPT 账户凭据已在内存中改为加密副本，但重写 settings.json 失败：{e}"),
                None,
            ),
        }
    }
    Ok(())
}

/// 确保数据已载入；未载入则当场重试一次（短暂占用可自愈），仍失败则向调用方报错。
/// 所有读写命令入口都要先过这道闸，绝不以未载入的空状态对外服务或写盘。
pub fn ensure_loaded() -> Result<(), String> {
    if LOADED.load(Ordering::SeqCst) {
        return Ok(());
    }
    attempt_load()
}

/// 应用启动时从配置文件载入全部设置到全局；短暂读取失败时小退避重试。
/// 最终失败只记审计日志，不让应用崩溃——后续命令会通过 ensure_loaded 自愈。
pub fn load() {
    let mut last_err = String::new();
    for _ in 0..3 {
        match attempt_load() {
            Ok(()) => return,
            Err(e) => last_err = e,
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    audit::log(
        "settings_load_failed",
        format!("启动时读取设置失败：{last_err}（不会覆盖原文件，首次访问时自动重试）"),
        None,
    );
}

fn save_to_disk(data: &Settings) -> Result<(), String> {
    let path = file_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(&persisted_form(data)).map_err(|e| e.to_string())?;
    // 先写临时文件再原子替换：写入中途崩溃 / 被杀不会损坏原文件，
    // 并发读取方也永远不会读到半截内容。
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

/// 加写锁修改内存状态，随后立即写盘（同步 IO，锁内完成，保证内存与磁盘顺序一致）。
pub fn mutate<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce(&mut Settings) -> Result<T, String>,
{
    ensure_loaded()?;
    let mut guard = cell()
        .write()
        .map_err(|_| "state_lock_poisoned".to_string())?;
    let result = f(&mut guard)?;
    save_to_disk(&guard)?;
    Ok(result)
}

pub fn read<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce(&Settings) -> T,
{
    let guard = cell()
        .read()
        .map_err(|_| "state_lock_poisoned".to_string())?;
    Ok(f(&guard))
}

//! 审计日志：追加写入 {home}/.xilore/myaiassistant/audit.jsonl，每行一条 JSON。
//! 记录账户增删改、导入导出、状态变化、刷新失败、凭据续期与本机同步、切号、设置变更等事件。
//!
//! 每条记录带级别（info / warn / error）与分类（账户管理 / 状态与刷新 / 凭据与同步 /
//! 用量数据 / 设置与系统），由事件类型在 [`EVENTS`] 登记表里的默认值决定；同一事件按结果
//! 需要不同级别时用 [`log_at`] 指定。界面标签也登记在表里，前端不再各自维护一份映射。

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::paths;

/// 文件超过该大小时触发轮转。
const ROTATE_BYTES: u64 = 2_000_000;
/// 轮转后保留的最新条数。
const MAX_KEEP: usize = 2000;
/// 单次查询默认/最大返回条数。
const DEFAULT_LIMIT: usize = 500;
const MAX_LIMIT: usize = 2000;

/// 记录级别：info 为正常操作与结果，warn 为需要留意但不影响使用的情况（状态失效、
/// 被拒后重试成功等），error 为操作失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// 记录分类，对应界面「分类」筛选项；标签文案在前端 audit.js 维护。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// 账户增删改、导入导出、切换本机登录。
    Account,
    /// 存活状态变化、刷新失败。
    State,
    /// access_token 续期、与本机客户端登录文件的双向同步、凭据存储迁移。
    Credential,
    /// 用量快照、原始账单导出、本地用量数据的删除 / 沿用 / 清除、已删除账户备注修改。
    Usage,
    /// 各项设置变更、价格表更新、备份导入导出、启动期的系统事件。
    Settings,
}

use Category::{Account, Credential, Settings, State, Usage};
use Level::{Error, Info, Warn};

/// 事件登记表：事件标识 → 默认级别、分类、界面标签。新增事件必须在此登记，
/// 未登记的事件按 info / 设置与系统 / 原始标识兜底（写入侧在 debug 构建下会断言失败）。
const EVENTS: &[(&str, Level, Category)] = &[
    ("account_add", Info, Account),
    ("account_opencode_link", Info, Account),
    ("account_update", Info, Account),
    ("account_delete", Info, Account),
    ("account_import", Info, Account),
    ("account_import_file", Info, Account),
    ("account_export", Info, Account),
    ("cursor_switch_local", Info, Account),
    ("codex_switch_local", Info, Account),
    ("claude_switch_local", Info, Account),
    ("client_switch_result", Info, Account),
    ("claude_desktop_binding_start", Info, Credential),
    ("claude_desktop_binding_saved", Info, Credential),
    ("claude_desktop_binding_cancelled", Info, Credential),
    ("codex_force_write", Warn, Account),
    ("cursor_logout_local", Info, Account),
    ("codex_logout_local", Info, Account),
    ("claude_logout_local", Info, Account),
    ("account_state_changed", Warn, State),
    ("account_refresh_failed", Error, State),
    ("codex_renewed", Info, Credential),
    ("codex_renew_failed", Error, Credential),
    ("claude_renewed", Info, Credential),
    ("claude_renew_failed", Error, Credential),
    ("codex_sync", Info, Credential),
    ("codex_sync_failed", Error, Credential),
    ("codex_local_changed", Info, Credential),
    ("codex_adopt_local", Info, Credential),
    ("codex_adopt_failed", Error, Credential),
    ("codex_auth_migrated", Info, Credential),
    ("usage_snapshot", Info, Usage),
    ("codex_usage_sync_config", Info, Settings),
    ("codex_usage_sync_ok", Info, Usage),
    ("codex_usage_sync_failed", Error, Usage),
    ("codex_usage_cache_clear", Info, Usage),
    ("usage_events_export", Info, Usage),
    ("usage_data_adopt", Info, Usage),
    ("usage_data_delete", Info, Usage),
    ("usage_data_note", Info, Usage),
    ("usage_data_edit", Info, Usage),
    ("usage_data_plan", Info, Usage),
    ("usage_data_clear", Info, Usage),
    ("interval_set", Info, Settings),
    ("local_sync_interval_set", Info, Settings),
    ("autostart_set", Info, Settings),
    ("pricing_update", Info, Settings),
    ("backup_export", Info, Settings),
    ("backup_import", Info, Settings),
    ("settings_load_failed", Error, Settings),
    // 【过渡期临时代码，到期删除】旧用户目录迁移事件，随 paths.rs 的迁移逻辑一起删除
    ("data_dir_migrated", Info, Settings),
    ("data_dir_migrate_failed", Error, Settings),
];

fn meta_of(event: &str) -> (Level, Category) {
    EVENTS
        .iter()
        .find(|(id, ..)| *id == event)
        .map(|(_, level, category)| (*level, *category))
        .unwrap_or((Info, Settings))
}

/// 落盘的一条记录。level / category 为后来新增的字段，旧文件里的条目没有，读取时按事件类型补齐。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    /// unix 毫秒。
    pub ts: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<Level>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
    /// 事件类型标识（snake_case），须在 [`EVENTS`] 登记。
    pub event: String,
    /// 人类可读描述（中文，敏感信息仅保留打码后的片段）。
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

fn file_path() -> Result<PathBuf, String> {
    let dir = paths::app_dir().ok_or_else(|| "home_dir_unavailable".to_string())?;
    Ok(dir.join("audit.jsonl"))
}

/// 追加一条审计记录，级别取事件登记的默认值。写入失败静默忽略，绝不阻断业务流程。
pub fn log(event: &str, message: String, detail: Option<Value>) {
    let (level, _) = meta_of(event);
    log_at(level, event, message, detail);
}

/// 追加一条审计记录并指定级别：同一事件按结果区分轻重时使用（例如状态变为失效记 warn、
/// 恢复有效记 info）。
pub fn log_at(level: Level, event: &str, message: String, detail: Option<Value>) {
    // 只在写入侧断言：读取旧日志时可能遇到已废弃的事件，不能因此失败
    debug_assert!(
        EVENTS.iter().any(|(id, ..)| *id == event),
        "未登记的审计事件：{event}"
    );
    let (_, category) = meta_of(event);
    let entry = AuditEntry {
        ts: Utc::now().timestamp_millis(),
        level: Some(level),
        category: Some(category),
        event: event.to_string(),
        message,
        detail,
    };
    let Ok(path) = file_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    rotate_if_needed(&path);
    let Ok(line) = serde_json::to_string(&entry) else { return };
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{line}");
    }
}

/// 文件过大时只保留最新 MAX_KEEP 条，防止无限增长。
fn rotate_if_needed(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else { return };
    if meta.len() < ROTATE_BYTES {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= MAX_KEEP {
        return;
    }
    let keep = &lines[lines.len() - MAX_KEEP..];
    let _ = std::fs::write(path, format!("{}\n", keep.join("\n")));
}

/// 供界面展示的一条记录：级别与分类已补齐，并附事件的界面标签。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditRow {
    pub ts: i64,
    pub level: Level,
    pub category: Category,
    pub event: String,
    pub label: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

impl From<AuditEntry> for AuditRow {
    fn from(entry: AuditEntry) -> Self {
        let (level, category) = meta_of(&entry.event);
        let label = crate::ui_language::builtin_text(&format!("audit.event.{}", entry.event));
        AuditRow {
            ts: entry.ts,
            level: entry.level.unwrap_or(level),
            category: entry.category.unwrap_or(category),
            label: if label.is_empty() { entry.event.clone() } else { label.to_string() },
            event: entry.event,
            message: entry.message,
            detail: entry.detail,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditView {
    /// 最新在前。
    pub entries: Vec<AuditRow>,
    /// 文件中的总条数（可能大于 entries 长度）。
    pub total: usize,
    pub path: String,
}

#[tauri::command]
pub fn audit_list(limit: Option<usize>) -> Result<AuditView, String> {
    let _operation = crate::operations::begin()?;
    let path = file_path()?;
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut entries: Vec<AuditEntry> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let total = entries.len();
    entries.reverse();
    entries.truncate(limit);
    Ok(AuditView {
        entries: entries.into_iter().map(AuditRow::from).collect(),
        total,
        path: path.to_string_lossy().to_string(),
    })
}

#[tauri::command]
pub fn audit_clear() -> Result<(), String> {
    let _operation = crate::operations::begin()?;
    let path = file_path()?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/audit.rs"]
mod tests;

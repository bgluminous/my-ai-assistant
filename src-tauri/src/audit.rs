//! 审计日志：追加写入 {home}/.xilore/myaiassistant/audit.jsonl，每行一条 JSON。
//! 记录账户增删改、导入导出、状态变化、刷新失败、凭据续期与切号、设置变更等事件。

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    /// unix 毫秒。
    pub ts: i64,
    /// 事件类型标识（snake_case）。前端 audit.js 的 EVENT_META 按此映射展示标签与筛选分组，
    /// 新增事件时需同步登记，否则界面显示原始标识且无法按类型筛选。
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

/// 追加一条审计记录。写入失败静默忽略，绝不阻断业务流程。
pub fn log(event: &str, message: String, detail: Option<Value>) {
    let entry = AuditEntry {
        ts: Utc::now().timestamp_millis(),
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditView {
    /// 最新在前。
    pub entries: Vec<AuditEntry>,
    /// 文件中的总条数（可能大于 entries 长度）。
    pub total: usize,
    pub path: String,
}

#[tauri::command]
pub fn audit_list(limit: Option<usize>) -> Result<AuditView, String> {
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
        entries,
        total,
        path: path.to_string_lossy().to_string(),
    })
}

#[tauri::command]
pub fn audit_clear() -> Result<(), String> {
    let path = file_path()?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

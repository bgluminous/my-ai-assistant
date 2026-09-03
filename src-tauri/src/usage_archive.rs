//! Cursor 账户全量用量存档与快照图片保存。
//!
//! 存档：前端拉取「全部」跨度用量成功后（cursor_aggregate 带 archive_account_id），
//! 把聚合结果写入 `{app_dir}/usage-archive/<账户id>.json`。账户失效（token 401）后
//! 无法再拉取在线数据，「生成快照」用最后一次存档渲染图片；账户删除时存档一并清理。
//! 快照保存：接收前端 canvas 渲染好的 PNG data URL，弹系统保存框写入用户选择的位置。

use base64::Engine;
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

use crate::audit;
use crate::paths;
use crate::pricing::UsageAggregate;

/// 存档目录：`{app_dir}/usage-archive`。
fn archive_dir() -> Result<PathBuf, String> {
    let dir = paths::app_dir().ok_or_else(|| "home_dir_unavailable".to_string())?;
    Ok(dir.join("usage-archive"))
}

/// 账户 id 正常形如 `acc-<millis>-<seq>`；防御性过滤后拼文件名，避免路径穿越。
fn file_path(account_id: &str) -> Result<PathBuf, String> {
    let safe: String = account_id
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() {
        return Err("invalid_account_id".into());
    }
    Ok(archive_dir()?.join(format!("{safe}.json")))
}

/// 写入存档（临时文件 + 原子替换）。失败静默：存档只是快照的兜底数据源，
/// 绝不能因写盘问题阻断用量统计主流程。
pub fn store(account_id: &str, agg: &UsageAggregate) {
    let Ok(path) = file_path(account_id) else { return };
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(agg_value) = serde_json::to_value(agg) else { return };
    let payload = json!({
        "accountId": account_id,
        "savedAt": Utc::now().timestamp_millis(),
        "agg": agg_value,
    });
    let Ok(text) = serde_json::to_string(&payload) else { return };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// 删除存档（账户删除时调用）；文件不存在或删除失败都忽略。
pub fn remove(account_id: &str) {
    if let Ok(path) = file_path(account_id) {
        let _ = std::fs::remove_file(path);
    }
}

/// 读取存档，返回 `{ accountId, savedAt, agg }`；无存档返回 None。
#[tauri::command]
pub fn usage_archive_get(account_id: String) -> Result<Option<Value>, String> {
    let path = file_path(&account_id)?;
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let v: Value = serde_json::from_str(&text).map_err(|_| "invalid_archive".to_string())?;
    if v.get("agg").map(|a| a.is_object()) != Some(true) {
        return Ok(None);
    }
    Ok(Some(v))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotSaveResult {
    pub cancelled: bool,
    pub path: String,
}

const PNG_DATA_URL_PREFIX: &str = "data:image/png;base64,";

fn pick_png_path(app: &AppHandle, file_name: &str) -> Option<PathBuf> {
    let mut builder = app
        .dialog()
        .file()
        .set_title("保存用量快照")
        .add_filter("PNG 图片", &["png"])
        .set_file_name(file_name);
    if let Some(win) = app.get_webview_window("main") {
        builder = builder.set_parent(&win);
    }
    builder
        .blocking_save_file()
        .and_then(|p| p.simplified().into_path().ok())
}

/// 保存快照 PNG：解码前端渲染好的 data URL，弹保存框写盘。对话框取消返回 cancelled。
#[tauri::command]
pub async fn usage_snapshot_save(
    app: AppHandle,
    file_name: String,
    data_url: String,
) -> Result<SnapshotSaveResult, String> {
    let b64 = data_url
        .strip_prefix(PNG_DATA_URL_PREFIX)
        .ok_or_else(|| "invalid_image_data".to_string())?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| "invalid_image_data".to_string())?;
    if bytes.is_empty() {
        return Err("invalid_image_data".into());
    }
    let app_for_dialog = app.clone();
    let picked =
        tokio::task::spawn_blocking(move || pick_png_path(&app_for_dialog, &file_name))
            .await
            .map_err(|e| e.to_string())?;
    let Some(mut path) = picked else {
        return Ok(SnapshotSaveResult {
            cancelled: true,
            path: String::new(),
        });
    };
    let missing_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| !e.eq_ignore_ascii_case("png"))
        .unwrap_or(true);
    if missing_ext {
        path.set_extension("png");
    }
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    let shown = path.to_string_lossy().to_string();
    audit::log(
        &app,
        "usage_snapshot",
        format!("保存用量快照：{shown}"),
        Some(json!({ "path": shown, "bytes": bytes.len() })),
    );
    Ok(SnapshotSaveResult {
        cancelled: false,
        path: shown,
    })
}

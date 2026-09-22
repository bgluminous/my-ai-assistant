//! Cursor 账户用量事件库与快照图片保存。
//!
//! 事件库：每个 Cursor 账户一份 `{app_dir}/usage-archive/<账户id>.json`，保存从官方接口拉到的
//! 全部原始用量事件（模型、四类 token、实扣、时间戳）。用量页任何时间跨度都从本地事件库切片并按
//! 当前价格表折算，切换跨度不联网；只有刷新才同步——增量同步从库内最后一条事件所在日的 0 点起
//! 拉取并替换该日之后的数据，用量页「刷新」按钮强制全量重拉。
//! 已删除账户：删除时勾选「保留统计数据」会先做一次最终同步，再把事件库连同账户展示信息
//! （备注 / 邮箱 / 用户名 / 套餐 / 删除时间 / 账户身份，不含 token）标记为已删除保留，
//! 用量页总览继续把它作为独立来源计入；重新添加同一账户（同 user_id）时直接沿用该事件库。
//! 快照保存：接收前端 canvas 渲染好的 PNG data URL，弹系统保存框写入用户选择的位置。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use base64::Engine;
use chrono::{DateTime, Local, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

use crate::accounts::{self, Account};
use crate::audit;
use crate::cursor;
use crate::http;
use crate::paths;
use crate::pricing::{self, TokenRow, UsageAggregate};

/// 事件库文件格式版本；v1 为旧版「仅聚合结果」存档，读到时视为不存在（下次同步整体重建）。
const ARCHIVE_VERSION: u32 = 2;
/// `mode = "sync"` 时距上次成功同步不足该毫秒数则跳过联网：主窗口与托盘几乎同时触发的去重。
const SYNC_DEDUPE_MS: i64 = 5_000;
/// `mode = "auto"` 未给 max_age_ms 时的默认有效期（与前端默认缓存有效期一致）。
const DEFAULT_MAX_AGE_MS: i64 = 5 * 60_000;

// ---------------------------------------------------------------------------
// 数据结构
// ---------------------------------------------------------------------------

/// 单条事件的紧凑存储形式：[模型, 输入, 输出, 缓存读, 缓存写, 实扣（分）, 时间戳毫秒 | null]。
type EventTuple = (String, f64, f64, f64, f64, f64, Option<i64>);

fn to_tuple(r: &TokenRow) -> EventTuple {
    (
        r.model.clone(),
        r.input,
        r.output,
        r.cache_read,
        r.cache_write,
        r.actual_cents,
        r.timestamp_ms,
    )
}

fn from_tuple(t: &EventTuple) -> TokenRow {
    TokenRow {
        model: t.0.clone(),
        input: t.1,
        output: t.2,
        cache_read: t.3,
        cache_write: t.4,
        actual_cents: t.5,
        timestamp_ms: t.6,
    }
}

/// 已删除账户随事件库保留的展示信息（不含任何凭据）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletedMeta {
    /// 删除时刻（unix 毫秒）。
    pub deleted_at: i64,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub note_auto: bool,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub membership_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UsageArchive {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub account_id: String,
    /// 账户身份（accounts::account_identity，形如 `cursor:user_xxx`），重新添加同一账户时据此接管。
    #[serde(default)]
    pub identity: Option<String>,
    /// 上次成功同步时刻（unix 毫秒）。
    #[serde(default)]
    pub synced_at: Option<i64>,
    #[serde(default)]
    pub events: Vec<EventTuple>,
    /// 为 Some 表示该账户已删除、事件库作为保留的统计数据存在。
    #[serde(default)]
    pub deleted: Option<DeletedMeta>,
}

/// 供前端总览列出的已删除账户记录（不含事件明细）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletedUsageRecord {
    pub account_id: String,
    pub identity: Option<String>,
    pub synced_at: Option<i64>,
    pub deleted_at: i64,
    pub note: String,
    pub note_auto: bool,
    pub email: Option<String>,
    pub name: Option<String>,
    pub membership_type: Option<String>,
    pub events: usize,
    pub first_event_at: Option<i64>,
    pub last_event_at: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSlice {
    pub agg: UsageAggregate,
    /// 事件库上次成功同步时刻（unix 毫秒）；前端以此作为缓存条目的数据时间。
    pub synced_at: Option<i64>,
    /// 本次联网同步失败（仍返回本地数据）时的错误码；成功或无需同步为 None。
    pub sync_error: Option<String>,
}

// ---------------------------------------------------------------------------
// 文件与锁
// ---------------------------------------------------------------------------

/// 事件库目录：`{app_dir}/usage-archive`。
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

/// 每账户一把异步锁：同步（含联网）、写盘、改名、删除都在锁内进行。
/// 主窗口与托盘并发触发同一账户时排队执行，后到者看到刚同步好的事件库后直接切片。
fn account_lock(account_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    let map = LOCKS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .entry(account_id.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

fn read_archive_file(path: &Path) -> Option<UsageArchive> {
    let text = std::fs::read_to_string(path).ok()?;
    let archive: UsageArchive = serde_json::from_str(&text).ok()?;
    // 旧版存档（v1 只有聚合结果）没有事件明细，视为不存在，下次同步整体重建
    (archive.version >= ARCHIVE_VERSION).then_some(archive)
}

/// 读取事件库；文件不存在、旧版格式或损坏都返回 None。
fn load(account_id: &str) -> Option<UsageArchive> {
    read_archive_file(&file_path(account_id).ok()?)
}

/// 写入事件库（临时文件 + 原子替换）。
fn save(archive: &UsageArchive) -> Result<(), String> {
    let path = file_path(&archive.account_id)?;
    let parent = path
        .parent()
        .ok_or_else(|| "invalid_archive_path".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let text = serde_json::to_string(archive).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

/// 删除事件库文件；不存在或删除失败都忽略。
fn remove_file(account_id: &str) {
    if let Ok(path) = file_path(account_id) {
        let _ = std::fs::remove_file(path);
    }
}

/// 遍历事件库目录，返回全部已删除账户的事件库（含事件明细），按删除时间倒序。
fn deleted_archives() -> Vec<UsageArchive> {
    let Ok(dir) = archive_dir() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut list: Vec<UsageArchive> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        })
        .filter_map(|path| read_archive_file(&path))
        .filter(|archive| archive.deleted.is_some())
        .collect();
    list.sort_by_key(|a| std::cmp::Reverse(a.deleted.as_ref().map_or(0, |d| d.deleted_at)));
    list
}

fn same_identity(a: Option<&str>, b: Option<&str>) -> bool {
    matches!((a, b), (Some(x), Some(y)) if x.eq_ignore_ascii_case(y))
}

// ---------------------------------------------------------------------------
// 同步与切片
// ---------------------------------------------------------------------------

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// 时间戳所在本地自然日的 0 点（unix 毫秒）；解析失败时退回原值。
fn local_day_start_ms(ts: i64) -> i64 {
    DateTime::from_timestamp_millis(ts)
        .map(|dt| dt.with_timezone(&Local).date_naive())
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .and_then(|naive| Local.from_local_datetime(&naive).earliest())
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(ts)
}

/// 联网同步事件库。full 或库内没有带时间戳的事件时全量重拉；否则增量：从最后一条事件所在日
/// 0 点起拉取，替换库内该日之后的数据。无时间戳的旧事件保留，增量结果里无时间戳的丢弃，
/// 避免每次同步重复累加。失败时事件库保持原样。
async fn sync_events(archive: &mut UsageArchive, token: &str, full: bool) -> Result<(), String> {
    let last_ts = if full {
        None
    } else {
        archive.events.iter().filter_map(|e| e.6).max()
    };
    match last_ts {
        None => {
            let rows = cursor::fetch_all_events(token, None, None).await?;
            archive.events = rows.iter().map(to_tuple).collect();
        }
        Some(ts) => {
            let day_start = local_day_start_ms(ts);
            let rows = cursor::fetch_all_events(token, Some(day_start), None).await?;
            archive.events.retain(|e| e.6.is_none_or(|t| t < day_start));
            archive.events.extend(
                rows.iter()
                    .filter(|r| r.timestamp_ms.is_some_and(|t| t >= day_start))
                    .map(to_tuple),
            );
        }
    }
    archive.synced_at = Some(now_ms());
    Ok(())
}

/// 按 [start, end]（unix 毫秒，闭区间，None = 不限）切片并按当前价格表聚合。
/// 有界范围只计入带时间戳的事件；「全部」（两端都为 None）连无时间戳的事件一起计入。
/// 区间不超过两天时按小时序列覆盖区间内的日期（任意历史日期都能画 24 小时柱图）。
fn slice(archive: &UsageArchive, start: Option<i64>, end: Option<i64>) -> UsageAggregate {
    let bounded = start.is_some() || end.is_some();
    let rows: Vec<TokenRow> = archive
        .events
        .iter()
        .filter(|e| {
            if !bounded {
                return true;
            }
            e.6.is_some_and(|t| start.is_none_or(|s| t >= s) && end.is_none_or(|x| t <= x))
        })
        .map(from_tuple)
        .collect();
    let hourly_dates = pricing::hourly_dates_for(start, end);
    pricing::aggregate_and_price_for(rows, &pricing::load(), hourly_dates.as_deref())
}

fn status_field(acc: &Account, key: &str) -> Option<String> {
    acc.status
        .as_ref()
        .and_then(|s| s.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 已删除账户的显示名，与前端 cursorIdentity 同口径：手填备注 > 用户名 > 邮箱 > 自动备注。
fn deleted_label(meta: &DeletedMeta) -> String {
    let note = meta.note.trim();
    if !meta.note_auto && !note.is_empty() {
        return note.to_string();
    }
    for value in [&meta.name, &meta.email] {
        if let Some(s) = value.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            return s.to_string();
        }
    }
    if note.is_empty() {
        "未命名账户".to_string()
    } else {
        note.to_string()
    }
}

/// 已删除账户的自动备注，与在用账户的兜底口径一致：优先保留的邮箱，其次账户身份里的账号 ID
/// （`cursor:user_xxx` → `user_xxx`），都没有为空串。
fn deleted_auto_note(meta: &DeletedMeta, identity: Option<&str>) -> String {
    if let Some(email) = meta.email.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return email.to_string();
    }
    identity
        .and_then(|s| s.strip_prefix("cursor:"))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_default()
}

/// 应用备注修改：非空为手填备注（显示时优先于用户名 / 邮箱），留空恢复自动备注。
fn apply_deleted_note(meta: &mut DeletedMeta, identity: Option<&str>, note: &str) {
    let trimmed = note.trim();
    if trimmed.is_empty() {
        meta.note = deleted_auto_note(meta, identity);
        meta.note_auto = true;
    } else {
        meta.note = trimmed.to_string();
        meta.note_auto = false;
    }
}

fn deleted_record(archive: &UsageArchive) -> Option<DeletedUsageRecord> {
    let meta = archive.deleted.as_ref()?;
    let stamps = || archive.events.iter().filter_map(|e| e.6);
    Some(DeletedUsageRecord {
        account_id: archive.account_id.clone(),
        identity: archive.identity.clone(),
        synced_at: archive.synced_at,
        deleted_at: meta.deleted_at,
        note: meta.note.clone(),
        note_auto: meta.note_auto,
        email: meta.email.clone(),
        name: meta.name.clone(),
        membership_type: meta.membership_type.clone(),
        events: archive.events.len(),
        first_event_at: stamps().min(),
        last_event_at: stamps().max(),
    })
}

// ---------------------------------------------------------------------------
// 命令：在用账户拉取 / 只读切片 / 已删除账户列表与清理
// ---------------------------------------------------------------------------

/// 在用 Cursor 账户的用量：按 mode 决定是否联网同步事件库，随后按范围本地切片。
/// - auto：距上次同步超过 max_age_ms（或尚无事件库）才同步（增量）；
/// - sync：立即增量同步（刚同步完不足 5 秒的并发请求除外）；
/// - full：强制全量重拉。
///
/// 同步失败但本地已有数据时仍返回切片并带 sync_error；本地什么都没有才报错。
#[tauri::command]
pub async fn cursor_usage_fetch(
    account_id: String,
    session_token: String,
    start: Option<i64>,
    end: Option<i64>,
    mode: String,
    max_age_ms: Option<i64>,
) -> Result<UsageSlice, String> {
    let token = http::normalize_cursor_token(&session_token);
    if token.is_empty() {
        return Err("empty_token".into());
    }
    let lock = account_lock(&account_id);
    let _guard = lock.lock().await;
    let mut archive = load(&account_id).unwrap_or_default();
    let age = archive.synced_at.map(|t| now_ms() - t);
    let (do_sync, full) = match mode.as_str() {
        "full" => (true, true),
        "sync" => (age.is_none_or(|a| a > SYNC_DEDUPE_MS), false),
        _ => (
            age.is_none_or(|a| a > max_age_ms.unwrap_or(DEFAULT_MAX_AGE_MS)),
            false,
        ),
    };
    let mut sync_error = None;
    if do_sync {
        match sync_events(&mut archive, &token, full).await {
            Ok(()) => {
                archive.version = ARCHIVE_VERSION;
                archive.account_id = account_id.clone();
                if let Some(identity) = accounts::account_identity("cursor", &token) {
                    archive.identity = Some(identity);
                }
                // 在用账户的事件库不该带已删除标记（正常不会出现，防御性清掉）
                archive.deleted = None;
                if let Err(e) = save(&archive) {
                    sync_error = Some(format!("archive_write_failed: {e}"));
                }
            }
            Err(e) => {
                if archive.synced_at.is_none() {
                    return Err(e);
                }
                sync_error = Some(e);
            }
        }
    }
    Ok(UsageSlice {
        agg: slice(&archive, start, end),
        synced_at: archive.synced_at,
        sync_error,
    })
}

/// 只读切片（不联网）：已删除账户的保留数据、快照的本地回退数据源。无事件库时报 no_usage_data。
#[tauri::command]
pub async fn cursor_usage_slice(
    account_id: String,
    start: Option<i64>,
    end: Option<i64>,
) -> Result<UsageSlice, String> {
    let lock = account_lock(&account_id);
    let _guard = lock.lock().await;
    let archive = load(&account_id).ok_or_else(|| "no_usage_data".to_string())?;
    Ok(UsageSlice {
        agg: slice(&archive, start, end),
        synced_at: archive.synced_at,
        sync_error: None,
    })
}

#[tauri::command]
pub fn cursor_usage_deleted_list() -> Result<Vec<DeletedUsageRecord>, String> {
    Ok(deleted_archives().iter().filter_map(deleted_record).collect())
}

/// 删除一份已删除账户保留的统计数据。在用账户的事件库不经此命令删除（随账户删除时清理）。
#[tauri::command]
pub async fn cursor_usage_deleted_remove(account_id: String) -> Result<(), String> {
    let lock = account_lock(&account_id);
    let _guard = lock.lock().await;
    let archive = load(&account_id).ok_or_else(|| "no_usage_data".to_string())?;
    let Some(meta) = archive.deleted.as_ref() else {
        return Err("not_deleted_account".into());
    };
    std::fs::remove_file(file_path(&account_id)?).map_err(|e| e.to_string())?;
    audit::log(
        "usage_data_delete",
        format!(
            "删除已删除账户「{}」保留的统计数据（{} 条用量事件）",
            deleted_label(meta),
            archive.events.len()
        ),
        Some(json!({ "id": account_id, "events": archive.events.len() })),
    );
    Ok(())
}

/// 修改一份已删除账户保留记录的备注：非空为手填备注（显示时优先于用户名 / 邮箱），
/// 留空恢复自动备注（邮箱，其次账号 ID）。返回更新后的记录，前端据此就地刷新。
#[tauri::command]
pub async fn cursor_usage_deleted_set_note(
    account_id: String,
    note: String,
) -> Result<DeletedUsageRecord, String> {
    let lock = account_lock(&account_id);
    let _guard = lock.lock().await;
    let mut archive = load(&account_id).ok_or_else(|| "no_usage_data".to_string())?;
    let identity = archive.identity.clone();
    let Some(meta) = archive.deleted.as_mut() else {
        return Err("not_deleted_account".into());
    };
    let before = deleted_label(meta);
    apply_deleted_note(meta, identity.as_deref(), &note);
    let after = deleted_label(meta);
    let auto = meta.note_auto;
    save(&archive)?;
    let message = if auto {
        format!("已删除账户「{before}」的备注恢复为自动备注，现显示为「{after}」")
    } else {
        format!("已删除账户「{before}」的备注改为「{after}」")
    };
    audit::log(
        "usage_data_note",
        message,
        Some(json!({ "id": account_id, "auto": auto })),
    );
    deleted_record(&archive).ok_or_else(|| "not_deleted_account".to_string())
}

/// 已删除账户可手动指定的 Cursor 套餐档位（与前端套餐名 / 月费表一致）。
const MEMBERSHIP_CHOICES: &[&str] = &[
    "free",
    "pro",
    "pro_plus",
    "ultra",
    "business",
    "team",
    "enterprise",
];

/// 归一化手动指定的套餐档位：空白为 None（清除为未知），其余必须是登记的档位之一。
pub(crate) fn normalize_membership_choice(raw: &str) -> Result<Option<String>, String> {
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        return Ok(None);
    }
    if !MEMBERSHIP_CHOICES.contains(&value.as_str()) {
        return Err("invalid_membership".into());
    }
    Ok(Some(value))
}

/// 修改某个已删除账户保留记录的套餐档位（留空清除为未知），返回更新后的记录。
/// 在用账户的套餐来自接口刷新，不经此命令修改；只有删除时会话已失效、没记下套餐的记录
/// 才需要手动补，供统计页的套餐列与月费倍数对比使用。
#[tauri::command]
pub async fn cursor_usage_deleted_set_membership(
    account_id: String,
    membership_type: String,
) -> Result<DeletedUsageRecord, String> {
    let next = normalize_membership_choice(&membership_type)?;
    let lock = account_lock(&account_id);
    let _guard = lock.lock().await;
    let mut archive = load(&account_id).ok_or_else(|| "no_usage_data".to_string())?;
    let Some(meta) = archive.deleted.as_mut() else {
        return Err("not_deleted_account".into());
    };
    let label = deleted_label(meta);
    let before = meta.membership_type.take();
    meta.membership_type = next.clone();
    save(&archive)?;
    let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "未知".to_string());
    audit::log(
        "usage_data_plan",
        format!(
            "已删除账户「{label}」的套餐由「{}」改为「{}」",
            show(&before),
            show(&next)
        ),
        Some(json!({ "id": account_id, "membershipType": next })),
    );
    deleted_record(&archive).ok_or_else(|| "not_deleted_account".to_string())
}

// ---------------------------------------------------------------------------
// 命令：原始事件明细（原始账单）/ 清除在用账户的本地数据
// ---------------------------------------------------------------------------

/// 事件库里的一条原始用量事件，附带当前价格表下的归一结果与折算，供「原始账单」查看与导出。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawUsageEvent {
    /// Cursor 接口上报的原始模型名，未做任何归一。
    pub model: String,
    /// 聚合展示用的归一名（与用量页明细表同口径）。
    pub display_model: String,
    /// 命中的价格表键；None 为未定价。
    pub priced_as: Option<String>,
    pub input_tokens: f64,
    pub output_tokens: f64,
    pub cache_read_tokens: f64,
    pub cache_write_tokens: f64,
    pub total_tokens: f64,
    /// 官方实扣（美元）。
    pub actual_usd: f64,
    /// 按当前价格表折算的等价费用（美元）；未定价为 None。
    pub equivalent_usd: Option<f64>,
    pub timestamp_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawUsageEvents {
    /// 范围内的事件，按时间倒序，无时间戳的排在最后。
    pub events: Vec<RawUsageEvent>,
    /// 事件库全部事件数（不限范围）。
    pub total: usize,
    pub synced_at: Option<i64>,
    /// 该事件库属于已删除账户保留的统计数据。
    pub deleted: bool,
    /// 事件库文件路径。
    pub path: String,
}

/// 只读列出事件库在 [start, end]（unix 毫秒，闭区间，None = 不限）内的原始事件（不联网）。
/// 范围语义与聚合切片一致：有界范围只含带时间戳的事件，「全部」连无时间戳的一起列出。
/// 在用账户与已删除账户保留的事件库都可查看。无事件库时报 no_usage_data。
#[tauri::command]
pub async fn cursor_usage_events(
    account_id: String,
    start: Option<i64>,
    end: Option<i64>,
) -> Result<RawUsageEvents, String> {
    let lock = account_lock(&account_id);
    let _guard = lock.lock().await;
    let archive = load(&account_id).ok_or_else(|| "no_usage_data".to_string())?;
    let table = pricing::load();
    let bounded = start.is_some() || end.is_some();
    let mut events: Vec<RawUsageEvent> = archive
        .events
        .iter()
        .filter(|e| {
            if !bounded {
                return true;
            }
            e.6.is_some_and(|t| start.is_none_or(|s| t >= s) && end.is_none_or(|x| t <= x))
        })
        .map(|e| {
            let row = from_tuple(e);
            let priced = pricing::price_row(&row, &table);
            RawUsageEvent {
                display_model: crate::model_match::display_key(&table, &row.model),
                priced_as: priced.map(|(k, _)| k.to_string()),
                equivalent_usd: priced.map(|(_, usd)| usd),
                total_tokens: row.input + row.output + row.cache_read + row.cache_write,
                input_tokens: row.input,
                output_tokens: row.output,
                cache_read_tokens: row.cache_read,
                cache_write_tokens: row.cache_write,
                actual_usd: row.actual_cents / 100.0,
                timestamp_ms: row.timestamp_ms,
                model: row.model,
            }
        })
        .collect();
    events.sort_by_key(|e| std::cmp::Reverse(e.timestamp_ms.unwrap_or(i64::MIN)));
    Ok(RawUsageEvents {
        events,
        total: archive.events.len(),
        synced_at: archive.synced_at,
        deleted: archive.deleted.is_some(),
        path: file_path(&account_id)?.to_string_lossy().to_string(),
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageClearResult {
    /// 被清除的事件数。
    pub events: usize,
}

/// 清除在用 Cursor 账户的本地用量数据：删除其事件库文件（账户与 Token 保留），
/// 下次拉取用量时重新全量同步。已删除账户保留的统计数据走 cursor_usage_deleted_remove。
#[tauri::command]
pub async fn cursor_usage_clear(account_id: String) -> Result<UsageClearResult, String> {
    let lock = account_lock(&account_id);
    let _guard = lock.lock().await;
    let archive = load(&account_id).ok_or_else(|| "no_usage_data".to_string())?;
    if archive.deleted.is_some() {
        return Err("not_live_account".into());
    }
    std::fs::remove_file(file_path(&account_id)?).map_err(|e| e.to_string())?;
    let events = archive.events.len();
    let name = accounts::account_snapshot(&account_id)
        .map(|a| accounts::display_name(&a.kind, &a.note, &a.token))
        .unwrap_or_else(|_| account_id.clone());
    audit::log(
        "usage_data_clear",
        format!("清除 {name} 的本地用量数据（{events} 条用量事件），下次刷新重新全量拉取"),
        Some(json!({ "id": account_id, "events": events })),
    );
    Ok(UsageClearResult { events })
}

// ---------------------------------------------------------------------------
// 账户生命周期联动（accounts / backup 调用）
// ---------------------------------------------------------------------------

pub enum Retained {
    /// 事件库已标记为已删除并保留。
    Kept,
    /// 同步成功但该账户没有任何用量事件，没有可保留的数据。
    Empty,
}

/// 删除 Cursor 账户并保留统计数据前的最终同步：增量同步一次（无事件库则全量），
/// 再把事件库标记为已删除并写入账户展示信息。
/// - 同步成功但确实没有任何事件：删掉空事件库，返回 Empty；
/// - 同步失败但本地已有事件：用已有数据保留，返回 Kept；
/// - 同步失败且本地没有任何数据：返回 Err(no_usage_data:<原因>)，由前端询问是否仍然删除。
pub async fn retain_for_deleted(acc: &Account) -> Result<Retained, String> {
    let lock = account_lock(&acc.id);
    let _guard = lock.lock().await;
    let mut archive = load(&acc.id).unwrap_or_default();
    let token = http::normalize_cursor_token(&acc.token);
    let synced = sync_events(&mut archive, &token, false).await;
    if archive.events.is_empty() {
        return match synced {
            Ok(()) => {
                remove_file(&acc.id);
                Ok(Retained::Empty)
            }
            Err(e) => Err(format!("no_usage_data:{e}")),
        };
    }
    archive.version = ARCHIVE_VERSION;
    archive.account_id = acc.id.clone();
    if let Some(identity) = accounts::account_identity("cursor", &token) {
        archive.identity = Some(identity);
    }
    archive.deleted = Some(DeletedMeta {
        deleted_at: now_ms(),
        note: acc.note.clone(),
        note_auto: acc.note_auto,
        email: status_field(acc, "email"),
        name: status_field(acc, "name"),
        membership_type: status_field(acc, "membershipType"),
    });
    save(&archive)?;
    Ok(Retained::Kept)
}

/// 账户删除且不保留统计数据时清理事件库。
pub async fn discard(account_id: &str) {
    let lock = account_lock(account_id);
    let _guard = lock.lock().await;
    remove_file(account_id);
}

/// 新添加的 Cursor 账户若与某个已删除账户身份相同（同 user_id），直接沿用其保留的事件库：
/// 文件改到新账户 id 下、去掉已删除标记，之后的增量同步从原数据继续；同身份的其余保留记录
/// 一并清理，避免总览重复计数。返回是否发生了接管。
pub async fn adopt_deleted(account_id: &str, token: &str, display_name: &str) -> bool {
    let Some(identity) = accounts::account_identity("cursor", token) else {
        return false;
    };
    let mut candidates: Vec<UsageArchive> = deleted_archives()
        .into_iter()
        .filter(|a| same_identity(a.identity.as_deref(), Some(&identity)))
        .collect();
    if candidates.is_empty() {
        return false;
    }
    // 多份同身份记录时沿用同步时间最新的一份
    candidates.sort_by_key(|a| std::cmp::Reverse(a.synced_at.unwrap_or(0)));
    let lock = account_lock(account_id);
    let _guard = lock.lock().await;
    let mut chosen = candidates.remove(0);
    let old_id = chosen.account_id.clone();
    let label = chosen
        .deleted
        .as_ref()
        .map(deleted_label)
        .unwrap_or_default();
    let events = chosen.events.len();
    {
        let old_lock = account_lock(&old_id);
        let _old_guard = old_lock.lock().await;
        chosen.account_id = account_id.to_string();
        chosen.identity = Some(identity);
        chosen.deleted = None;
        if save(&chosen).is_err() {
            return false;
        }
        remove_file(&old_id);
    }
    for other in candidates {
        let other_lock = account_lock(&other.account_id);
        let _other_guard = other_lock.lock().await;
        remove_file(&other.account_id);
    }
    audit::log(
        "usage_data_adopt",
        format!(
            "添加的 {display_name} 与已删除账户「{label}」为同一账号，沿用其保留的统计数据（{events} 条用量事件）"
        ),
        Some(json!({ "id": account_id, "from": old_id, "events": events })),
    );
    true
}

/// 全量备份导出：全部已删除账户的事件库（在用账户导入后可自行重新同步，不随备份携带）。
pub fn export_deleted() -> Vec<Value> {
    deleted_archives()
        .iter()
        .filter_map(|a| serde_json::to_value(a).ok())
        .collect()
}

/// 全量备份导入：恢复备份里已删除账户的事件库。同身份账户在本机仍在用则跳过（它会自行同步）；
/// 本机已有同身份 / 同 id 的已删除记录时只在备份数据更新时替换；其余直接写入。返回恢复条数。
pub async fn import_deleted(items: &[Value], live_identities: &[String]) -> usize {
    let mut restored = 0usize;
    for item in items {
        let Ok(mut incoming) = serde_json::from_value::<UsageArchive>(item.clone()) else {
            continue;
        };
        if incoming.version < ARCHIVE_VERSION
            || incoming.deleted.is_none()
            || incoming.events.is_empty()
            || file_path(&incoming.account_id).is_err()
        {
            continue;
        }
        if live_identities
            .iter()
            .any(|i| same_identity(Some(i), incoming.identity.as_deref()))
        {
            continue;
        }
        let local_same: Vec<UsageArchive> = deleted_archives()
            .into_iter()
            .filter(|a| {
                a.account_id == incoming.account_id
                    || same_identity(a.identity.as_deref(), incoming.identity.as_deref())
            })
            .collect();
        let incoming_synced = incoming.synced_at.unwrap_or(0);
        if local_same
            .iter()
            .any(|a| a.synced_at.unwrap_or(0) >= incoming_synced)
        {
            continue;
        }
        // 同 id 的事件库属于本机在用账户时不覆盖
        if load(&incoming.account_id).is_some_and(|a| a.deleted.is_none()) {
            continue;
        }
        let lock = account_lock(&incoming.account_id);
        let _guard = lock.lock().await;
        incoming.version = ARCHIVE_VERSION;
        if save(&incoming).is_err() {
            continue;
        }
        for old in local_same {
            if old.account_id != incoming.account_id {
                remove_file(&old.account_id);
            }
        }
        restored += 1;
    }
    restored
}

// ---------------------------------------------------------------------------
// 快照图片保存
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotSaveResult {
    pub cancelled: bool,
    pub path: String,
}

const PNG_DATA_URL_PREFIX: &str = "data:image/png;base64,";

/// 弹系统「另存为」对话框（挂在主窗口下），返回用户选择的路径；取消返回 None。
fn pick_save_path(
    app: &AppHandle,
    title: &str,
    filter_name: &str,
    ext: &str,
    file_name: &str,
) -> Option<PathBuf> {
    let mut builder = app
        .dialog()
        .file()
        .set_title(title)
        .add_filter(filter_name, &[ext])
        .set_file_name(file_name);
    if let Some(win) = app.get_webview_window("main") {
        builder = builder.set_parent(&win);
    }
    builder
        .blocking_save_file()
        .and_then(|p| p.simplified().into_path().ok())
}

fn pick_png_path(app: &AppHandle, file_name: &str) -> Option<PathBuf> {
    pick_save_path(app, "保存用量快照", "PNG 图片", "png", file_name)
}

/// 路径缺少期望扩展名时补上（用户在对话框里删掉了后缀的情形）。
fn ensure_ext(mut path: PathBuf, ext: &str) -> PathBuf {
    let missing = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| !e.eq_ignore_ascii_case(ext))
        .unwrap_or(true);
    if missing {
        path.set_extension(ext);
    }
    path
}

/// 导出原始账单 CSV：前端已把事件明细拼成文本，这里只负责弹保存框并写盘。
/// 文件带 UTF-8 BOM，Excel 直接打开不乱码。对话框取消返回 cancelled。
#[tauri::command]
pub async fn usage_events_export(
    app: AppHandle,
    file_name: String,
    content: String,
) -> Result<SnapshotSaveResult, String> {
    if content.is_empty() {
        return Err("empty_content".into());
    }
    let app_for_dialog = app.clone();
    let picked = tokio::task::spawn_blocking(move || {
        pick_save_path(&app_for_dialog, "导出原始账单", "CSV 文件", "csv", &file_name)
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(path) = picked else {
        return Ok(SnapshotSaveResult {
            cancelled: true,
            path: String::new(),
        });
    };
    let path = ensure_ext(path, "csv");
    let mut bytes = Vec::with_capacity(content.len() + 3);
    bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    bytes.extend_from_slice(content.as_bytes());
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    let shown = path.to_string_lossy().to_string();
    audit::log(
        "usage_events_export",
        format!("导出原始账单：{shown}"),
        Some(json!({ "path": shown, "bytes": bytes.len() })),
    );
    Ok(SnapshotSaveResult {
        cancelled: false,
        path: shown,
    })
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
    let Some(path) = picked else {
        return Ok(SnapshotSaveResult {
            cancelled: true,
            path: String::new(),
        });
    };
    let path = ensure_ext(path, "png");
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    let shown = path.to_string_lossy().to_string();
    audit::log(
        "usage_snapshot",
        format!("保存用量快照：{shown}"),
        Some(json!({ "path": shown, "bytes": bytes.len() })),
    );
    Ok(SnapshotSaveResult {
        cancelled: false,
        path: shown,
    })
}

#[cfg(test)]
#[path = "tests/usage_archive.rs"]
mod tests;

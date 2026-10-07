//! Cursor 账户用量事件库与快照图片保存。
//!
//! 事件库：每个 Cursor 账户一份 `{app_dir}/usage-archive/<账户id>.json`，保存从官方接口拉到的
//! 用量事件（模型、四类 token、实扣、时间戳、计费类别）。开启「不记录 API 额度用量」后，
//! 清理套餐内 API 额度及超额按量付费事件，
//! 后续同步也不再保存。用量页任何时间跨度都从本地事件库切片并按当前价格表折算，
//! 切换跨度不联网；只有刷新才同步——增量同步从库内最后一条事件所在日的 0 点起
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

/// v1 仅存聚合结果，v2 保存事件；v3 增加计费元数据，仍兼容读取 v2 的七列事件。
const ARCHIVE_VERSION: u32 = 3;
const MIN_EVENT_ARCHIVE_VERSION: u32 = 2;
/// `mode = "sync"` 时距上次成功同步不足该毫秒数则跳过联网：主窗口与托盘几乎同时触发的去重。
const SYNC_DEDUPE_MS: i64 = 5_000;
/// `mode = "auto"` 未给 max_age_ms 时的默认有效期（与前端默认缓存有效期一致）。
const DEFAULT_MAX_AGE_MS: i64 = 5 * 60_000;

// ---------------------------------------------------------------------------
// 数据结构
// ---------------------------------------------------------------------------

/// 前七列沿用 v2；末列为计费元数据，在用账户缺失时必须联网补齐后才能清理。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventTuple(
    String, f64, f64, f64, f64, f64, Option<i64>,
    #[serde(default)] Option<cursor::UsageBilling>,
);

fn to_tuple(event: &cursor::UsageEvent) -> EventTuple {
    let r = &event.row;
    EventTuple(
        r.model.clone(),
        r.input,
        r.output,
        r.cache_read,
        r.cache_write,
        r.actual_cents,
        r.timestamp_ms,
        Some(event.billing.clone()),
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
    /// 删除账户时保留其统计开关；备份恢复后沿用相同口径。
    #[serde(default)]
    pub ignore_api_models: bool,
    /// 已删除账户允许用模型归属近似清理缺少计费信息的旧事件，备份恢复沿用该方式。
    #[serde(default)]
    pub api_filter_legacy: bool,
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
    /// 同步游标：过滤前最新事件的时间戳，避免 API 事件被清理后反复全量拉取。
    #[serde(default)]
    pub last_event_at: Option<i64>,
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
    /// 已移除 API 历史后为 true；已删除账户只允许从 false 变为 true。
    pub ignore_api_models: bool,
    /// 此次清理需按模型近似判断，或此前已经按这种方式清理过。
    pub api_removal_approximate: bool,
    /// 模型归属或已有计费类别仍无法识别时，不允许猜测清理。
    pub api_removal_error: Option<String>,
    pub events: usize,
    pub first_event_at: Option<i64>,
    pub last_event_at: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSlice {
    pub agg: UsageAggregate,
    /// 本次聚合采用的账户开关，供前端验证缓存口径。
    pub ignore_api_models: bool,
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
pub(crate) fn account_lock(account_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    let map = LOCKS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .entry(account_id.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// 关闭历史记录与备份导入可能涉及不同 id 下的同一身份，串行处理以免旧备份覆盖关闭状态。
fn deleted_mutation_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn read_archive_file(path: &Path) -> Option<UsageArchive> {
    let text = std::fs::read_to_string(path).ok()?;
    let archive: UsageArchive = serde_json::from_str(&text).ok()?;
    // 旧版存档（v1 只有聚合结果）没有事件明细，视为不存在，下次同步整体重建
    (archive.version >= MIN_EVENT_ARCHIVE_VERSION).then_some(archive)
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

/// 联网同步事件库。full 或没有同步游标时全量重拉；否则增量：从最后一条事件所在日
/// 0 点起拉取，替换库内该日之后的数据。无时间戳的旧事件保留，增量结果里无时间戳的丢弃，
/// 避免每次同步重复累加。失败时事件库保持原样。
async fn sync_events(
    archive: &mut UsageArchive,
    token: &str,
    full: bool,
    ignore_api_models: bool,
) -> Result<(), String> {
    // 计费信息补齐、合并与清理先在副本中完成；任一环节失败均不更改原库。
    let mut next = archive.clone();
    if !full {
        prepare_api_filter(&mut next, token, ignore_api_models).await?;
    }
    let last_ts = if full {
        None
    } else {
        next.last_event_at.max(next.events.iter().filter_map(|e| e.6).max())
    };
    let day_start = last_ts.map(local_day_start_ms);
    let rows = cursor::fetch_all_events(token, day_start, None).await?;
    merge_events(&mut next, &rows, day_start, ignore_api_models)?;
    next.synced_at = Some(now_ms());
    next.version = ARCHIVE_VERSION;
    *archive = next;
    Ok(())
}

/// 合并已成功拉取的数据，先更新游标，再丢弃 API 事件；过滤不会影响分页和增量范围。
fn merge_events(
    archive: &mut UsageArchive,
    rows: &[cursor::UsageEvent],
    day_start: Option<i64>,
    ignore_api_models: bool,
) -> Result<(), String> {
    // 先验证新记录，防止遇到未知计费类型时只合并了一半。
    if ignore_api_models {
        for event in rows {
            require_api_usage(&event.row.model, Some(&event.billing))?;
        }
        if day_start.is_some() {
            validate_api_filter(archive)?;
        }
    }
    if let Some(start) = day_start {
        archive.last_event_at = archive.last_event_at.max(archive.events.iter().filter_map(|e| e.6).max());
        archive.events.retain(|e| e.6.is_none_or(|t| t < start));
    } else {
        archive.events.clear();
        archive.last_event_at = None;
    }
    archive.events.extend(
        rows.iter()
            .filter(|r| day_start.is_none_or(|start| r.row.timestamp_ms.is_some_and(|t| t >= start)))
            .map(to_tuple),
    );
    archive.last_event_at = archive.last_event_at.max(archive.events.iter().filter_map(|e| e.6).max());
    if ignore_api_models {
        prune_api_events(archive)?;
    }
    Ok(())
}

/// 真正移除 API 明细（含无时间戳事件），只保留增量同步所需的时间游标。
fn prune_api_events(archive: &mut UsageArchive) -> Result<bool, String> {
    validate_api_filter(archive)?;
    let before = archive.events.len();
    archive.last_event_at = archive.last_event_at.max(archive.events.iter().filter_map(|e| e.6).max());
    let legacy = legacy_api_filter(archive);
    archive.events.retain(|e| event_api_usage(e, legacy) != Some(true));
    Ok(archive.events.len() != before)
}

fn legacy_api_filter(archive: &UsageArchive) -> bool {
    archive.deleted.as_ref().is_some_and(|meta| meta.api_filter_legacy)
}

/// 已有计费信息优先；近似模式仅补充旧格式缺失的元数据，不掩盖未知的新计费类别。
fn event_api_usage(event: &EventTuple, legacy: bool) -> Option<bool> {
    match &event.7 {
        Some(billing) => billing.api_usage(&event.0),
        None if legacy => cursor::model_uses_api_pool(&event.0),
        None => None,
    }
}

fn require_api_usage(model: &str, billing: Option<&cursor::UsageBilling>) -> Result<bool, String> {
    billing.and_then(|b| b.api_usage(model)).ok_or_else(|| {
        "账单缺少可识别的计费信息，未清理数据。请刷新用量后重试。".to_string()
    })
}

fn validate_api_filter(archive: &UsageArchive) -> Result<(), String> {
    for event in &archive.events {
        if event_api_usage(event, legacy_api_filter(archive)).is_none() {
            return Err(if archive.deleted.is_some() {
                "历史记录包含无法识别的模型或计费类别，无法清理，原数据保持不变。".into()
            } else {
                "账单缺少可识别的计费信息，未清理数据。请刷新用量后重试。".into()
            });
        }
    }
    Ok(())
}

fn needs_billing(archive: &UsageArchive) -> bool {
    archive.events.iter().any(|e| require_api_usage(&e.0, e.7.as_ref()).is_err())
}

/// 不使用金额匹配：Cursor 可能隐藏或调整历史金额。相同时间、模型及 token 的重复事件
/// 逐条配对，避免把同一远端事件的计费类别重复套给多条本地记录。
fn billing_key(event: &EventTuple) -> String {
    json!([event.0, event.1, event.2, event.3, event.4, event.6]).to_string()
}

fn hydrate_billing(archive: &mut UsageArchive, rows: &[cursor::UsageEvent]) -> Result<(), String> {
    let mut available: HashMap<String, Vec<cursor::UsageBilling>> = HashMap::new();
    for row in rows {
        available.entry(billing_key(&to_tuple(row))).or_default().push(row.billing.clone());
    }
    let mut events = archive.events.clone();
    for event in &mut events {
        if require_api_usage(&event.0, event.7.as_ref()).is_ok() {
            continue;
        }
        let matches = available.get_mut(&billing_key(event));
        if let Some(matches) = matches {
            let first = matches.first().and_then(|b| b.api_usage(&event.0));
            if matches.iter().any(|b| b.api_usage(&event.0) != first) {
                return Err("历史账单存在无法唯一匹配的计费归属，未清理数据。".into());
            }
            event.7 = matches.pop();
        }
        if event.7.is_none() {
            return Err("部分历史账单无法从 Cursor 补齐计费信息，未清理数据。可先导出账单，再清除本地数据并重新同步。".into());
        }
        require_api_usage(&event.0, event.7.as_ref())?;
    }
    archive.events = events;
    archive.version = ARCHIVE_VERSION;
    Ok(())
}

async fn prepare_api_filter(
    archive: &mut UsageArchive,
    token: &str,
    ignore_api_models: bool,
) -> Result<(), String> {
    if !ignore_api_models {
        return Ok(());
    }
    if needs_billing(archive) {
        let rows = cursor::fetch_all_events(token, None, None).await?;
        hydrate_billing(archive, &rows)?;
    }
    prune_api_events(archive)?;
    Ok(())
}

fn persist_api_filter(archive: &mut UsageArchive, ignore_api_models: bool) -> Result<(), String> {
    if ignore_api_models && prune_api_events(archive)? {
        save(archive)?;
    }
    Ok(())
}

/// 编辑保存前准备清理后的事件库；联网及验证成功前不写盘。调用方须持有 account_lock。
pub(crate) async fn prepare_api_events_removal(
    account_id: &str,
    token: &str,
) -> Result<Option<UsageArchive>, String> {
    // 编辑保存不能把读取失败当作「没有记录」，否则会在明细未清理时报告开关已生效。
    let path = file_path(account_id)?;
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("读取本地用量数据失败：{e}")),
    };
    let mut archive: UsageArchive = serde_json::from_str(&text)
        .map_err(|e| format!("解析本地用量数据失败：{e}"))?;
    if archive.version < MIN_EVENT_ARCHIVE_VERSION {
        return Err("本地用量数据格式过旧，请先刷新用量再开启此开关。".into());
    }
    archive.account_id = account_id.to_string();
    prepare_api_filter(&mut archive, &http::normalize_cursor_token(token), true).await?;
    Ok(Some(archive))
}

/// 编辑字段验证通过后，在保存账户开关前落盘；调用方仍须持有 account_lock。
pub(crate) fn save_api_events_removal(archive: Option<&UsageArchive>) -> Result<(), String> {
    archive.map_or(Ok(()), save)
}

/// 读取时也校验口径，兼容较早版本生成的事件库与备份。
fn recorded_events(
    archive: &UsageArchive,
    ignore_api_models: bool,
) -> impl Iterator<Item = &EventTuple> {
    archive.events.iter()
        .filter(move |e| !ignore_api_models || event_api_usage(e, legacy_api_filter(archive)) != Some(true))
}

fn account_api_filter(archive: &UsageArchive) -> Result<bool, String> {
    if let Some(meta) = &archive.deleted {
        return Ok(meta.ignore_api_models);
    }
    crate::settings::ensure_loaded()?;
    Ok(accounts::account_snapshot(&archive.account_id)
        .map(|a| a.kind == "cursor" && a.ignore_api_models)
        .unwrap_or(false))
}

/// 按 [start, end]（unix 毫秒，闭区间，None = 不限）切片并按当前价格表聚合。
/// 有界范围只计入带时间戳的事件；「全部」（两端都为 None）连无时间戳的事件一起计入。
/// 区间不超过两天时按小时序列覆盖区间内的日期（任意历史日期都能画 24 小时柱图）。
fn slice(
    archive: &UsageArchive,
    start: Option<i64>,
    end: Option<i64>,
    ignore_api_models: bool,
) -> UsageAggregate {
    let bounded = start.is_some() || end.is_some();
    let rows: Vec<TokenRow> = recorded_events(archive, ignore_api_models)
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
    let stamps = || recorded_events(archive, meta.ignore_api_models).filter_map(|e| e.6);
    let api_removal_error = archive.events.iter()
        .any(|event| event_api_usage(event, true).is_none())
        .then(|| "历史记录包含无法识别的模型或计费类别，暂时无法移除 API 历史记录。".to_string());
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
        ignore_api_models: meta.ignore_api_models,
        api_removal_approximate: meta.api_filter_legacy || archive.events.iter().any(|e| e.7.is_none()),
        api_removal_error,
        events: recorded_events(archive, meta.ignore_api_models).count(),
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
    archive.account_id = account_id.clone();
    archive.deleted = None;
    let ignore_api_models = account_api_filter(&archive)?;
    let upgrade_billing = ignore_api_models && needs_billing(&archive);
    if !upgrade_billing {
        persist_api_filter(&mut archive, ignore_api_models)?;
    }
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
    if do_sync || upgrade_billing {
        match sync_events(&mut archive, &token, full, ignore_api_models).await {
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
                if archive.synced_at.is_none() || upgrade_billing {
                    return Err(e);
                }
                sync_error = Some(e);
            }
        }
    }
    Ok(UsageSlice {
        agg: slice(&archive, start, end, ignore_api_models),
        ignore_api_models,
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
    let mut archive = load(&account_id).ok_or_else(|| "no_usage_data".to_string())?;
    let ignore_api_models = account_api_filter(&archive)?;
    persist_api_filter(&mut archive, ignore_api_models)?;
    Ok(UsageSlice {
        agg: slice(&archive, start, end, ignore_api_models),
        ignore_api_models,
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletedUsageUpdateResult {
    pub record: DeletedUsageRecord,
    pub removed_events: usize,
}

/// 一次性移除 API 历史并编辑备注。remove_api_usage = false 只编辑备注，绝不重新开启记录。
/// 先处理副本，无法识别任一事件时，备注、开关与事件均不改变。
fn apply_deleted_update(
    archive: &mut UsageArchive,
    note: &str,
    remove_api_usage: bool,
) -> Result<usize, String> {
    if archive.deleted.is_none() {
        return Err("not_deleted_account".into());
    }
    let mut next = archive.clone();
    let before = next.events.len();
    if remove_api_usage {
        if next.events.iter().any(|e| e.7.is_none()) {
            next.deleted.as_mut().unwrap().api_filter_legacy = true;
        }
        prune_api_events(&mut next)?;
        next.deleted.as_mut().unwrap().ignore_api_models = true;
    }
    let identity = next.identity.clone();
    apply_deleted_note(next.deleted.as_mut().unwrap(), identity.as_deref(), note);
    next.version = ARCHIVE_VERSION;
    let removed = before - next.events.len();
    *archive = next;
    Ok(removed)
}

/// 已删除账户的编辑入口：仅接受「移除 API 历史」，没有恢复 / 重新开启的参数。
#[tauri::command]
pub async fn cursor_usage_deleted_update(
    app: AppHandle,
    account_id: String,
    note: String,
    remove_api_usage: bool,
) -> Result<DeletedUsageUpdateResult, String> {
    let _deleted_guard = deleted_mutation_lock().lock().await;
    let lock = account_lock(&account_id);
    let _guard = lock.lock().await;
    let mut archive = load(&account_id).ok_or_else(|| "no_usage_data".to_string())?;
    let removed_events = apply_deleted_update(&mut archive, &note, remove_api_usage)?;
    save(&archive)?;
    let record = deleted_record(&archive).ok_or_else(|| "not_deleted_account".to_string())?;
    audit::log(
        "usage_data_edit",
        format!(
            "编辑已删除账户「{}」{}",
            deleted_label(archive.deleted.as_ref().unwrap()),
            if remove_api_usage {
                let method = if record.api_removal_approximate { "按模型归属近似移除" } else { "永久移除" };
                format!("，{method} {removed_events} 条 API 历史记录")
            } else {
                "的备注".into()
            },
        ),
        Some(json!({"id": account_id, "removedEvents": removed_events, "approximate": record.api_removal_approximate})),
    );
    accounts::notify_usage_archive_changed(&app);
    Ok(DeletedUsageUpdateResult { record, removed_events })
}

/// 导入备份也遵守单向关闭：同身份的本地记录已经移除 API 历史时，不允许旧备份重新开启。
fn preserve_deleted_api_filter(
    incoming: &mut UsageArchive,
    local: &[UsageArchive],
) -> Result<(), String> {
    if local.iter().any(|a| a.deleted.as_ref().is_some_and(|m| m.ignore_api_models)) {
        let Some(meta) = incoming.deleted.as_mut() else {
            return Err("not_deleted_account".into());
        };
        meta.ignore_api_models = true;
        meta.api_filter_legacy |= local.iter().any(|a| {
            a.deleted.as_ref().is_some_and(|m| m.ignore_api_models && m.api_filter_legacy)
        });
    }
    if incoming.deleted.as_ref().is_some_and(|m| m.ignore_api_models) {
        prune_api_events(incoming)?;
    }
    Ok(())
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
    let mut archive = load(&account_id).ok_or_else(|| "no_usage_data".to_string())?;
    let table = pricing::load();
    let ignore_api_models = account_api_filter(&archive)?;
    persist_api_filter(&mut archive, ignore_api_models)?;
    let bounded = start.is_some() || end.is_some();
    let mut events: Vec<RawUsageEvent> = recorded_events(&archive, ignore_api_models)
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
        total: recorded_events(&archive, ignore_api_models).count(),
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
    // 删除流程可能排在编辑账户之后，锁内重读偏好以免使用旧快照。
    let ignore_api_models = accounts::account_snapshot(&acc.id)
        .map(|a| a.ignore_api_models)
        .unwrap_or(acc.ignore_api_models);
    archive.account_id = acc.id.clone();
    let token = http::normalize_cursor_token(&acc.token);
    let synced = sync_events(&mut archive, &token, false, ignore_api_models).await;
    // 同步失败可使用旧数据，但不能把缺失计费归属的旧事件作为已过滤数据保留。
    persist_api_filter(&mut archive, ignore_api_models)?;
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
        ignore_api_models,
        api_filter_legacy: false,
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
    let _deleted_guard = deleted_mutation_lock().lock().await;
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
    {
        let old_lock = account_lock(&old_id);
        let _old_guard = old_lock.lock().await;
        if chosen.deleted.as_ref().is_some_and(|meta| meta.ignore_api_models)
            && prune_api_events(&mut chosen).is_err() {
            return false;
        }
        chosen.deleted = None;
        if accounts::account_snapshot(account_id).is_ok_and(|a| a.ignore_api_models) {
            if prepare_api_filter(&mut chosen, &http::normalize_cursor_token(token), true).await.is_err() {
                return false;
            }
        }
        chosen.account_id = account_id.to_string();
        chosen.identity = Some(identity);
        if save(&chosen).is_err() {
            return false;
        }
        remove_file(&old_id);
    }
    let events = chosen.events.len();
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
pub fn export_deleted() -> Result<Vec<Value>, String> {
    deleted_archives()
        .into_iter()
        .map(|mut a| {
            if a.deleted.as_ref().is_some_and(|meta| meta.ignore_api_models) {
                // 无凭据可补齐的旧库不能静默删除事件，也不能作为已过滤数据导出。
                prune_api_events(&mut a).map_err(|e| format!("已删除账户的统计数据需要补齐计费信息；重新添加该账户并刷新后再导出：{e}"))?;
            }
            serde_json::to_value(a).map_err(|e| e.to_string())
        })
        .collect()
}

/// 全量备份导入：恢复备份里已删除账户的事件库。同身份账户在本机仍在用则跳过（它会自行同步）；
/// 本机已有同身份 / 同 id 的已删除记录时只在备份数据更新时替换；其余直接写入。返回恢复条数。
pub async fn import_deleted(items: &[Value], live_identities: &[String]) -> usize {
    let _deleted_guard = deleted_mutation_lock().lock().await;
    let mut restored = 0usize;
    for item in items {
        let Ok(mut incoming) = serde_json::from_value::<UsageArchive>(item.clone()) else {
            continue;
        };
        if incoming.version < MIN_EVENT_ARCHIVE_VERSION
            || incoming.deleted.is_none()
            || (incoming.events.is_empty() && !incoming.deleted.as_ref().is_some_and(|m| m.ignore_api_models))
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
        if preserve_deleted_api_filter(&mut incoming, &local_same).is_err() {
            continue;
        }
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

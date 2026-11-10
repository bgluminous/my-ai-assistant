//! OpenCode 官方请求明细归档。工作区 + 记录 ID 去重；分页落盘但仅完整同步推进水位。
//! 不保留请求头、正文、客户端位置、成员等与工作区总量无关的信息。
use crate::{accounts, opencode, paths, pricing};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tauri::{AppHandle, Emitter};

const DAY: i64 = 86_400_000;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Event {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub time: i64,
    pub model: String,
    pub product: String,
    pub input: u64,
    pub output: u64,
    #[serde(default)]
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    #[serde(default)]
    pub cache_write_1h: u64,
    pub reported_cost_usd: Option<f64>,
}

fn number(v: &Value, key: &str) -> Result<u64, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(0),
        Some(n) => n
            .as_u64()
            .filter(|n| *n <= 9_007_199_254_740_991)
            .ok_or_else(|| "opencode_invalid_usage".into()),
    }
}

pub fn parse_event(v: &Value, workspace: &str) -> Result<Option<Event>, String> {
    if opencode::text(v, "category") != "inference"
        || opencode::text(v, "provider") != "opencode"
        || !matches!(
            opencode::text(v, "product"),
            "standard" | "go" | "go-plus" | "go-teams"
        )
    {
        return Ok(None);
    }
    if v.get("workspaceID")
        .and_then(Value::as_str)
        .is_some_and(|id| id != workspace)
    {
        return Err("opencode_workspace_mismatch".into());
    }
    let id = opencode::text(v, "id");
    let model = opencode::text(v, "model");
    let model = if model.is_empty() {
        opencode::text(v, "requestedModel")
    } else {
        model
    };
    let model = if model.is_empty() { "unknown" } else { model };
    let time = v["startedAt"]
        .as_i64()
        .filter(|t| *t > 0)
        .ok_or("opencode_invalid_usage")?;
    if id.is_empty() || id.len() > 256 || model.is_empty() || model.len() > 512 {
        return Err("opencode_invalid_usage".into());
    }
    let cost = match v.get("cost") {
        None | Some(Value::Null) => None,
        Some(cost) => Some(
            cost.as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .ok_or("opencode_invalid_usage")?,
        ),
    };
    let request_id = v
        .get("requestID")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if request_id.as_ref().is_some_and(|s| s.len() > 512) {
        return Err("opencode_invalid_usage".into());
    }
    Ok(Some(Event {
        id: id.into(),
        request_id,
        time,
        model: model.into(),
        product: opencode::text(v, "product").into(),
        input: number(v, "inputTokens")?,
        output: number(v, "outputTokens")?,
        reasoning: number(v, "reasoningTokens")?,
        cache_read: number(v, "cacheReadTokens")?,
        // request-logs 的 cacheWriteTokens 为总缓存写入，1h 是其中的细分，不能再次相加。
        cache_write: number(v, "cacheWriteTokens")?,
        cache_write_1h: number(v, "cacheWrite1hTokens")?,
        reported_cost_usd: cost,
    }))
}

fn open_path(path: &Path) -> Result<Connection, String> {
    let db = Connection::open(path).map_err(|_| "opencode_archive_open_failed")?;
    db.busy_timeout(Duration::from_secs(5))
        .map_err(|_| "opencode_archive_open_failed")?;
    db.execute_batch("PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS events(workspace TEXT NOT NULL,id TEXT NOT NULL,time INTEGER NOT NULL,data TEXT NOT NULL,PRIMARY KEY(workspace,id));
        CREATE INDEX IF NOT EXISTS opencode_event_time ON events(workspace,time);
        CREATE TABLE IF NOT EXISTS sync(workspace TEXT PRIMARY KEY,until_ms INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS checks(workspace TEXT PRIMARY KEY,data TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS coverage(workspace TEXT NOT NULL,start_ms INTEGER NOT NULL,end_ms INTEGER NOT NULL,PRIMARY KEY(workspace,start_ms));")
        .map_err(|_| "opencode_archive_open_failed")?;
    Ok(db)
}
fn open() -> Result<Connection, String> {
    let dir = paths::app_dir().ok_or("home_dir_unavailable")?;
    std::fs::create_dir_all(&dir).map_err(|_| "opencode_archive_open_failed")?;
    open_path(&dir.join("opencode-usage.sqlite"))
}
fn watermark(db: &Connection, workspace: &str) -> Result<i64, String> {
    db.query_row(
        "SELECT until_ms FROM sync WHERE workspace=?1",
        [workspace],
        |r| r.get(0),
    )
    .optional()
    .map(|n| n.unwrap_or(0))
    .map_err(|_| "opencode_archive_read_failed".into())
}
fn save_page(db: &mut Connection, workspace: &str, rows: &[Event]) -> Result<(), String> {
    let tx = db
        .transaction()
        .map_err(|_| "opencode_archive_write_failed")?;
    save_events(&tx, workspace, rows)?;
    tx.commit().map_err(|_| "opencode_archive_write_failed".into())
}

fn save_events(db: &Connection, workspace: &str, rows: &[Event]) -> Result<(), String> {
    for row in rows {
        db.execute("INSERT INTO events VALUES(?1,?2,?3,?4) ON CONFLICT(workspace,id) DO UPDATE SET time=excluded.time,data=excluded.data",
            params![workspace,row.id,row.time,serde_json::to_string(row).map_err(|_|"opencode_invalid_usage")?])
            .map_err(|_| "opencode_archive_write_failed")?;
    }
    Ok(())
}

fn coverage(db: &Connection, workspace: &str) -> Result<Vec<(i64, i64)>, String> {
    let mut s = db
        .prepare("SELECT start_ms,end_ms FROM coverage WHERE workspace=?1 ORDER BY start_ms")
        .map_err(|_| "opencode_archive_read_failed")?;
    let rows = s
        .query_map([workspace], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|_| "opencode_archive_read_failed")?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|_| "opencode_archive_read_failed".into())
}
fn completed(db: &mut Connection, workspace: &str, start: i64, end: i64) -> Result<(), String> {
    let tx = db.transaction().map_err(|_| "opencode_archive_write_failed")?;
    complete_range(&tx, workspace, start, end)?;
    tx.commit().map_err(|_| "opencode_archive_write_failed".into())
}

fn complete_range(db: &Connection, workspace: &str, start: i64, end: i64) -> Result<(), String> {
    let mut intervals = coverage(db, workspace)?;
    intervals.push((start, end));
    intervals.sort_unstable();
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (a, b) in intervals {
        if let Some(last) = merged.last_mut().filter(|last| a <= last.1) {
            last.1 = last.1.max(b);
        } else {
            merged.push((a, b));
        }
    }
    db.execute("DELETE FROM coverage WHERE workspace=?1", [workspace])
        .map_err(|_| "opencode_archive_write_failed")?;
    for (a, b) in merged {
        db.execute(
            "INSERT INTO coverage VALUES(?1,?2,?3)",
            params![workspace, a, b],
        )
        .map_err(|_| "opencode_archive_write_failed")?;
    }
    db.execute("INSERT INTO sync VALUES(?1,?2) ON CONFLICT(workspace) DO UPDATE SET until_ms=max(until_ms,excluded.until_ms)",params![workspace,end]).map_err(|_|"opencode_archive_write_failed")?;
    Ok(())
}

fn locks() -> &'static Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}
pub fn workspace_lock(workspace: &str) -> Arc<tokio::sync::Mutex<()>> {
    locks()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(workspace.into())
        .or_default()
        .clone()
}

fn sync_start(last: i64, until: i64, full: bool) -> i64 {
    if full || last == 0 {
        until - 30 * DAY
    } else {
        (last - 2 * DAY).max(until - 30 * DAY)
    }
}

async fn sync(app: &AppHandle, id: &str, workspace: &str, full: bool) -> Result<(), String> {
    let context = opencode::account_get(app, id, "/api/orgs/current").await?;
    if context.pointer("/org/id").and_then(Value::as_str) != Some(workspace) {
        return Err("opencode_workspace_mismatch".into());
    }
    // Member 的成功响应只包含本人记录，不能被标记为工作区完整覆盖。
    if !opencode::workspace_usage_allowed(&context) {
        return Err("opencode_workspace_usage_forbidden".into());
    }
    let until = Utc::now().timestamp_millis();
    let mut db = open()?;
    let since = sync_start(watermark(&db, workspace)?, until, full);
    sync_pages(&mut db, workspace, since, until, |path| async move {
        opencode::account_get(app, id, &path).await
    })
    .await?;
    let _ = app.emit("opencode-usage-changed", json!({"workspaceId":workspace}));
    Ok(())
}

async fn sync_pages<F, Fut>(
    db: &mut Connection,
    workspace: &str,
    since: i64,
    until: i64,
    mut fetch: F,
) -> Result<(), String>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    let mut coverage_start = since;
    let mut bound = until;
    let mut cursor = String::new();
    let mut seen = HashSet::new();
    let mut restarted = false;
    loop {
        let mut url = reqwest::Url::parse("https://opencode.ai/console/api/request-logs").unwrap();
        url.query_pairs_mut()
            .append_pair("category", "inference")
            .append_pair("since", &since.to_string())
            .append_pair("until", &bound.to_string())
            .append_pair("limit", "100");
        if !cursor.is_empty() {
            url.query_pairs_mut().append_pair("cursor", &cursor);
        }
        let result = fetch(format!("/api/request-logs?{}", url.query().unwrap())).await;
        let body = match result {
            Err(e) if e == "opencode_http_409" && !restarted => {
                restarted = true;
                cursor.clear();
                seen.clear();
                continue;
            }
            other => other?,
        };
        let items = body["items"]
            .as_array()
            .ok_or("opencode_invalid_response")?;
        if let Some(days) = body.get("retentionDays") {
            let days = days
                .as_i64()
                .filter(|d| *d > 0 && *d <= 365)
                .ok_or("opencode_invalid_response")?;
            coverage_start = coverage_start.max(until - days * DAY);
        }
        let response_until = body["until"]
            .as_i64()
            .filter(|n| *n > since && *n <= until)
            .ok_or("opencode_invalid_response")?;
        if cursor.is_empty() {
            bound = response_until;
        } else if response_until != bound {
            return Err("opencode_cursor_changed".into());
        }
        let mut rows = Vec::new();
        for item in items {
            if let Some(row) = parse_event(item, workspace)? {
                if row.time >= since && row.time <= bound {
                    rows.push(row);
                }
            }
        }
        // 每页原子更新；失败后重复下载安全，旧水位不动。
        save_page(db, workspace, &rows)?;
        match body.get("nextCursor") {
            Some(Value::Null) => break,
            Some(Value::String(next)) if !next.is_empty() && seen.insert(next.clone()) => {
                cursor = next.clone()
            }
            _ => return Err("opencode_invalid_cursor".into()),
        }
    }
    completed(db, workspace, coverage_start, bound)?;
    Ok(())
}

pub fn query_db(
    db: &Connection,
    workspace: &str,
    start: Option<i64>,
    end: Option<i64>,
) -> Result<Value, String> {
    if start.zip(end).is_some_and(|(a, b)| a > b) {
        return Err("invalid_range".into());
    }
    let mut stmt = db
        .prepare(
            "SELECT data FROM events WHERE workspace=?1 AND time>=?2 AND time<=?3 ORDER BY time,id",
        )
        .map_err(|_| "opencode_archive_read_failed")?;
    let strings = stmt
        .query_map(
            params![workspace, start.unwrap_or(0), end.unwrap_or(i64::MAX)],
            |r| r.get::<_, String>(0),
        )
        .map_err(|_| "opencode_archive_read_failed")?;
    let table = pricing::load();
    let mut rows = Vec::new();
    let mut costs: HashMap<String, f64> = HashMap::new();
    let mut cost = 0.0;
    let mut unknown_cost = 0;
    let mut known_cost = 0;
    for raw in strings {
        let event: Event = serde_json::from_str(&raw.map_err(|_| "opencode_archive_read_failed")?)
            .map_err(|_| "opencode_archive_corrupt")?;
        if let Some(usd) = event.reported_cost_usd {
            known_cost += 1;
            cost += usd;
            *costs
                .entry(crate::model_match::display_key(&table, &event.model))
                .or_default() += usd;
        } else {
            unknown_cost += 1;
        }
        rows.push(pricing::TokenRow {
            model: event.model,
            input: event.input as f64,
            output: event.output as f64,
            cache_read: event.cache_read as f64,
            cache_write: event.cache_write as f64,
            actual_cents: 0.0,
            timestamp_ms: Some(event.time),
        });
    }
    let dates = pricing::hourly_dates_for(start, end);
    let mut agg = serde_json::to_value(pricing::aggregate_and_price_for(
        rows,
        &table,
        dates.as_deref(),
    ))
    .map_err(|_| "opencode_invalid_usage")?;
    for model in agg["models"]
        .as_array_mut()
        .ok_or("opencode_invalid_usage")?
    {
        model["reportedCostUsd"] = json!(costs.get(opencode::text(model, "model")).copied());
    }
    let intervals = coverage(db, workspace)?;
    let covered_start = intervals.first().map(|i| i.0);
    let expected_start = start
        .or(covered_start)
        .unwrap_or_else(|| Utc::now().timestamp_millis() - 30 * DAY);
    let expected_end = end
        .unwrap_or_else(|| Utc::now().timestamp_millis())
        .min(Utc::now().timestamp_millis());
    let synced = watermark(db, workspace)?;
    let mut cursor = expected_start;
    let mut gaps = Vec::new();
    for (a, b) in &intervals {
        if *b <= cursor {
            continue;
        }
        if *a > cursor && cursor < expected_end {
            gaps.push(json!({"start":cursor,"end":(*a).min(expected_end)}));
        }
        cursor = cursor.max(*b);
    }
    // 已确认的水位之后标记为待同步；单独显示，不把它误当作完整零用量。
    if synced == 0 && cursor < expected_end {
        gaps.push(json!({"start":cursor,"end":expected_end}));
    } else if cursor < expected_end.min(synced) {
        gaps.push(json!({"start":cursor,"end":expected_end.min(synced)}));
    }
    agg["pendingSince"] = if synced > 0 && synced < expected_end {
        json!(synced.max(expected_start))
    } else {
        Value::Null
    };
    let check: Option<String> = db
        .query_row(
            "SELECT data FROM checks WHERE workspace=?1",
            [workspace],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| "opencode_archive_read_failed")?;
    agg["reconciliation"] = check
        .and_then(|v| serde_json::from_str::<Value>(&v).ok())
        .unwrap_or(Value::Null);
    agg["source"] = json!("opencode");
    agg["totalReportedCostUsd"] = if known_cost == 0 && unknown_cost > 0 {
        Value::Null
    } else {
        json!(cost)
    };
    agg["unreportedCostEvents"] = json!(unknown_cost);
    agg["actualUnavailable"] = json!(true);
    agg["coverageStart"] = json!(covered_start);
    agg["coverageGaps"] = json!(gaps);
    Ok(json!({"agg":agg,"syncedAt":synced,"syncError":null}))
}

#[tauri::command]
pub async fn opencode_usage_fetch(
    app: AppHandle,
    account_id: String,
    start: Option<i64>,
    end: Option<i64>,
    mode: Option<String>,
    max_age_ms: Option<i64>,
) -> Result<Value, String> {
    let _operation = crate::operations::begin()?;
    let account = accounts::account_snapshot(&account_id)?;
    if account.kind != "opencode" || !opencode::valid_workspace(&account.token) {
        return Err("not_opencode_account".into());
    }
    let lock = workspace_lock(&account.token);
    let _guard = lock.lock().await;
    let last = watermark(&open()?, &account.token)?;
    let mode = mode.as_deref().unwrap_or("auto");
    let refresh = mode == "full"
        || mode == "sync"
        || (mode != "read"
            && Utc::now().timestamp_millis() - last
                > max_age_ms.unwrap_or(300_000).clamp(0, 86_400_000));
    let error = if refresh {
        sync(&app, &account_id, &account.token, mode == "full")
            .await
            .err()
    } else {
        None
    };
    if mode == "full" && error.is_none() {
        let check = match reconcile(&app, &account_id, &account.token).await {
            Ok(value) => value,
            Err(e) => json!({"state":"unavailable","error":e,"at":Utc::now().timestamp_millis()}),
        };
        open()?.execute("INSERT INTO checks VALUES(?1,?2) ON CONFLICT(workspace) DO UPDATE SET data=excluded.data",
            params![account.token,check.to_string()]).map_err(|_|"opencode_archive_write_failed")?;
    }
    let mut result = query_db(&open()?, &account.token, start, end)?;
    result["syncError"] = json!(error);
    Ok(result)
}

fn summary_tokens(value: &Value) -> Result<u64, String> {
    let mut total = 0u64;
    for key in [
        "totalInputTokens",
        "totalOutputTokens",
        "totalCacheReadTokens",
        "totalCacheWrite5mTokens",
        "totalCacheWrite1hTokens",
    ] {
        let n = value[key]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or("opencode_invalid_response")?;
        total = total.checked_add(n).ok_or("opencode_invalid_response")?;
    }
    Ok(total)
}

/// 核对最近完整 UTC 日，避免进行中窗口漂移。官方汇总可能延迟且请求数不含失败日志，
/// 因此仅以过滤官方模型后的 Tokens 比较；差异只提示，不覆盖明细或推导钱包实扣。
async fn reconcile(app: &AppHandle, id: &str, workspace: &str) -> Result<Value, String> {
    let end = Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis();
    let start = end - DAY;
    let day = chrono::DateTime::from_timestamp_millis(start)
        .unwrap()
        .format("%Y-%m-%d")
        .to_string();
    let local = query_db(&open()?, workspace, Some(start), Some(end - 1))?;
    if local["agg"]["coverageGaps"]
        .as_array()
        .is_none_or(|g| !g.is_empty())
        || local["syncedAt"].as_i64().unwrap_or(0) < end
    {
        return Ok(json!({"state":"incomplete","date":day}));
    }
    let query = format!("from={day}&to={day}");
    let summary = opencode::account_get(app, id, &format!("/api/usage/summary?{query}")).await?;
    let hours = opencode::account_get(
        app,
        id,
        &format!("/api/usage/cost-by-day?{query}&bucket=hour"),
    )
    .await?;
    let summary_total = summary_tokens(&summary)?;
    let mut hourly_total = 0u64;
    for hour in hours.as_array().ok_or("opencode_invalid_response")? {
        let n = hour["totalTokens"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or("opencode_invalid_response")?;
        hourly_total = hourly_total
            .checked_add(n)
            .ok_or("opencode_invalid_response")?;
    }
    let mut official_total = 0u64;
    let mut page = 1;
    loop {
        let models = opencode::account_get(
            app,
            id,
            &format!("/api/usage/models?{query}&page={page}&pageSize=100&includeLegacyKeys=true"),
        )
        .await?;
        for model in models["items"]
            .as_array()
            .ok_or("opencode_invalid_response")?
        {
            if matches!(
                opencode::text(model, "provider"),
                "opencode" | "opencode-go"
            ) {
                official_total = official_total
                    .checked_add(summary_tokens(model)?)
                    .ok_or("opencode_invalid_response")?;
            }
        }
        let pages = models["pageInfo"]["pageCount"]
            .as_u64()
            .filter(|n| *n <= 1000)
            .ok_or("opencode_invalid_response")?;
        if models["pageInfo"]["page"].as_u64() != Some(page) {
            return Err("opencode_invalid_response".into());
        }
        if page >= pages {
            break;
        }
        page += 1;
    }
    let local_total = local["agg"]["totalTokens"]
        .as_f64()
        .ok_or("opencode_invalid_usage")?;
    Ok(
        json!({"state":if local_total==official_total as f64 {"matched"} else {"different"},
        "date":day,"at":Utc::now().timestamp_millis(),"localTokens":local_total,"officialTokens":official_total,
        "summaryTokens":summary_total,"hourlyTokens":hourly_total,"summaryMatchesHours":summary_total==hourly_total}),
    )
}

/// 持有工作区锁时调用。先在事务内暂存删除，设置保存失败会回滚归档。
pub fn remove_after_save<T>(workspace: &str, save: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    remove_after_save_db(&mut open()?, workspace, save)
}

fn remove_after_save_db<T>(db: &mut Connection, workspace: &str, save: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let tx = db
        .transaction()
        .map_err(|_| "opencode_archive_write_failed")?;
    for table in ["events", "sync", "coverage", "checks"] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE workspace=?1"),
            [workspace],
        )
        .map_err(|_| "opencode_archive_write_failed")?;
    }
    let result = save()?;
    tx.commit().map_err(|_| "opencode_archive_cleanup_failed_account_deleted")?;
    Ok(result)
}

pub fn export_archives() -> Result<Value, String> {
    let mut connection = open()?;
    let db = connection
        .transaction()
        .map_err(|_| "opencode_archive_read_failed")?;
    export_db(&db)
}
fn export_db(db: &Connection) -> Result<Value, String> {
    let mut stmt = db
        .prepare("SELECT DISTINCT workspace FROM events UNION SELECT workspace FROM sync")
        .map_err(|_| "opencode_archive_read_failed")?;
    let spaces = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|_| "opencode_archive_read_failed")?;
    let mut out = Vec::new();
    for space in spaces {
        let workspace = space.map_err(|_| "opencode_archive_read_failed")?;
        let mut stmt = db
            .prepare("SELECT data FROM events WHERE workspace=?1 ORDER BY time,id")
            .map_err(|_| "opencode_archive_read_failed")?;
        let strings = stmt
            .query_map([&workspace], |r| r.get::<_, String>(0))
            .map_err(|_| "opencode_archive_read_failed")?;
        let mut events = Vec::new();
        for raw in strings {
            events.push(
                serde_json::from_str::<Event>(&raw.map_err(|_| "opencode_archive_read_failed")?)
                    .map_err(|_| "opencode_archive_corrupt")?,
            );
        }
        out.push(json!({"workspaceId":workspace,"events":events,"coverage":coverage(&db,&workspace)?,"syncedAt":watermark(&db,&workspace)?}));
    }
    Ok(json!({"version":1,"workspaces":out}))
}

type Archive = (String, Vec<Event>, Vec<(i64, i64)>);
fn parse_archives(value: &Value) -> Result<Vec<Archive>, String> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    if value["version"] != 1 {
        return Err("opencode_archive_invalid_backup".into());
    }
    let workspaces = value["workspaces"]
        .as_array()
        .ok_or("opencode_archive_invalid_backup")?;
    // 全部校验后再写入，损坏的备份不能半途导入。
    let mut parsed = Vec::new();
    for item in workspaces {
        let workspace = opencode::text(item, "workspaceId");
        if !opencode::valid_workspace(workspace) {
            return Err("opencode_archive_invalid_backup".into());
        }
        let events: Vec<Event> = serde_json::from_value(item["events"].clone())
            .map_err(|_| "opencode_archive_invalid_backup")?;
        if events.iter().any(|e| {
            e.id.is_empty()
                || e.id.len() > 256
                || e.model.is_empty()
                || e.model.len() > 512
                || e.time <= 0
                || !matches!(
                    e.product.as_str(),
                    "standard" | "go" | "go-plus" | "go-teams"
                )
                || e.request_id.as_ref().is_some_and(|id| id.len() > 512)
                || [
                    e.input,
                    e.output,
                    e.reasoning,
                    e.cache_read,
                    e.cache_write,
                    e.cache_write_1h,
                ]
                .iter()
                .any(|n| *n > 9_007_199_254_740_991)
                || e.reported_cost_usd
                    .is_some_and(|n| !n.is_finite() || n < 0.0)
        }) {
            return Err("opencode_archive_invalid_backup".into());
        }
        let ranges: Vec<(i64, i64)> = serde_json::from_value(item["coverage"].clone())
            .map_err(|_| "opencode_archive_invalid_backup")?;
        if ranges
            .iter()
            .any(|(a, b)| *a <= 0 || a > b || *b > Utc::now().timestamp_millis() + DAY)
        {
            return Err("opencode_archive_invalid_backup".into());
        }
        parsed.push((workspace.to_string(), events, ranges));
    }
    Ok(parsed)
}

pub fn validate_archives(value: &Value) -> Result<(), String> {
    parse_archives(value).map(|_| ())
}

pub async fn import_archives(value: &Value) -> Result<usize, String> {
    let parsed = parse_archives(value)?;
    if parsed.is_empty() { return Ok(0); }
    // 固定顺序一次获取全部工作区锁，重复工作区只锁一次，避免并发导入互相等待。
    let mut workspaces: Vec<_> = parsed.iter().map(|a| a.0.as_str()).collect();
    workspaces.sort_unstable();
    workspaces.dedup();
    let locks: Vec<_> = workspaces.into_iter().map(workspace_lock).collect();
    let mut guards = Vec::new();
    for lock in &locks { guards.push(lock.lock().await); }
    restore_archives(&mut open()?, parsed)
}

fn restore_archives(db: &mut Connection, archives: Vec<Archive>) -> Result<usize, String> {
    let tx = db.transaction().map_err(|_| "opencode_archive_write_failed")?;
    let mut count = 0;
    for (workspace, events, ranges) in archives {
        count += restore_workspace(&tx, &workspace, events, ranges)?;
    }
    tx.commit().map_err(|_| "opencode_archive_write_failed")?;
    Ok(count)
}

fn restore_workspace(
    db: &Connection,
    workspace: &str,
    events: Vec<Event>,
    ranges: Vec<(i64, i64)>,
) -> Result<usize, String> {
    let mut existing: HashSet<String> = {
        let mut statement = db
            .prepare("SELECT id FROM events WHERE workspace=?1")
            .map_err(|_| "opencode_archive_read_failed")?;
        let values = statement
            .query_map([workspace], |r| r.get::<_, String>(0))
            .map_err(|_| "opencode_archive_read_failed")?;
        values
            .collect::<Result<_, _>>()
            .map_err(|_| "opencode_archive_read_failed")?
    };
    // 本机已有记录优先；备份内重复 ID 只计一次。
    let events: Vec<_> = events
        .into_iter()
        .filter(|e| existing.insert(e.id.clone()))
        .collect();
    let count = events.len();
    save_events(db, workspace, &events)?;
    for (a, b) in ranges {
        complete_range(db, workspace, a, b)?;
    }
    Ok(count)
}

#[cfg(test)]
#[path = "tests/opencode_store.rs"]
mod tests;

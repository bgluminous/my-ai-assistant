use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;
use walkdir::WalkDir;

use crate::http;
use crate::paths;
use crate::pricing::{self, TokenRow, UsageAggregate};

const WHAM_USAGE: &str = "https://chatgpt.com/backend-api/wham/usage";
/// 订阅起止时间（access_token JWT 经常没有 chatgpt_subscription_active_*，以此接口为准）。
const SUBSCRIPTIONS: &str = "https://chatgpt.com/backend-api/subscriptions";

// ---------------------------------------------------------------------------
// 额度窗口（来自官方 wham/usage）
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub label: String,
    pub used_percent: Option<f64>,
    pub limit_window_seconds: Option<i64>,
    pub reset_at: Option<i64>,
    pub resets_in_seconds: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexUsage {
    pub alive: bool,
    pub email: Option<String>,
    pub plan: Option<String>,
    /// 套餐订阅生效时间（RFC3339）。优先 subscriptions.active_start，否则 JWT。
    pub plan_active_start: Option<String>,
    /// 套餐订阅到期时间（RFC3339）。优先 subscriptions.active_until，否则 JWT。
    pub plan_active_until: Option<String>,
    /// 订阅是否到期自动续期（来自 subscriptions.will_renew，可能缺失）。
    pub plan_will_renew: Option<bool>,
    pub account_id: Option<String>,
    pub exp: Option<i64>,
    pub windows: Vec<UsageWindow>,
    pub credits: Option<Value>,
    pub status: u16,
    pub raw: Value,
}

fn window_label(seconds: Option<i64>) -> String {
    match seconds {
        Some(18000) => "5 小时".to_string(),
        Some(604800) => "每周".to_string(),
        Some(2_592_000) => "每月".to_string(),
        Some(s) if s % 3600 == 0 => format!("{} 小时窗口", s / 3600),
        Some(s) => format!("{s} 秒窗口"),
        None => "用量窗口".to_string(),
    }
}

fn gi(v: &Value, keys: &[&str]) -> Option<i64> {
    for k in keys {
        if let Some(x) = v.get(k).and_then(|x| x.as_i64()) {
            return Some(x);
        }
        if let Some(x) = v.get(k).and_then(|x| x.as_f64()) {
            return Some(x as i64);
        }
    }
    None
}

fn gf(v: &Value, keys: &[&str]) -> Option<f64> {
    for k in keys {
        if let Some(x) = v.get(k).and_then(|x| x.as_f64()) {
            return Some(x);
        }
    }
    None
}

fn window_from_obj(v: &Value) -> UsageWindow {
    let seconds = gi(
        v,
        &["limit_window_seconds", "limitWindowSeconds", "window_size_seconds"],
    );
    let mut label = window_label(seconds);
    if let Some(name) = v.get("name").and_then(|x| x.as_str()) {
        if !name.is_empty() {
            label = format!("{label}（{name}）");
        }
    }
    UsageWindow {
        label,
        used_percent: gf(v, &["used_percent", "usedPercent"]),
        limit_window_seconds: seconds,
        reset_at: gi(v, &["reset_at", "resetAt"]),
        resets_in_seconds: gi(v, &["reset_after_seconds", "resets_in_seconds", "resetsInSeconds"]),
    }
}

/// 首选官方结构：顶层 rate_limit 的 primary_window / secondary_window。
/// additional_rate_limits 等嵌套结构里会出现重复窗口，因此不做全量递归收集。
fn windows_from_rate_limit(raw: &Value) -> Vec<UsageWindow> {
    let mut out = Vec::new();
    if let Some(rl) = raw.get("rate_limit") {
        for key in ["primary_window", "secondary_window"] {
            if let Some(w) = rl.get(key) {
                if w.is_object()
                    && (w.get("used_percent").is_some() || w.get("usedPercent").is_some())
                {
                    out.push(window_from_obj(w));
                }
            }
        }
    }
    out
}

/// 兜底：接口结构变化时递归收集所有含 used_percent 的对象。
fn collect_windows(v: &Value, out: &mut Vec<UsageWindow>) {
    match v {
        Value::Object(map) => {
            if map.contains_key("used_percent") || map.contains_key("usedPercent") {
                out.push(window_from_obj(v));
            }
            for (_, child) in map {
                collect_windows(child, out);
            }
        }
        Value::Array(arr) => {
            for child in arr {
                collect_windows(child, out);
            }
        }
        _ => {}
    }
}

/// unix 秒或毫秒 → RFC3339（UTC）。无法识别则返回 None。
fn unix_to_rfc3339(n: f64) -> Option<String> {
    if !n.is_finite() || n <= 0.0 {
        return None;
    }
    let secs = if n > 10_000_000_000.0 {
        (n / 1000.0) as i64
    } else {
        n as i64
    };
    Utc.timestamp_opt(secs, 0).single().map(|d| d.to_rfc3339())
}

/// 从 JSON 对象取日期字段：RFC3339 字符串、数字 unix 秒/毫秒、数字字符串均可。
fn json_date(v: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        let Some(x) = v.get(*k) else { continue };
        if let Some(s) = x.as_str().map(str::trim).filter(|s| !s.is_empty()) {
            if DateTime::parse_from_rfc3339(s).is_ok() {
                return Some(s.to_string());
            }
            if let Ok(n) = s.parse::<f64>() {
                if let Some(iso) = unix_to_rfc3339(n) {
                    return Some(iso);
                }
            }
            return Some(s.to_string());
        }
        let n = x.as_f64().or_else(|| x.as_i64().map(|i| i as f64));
        if let Some(n) = n {
            if let Some(iso) = unix_to_rfc3339(n) {
                return Some(iso);
            }
        }
    }
    None
}

struct SubscriptionInfo {
    plan_type: Option<String>,
    active_start: Option<String>,
    active_until: Option<String>,
    will_renew: Option<bool>,
}

/// 拉取 ChatGPT 订阅信息。失败不影响主流程（JWT 兜底）。
async fn fetch_subscription(jwt: &str, account_id: &str) -> Option<SubscriptionInfo> {
    let resp = http::client()
        .get(SUBSCRIPTIONS)
        .query(&[("account_id", account_id)])
        .header("Authorization", format!("Bearer {jwt}"))
        .header("Accept", "application/json")
        .header("Origin", "https://chatgpt.com")
        .header("Referer", "https://chatgpt.com/")
        .header("ChatGPT-Account-Id", account_id)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let raw: Value = resp.json().await.ok()?;
    Some(SubscriptionInfo {
        plan_type: raw
            .get("plan_type")
            .or_else(|| raw.get("planType"))
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        active_start: json_date(&raw, &["active_start", "activeStart"]),
        active_until: json_date(&raw, &["active_until", "activeUntil"]),
        will_renew: raw
            .get("will_renew")
            .or_else(|| raw.get("willRenew"))
            .and_then(|x| x.as_bool()),
    })
}

pub async fn codex_usage(jwt: String) -> Result<CodexUsage, String> {
    let jwt = jwt.trim().to_string();
    if jwt.is_empty() {
        return Err("empty_jwt".into());
    }
    let claims = http::decode_jwt_payload(&jwt);
    let auth = claims.as_ref().and_then(|c| c.get("https://api.openai.com/auth"));
    let profile = claims
        .as_ref()
        .and_then(|c| c.get("https://api.openai.com/profile"));
    let account_id = auth
        .and_then(|a| a.get("chatgpt_account_id"))
        .and_then(|x| x.as_str())
        .map(str::to_string);
    let mut plan = auth
        .and_then(|a| a.get("chatgpt_plan_type"))
        .and_then(|x| x.as_str())
        .map(str::to_string);
    // JWT 兜底：access_token 经常没有这两个字段，或给的是 unix 数字而非 RFC3339
    let mut plan_active_start = json_date(
        auth.unwrap_or(&Value::Null),
        &[
            "chatgpt_subscription_active_start",
            "chatgptSubscriptionActiveStart",
        ],
    );
    let mut plan_active_until = json_date(
        auth.unwrap_or(&Value::Null),
        &[
            "chatgpt_subscription_active_until",
            "chatgptSubscriptionActiveUntil",
        ],
    );
    let mut plan_will_renew = None;
    let email = profile
        .and_then(|p| p.get("email"))
        .and_then(|x| x.as_str())
        .map(str::to_string);
    let exp = claims.as_ref().and_then(|c| c.get("exp")).and_then(|x| x.as_i64());

    let client = http::client();
    let mut req = client
        .get(WHAM_USAGE)
        .header("Authorization", format!("Bearer {jwt}"))
        .header("Accept", "application/json")
        .header("Origin", "https://chatgpt.com")
        .header("Referer", "https://chatgpt.com/");
    if let Some(acc) = &account_id {
        req = req.header("ChatGPT-Account-Id", acc.clone());
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;

    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Ok(CodexUsage {
            alive: false,
            email,
            plan,
            plan_active_start,
            plan_active_until,
            plan_will_renew,
            account_id,
            exp,
            windows: vec![],
            credits: None,
            status: status.as_u16(),
            raw: Value::Null,
        });
    }

    // 用量接口成功后再拉订阅：access_token JWT 通常不含到期日，以此为准覆盖 JWT 兜底。
    if let Some(acc) = &account_id {
        if let Some(sub) = fetch_subscription(&jwt, acc).await {
            if plan.is_none() {
                plan = sub.plan_type;
            }
            if sub.active_start.is_some() {
                plan_active_start = sub.active_start;
            }
            if sub.active_until.is_some() {
                plan_active_until = sub.active_until;
            }
            plan_will_renew = sub.will_renew;
        }
    }

    let raw: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let mut windows = windows_from_rate_limit(&raw);
    if windows.is_empty() {
        // 老结构 / 结构漂移兜底：递归收集后按窗口时长去重
        let mut collected = Vec::new();
        collect_windows(&raw, &mut collected);
        let mut seen = std::collections::HashSet::new();
        for w in collected {
            if seen.insert(w.limit_window_seconds) {
                windows.push(w);
            }
        }
    }
    let credits = raw
        .get("credits")
        .cloned()
        .or_else(|| raw.pointer("/balance").cloned());

    Ok(CodexUsage {
        alive: status.is_success(),
        email,
        plan,
        plan_active_start,
        plan_active_until,
        plan_will_renew,
        account_id,
        exp,
        windows,
        credits,
        status: status.as_u16(),
        raw,
    })
}

// ---------------------------------------------------------------------------
// 本地会话日志扫描（每模型 token + 等价费用）
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexScan {
    pub aggregate: UsageAggregate,
    pub files_scanned: usize,
    pub sessions: usize,
    pub roots: Vec<String>,
    pub days: Option<i64>,
}

fn codex_roots(home: Option<String>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(h) = home {
        let h = h.trim();
        if !h.is_empty() {
            roots.push(PathBuf::from(h));
        }
    }
    if roots.is_empty() {
        if let Ok(env) = std::env::var("CODEX_HOME") {
            if !env.trim().is_empty() {
                roots.push(PathBuf::from(env.trim()));
            }
        }
        if let Some(home_dir) = paths::home_dir() {
            roots.push(home_dir.join(".codex"));
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn jf(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(|x| x.as_f64()).unwrap_or(0.0)
}

/// 已解析的单条记录（保留时间戳，扫描时再按 days 过滤，缓存因此与时间范围无关）。
#[derive(Clone)]
struct CachedRow {
    ts: Option<DateTime<Utc>>,
    row: TokenRow,
}

/// 会话文件解析缓存：mtime + 大小未变则复用，避免每次扫描都全量重解析。
struct FileCache {
    mtime: SystemTime,
    len: u64,
    rows: Vec<CachedRow>,
    had_meta: bool,
}

fn scan_cache() -> &'static Mutex<HashMap<PathBuf, FileCache>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, FileCache>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn parse_file(path: &std::path::Path) -> (Vec<CachedRow>, bool) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return (Vec::new(), false),
    };
    let mut current_model = String::new();
    let mut had_meta = false;
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let typ = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let payload = v.get("payload").cloned().unwrap_or(Value::Null);
        match typ {
            "session_meta" | "turn_context" => {
                had_meta = true;
                if let Some(m) = payload.get("model").and_then(|x| x.as_str()) {
                    if !m.is_empty() {
                        current_model = m.to_string();
                    }
                }
            }
            "event_msg" => {
                if payload.get("type").and_then(|x| x.as_str()) != Some("token_count") {
                    continue;
                }
                let ts = v.get("timestamp").and_then(|x| x.as_str()).and_then(parse_ts);
                if let Some(last) = payload.pointer("/info/last_token_usage") {
                    let input_total = jf(last, "input_tokens");
                    let cached = jf(last, "cached_input_tokens");
                    let output = jf(last, "output_tokens");
                    if input_total == 0.0 && output == 0.0 && cached == 0.0 {
                        continue;
                    }
                    let non_cached = (input_total - cached).max(0.0);
                    let model = if current_model.is_empty() {
                        "unknown".to_string()
                    } else {
                        current_model.clone()
                    };
                    rows.push(CachedRow {
                        ts,
                        row: TokenRow {
                            model,
                            input: non_cached,
                            output,
                            cache_read: cached,
                            cache_write: 0.0,
                            actual_cents: 0.0,
                            timestamp_ms: ts.map(|t| t.timestamp_millis()),
                        },
                    });
                }
            }
            _ => {}
        }
    }
    (rows, had_meta)
}

/// 扫描本地会话并折算等价费用。
/// 过滤起点：since_ms（unix 毫秒，可表达「本地自然日 0 点」这类固定时刻）优先于
/// days 滚动窗口，两者皆无则不过滤；until_ms（可选，开区间）为过滤终点，
/// 与 since_ms 搭配可表达「昨天」这类完整自然日。
/// 同步命令会在主线程执行，全量解析大量 jsonl 时会把 UI 冻住，
/// 因此这里声明为 async 并把重活丢到阻塞线程池。
#[tauri::command]
pub async fn codex_scan_sessions(
    app: tauri::AppHandle,
    days: Option<i64>,
    since_ms: Option<i64>,
    until_ms: Option<i64>,
    home: Option<String>,
) -> Result<CodexScan, String> {
    tauri::async_runtime::spawn_blocking(move || {
        scan_sessions_blocking(&app, days, since_ms, until_ms, home)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn scan_sessions_blocking(
    _app: &tauri::AppHandle,
    days: Option<i64>,
    since_ms: Option<i64>,
    until_ms: Option<i64>,
    home: Option<String>,
) -> Result<CodexScan, String> {
    let table = pricing::load();
    let roots = codex_roots(home);
    // since_ms（固定起点）优先于 days 滚动窗口
    let cutoff = match since_ms {
        Some(ms) => DateTime::from_timestamp_millis(ms),
        None => days.map(|d| Utc::now() - Duration::days(d.max(0))),
    };
    let until = until_ms.and_then(DateTime::from_timestamp_millis);
    let mut rows: Vec<TokenRow> = Vec::new();
    let mut files_scanned = 0usize;
    let mut sessions = 0usize;
    let mut scanned_roots: Vec<String> = Vec::new();

    for root in &roots {
        let mut root_used = false;
        for sub in ["sessions", "archived_sessions"] {
            let dir = root.join(sub);
            if !dir.exists() {
                continue;
            }
            root_used = true;
            for entry in WalkDir::new(&dir).into_iter().filter_map(|e| e.ok()) {
                if !entry.file_type().is_file() {
                    continue;
                }
                let is_jsonl = entry
                    .path()
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("jsonl"))
                    .unwrap_or(false);
                if !is_jsonl {
                    continue;
                }
                files_scanned += 1;
                let path = entry.path().to_path_buf();
                let (mtime, len) = entry
                    .metadata()
                    .ok()
                    .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()))
                    .unwrap_or((SystemTime::UNIX_EPOCH, 0));
                // 命中缓存（mtime + 大小一致）则跳过解析
                let cached = scan_cache().lock().ok().and_then(|c| {
                    c.get(&path)
                        .filter(|f| f.mtime == mtime && f.len == len)
                        .map(|f| (f.rows.clone(), f.had_meta))
                });
                let (file_rows, had_meta) = match cached {
                    Some(hit) => hit,
                    None => {
                        let (parsed_rows, meta) = parse_file(&path);
                        if let Ok(mut c) = scan_cache().lock() {
                            c.insert(
                                path,
                                FileCache {
                                    mtime,
                                    len,
                                    rows: parsed_rows.clone(),
                                    had_meta: meta,
                                },
                            );
                        }
                        (parsed_rows, meta)
                    }
                };
                if had_meta {
                    sessions += 1;
                }
                for cr in file_rows {
                    if let (Some(cut), Some(ts)) = (cutoff, cr.ts) {
                        if ts < cut {
                            continue;
                        }
                    }
                    if let (Some(u), Some(ts)) = (until, cr.ts) {
                        if ts >= u {
                            continue;
                        }
                    }
                    rows.push(cr.row);
                }
            }
        }
        if root_used {
            scanned_roots.push(root.to_string_lossy().to_string());
        }
    }

    let aggregate = pricing::aggregate_and_price(rows, &table);
    Ok(CodexScan {
        aggregate,
        files_scanned,
        sessions,
        roots: scanned_roots,
        days,
    })
}

#[cfg(test)]
mod json_date_tests {
    use super::json_date;
    use serde_json::json;

    #[test]
    fn reads_rfc3339_string() {
        let v = json!({ "active_until": "2026-06-10T02:52:15Z" });
        assert_eq!(
            json_date(&v, &["active_until"]).as_deref(),
            Some("2026-06-10T02:52:15Z")
        );
    }

    #[test]
    fn reads_unix_seconds() {
        let v = json!({ "chatgpt_subscription_active_until": 1781059935 });
        let got = json_date(&v, &["chatgpt_subscription_active_until"]).unwrap();
        assert!(got.starts_with("2026-06-10T"));
    }

    #[test]
    fn reads_unix_millis() {
        let v = json!({ "active_until": 1781059935000i64 });
        let got = json_date(&v, &["active_until"]).unwrap();
        assert!(got.starts_with("2026-06-10T"));
    }

    #[test]
    fn skips_missing_or_empty() {
        let v = json!({ "active_until": "" });
        assert_eq!(json_date(&v, &["active_until", "activeUntil"]), None);
        assert_eq!(json_date(&json!({}), &["active_until"]), None);
    }
}

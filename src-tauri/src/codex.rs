use chrono::{DateTime, TimeZone, Utc};
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;
use walkdir::WalkDir;

use crate::http;
use crate::paths;
use crate::pricing::{self, TokenRow, UsageAggregate};
use crate::session_scan::{self, FileCache, TimeRange};

const WHAM_USAGE: &str = "https://chatgpt.com/backend-api/wham/usage";
/// 订阅起止时间（access_token JWT 经常没有 chatgpt_subscription_active_*，以此接口为准）。
const SUBSCRIPTIONS: &str = "https://chatgpt.com/backend-api/subscriptions";
/// 额度重置次数明细（逐条的状态与过期时间）；wham/usage 只给可用总数。
const RESET_CREDITS: &str = "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";

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
    /// 剩余可用的额度重置次数（wham/usage 的 rate_limit_reset_credits.available_count；
    /// 消耗一次可立即重置 5 小时与每周窗口）。套餐不提供或字段缺失为 None。
    pub reset_credits_available: Option<i64>,
    /// 可用重置次数中最早一次的过期时间（RFC3339，来自明细接口）；次数为 0 或明细拉取失败为 None。
    pub reset_credits_expires_at: Option<String>,
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

/// 拉取额度重置次数明细，返回状态为 available 的条目中最早的过期时间（RFC3339）。
/// 接口失败、结构不符或没有可用条目都返回 None，不影响主流程。
async fn fetch_reset_credits_earliest_expiry(jwt: &str, account_id: Option<&str>) -> Option<String> {
    let mut req = http::client()
        .get(RESET_CREDITS)
        .header("Authorization", format!("Bearer {jwt}"))
        .header("Accept", "application/json")
        .header("Origin", "https://chatgpt.com")
        .header("Referer", "https://chatgpt.com/");
    if let Some(acc) = account_id {
        req = req.header("ChatGPT-Account-Id", acc);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let raw: Value = resp.json().await.ok()?;
    raw.get("credits")
        .and_then(Value::as_array)?
        .iter()
        .filter(|c| {
            c.get("status")
                .and_then(Value::as_str)
                .is_some_and(|s| s.eq_ignore_ascii_case("available"))
        })
        .filter_map(|c| json_date(c, &["expires_at", "expiresAt"]))
        .filter_map(|iso| {
            DateTime::parse_from_rfc3339(&iso)
                .ok()
                .map(|dt| (dt.timestamp(), iso))
        })
        .min_by_key(|(ts, _)| *ts)
        .map(|(_, iso)| iso)
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
            reset_credits_available: None,
            reset_credits_expires_at: None,
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
    // 以 available_count 为准（total_earned_count 可能为 0 却仍有可用次数，不可用）；
    // 整个对象为 null / 缺失表示套餐不提供
    let reset_credits_available = raw
        .get("rate_limit_reset_credits")
        .filter(|v| v.is_object())
        .and_then(|v| gi(v, &["available_count", "availableCount"]))
        .map(|n| n.max(0));
    // 有可用次数时再拉一次明细，取最早过期的那一次；为 0 或不提供时省掉这次请求
    let reset_credits_expires_at = if reset_credits_available.is_some_and(|n| n > 0) {
        fetch_reset_credits_earliest_expiry(&jwt, account_id.as_deref()).await
    } else {
        None
    };

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
        reset_credits_available,
        reset_credits_expires_at,
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

/// token_count 事件里累计计数器（info.total_token_usage）的快照。它在一个线程内单调递增，
/// 因此同一会话谱系里两条快照相同的记录必然指向同一次消耗——要么是重复上报，要么是分叉时复制的历史。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct UsageKey {
    input: i64,
    cached: i64,
    output: i64,
    reasoning: i64,
    total: i64,
}

fn usage_key(total: &Value) -> Option<UsageKey> {
    if !total.is_object() {
        return None;
    }
    let n = |key: &str| {
        total
            .get(key)
            .and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)))
            .unwrap_or(0)
    };
    Some(UsageKey {
        input: n("input_tokens"),
        cached: n("cached_input_tokens"),
        output: n("output_tokens"),
        reasoning: n("reasoning_output_tokens"),
        total: n("total_tokens"),
    })
}

/// 已解析的单条记录（保留时间戳，扫描时再按范围过滤，缓存因此与时间范围无关）。
#[derive(Clone)]
struct CachedRow {
    ts: Option<DateTime<Utc>>,
    /// 累计计数器快照；老版本日志没有 total_token_usage 时为 None，这类记录不参与去重。
    key: Option<UsageKey>,
    row: TokenRow,
}

/// 单个会话文件的解析结果。
#[derive(Clone, Default)]
struct ParsedFile {
    /// 文件首条 session_meta 的线程 id。文件里后续的 session_meta 要么是恢复会话时追加的（同 id），
    /// 要么是分叉时随历史一起复制进来的父任务 meta（异 id），都不代表本文件的身份。
    thread_id: Option<String>,
    /// 分叉来源线程 id。子 Agent（thread_spawn）与手动分叉都带此字段，且日志开头是父任务
    /// 全部历史的复制件；guardian 等未分叉的子线程只有 parent_thread_id，不复制历史。
    forked_from_id: Option<String>,
    /// 首条 session_meta 的时间，用于保证父任务先于它的分叉被处理。
    created: Option<DateTime<Utc>>,
    /// 是否含会话元信息（用于计会话数）。
    had_meta: bool,
    rows: Vec<CachedRow>,
}

fn scan_cache() -> &'static FileCache<ParsedFile> {
    static CACHE: OnceLock<FileCache<ParsedFile>> = OnceLock::new();
    CACHE.get_or_init(FileCache::new)
}

fn json_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn parse_file(path: &std::path::Path) -> ParsedFile {
    let mut file = ParsedFile::default();
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return file,
    };
    let mut current_model = String::new();
    let mut identity_taken = false;
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
                file.had_meta = true;
                if typ == "session_meta" && !identity_taken {
                    identity_taken = true;
                    file.thread_id = json_str(&payload, "id");
                    file.forked_from_id = json_str(&payload, "forked_from_id");
                    file.created = json_str(&v, "timestamp")
                        .or_else(|| json_str(&payload, "timestamp"))
                        .and_then(|s| session_scan::parse_ts(&s));
                }
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
                let ts = v
                    .get("timestamp")
                    .and_then(|x| x.as_str())
                    .and_then(session_scan::parse_ts);
                if let Some(last) = payload.pointer("/info/last_token_usage") {
                    let input_total = session_scan::json_f64(last, "input_tokens");
                    let cached = session_scan::json_f64(last, "cached_input_tokens");
                    let output = session_scan::json_f64(last, "output_tokens");
                    // 上下文压缩 / 回滚后 Codex 会上报一条输入输出皆 0、仅 total_tokens 为当前上下文
                    // 体积的记录，不是真实消耗
                    if input_total == 0.0 && output == 0.0 && cached == 0.0 {
                        continue;
                    }
                    let non_cached = (input_total - cached).max(0.0);
                    let model = if current_model.is_empty() {
                        "unknown".to_string()
                    } else {
                        current_model.clone()
                    };
                    file.rows.push(CachedRow {
                        ts,
                        key: payload
                            .pointer("/info/total_token_usage")
                            .and_then(usage_key),
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
    file
}

/// 沿 forked_from_id 追溯到谱系根的线程 id。父文件不存在（已删除）时以找不到的那个 id 为根，
/// 同一父任务的多个分叉仍落在同一谱系内互相去重；没有 session_meta 的文件自成一系。
fn lineage_root(files: &[ParsedFile], by_id: &HashMap<&str, usize>, start: usize) -> String {
    let mut idx = start;
    let mut hops = 0usize;
    loop {
        let Some(parent) = files[idx].forked_from_id.as_deref() else {
            break;
        };
        hops += 1;
        match by_id.get(parent) {
            // hops 上限只是防御畸形日志里的引用环
            Some(&p) if p != idx && hops < 64 => idx = p,
            _ => return parent.to_string(),
        }
    }
    files[idx]
        .thread_id
        .clone()
        .unwrap_or_else(|| format!("file#{idx}"))
}

/// 跨文件去重后按时间范围过滤。Codex 会把同一次消耗写进日志多遍：
/// - 同一文件内重复上报：额度信息更新、恢复会话、上下文压缩 / 回滚时再发一条 token_count，
///   累计值与上一条相同；
/// - 分叉 / 子 Agent：新线程日志的开头是父任务全部历史的复制件（含全部 token_count），
///   时间戳改写为分叉时刻，累计值与父任务逐条相同，之后子任务自己的消耗在此基础上继续累加。
///
/// 因此以「谱系根 + 累计值快照」为记录身份，每个身份只计第一次出现的那条。文件按创建时间先后处理，
/// 父任务总在分叉之前，保留的是父任务里带真实时间戳的那条；父任务日志已删除时，复制件按最早的分叉
/// 计一次（时间只能落在分叉时刻）。去重必须先于时间过滤，否则范围外的父记录挡不住范围内的复制件。
fn dedupe_rows(mut files: Vec<ParsedFile>, range: &TimeRange) -> Vec<TokenRow> {
    files.sort_by_key(|f| (f.created.is_none(), f.created));
    let by_id: HashMap<&str, usize> = files
        .iter()
        .enumerate()
        .filter_map(|(i, f)| f.thread_id.as_deref().map(|id| (id, i)))
        .collect();
    let mut seen: HashMap<String, HashSet<UsageKey>> = HashMap::new();
    let mut rows = Vec::new();
    for (i, file) in files.iter().enumerate() {
        let lineage = seen.entry(lineage_root(&files, &by_id, i)).or_default();
        for cr in &file.rows {
            if cr.key.is_some_and(|key| !lineage.insert(key)) {
                continue;
            }
            if range.contains(cr.ts) {
                rows.push(cr.row.clone());
            }
        }
    }
    rows
}

/// 扫描本地会话并折算等价费用。时间范围语义见 [`TimeRange`]（since_ms 优先于 days，
/// until_ms 为开区间终点，与 since_ms 搭配可表达「昨天」这类完整自然日）。
/// 同步命令会在主线程执行，全量解析大量 jsonl 时会把 UI 冻住，
/// 因此这里声明为 async 并把重活丢到阻塞线程池。
#[tauri::command]
pub async fn codex_scan_sessions(
    days: Option<i64>,
    since_ms: Option<i64>,
    until_ms: Option<i64>,
    home: Option<String>,
) -> Result<CodexScan, String> {
    tauri::async_runtime::spawn_blocking(move || scan_sessions_blocking(days, since_ms, until_ms, home))
        .await
        .map_err(|e| e.to_string())?
}

fn scan_sessions_blocking(
    days: Option<i64>,
    since_ms: Option<i64>,
    until_ms: Option<i64>,
    home: Option<String>,
) -> Result<CodexScan, String> {
    let table = pricing::load();
    let roots = codex_roots(home);
    let range = TimeRange::new(days, since_ms, until_ms);
    let mut files: Vec<ParsedFile> = Vec::new();
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
                if !session_scan::is_jsonl_file(&entry) {
                    continue;
                }
                files.push(scan_cache().get_or_parse(&entry, parse_file));
            }
        }
        if root_used {
            scanned_roots.push(root.to_string_lossy().to_string());
        }
    }

    let files_scanned = files.len();
    let sessions = files.iter().filter(|f| f.had_meta).count();
    let rows = dedupe_rows(files, &range);

    // until_ms 为开区间终点，按小时聚合的日期按闭区间末毫秒推算
    let hourly_dates = pricing::hourly_dates_for(since_ms, until_ms.map(|u| u - 1));
    let aggregate = pricing::aggregate_and_price_for(rows, &table, hourly_dates.as_deref());
    Ok(CodexScan {
        aggregate,
        files_scanned,
        sessions,
        roots: scanned_roots,
        days,
    })
}

#[cfg(test)]
mod dedupe_tests {
    use super::*;

    fn key(n: i64) -> UsageKey {
        UsageKey {
            input: n,
            cached: 0,
            output: 1,
            reasoning: 0,
            total: n + 1,
        }
    }

    fn row(ts_ms: i64, key: Option<UsageKey>, input: f64) -> CachedRow {
        let ts = DateTime::from_timestamp_millis(ts_ms);
        CachedRow {
            ts,
            key,
            row: TokenRow {
                model: "gpt-5".into(),
                input,
                output: 1.0,
                cache_read: 0.0,
                cache_write: 0.0,
                actual_cents: 0.0,
                timestamp_ms: Some(ts_ms),
            },
        }
    }

    fn file(id: &str, forked_from: Option<&str>, created_ms: i64, rows: Vec<CachedRow>) -> ParsedFile {
        ParsedFile {
            thread_id: Some(id.into()),
            forked_from_id: forked_from.map(str::to_string),
            created: DateTime::from_timestamp_millis(created_ms),
            had_meta: true,
            rows,
        }
    }

    fn all() -> TimeRange {
        TimeRange::new(None, None, None)
    }

    fn inputs(rows: &[TokenRow]) -> Vec<f64> {
        rows.iter().map(|r| r.input).collect()
    }

    #[test]
    fn repeated_report_in_same_file_counted_once() {
        // 累计值没变的第二条是重复上报，哪怕中间隔着别的记录
        let f = file(
            "a",
            None,
            0,
            vec![
                row(1, Some(key(100)), 100.0),
                row(2, Some(key(100)), 100.0),
                row(3, Some(key(250)), 150.0),
                row(4, Some(key(100)), 100.0),
            ],
        );
        assert_eq!(inputs(&dedupe_rows(vec![f], &all())), vec![100.0, 150.0]);
    }

    #[test]
    fn fork_copies_skipped_and_parent_timestamps_kept() {
        let parent = file(
            "p",
            None,
            0,
            vec![row(10, Some(key(100)), 100.0), row(20, Some(key(300)), 200.0)],
        );
        // 分叉复制件时间戳被改写到分叉时刻，之后是子任务自己的消耗
        let child = file(
            "c",
            Some("p"),
            1_000,
            vec![
                row(1_000, Some(key(100)), 100.0),
                row(1_000, Some(key(300)), 200.0),
                row(1_500, Some(key(350)), 50.0),
            ],
        );
        // 目录遍历顺序不保证父先于子
        let rows = dedupe_rows(vec![child, parent], &all());
        assert_eq!(inputs(&rows), vec![100.0, 200.0, 50.0]);
        assert_eq!(rows[0].timestamp_ms, Some(10));
        assert_eq!(rows[1].timestamp_ms, Some(20));
    }

    #[test]
    fn dedupe_happens_before_range_filter() {
        let parent = file("p", None, 0, vec![row(10, Some(key(100)), 100.0)]);
        let child = file(
            "c",
            Some("p"),
            5_000,
            vec![row(5_000, Some(key(100)), 100.0), row(6_000, Some(key(140)), 40.0)],
        );
        // 范围只覆盖分叉之后：父记录不在范围内，它的复制件也不得被算进来
        let range = TimeRange::new(None, Some(4_000), None);
        assert_eq!(inputs(&dedupe_rows(vec![parent, child], &range)), vec![40.0]);
    }

    #[test]
    fn siblings_of_missing_parent_share_copy_once() {
        let a = file(
            "a",
            Some("gone"),
            1_000,
            vec![row(1_000, Some(key(100)), 100.0), row(1_100, Some(key(130)), 30.0)],
        );
        let b = file(
            "b",
            Some("gone"),
            2_000,
            vec![row(2_000, Some(key(100)), 100.0), row(2_100, Some(key(170)), 70.0)],
        );
        assert_eq!(inputs(&dedupe_rows(vec![b, a], &all())), vec![100.0, 30.0, 70.0]);
    }

    #[test]
    fn nested_fork_resolves_to_same_lineage() {
        let root = file("r", None, 0, vec![row(10, Some(key(100)), 100.0)]);
        let child = file(
            "c",
            Some("r"),
            1_000,
            vec![row(1_000, Some(key(100)), 100.0), row(1_100, Some(key(130)), 30.0)],
        );
        let grandchild = file(
            "g",
            Some("c"),
            2_000,
            vec![
                row(2_000, Some(key(100)), 100.0),
                row(2_000, Some(key(130)), 30.0),
                row(2_100, Some(key(150)), 20.0),
            ],
        );
        assert_eq!(
            inputs(&dedupe_rows(vec![grandchild, child, root], &all())),
            vec![100.0, 30.0, 20.0]
        );
    }

    #[test]
    fn unrelated_threads_with_equal_counters_both_counted() {
        let a = file("a", None, 0, vec![row(10, Some(key(100)), 100.0)]);
        let b = file("b", None, 1, vec![row(20, Some(key(100)), 100.0)]);
        assert_eq!(inputs(&dedupe_rows(vec![a, b], &all())), vec![100.0, 100.0]);
    }

    #[test]
    fn rows_without_counter_not_deduped() {
        let f = file("a", None, 0, vec![row(1, None, 100.0), row(2, None, 100.0)]);
        assert_eq!(inputs(&dedupe_rows(vec![f], &all())), vec![100.0, 100.0]);
    }

    #[test]
    fn usage_key_reads_integers_and_floats() {
        let v = serde_json::json!({
            "input_tokens": 17305,
            "cached_input_tokens": 9984.0,
            "output_tokens": 227,
            "reasoning_output_tokens": 98,
            "total_tokens": 17532
        });
        assert_eq!(
            usage_key(&v),
            Some(UsageKey {
                input: 17305,
                cached: 9984,
                output: 227,
                reasoning: 98,
                total: 17532
            })
        );
        assert_eq!(usage_key(&Value::Null), None);
    }
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

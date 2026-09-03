//! Claude 集成：账户额度窗口查询（Anthropic 非公开 OAuth 接口）
//! 与本机 Claude Code 会话日志扫描（token 用量按官方 API 价折算）。

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;
use walkdir::WalkDir;

use crate::claude_oauth;
use crate::codex::UsageWindow;
use crate::http;
use crate::paths;
use crate::pricing::{self, TokenRow, UsageAggregate};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
/// OAuth 控制面接口要求的 beta 标记（Claude Code 同款）。
const OAUTH_BETA: &str = "oauth-2025-04-20";

// ---------------------------------------------------------------------------
// 额度窗口（5 小时 / 每周，来自官方 /api/oauth/usage）
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeUsage {
    pub alive: bool,
    pub email: Option<String>,
    /// 订阅类型（free / pro / max…），接口结构漂移时可能拿不到。
    pub plan: Option<String>,
    pub account_uuid: Option<String>,
    pub organization: Option<String>,
    pub windows: Vec<UsageWindow>,
    pub status: u16,
    pub raw: Value,
}

fn window_label(key: &str) -> String {
    match key {
        "five_hour" => "5 小时".into(),
        "seven_day" => "每周".into(),
        "seven_day_opus" => "每周 Opus".into(),
        "seven_day_sonnet" => "每周 Sonnet".into(),
        "seven_day_oauth_apps" => "每周 OAuth 应用".into(),
        other => other.replace('_', " "),
    }
}

fn rfc3339_to_unix(s: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp())
}

/// 单个窗口对象：{"utilization": 0-100, "resets_at": RFC3339}。
fn parse_window(key: &str, v: &Value) -> Option<UsageWindow> {
    let used = v.get("utilization").and_then(|x| x.as_f64())?;
    let reset_at = v
        .get("resets_at")
        .and_then(|x| x.as_str())
        .and_then(rfc3339_to_unix);
    let seconds = if key == "five_hour" {
        Some(18000)
    } else if key.starts_with("seven_day") {
        Some(604800)
    } else {
        None
    };
    Some(UsageWindow {
        label: window_label(key),
        used_percent: Some(used),
        limit_window_seconds: seconds,
        reset_at,
        resets_in_seconds: None,
    })
}

/// 已知窗口按固定顺序优先收集，结构漂移时兜底遍历其余含 utilization 的对象。
fn collect_windows(raw: &Value) -> Vec<UsageWindow> {
    let mut out = Vec::new();
    let Some(obj) = raw.as_object() else {
        return out;
    };
    let known = ["five_hour", "seven_day", "seven_day_opus", "seven_day_sonnet"];
    let mut seen: HashSet<&str> = HashSet::new();
    for key in known {
        if let Some(w) = obj.get(key).and_then(|v| parse_window(key, v)) {
            out.push(w);
            seen.insert(key);
        }
    }
    for (k, v) in obj {
        if seen.contains(k.as_str()) || !v.is_object() {
            continue;
        }
        if v.get("utilization").is_some() {
            if let Some(w) = parse_window(k, v) {
                out.push(w);
            }
        }
    }
    out
}

/// 从 JSON 里尽力找订阅类型字段（顶层与 account / organization 子对象）。
fn find_plan(v: &Value) -> Option<String> {
    const KEYS: &[&str] = &[
        "subscription_type",
        "subscriptionType",
        "plan_type",
        "planType",
        "rate_limit_tier",
    ];
    let pick = |o: &Value| -> Option<String> {
        for k in KEYS {
            if let Some(s) = o
                .get(*k)
                .and_then(|x| x.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return Some(s.to_string());
            }
        }
        None
    };
    pick(v)
        .or_else(|| v.get("account").and_then(|a| pick(a)))
        .or_else(|| v.get("organization").and_then(|o| pick(o)))
}

struct ProfileInfo {
    email: Option<String>,
    account_uuid: Option<String>,
    organization: Option<String>,
    plan: Option<String>,
}

/// 拉取账户身份（邮箱 / 组织）。失败不影响主流程。
async fn fetch_profile(access_token: &str) -> Option<ProfileInfo> {
    let resp = http::client()
        .get(PROFILE_URL)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Accept", "application/json, text/plain, */*")
        .header("User-Agent", claude_oauth::OAUTH_UA)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let raw: Value = resp.json().await.ok()?;
    let text = |ptr: &str| {
        raw.pointer(ptr)
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Some(ProfileInfo {
        email: text("/account/email").or_else(|| text("/account/email_address")),
        account_uuid: text("/account/uuid"),
        organization: text("/organization/name"),
        plan: find_plan(&raw),
    })
}

/// 查询 Claude 账户的额度窗口与身份信息。access_token 为不透明值（非 JWT），
/// 过期与否只能靠请求结果判断：401/403 视为凭据失效（调用方决定是否续期重试）。
pub async fn claude_usage(access_token: String) -> Result<ClaudeUsage, String> {
    let access_token = access_token.trim().to_string();
    if access_token.is_empty() {
        return Err("empty_token".into());
    }
    let resp = http::client()
        .get(USAGE_URL)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Accept", "application/json, text/plain, */*")
        .header("anthropic-beta", OAUTH_BETA)
        .header("User-Agent", claude_oauth::OAUTH_UA)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;

    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Ok(ClaudeUsage {
            alive: false,
            email: None,
            plan: None,
            account_uuid: None,
            organization: None,
            windows: vec![],
            status: status.as_u16(),
            raw: Value::Null,
        });
    }
    if !status.is_success() {
        return Err(format!("usage_http_{}", status.as_u16()));
    }

    let raw: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let windows = collect_windows(&raw);
    let mut plan = find_plan(&raw);
    let mut email = None;
    let mut account_uuid = None;
    let mut organization = None;
    if let Some(p) = fetch_profile(&access_token).await {
        email = p.email;
        account_uuid = p.account_uuid;
        organization = p.organization;
        if plan.is_none() {
            plan = p.plan;
        }
    }

    Ok(ClaudeUsage {
        alive: true,
        email,
        plan,
        account_uuid,
        organization,
        windows,
        status: status.as_u16(),
        raw,
    })
}

// ---------------------------------------------------------------------------
// 本地会话日志扫描（~/.claude/projects/**/*.jsonl）
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeScan {
    pub aggregate: UsageAggregate,
    pub files_scanned: usize,
    pub sessions: usize,
    pub roots: Vec<String>,
    pub days: Option<i64>,
}

/// 指到 projects 目录本身时归一为其父目录（配置根）。
fn normalize_root(p: PathBuf) -> PathBuf {
    if p.file_name().map(|n| n == "projects").unwrap_or(false) {
        if let Some(parent) = p.parent() {
            return parent.to_path_buf();
        }
    }
    p
}

fn split_roots(raw: &str) -> Vec<PathBuf> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| normalize_root(PathBuf::from(s)))
        .collect()
}

/// 扫描根目录：入参 > CLAUDE_CONFIG_DIR（支持逗号分隔多个）> ~/.claude 与 ~/.config/claude。
/// 不检查存在性，扫描时缺 projects/ 自动跳过。
fn claude_roots(home: Option<String>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(h) = home {
        let h = h.trim().to_string();
        if !h.is_empty() {
            roots = split_roots(&h);
        }
    }
    if roots.is_empty() {
        if let Ok(env) = std::env::var("CLAUDE_CONFIG_DIR") {
            roots = split_roots(&env);
        }
    }
    if roots.is_empty() {
        if let Some(home_dir) = paths::home_dir() {
            roots.push(home_dir.join(".claude"));
            let xdg = std::env::var("XDG_CONFIG_HOME")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home_dir.join(".config"));
            roots.push(xdg.join("claude"));
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

fn js<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("")
}

/// 从文件路径推导会话 id（行内缺 sessionId 时兜底）：
/// projects/<项目>/<会话>.jsonl 取文件名；更深层（子代理等）取 projects/<项目>/ 下的目录名。
fn session_from_path(path: &Path) -> String {
    let parts: Vec<&str> = path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    let Some(idx) = parts.iter().position(|p| *p == "projects") else {
        return path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
    };
    let relative = &parts[idx + 1..];
    if relative.len() == 2 {
        return Path::new(relative[1])
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
    }
    // 子代理日志：projects/<项目>/<会话>/subagents/<文件>.jsonl 归属上层会话
    if relative.len() >= 4 && relative[relative.len() - 2] == "subagents" {
        return relative[relative.len() - 3].to_string();
    }
    if relative.len() >= 3 {
        return relative[relative.len() - 2].to_string();
    }
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// 已解析的单条 usage 记录（去重与时间过滤在聚合阶段进行，缓存与范围无关）。
#[derive(Clone)]
struct ClaudeRow {
    ts: Option<DateTime<Utc>>,
    session: String,
    message_id: String,
    request_id: String,
    sidechain: bool,
    row: TokenRow,
}

fn row_total(r: &TokenRow) -> f64 {
    r.input + r.output + r.cache_read + r.cache_write
}

/// 会话文件解析缓存：mtime + 大小未变则复用（与 codex 扫描同策略）。
struct FileCache {
    mtime: SystemTime,
    len: u64,
    rows: Vec<ClaudeRow>,
}

fn scan_cache() -> &'static Mutex<HashMap<PathBuf, FileCache>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, FileCache>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn parse_file(path: &Path) -> Vec<ClaudeRow> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let file_session = session_from_path(path);
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        // 快筛：只有带 usage 对象的行才值得完整解析（其余行可能非常大）
        if line.is_empty() || !line.contains("\"usage\"") {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let Some(message) = v.get("message") else {
            continue;
        };
        let Some(usage) = message.get("usage") else {
            continue;
        };
        let model = js(message, "model");
        // <synthetic> 为 Claude Code 本地合成消息（如错误提示），不产生真实用量
        if model == "<synthetic>" {
            continue;
        }
        let input = jf(usage, "input_tokens");
        let output = jf(usage, "output_tokens");
        let cache_write = jf(usage, "cache_creation_input_tokens");
        let cache_read = jf(usage, "cache_read_input_tokens");
        if input == 0.0 && output == 0.0 && cache_write == 0.0 && cache_read == 0.0 {
            continue;
        }
        let ts = v.get("timestamp").and_then(|x| x.as_str()).and_then(parse_ts);
        let session = {
            let s = js(&v, "sessionId").trim();
            if s.is_empty() {
                file_session.clone()
            } else {
                s.to_string()
            }
        };
        rows.push(ClaudeRow {
            ts,
            session,
            message_id: js(message, "id").trim().to_string(),
            request_id: js(&v, "requestId").trim().to_string(),
            sidechain: v.get("isSidechain").and_then(|x| x.as_bool()).unwrap_or(false),
            row: TokenRow {
                model: if model.is_empty() {
                    "unknown".to_string()
                } else {
                    model.to_string()
                },
                input,
                output,
                cache_read,
                cache_write,
                actual_cents: 0.0,
                timestamp_ms: ts.map(|t| t.timestamp_millis()),
            },
        });
    }
    rows
}

/// 全局去重器：流式日志会把同一响应写多行（usage 递增），复制的会话与
/// sidechain 重放也会重复计数。口径对齐 ccusage：
/// - 主键 (session, message_id, request_id)：重复时保留 token 总量大的（非 sidechain 优先）；
/// - 重放键 (session, message_id, timestamp)：任一方是 sidechain 即视为同一条。
/// 无 message_id 的行不去重。
struct Dedupe {
    rows: Vec<TokenRow>,
    meta: Vec<(f64, bool)>,
    primary: HashMap<(String, String, String), usize>,
    replay: HashMap<(String, String, i64), usize>,
}

impl Dedupe {
    fn new() -> Self {
        Dedupe {
            rows: Vec::new(),
            meta: Vec::new(),
            primary: HashMap::new(),
            replay: HashMap::new(),
        }
    }

    fn should_replace(candidate: (f64, bool), existing: (f64, bool)) -> bool {
        if candidate.1 != existing.1 {
            return existing.1; // 非 sidechain 优先
        }
        candidate.0 > existing.0
    }

    fn push(&mut self, cr: ClaudeRow) {
        let total = row_total(&cr.row);
        if cr.message_id.is_empty() {
            self.rows.push(cr.row);
            self.meta.push((total, cr.sidechain));
            return;
        }
        let pkey = (cr.session.clone(), cr.message_id.clone(), cr.request_id.clone());
        if let Some(&i) = self.primary.get(&pkey) {
            if Self::should_replace((total, cr.sidechain), self.meta[i]) {
                self.rows[i] = cr.row;
                self.meta[i] = (total, cr.sidechain);
            }
            return;
        }
        let ts_ms = cr.row.timestamp_ms.unwrap_or(0);
        let rkey = (cr.session.clone(), cr.message_id.clone(), ts_ms);
        if let Some(&i) = self.replay.get(&rkey) {
            if cr.sidechain || self.meta[i].1 {
                if Self::should_replace((total, cr.sidechain), self.meta[i]) {
                    self.rows[i] = cr.row;
                    self.meta[i] = (total, cr.sidechain);
                    self.primary.insert(pkey, i);
                }
                return;
            }
        }
        let idx = self.rows.len();
        self.rows.push(cr.row);
        self.meta.push((total, cr.sidechain));
        self.primary.insert(pkey, idx);
        self.replay.insert(rkey, idx);
    }
}

/// 扫描本机 Claude Code 会话日志并折算等价费用。since_ms（unix 毫秒）优先于
/// days 滚动窗口，两者皆无则不过滤。重活丢到阻塞线程池，避免冻住 UI。
#[tauri::command]
pub async fn claude_scan_sessions(
    app: tauri::AppHandle,
    days: Option<i64>,
    since_ms: Option<i64>,
    home: Option<String>,
) -> Result<ClaudeScan, String> {
    tauri::async_runtime::spawn_blocking(move || scan_sessions_blocking(&app, days, since_ms, home))
        .await
        .map_err(|e| e.to_string())?
}

fn scan_sessions_blocking(
    _app: &tauri::AppHandle,
    days: Option<i64>,
    since_ms: Option<i64>,
    home: Option<String>,
) -> Result<ClaudeScan, String> {
    let table = pricing::load();
    let roots = claude_roots(home);
    let cutoff = match since_ms {
        Some(ms) => DateTime::from_timestamp_millis(ms),
        None => days.map(|d| Utc::now() - Duration::days(d.max(0))),
    };
    let mut dedupe = Dedupe::new();
    let mut sessions: HashSet<String> = HashSet::new();
    let mut files_scanned = 0usize;
    let mut scanned_roots: Vec<String> = Vec::new();

    for root in &roots {
        let dir = root.join("projects");
        if !dir.exists() {
            continue;
        }
        scanned_roots.push(root.to_string_lossy().to_string());
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
            let cached = scan_cache().lock().ok().and_then(|c| {
                c.get(&path)
                    .filter(|f| f.mtime == mtime && f.len == len)
                    .map(|f| f.rows.clone())
            });
            let file_rows = match cached {
                Some(hit) => hit,
                None => {
                    let parsed = parse_file(&path);
                    if let Ok(mut c) = scan_cache().lock() {
                        c.insert(
                            path,
                            FileCache {
                                mtime,
                                len,
                                rows: parsed.clone(),
                            },
                        );
                    }
                    parsed
                }
            };
            for cr in file_rows {
                if let (Some(cut), Some(ts)) = (cutoff, cr.ts) {
                    if ts < cut {
                        continue;
                    }
                }
                sessions.insert(cr.session.clone());
                dedupe.push(cr);
            }
        }
    }

    let aggregate = pricing::aggregate_and_price(dedupe.rows, &table);
    Ok(ClaudeScan {
        aggregate,
        files_scanned,
        sessions: sessions.len(),
        roots: scanned_roots,
        days,
    })
}

#[cfg(test)]
mod dedupe_tests {
    use super::*;

    fn cr(
        session: &str,
        msg: &str,
        req: &str,
        sidechain: bool,
        ts_ms: i64,
        input: f64,
        cache_read: f64,
    ) -> ClaudeRow {
        ClaudeRow {
            ts: DateTime::from_timestamp_millis(ts_ms).map(|d| d.with_timezone(&Utc)),
            session: session.into(),
            message_id: msg.into(),
            request_id: req.into(),
            sidechain,
            row: TokenRow {
                model: "claude-sonnet-4-5".into(),
                input,
                output: 1.0,
                cache_read,
                cache_write: 0.0,
                actual_cents: 0.0,
                timestamp_ms: Some(ts_ms),
            },
        }
    }

    fn total(rows: &[TokenRow]) -> f64 {
        rows.iter().map(row_total).sum()
    }

    #[test]
    fn streaming_duplicates_keep_largest() {
        let mut d = Dedupe::new();
        d.push(cr("s", "m1", "r1", false, 1000, 100.0, 0.0));
        d.push(cr("s", "m1", "r1", false, 2000, 100.0, 500.0));
        assert_eq!(d.rows.len(), 1);
        assert_eq!(d.rows[0].cache_read, 500.0);
    }

    #[test]
    fn distinct_sessions_with_same_message_id_kept() {
        let mut d = Dedupe::new();
        d.push(cr("s1", "m1", "r1", false, 1000, 100.0, 0.0));
        d.push(cr("s2", "m1", "r1", false, 1000, 300.0, 0.0));
        assert_eq!(d.rows.len(), 2);
        assert_eq!(total(&d.rows), 402.0);
    }

    #[test]
    fn sidechain_replay_prefers_parent() {
        let mut d = Dedupe::new();
        d.push(cr("s", "m1", "r-parent", false, 1000, 0.0, 20.0));
        // sidechain 用新 requestId 重放父消息（同 timestamp）：不得重复计数
        d.push(cr("s", "m1", "r-replay", true, 1000, 0.0, 50_000.0));
        // sidechain 自己的回答是独立消息，照常保留
        d.push(cr("s", "m2", "r-side", true, 2000, 0.0, 700.0));
        assert_eq!(d.rows.len(), 2);
        assert_eq!(d.rows[0].cache_read, 20.0);
        assert_eq!(d.rows[1].cache_read, 700.0);
    }

    #[test]
    fn rows_without_message_id_not_deduped() {
        let mut d = Dedupe::new();
        d.push(cr("s", "", "", false, 1000, 10.0, 0.0));
        d.push(cr("s", "", "", false, 1000, 10.0, 0.0));
        assert_eq!(d.rows.len(), 2);
    }

    #[test]
    fn session_from_path_variants() {
        assert_eq!(
            session_from_path(Path::new("/h/.claude/projects/proj-a/sess-1.jsonl")),
            "sess-1"
        );
        assert_eq!(
            session_from_path(Path::new("/h/.claude/projects/proj-a/sess-1/chat.jsonl")),
            "sess-1"
        );
        assert_eq!(
            session_from_path(Path::new(
                "/h/.claude/projects/proj-a/sess-1/subagents/worker.jsonl"
            )),
            "sess-1"
        );
    }
}

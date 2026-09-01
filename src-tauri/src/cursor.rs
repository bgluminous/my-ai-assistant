use base64::Engine;
use serde::Serialize;
use serde_json::Value;

use crate::http;
use crate::pricing::{self, TokenRow, UsageAggregate};

const USAGE_SUMMARY: &str = "https://cursor.com/api/usage-summary";
/// Grok Bot 周额度（"sand" 是 Cursor 的内部代号），独立于月度计划额度。
const SAND_USAGE: &str = "https://cursor.com/api/dashboard/get-sand-usage-status";
/// 账号信息（邮箱 / userId）。
const AUTH_ME: &str = "https://cursor.com/api/auth/me";
const FILTERED_EVENTS: &str = "https://cursor.com/api/dashboard/get-filtered-usage-events";
const DEEP_CALLBACK_WEB: &str = "https://cursor.com/api/auth/loginDeepCallbackControl";
const DEEP_CONTROL: &str = "https://cursor.com/loginDeepControl";
const AUTH_POLL: &str = "https://api2.cursor.sh/auth/poll";

fn cookie(token: &str) -> String {
    format!("WorkosCursorSessionToken={token}")
}

fn f(v: &Value, path: &[&str]) -> Option<f64> {
    let mut cur = v;
    for key in path {
        cur = cur.get(key)?;
    }
    cur.as_f64()
}

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(str::to_string)
}

// ---------------------------------------------------------------------------
// 账户状态查询（供账户管理刷新内部复用）
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanInfo {
    pub used: Option<f64>,
    pub limit: Option<f64>,
    pub remaining: Option<f64>,
    pub included: Option<f64>,
    pub bonus: Option<f64>,
    pub total: Option<f64>,
    pub auto_percent_used: Option<f64>,
    pub api_percent_used: Option<f64>,
    pub total_percent_used: Option<f64>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OnDemandInfo {
    pub enabled: Option<bool>,
    pub used: Option<f64>,
    pub limit: Option<f64>,
}

/// Grok Bot 周额度（get-sand-usage-status）。
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandInfo {
    /// 套餐是否包含该额度（hasNonZeroIncludedLimit），false 时前端不展示。
    pub included: Option<bool>,
    /// 已用百分比。
    pub usage_percent: Option<f64>,
    /// 是否还有可用额度，false 视为已耗尽。
    pub has_available_usage: Option<bool>,
    /// 下次周重置时间（ISO 字符串）。
    pub next_reset_at: Option<String>,
    /// 套餐标签（如 "Grok Bot Plan"）。
    pub plan_label: Option<String>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectionPayload {
    pub alive: bool,
    pub membership_type: Option<String>,
    /// 账号邮箱（来自 /api/auth/me，best-effort）。
    pub email: Option<String>,
    /// 账号用户名（来自 /api/auth/me，best-effort）。
    pub name: Option<String>,
    pub billing_cycle_start: Option<String>,
    pub billing_cycle_end: Option<String>,
    pub plan: PlanInfo,
    pub on_demand: OnDemandInfo,
    /// Grok 周额度；接口失败或无数据时为 None，不影响主流程。
    pub sand: Option<SandInfo>,
    pub raw: Value,
}

impl InspectionPayload {
    fn dead() -> Self {
        InspectionPayload {
            alive: false,
            raw: Value::Null,
            ..Default::default()
        }
    }

    fn from_summary(v: &Value) -> Self {
        let plan = PlanInfo {
            used: f(v, &["individualUsage", "plan", "used"]),
            limit: f(v, &["individualUsage", "plan", "limit"]),
            remaining: f(v, &["individualUsage", "plan", "remaining"]),
            included: f(v, &["individualUsage", "plan", "breakdown", "included"]),
            bonus: f(v, &["individualUsage", "plan", "breakdown", "bonus"]),
            total: f(v, &["individualUsage", "plan", "breakdown", "total"]),
            auto_percent_used: f(v, &["individualUsage", "plan", "autoPercentUsed"]),
            api_percent_used: f(v, &["individualUsage", "plan", "apiPercentUsed"]),
            total_percent_used: f(v, &["individualUsage", "plan", "totalPercentUsed"]),
        };
        let on_demand = OnDemandInfo {
            enabled: v
                .pointer("/individualUsage/onDemand/enabled")
                .and_then(|x| x.as_bool()),
            used: f(v, &["individualUsage", "onDemand", "used"]),
            limit: f(v, &["individualUsage", "onDemand", "limit"]),
        };
        InspectionPayload {
            alive: true,
            membership_type: s(v, "membershipType"),
            email: None,
            name: None,
            billing_cycle_start: s(v, "billingCycleStart"),
            billing_cycle_end: s(v, "billingCycleEnd"),
            plan,
            on_demand,
            sand: None,
            raw: v.clone(),
        }
    }
}

/// 拉取 Grok Bot 周额度。任何失败都返回 None，绝不影响主检查流程。
/// 注意：cursor.com 的 dashboard POST 接口必须带 Origin 头，否则会被拒（表现为 4xx）。
async fn fetch_sand(client: &reqwest::Client, token: &str) -> Option<SandInfo> {
    let resp = client
        .post(SAND_USAGE)
        .header("Cookie", cookie(token))
        .header("Origin", "https://cursor.com")
        .header("Referer", "https://cursor.com/dashboard")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&serde_json::json!({}))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: Value = resp.json().await.ok()?;
    v.get("usagePercent")?;
    Some(SandInfo {
        included: v.get("hasNonZeroIncludedLimit").and_then(|x| x.as_bool()),
        usage_percent: v.get("usagePercent").and_then(|x| x.as_f64()),
        has_available_usage: v.get("hasAvailableUsage").and_then(|x| x.as_bool()),
        next_reset_at: s(&v, "nextResetTimestampUtc"),
        plan_label: s(&v, "grokPlanLabel"),
    })
}

/// 仅拉取账号邮箱（auth/me），供账户默认备注使用；归一化 token，失败返回 None。
pub async fn fetch_email(session_token: &str) -> Option<String> {
    let token = http::normalize_cursor_token(session_token);
    if token.is_empty() {
        return None;
    }
    fetch_auth_profile(&http::client(), &token).await?.0
}

/// 拉取账号邮箱与用户名（/api/auth/me），返回 (email, name)。
/// 任何失败都返回 None，绝不影响主检查流程。
async fn fetch_auth_profile(
    client: &reqwest::Client,
    token: &str,
) -> Option<(Option<String>, Option<String>)> {
    let resp = client
        .get(AUTH_ME)
        .header("Cookie", cookie(token))
        .header("Accept", "application/json")
        .header("Referer", "https://cursor.com/dashboard")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: Value = resp.json().await.ok()?;
    let field = |key: &str| {
        v.get(key)
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_string)
    };
    Some((field("email"), field("name")))
}

/// 查询 Cursor 账户的存活状态、套餐与额度摘要（accounts.rs 刷新账户时调用）。
pub async fn cursor_inspect_token(session_token: String) -> Result<InspectionPayload, String> {
    let token = http::normalize_cursor_token(&session_token);
    if token.is_empty() {
        return Err("empty_token".into());
    }
    let client = http::client();
    let resp = client
        .get(USAGE_SUMMARY)
        .header("Cookie", cookie(&token))
        .header("Accept", "application/json")
        .header("Referer", "https://cursor.com/dashboard")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    // 仅 401 视为会话失效。403 可能是边缘风控，不应把有效 Token 标成「已失效」。
    if status.as_u16() == 401 {
        return Ok(InspectionPayload::dead());
    }
    if !status.is_success() {
        return Err(format!("cursor_http_{}", status.as_u16()));
    }
    let v: Value =
        serde_json::from_str(&text).map_err(|_| "invalid_response".to_string())?;
    // 未认证时接口也可能返回 200 + {error:not_authenticated}
    if v.get("error").is_some() && v.get("membershipType").is_none() {
        return Ok(InspectionPayload::dead());
    }
    let mut payload = InspectionPayload::from_summary(&v);
    payload.sand = fetch_sand(&client, &token).await;
    (payload.email, payload.name) = fetch_auth_profile(&client, &token).await.unwrap_or_default();
    Ok(payload)
}

// ---------------------------------------------------------------------------
// 用量事件（分页拉取 + 解析，仅供聚合统计使用）
// ---------------------------------------------------------------------------

/// 事件时间：官方多为毫秒字符串，兼容 number；小于 1e12 视为秒。
fn parse_timestamp_ms(item: &Value) -> Option<i64> {
    let v = item.get("timestamp")?;
    let n = v
        .as_i64()
        .or_else(|| v.as_u64().map(|x| x as i64))
        .or_else(|| v.as_f64().map(|x| x as i64))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))?;
    Some(if n.abs() < 1_000_000_000_000 { n.saturating_mul(1000) } else { n })
}

/// 把单条用量事件解析为计价行；模型缺失归入 "unknown"。
fn parse_event(item: &Value) -> TokenRow {
    let tu = item.get("tokenUsage").cloned().unwrap_or(Value::Null);
    let num = |v: &Value, key: &str| v.get(key).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let model = item.get("model").and_then(|x| x.as_str()).unwrap_or("");
    TokenRow {
        model: if model.is_empty() {
            "unknown".to_string()
        } else {
            model.to_string()
        },
        input: num(&tu, "inputTokens"),
        output: num(&tu, "outputTokens"),
        cache_read: num(&tu, "cacheReadTokens"),
        cache_write: num(&tu, "cacheWriteTokens"),
        actual_cents: num(item, "chargedCents"),
        timestamp_ms: parse_timestamp_ms(item),
    }
}

async fn fetch_events_page(
    client: &reqwest::Client,
    token: &str,
    page: u32,
    page_size: u32,
    start: Option<i64>,
    end: Option<i64>,
) -> Result<(Vec<Value>, u64), String> {
    let mut body = serde_json::json!({ "page": page, "pageSize": page_size });
    if let Some(start) = start {
        body["startDate"] = Value::String(start.to_string());
    }
    if let Some(end) = end {
        body["endDate"] = Value::String(end.to_string());
    }
    let resp = client
        .post(FILTERED_EVENTS)
        .header("Cookie", cookie(token))
        .header("Origin", "https://cursor.com")
        .header("Referer", "https://cursor.com/dashboard/usage")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if status.as_u16() == 401 {
        return Err("invalid_session_token".into());
    }
    if !status.is_success() {
        return Err(format!("cursor_http_{}", status.as_u16()));
    }
    let v: Value = resp.json().await.map_err(|_| "invalid_response".to_string())?;
    let total = v
        .get("totalUsageEventsCount")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let items = v
        .get("usageEventsDisplay")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    Ok((items, total))
}

// ---------------------------------------------------------------------------
// 聚合 + 等价费用（喂给图表/表格）
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn cursor_aggregate(
    _app: tauri::AppHandle,
    session_token: String,
    start: Option<i64>,
    end: Option<i64>,
) -> Result<UsageAggregate, String> {
    let token = http::normalize_cursor_token(&session_token);
    let client = http::client();
    let page_size: u32 = 1000;
    let mut page: u32 = 1;
    let mut rows: Vec<TokenRow> = Vec::new();
    loop {
        let (items, total) =
            fetch_events_page(&client, &token, page, page_size, start, end).await?;
        if items.is_empty() {
            break;
        }
        for item in &items {
            rows.push(parse_event(item));
        }
        if (page as u64) * (page_size as u64) >= total {
            break;
        }
        page += 1;
        if page > 200 {
            break;
        }
    }
    let table = pricing::load();
    Ok(pricing::aggregate_and_price(rows, &table))
}

// ---------------------------------------------------------------------------
// 本机切换用的 PKCE 换凭证辅助（深度登录回调）
// ---------------------------------------------------------------------------

/// 生成 PKCE code_verifier：32 字节随机 -> BASE64URL_NOPAD。
fn verifier() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("getrandom");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// 由 verifier 计算 PKCE code_challenge：BASE64URL_NOPAD(SHA256(verifier))。
fn challenge(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize())
}

/// 手拼 UUIDv4（避免引入 uuid crate）：设置版本位/变体位后格式化为 8-4-4-4-12。
fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).expect("getrandom");
    b[6] = (b[6] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14],
        b[15]
    )
}

/// 生成一组 PKCE 参数：(verifier, challenge, uuid)。
fn gen_pkce() -> (String, String, String) {
    let v = verifier();
    let c = challenge(&v);
    let u = uuid_v4();
    (v, c, u)
}

/// 向 Cursor 深度登录回调提交授权（cookie 鉴权）。返回 (状态码, 响应体)。
/// body 只带 uuid + challenge（多带字段可能被判 400）；headers 与浏览器完成授权那步一致。
async fn post_deep_callback(
    client: &reqwest::Client,
    token: &str,
    uuid: &str,
    challenge: &str,
    referer: &str,
) -> Result<(reqwest::StatusCode, String), String> {
    let body = serde_json::json!({
        "uuid": uuid,
        "challenge": challenge,
    });
    let resp = client
        .post(DEEP_CALLBACK_WEB)
        .header("Cookie", cookie(token))
        .header("Origin", "https://cursor.com")
        .header("Referer", referer)
        .header("Content-Type", "application/json")
        .header("Accept", "*/*")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    Ok((status, text))
}

// ---------------------------------------------------------------------------
// web token -> session token（PKCE 授权 + 轮询 poll）
// ---------------------------------------------------------------------------

/// 换取到的 session 凭证（type=session），用于写本地 Cursor 认证库。
#[derive(Debug, Clone)]
pub struct SessionTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub auth_id: Option<String>,
}

/// 用完整 web token（`user_xxx::<jwt>`，type=web）走 PKCE 授权 + 轮询 poll，
/// 换取 type=session 的 accessToken/refreshToken。
/// 错误码仅四种：empty_token / invalid_session_token / poll_timeout / session_exchange_failed。
pub async fn exchange_web_to_session(session_token: &str) -> Result<SessionTokens, String> {
    // Cookie 必须用【完整】 user_xxx::<jwt>，不能只取 jwt 段。
    let token = http::normalize_cursor_token(session_token);
    if token.is_empty() {
        return Err("empty_token".into());
    }
    let (verifier, challenge, uuid) = gen_pkce();
    let client = http::client();

    // 预热：对齐真实浏览器流程（GET loginDeepControl）；结果与错误一律忽略。
    let warm = format!(
        "{DEEP_CONTROL}?challenge={challenge}&uuid={uuid}&mode=login&supportsSelectedTeamLogin=true"
    );
    let _ = client.get(&warm).header("Cookie", cookie(&token)).send().await;

    // 授权：POST loginDeepCallbackControl（Referer 用对应的 loginDeepControl 链接）。
    let referer = format!("{DEEP_CONTROL}?challenge={challenge}&uuid={uuid}&mode=login");
    let (status, _text) = post_deep_callback(&client, &token, &uuid, &challenge, &referer).await?;
    if !status.is_success() {
        return match status.as_u16() {
            400 | 401 | 403 | 422 => Err("invalid_session_token".into()),
            _ => Err("session_exchange_failed".into()),
        };
    }

    // 轮询 auth/poll 换 session token：每 1500ms 一次、最多 20 次（约 30s）。
    let poll_url = format!("{AUTH_POLL}?uuid={uuid}&verifier={verifier}");
    let mut transport_errors = 0u32;
    for _ in 0..20 {
        match client
            .get(&poll_url)
            .header("Accept", "application/json")
            .send()
            .await
        {
            Ok(resp) => {
                let ready = resp.status().is_success();
                let text = resp.text().await.unwrap_or_default();
                // 就绪判定：accessToken 与 refreshToken 均非空。
                if ready {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        let access = s(&v, "accessToken").unwrap_or_default();
                        let refresh = s(&v, "refreshToken").unwrap_or_default();
                        if !access.is_empty() && !refresh.is_empty() {
                            // authId 优先取 poll 返回；缺失则回退解析 accessToken 的 sub。
                            let auth_id = s(&v, "authId").filter(|x| !x.is_empty()).or_else(|| {
                                http::decode_jwt_payload(http::jwt_part(&access))
                                    .and_then(|p| s(&p, "sub"))
                                    .filter(|x| !x.is_empty())
                            });
                            return Ok(SessionTokens {
                                access_token: access,
                                refresh_token: refresh,
                                auth_id,
                            });
                        }
                    }
                    // 200 但尚未就绪（无 token）：继续等待。
                }
                // 非 2xx（含 pending 类 4xx）：继续等待。
            }
            // 偶发传输错误容忍并继续；累计过多才判失败。
            Err(_) => {
                transport_errors += 1;
                if transport_errors >= 5 {
                    return Err("session_exchange_failed".into());
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    }
    Err("poll_timeout".into())
}

#[cfg(test)]
mod parse_timestamp_tests {
    use super::parse_timestamp_ms;
    use serde_json::json;

    #[test]
    fn millis_string() {
        assert_eq!(
            parse_timestamp_ms(&json!({ "timestamp": "1775418973898" })),
            Some(1_775_418_973_898)
        );
    }

    #[test]
    fn millis_number() {
        assert_eq!(
            parse_timestamp_ms(&json!({ "timestamp": 1_750_979_225_854_i64 })),
            Some(1_750_979_225_854)
        );
    }

    #[test]
    fn seconds_promoted_to_millis() {
        assert_eq!(
            parse_timestamp_ms(&json!({ "timestamp": 1_750_979_225 })),
            Some(1_750_979_225_000)
        );
    }
}

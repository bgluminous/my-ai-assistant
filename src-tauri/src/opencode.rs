//! OpenCode Console：独立设备码授权、套餐与余额。token 字段仅保存工作区 ID；
//! access/refresh token 始终在 opencode_auth 加密副本中，不复用客户端的 OAuth 会话。
use crate::{accounts, http, local_crypto};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::AppHandle;

pub const SERVER: &str = "https://opencode.ai/console";
const CLIENT_ID: &str = "opencode-cli";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credential {
    pub server: String,
    pub workspace_id: String,
    pub workspace_name: String,
    pub user_id: String,
    pub email: String,
    pub access: String,
    pub refresh: String,
    pub expires_at: i64,
}

pub fn valid_workspace(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

pub fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// 未填写备注时优先使用邮箱，身份尚未读取到时回退工作区名与 ID。
pub fn default_note(email: &str, name: &str, workspace: &str) -> String {
    [email, name, workspace]
        .into_iter()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or_default()
        .to_string()
}

impl Credential {
    pub fn valid(&self) -> bool {
        self.server == SERVER
            && valid_workspace(&self.workspace_id)
            && !self.access.is_empty()
            && !self.refresh.is_empty()
            && self.expires_at > 0
    }
    pub fn seal(&self) -> Result<String, String> {
        if !self.valid() {
            return Err("opencode_invalid_credential".into());
        }
        local_crypto::seal(&serde_json::to_string(self).map_err(|_| "opencode_invalid_credential")?)
    }
}

pub fn decode(blob: &str, workspace: &str) -> Option<Credential> {
    let c: Credential = serde_json::from_str(&local_crypto::open(blob)?).ok()?;
    (c.valid() && c.workspace_id == workspace).then_some(c)
}

pub fn account_credential(account: &accounts::Account) -> Result<Credential, String> {
    if account.kind != "opencode" {
        return Err("not_opencode_account".into());
    }
    account
        .opencode_auth
        .as_deref()
        .and_then(|s| decode(s, &account.token))
        .ok_or_else(|| "opencode_auth_required".into())
}

/// Console 设备码接口返回 /console/device?...；兼容相对地址和完整官方地址。
/// 解析后再校验来源，禁止相对地址跳到其它主机或携带 URL 登录信息。
pub fn authorization_url(raw: &str) -> Result<String, String> {
    if raw.trim().is_empty() {
        return Err("opencode_oauth_invalid_response".into());
    }
    let base = reqwest::Url::parse(&format!("{SERVER}/"))
        .map_err(|_| "opencode_oauth_invalid_response")?;
    let parsed = base
        .join(raw)
        .map_err(|_| "opencode_oauth_invalid_response")?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("opencode.ai")
        || parsed.port_or_known_default() != Some(443)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err("opencode_oauth_invalid_response".into());
    }
    Ok(parsed.to_string())
}

pub fn open_browser(url: &str) -> bool {
    let Ok(url) = authorization_url(url) else {
        return false;
    };
    if cfg!(test) {
        return false;
    }
    let mut command = if cfg!(target_os = "windows") {
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", &url]);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    command.spawn().is_ok()
}

async fn post(path: &str, body: Value) -> Result<(u16, Value), String> {
    #[cfg(test)]
    return tests::transport("POST", path, &body.to_string()).await;
    #[cfg(not(test))]
    {
        let response = http::private_client()?
            .post(format!("{SERVER}{path}"))
            .header("User-Agent", "my-ai-assistant")
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(30))
            .json(&body)
            .send()
            .await
            .map_err(|_| "opencode_network_error")?;
        let code = response.status().as_u16();
        let data = response
            .json::<Value>()
            .await
            .map_err(|_| "opencode_invalid_response")?;
        Ok((code, data))
    }
}

pub async fn get(credential: &Credential, path: &str) -> Result<Value, String> {
    if !credential.valid() {
        return Err("opencode_invalid_credential".into());
    }
    // 两个入口固定在官方同一源；不接受导入凭据或接口响应提供的任意主机。
    let _url = if path == "/zen/go/v1/usage" {
        format!("https://opencode.ai{path}")
    } else if path.starts_with("/api/") {
        format!("{SERVER}{path}")
    } else {
        return Err("opencode_invalid_endpoint".into());
    };
    #[cfg(test)]
    {
        let (code, body) = tests::transport("GET", path, &credential.access).await?;
        return if (200..300).contains(&code) {
            Ok(body)
        } else {
            Err(format!("opencode_http_{code}"))
        };
    }
    #[cfg(not(test))]
    {
        let mut request = http::private_client()?
            .get(_url)
            .bearer_auth(&credential.access)
            .header("User-Agent", "my-ai-assistant")
            .header("Accept", "application/json");
        request = request.header(
            if path.starts_with("/zen/") {
                "x-opencode-org-id"
            } else {
                "x-org-id"
            },
            &credential.workspace_id,
        );
        let response = request.send().await.map_err(|_| "opencode_network_error")?;
        let code = response.status().as_u16();
        if !(200..300).contains(&code) {
            return Err(format!("opencode_http_{code}"));
        }
        response
            .json()
            .await
            .map_err(|_| "opencode_invalid_response".into())
    }
}

fn auth_locks() -> &'static Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn credential(
    app: &AppHandle,
    id: &str,
    rejected_access: Option<&str>,
) -> Result<Credential, String> {
    let lock = auth_locks()
        .lock()
        .map_err(|_| "state_lock_poisoned")?
        .entry(id.into())
        .or_default()
        .clone();
    let _guard = lock.lock().await;
    let snapshot = accounts::account_snapshot(id)?;
    let old = account_credential(&snapshot)?;
    let due = old.expires_at <= Utc::now().timestamp_millis() + 300_000;
    if !due && rejected_access != Some(old.access.as_str()) {
        return Ok(old);
    }
    let (code, body) = post(
        "/auth/device/token",
        json!({
            "client_id": CLIENT_ID, "grant_type": "refresh_token", "refresh_token": old.refresh,
        }),
    )
    .await?;
    if code == 400 || code == 401 {
        return Err("opencode_auth_required".into());
    }
    if !(200..300).contains(&code) {
        return Err(format!("opencode_http_{code}"));
    }
    let mut new = old;
    apply_token(&mut new, &body)?;
    if let Some(org) = body.get("org_id").and_then(Value::as_str) {
        if org != new.workspace_id {
            return Err("opencode_workspace_mismatch".into());
        }
    }
    // 授权添加可能在网络请求期间换了一组凭据。仅更新仍等于快照的副本。
    accounts::replace_opencode_auth(app, id, snapshot.opencode_auth.as_deref(), &new)?;
    account_credential(&accounts::account_snapshot(id)?)
}

fn apply_token(credential: &mut Credential, body: &Value) -> Result<(), String> {
    let access = text(body, "access_token");
    let refresh = text(body, "refresh_token");
    let expires = body
        .get("expires_in")
        .and_then(Value::as_i64)
        .filter(|n| *n > 0 && *n < 366 * 86400)
        .ok_or("opencode_invalid_response")?;
    if access.is_empty() || refresh.is_empty() {
        return Err("opencode_invalid_response".into());
    }
    credential.access = access.into();
    credential.refresh = refresh.into();
    credential.expires_at = Utc::now().timestamp_millis() + expires * 1000;
    Ok(())
}

pub async fn account_get(app: &AppHandle, id: &str, path: &str) -> Result<Value, String> {
    let auth = credential(app, id, None).await?;
    match get(&auth, path).await {
        Err(e) if e == "opencode_http_401" => {
            let renewed = credential(app, id, Some(&auth.access)).await?;
            get(&renewed, path).await
        }
        result => result,
    }
}

#[derive(Clone)]
struct Pending {
    device: String,
    deadline: i64,
    next_poll: i64,
    interval: i64,
    expected: Option<String>,
    note: Option<String>,
    cancelled: Arc<AtomicBool>,
    polling: bool,
    token: Option<Value>,
}
fn pending() -> &'static Mutex<HashMap<String, Pending>> {
    static PENDING: OnceLock<Mutex<HashMap<String, Pending>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

#[tauri::command]
pub async fn opencode_oauth_start(
    workspace_id: Option<String>,
    note: Option<String>,
) -> Result<Value, String> {
    let _operation = crate::operations::begin()?;
    if workspace_id.as_deref().is_some_and(|s| !valid_workspace(s)) {
        return Err("opencode_invalid_workspace".into());
    }
    let (code, body) = post(
        "/auth/device/code",
        json!({"client_id":CLIENT_ID,"supports_org_scope":true}),
    )
    .await?;
    if code != 200 {
        return Err(format!("opencode_http_{code}"));
    }
    let device = text(&body, "device_code").to_string();
    let url = authorization_url(text(&body, "verification_uri_complete"))?;
    if device.is_empty() {
        return Err("opencode_oauth_invalid_response".into());
    }
    let duration = body["expires_in"]
        .as_i64()
        .filter(|s| *s > 0 && *s <= 3600)
        .ok_or("opencode_invalid_response")?;
    let interval = body["interval"].as_i64().unwrap_or(5).clamp(1, 60);
    let now = Utc::now().timestamp_millis();
    let id = accounts::new_id();
    let mut list = pending().lock().map_err(|_| "state_lock_poisoned")?;
    list.retain(|_, p| p.deadline > now);
    list.insert(
        id.clone(),
        Pending {
            device,
            deadline: now + duration * 1000,
            next_poll: now + interval * 1000,
            interval,
            expected: workspace_id,
            note,
            cancelled: Arc::new(AtomicBool::new(false)),
            polling: false,
            token: None,
        },
    );
    drop(list);
    let opened = open_browser(&url);
    Ok(
        json!({"id":id,"url":url,"code":body["user_code"],"interval":interval,"expiresAt":now+duration*1000,"opened":opened}),
    )
}

#[tauri::command]
pub fn opencode_oauth_cancel(id: String) -> Result<(), String> {
    if let Some(p) = pending()
        .lock()
        .map_err(|_| "state_lock_poisoned")?
        .remove(&id)
    {
        p.cancelled.store(true, Ordering::SeqCst);
    }
    Ok(())
}

async fn identify(body: &Value, expected: Option<&str>) -> Result<Credential, String> {
    let mut auth = Credential {
        server: SERVER.into(),
        workspace_id: String::new(),
        workspace_name: String::new(),
        user_id: String::new(),
        email: String::new(),
        access: String::new(),
        refresh: String::new(),
        expires_at: 0,
    };
    apply_token(&mut auth, body)?;
    let scoped = body
        .get("org_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    if let (Some(want), Some(actual)) = (expected, scoped) {
        if want != actual {
            return Err("opencode_workspace_mismatch".into());
        }
    }
    auth.workspace_id = scoped.or(expected).unwrap_or("").into();
    if auth.workspace_id.is_empty() {
        let response = http::private_client()?
            .get(format!("{SERVER}/api/orgs"))
            .bearer_auth(&auth.access)
            .send()
            .await
            .map_err(|_| "opencode_network_error")?;
        if !response.status().is_success() {
            return Err("opencode_auth_required".into());
        }
        let orgs: Vec<Value> = response
            .json()
            .await
            .map_err(|_| "opencode_invalid_response")?;
        if orgs.len() != 1 {
            return Err("opencode_choose_workspace".into());
        }
        auth.workspace_id = text(&orgs[0], "id").into();
    }
    let context = get(&auth, "/api/orgs/current").await?;
    if context.pointer("/org/id").and_then(Value::as_str) != Some(auth.workspace_id.as_str()) {
        return Err("opencode_workspace_mismatch".into());
    }
    auth.workspace_name = context
        .pointer("/org/name")
        .and_then(Value::as_str)
        .unwrap_or(&auth.workspace_id)
        .into();
    auth.user_id = text(&context, "userId").into();
    if let Ok(user) = get(&auth, "/api/user").await {
        auth.email = text(&user, "email").into();
    }
    Ok(auth)
}

fn claim_poll(id: &str) -> Result<Option<Pending>, String> {
    let mut list = pending().lock().map_err(|_| "state_lock_poisoned")?;
    let now = Utc::now().timestamp_millis();
    let p = list.get_mut(id).ok_or("opencode_oauth_expired")?;
    if p.deadline <= now {
        list.remove(id);
        return Err("opencode_oauth_expired".into());
    }
    if p.polling || now < p.next_poll {
        return Ok(None);
    }
    p.polling = true;
    p.next_poll = now + p.interval * 1000;
    Ok(Some(p.clone()))
}

#[tauri::command]
pub async fn opencode_oauth_poll(app: AppHandle, id: String) -> Result<Value, String> {
    let _operation = crate::operations::begin()?;
    let Some(p) = claim_poll(&id)? else {
        return Ok(json!({"state":"pending"}));
    };
    let result = async {
        let body = if let Some(body) = p.token.clone() { body } else {
            let (code, body) = post("/auth/device/token", json!({
                "client_id":CLIENT_ID,"grant_type":"urn:ietf:params:oauth:grant-type:device_code","device_code":p.device
            })).await?;
            match text(&body, "error") {
                "authorization_pending" => return Ok(None),
                "slow_down" => {
                    if let Some(p) = pending().lock().map_err(|_| "state_lock_poisoned")?.get_mut(&id) {
                        p.interval = (p.interval + 5).min(60);
                        p.next_poll = Utc::now().timestamp_millis() + p.interval * 1000;
                    }
                    return Ok(None);
                }
                "expired_token" => return Err("opencode_oauth_expired".into()),
                "access_denied" => return Err("opencode_oauth_denied".into()),
                "" if (200..300).contains(&code) => {},
                _ => return Err(format!("opencode_oauth_http_{code}")),
            }
            // 设备码成功后不可再次换票，身份查询的网络重试复用内存里的结果。
            if let Some(p) = pending().lock().map_err(|_| "state_lock_poisoned")?.get_mut(&id) { p.token = Some(body.clone()); }
            body
        };
        let auth = identify(&body, p.expected.as_deref()).await?;
        Ok(Some(auth))
    }.await;
    let mut list = pending().lock().map_err(|_| "state_lock_poisoned")?;
    if p.cancelled.load(Ordering::SeqCst) || !list.contains_key(&id) {
        return Err("opencode_oauth_cancelled".into());
    }
    if let Some(p) = list.get_mut(&id) {
        p.polling = false;
    }
    match result {
        Ok(Some(auth)) => {
            let account = accounts::save_opencode(
                &app,
                Some(&auth),
                &auth.workspace_id,
                &auth.workspace_name,
                &auth.email,
                p.note.as_deref(),
            )?;
            list.remove(&id);
            Ok(json!({"state":"complete","account":account}))
        }
        Ok(None) => Ok(json!({"state":"pending"})),
        Err(e) => {
            if !retryable(&e) {
                list.remove(&id);
            }
            Err(e)
        }
    }
}

fn retryable(error: &str) -> bool {
    error == "opencode_network_error" || error.contains("http_5") || error.ends_with("http_429")
}

/// MicroCents = 10^-8 美元。只接受十进制整数字符串，避免 JS 先转 Number 丢精度。
pub fn workspace_usage_allowed(context: &Value) -> bool {
    matches!(text(context, "role"), "owner" | "admin")
}

pub fn dollars(value: &Value) -> Option<f64> {
    let amount = value.as_str()?.parse::<i128>().ok()?;
    let result = amount as f64 / 100_000_000.0;
    result.is_finite().then_some(result)
}

pub fn merge_subscription(status: &mut Value, context: &Value, subscription: &Value) {
    if subscription.is_null() {
        if context["hasGoSubscription"] == false {
            status["plan"] = json!("payg");
            status["subscriptionActive"] = json!(false);
            for key in [
                "billingCycleStart",
                "billingCycleEnd",
                "memberCount",
                "planWillRenew",
                "extraUsage",
                "renewalPending",
            ] {
                status[key] = Value::Null;
            }
            status["windows"] = json!([]);
        }
        return;
    }
    status["plan"] = subscription["product"].clone();
    status["memberCount"] = subscription["memberCount"]
        .as_u64()
        .filter(|n| *n > 0)
        .map_or(Value::Null, |n| json!(n));
    status["billingCycleStart"] = subscription
        .pointer("/access/startsAt")
        .cloned()
        .unwrap_or(Value::Null);
    status["billingCycleEnd"] = subscription
        .pointer("/access/endsAt")
        .cloned()
        .unwrap_or(Value::Null);
    status["planWillRenew"] = subscription["cancelAtPeriodEnd"]
        .as_bool()
        .map_or(Value::Null, |v| json!(!v));
    status["subscriptionActive"] = json!(!subscription["access"].is_null());
    status["extraUsage"] = subscription["useBalance"].clone();
    status["renewalPending"] = subscription["renewalPending"].clone();
}

pub fn quota_windows(value: &Value) -> Result<Value, String> {
    let mut windows = Vec::new();
    for (key, label) in [
        ("rolling", "5 小时"),
        ("weekly", "每周"),
        ("monthly", "每月"),
    ] {
        let meter = value
            .pointer(&format!("/usage/{key}"))
            .ok_or("opencode_invalid_response")?;
        let percent = meter["percent"]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .ok_or("opencode_invalid_response")?;
        let reset = text(meter, "resetsAt");
        if chrono::DateTime::parse_from_rfc3339(reset).is_err() {
            return Err("opencode_invalid_response".into());
        }
        windows.push(json!({"label":label,"usedPercent":percent,
            "resetAt":chrono::DateTime::parse_from_rfc3339(reset).map_err(|_|"opencode_invalid_response")?.timestamp(),
            "limited":meter["status"]=="rate-limited"}));
    }
    Ok(json!(windows))
}

fn merge_field(
    status: &mut Value,
    fields: &mut Value,
    context: &Value,
    part: &str,
    result: Result<Value, String>,
) {
    let result = result.and_then(|body| {
        match part {
            "subscription" => {
                if body.is_null() && context["hasGoSubscription"] != false
                    || !body.is_null() && text(&body, "product").is_empty()
                {
                    return Err("opencode_invalid_response".into());
                }
                merge_subscription(status, context, &body);
            }
            "quota" => status["windows"] = quota_windows(&body)?,
            "balance" => {
                let balance =
                    dollars(&body["balanceMicroCents"]).ok_or("opencode_invalid_response")?;
                status["balanceUsd"] = json!(balance);
                status["availableUsd"] =
                    dollars(&body["availableMicroCents"]).map_or(Value::Null, |v| json!(v));
                status["billingMode"] = body["billingMode"].clone();
            }
            _ => return Err("opencode_invalid_response".into()),
        }
        Ok(())
    });
    fields[part] = match result {
        Ok(()) => json!({"state":"ok","at":Utc::now().timestamp_millis()}),
        Err(e) => json!({"state":match e.as_str() {
            "opencode_http_403"=>"forbidden",
            "opencode_http_401"|"opencode_auth_required"=>"unauthorized",
            _=>"unavailable",
        },"error":e,"at":fields[part]["at"]}),
    };
}

pub async fn refresh_account(
    app: &AppHandle,
    id: &str,
    old: accounts::Account,
) -> Result<accounts::Account, String> {
    let mut status = old
        .status
        .clone()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let mut fields = old
        .status
        .as_ref()
        .and_then(|s| s.get("fields"))
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let context = match account_get(app, id, "/api/orgs/current").await {
        Ok(context)
            if context.pointer("/org/id").and_then(Value::as_str) == Some(old.token.as_str()) =>
        {
            context
        }
        Ok(_) => return Err("opencode_workspace_mismatch".into()),
        Err(error) => {
            status["alive"] = if error == "opencode_auth_required" || error == "opencode_http_401" {
                json!(false)
            } else {
                Value::Null
            };
            status["error"] = json!(error);
            status["stale"] = json!(true);
            return accounts::finish(app, id, status);
        }
    };
    status["alive"] = json!(true);
    status["error"] = Value::Null;
    status["workspaceId"] = json!(old.token);
    status["workspaceUsageAllowed"] = json!(workspace_usage_allowed(&context));
    status["workspaceName"] = context["org"]["name"].clone();
    status["name"] = context["org"]["name"].clone();
    let auth = account_credential(&accounts::account_snapshot(id)?)?;
    status["email"] = json!(auth.email);
    status["tokenExpiresAtMs"] = json!(auth.expires_at);
    status["quotaScope"] = json!("当前授权成员");
    status["balanceScope"] = json!("工作区共享");
    for (part, path) in [
        ("subscription", "/api/go/status"),
        ("quota", "/zen/go/v1/usage"),
        ("balance", "/api/billing/status"),
    ] {
        if part == "quota" && context["hasGoSubscription"] == false {
            status["windows"] = json!([]);
            fields[part] = json!({"state":"not_subscribed"});
            continue;
        }
        let result = account_get(app, id, path).await;
        merge_field(&mut status, &mut fields, &context, part, result);
    }
    status["stale"] = json!(fields
        .as_object()
        .unwrap()
        .values()
        .any(|v| !matches!(text(v, "state"), "ok" | "not_subscribed")));
    status["fields"] = fields;
    accounts::finish(app, id, status)
}

#[cfg(test)]
#[path = "tests/opencode.rs"]
pub(crate) mod tests;

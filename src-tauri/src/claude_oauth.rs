//! Claude (Anthropic) OAuth：PKCE 授权、授权码交换与 refresh_token 续期。
//! 端点与参数对齐 Claude Code CLI 的官方登录流程（client_id 为 Claude Code 公开值）。
//! 授权走 code=true 模式：浏览器完成登录后回调页直接展示可复制的授权码
//! （`<code>#<state>` 格式），无需本地回调服务器。

use base64::Engine;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use std::time::Duration;

use crate::http;

/// Claude Code CLI 的公开 OAuth client_id。
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
/// 授权码交换与续期端点（Claude Code 2.1.x 起使用 platform.claude.com）。
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// code=true 模式的回调页：授权完成后页面展示授权码由用户手动复制。
const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
/// 与 Claude Code 登录一致的完整 scope（写入本机凭据后 CLI 可全功能使用）。
pub const SCOPE: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

/// OAuth 控制面请求的 UA（Claude Code 自身即 Node axios，对齐可避免被风控误判）。
pub const OAUTH_UA: &str = "axios/1.15.2";

struct Pending {
    verifier: String,
    state: String,
}

/// 进行中的授权（begin 写入、finish 消费）。同一时刻只保留最后一次 begin。
static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

fn random_urlsafe(bytes: usize) -> Result<String, String> {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf).map_err(|e| format!("rng_failed: {e}"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf))
}

fn open_in_browser(url: &str) -> bool {
    if cfg!(target_os = "windows") {
        // rundll32 属 GUI 子系统不闪终端窗口，且参数原样传递（URL 含 & 也安全）
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .spawn()
            .is_ok()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn().is_ok()
    } else {
        false
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OauthBegin {
    pub url: String,
    /// 是否已成功唤起系统浏览器；失败时前端展示 url 供手动打开。
    pub opened: bool,
}

/// 生成 PKCE 授权地址并尝试打开系统浏览器。授权上下文保存在内存，等待 finish 消费。
#[tauri::command]
pub fn claude_oauth_begin() -> Result<OauthBegin, String> {
    let verifier = random_urlsafe(64)?;
    let challenge = {
        let digest = Sha256::digest(verifier.as_bytes());
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
    };
    let state = random_urlsafe(32)?;
    let url = reqwest::Url::parse_with_params(
        AUTHORIZE_URL,
        &[
            ("code", "true"),
            ("client_id", CLIENT_ID),
            ("response_type", "code"),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPE),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state.as_str()),
        ],
    )
    .map_err(|e| format!("oauth_url_failed: {e}"))?
    .to_string();
    *PENDING.lock().map_err(|_| "state_lock_poisoned".to_string())? =
        Some(Pending { verifier, state });
    let opened = open_in_browser(&url);
    Ok(OauthBegin { url, opened })
}

/// 授权码交换结果（expires_at_ms 由 expires_in 折算）。
pub struct ExchangeResult {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at_ms: Option<i64>,
    pub email: Option<String>,
}

/// token 端点响应的公共解析：access_token 必须存在。
fn parse_token_response(v: &Value) -> Result<ExchangeResult, String> {
    let access_token = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "oauth_missing_access_token".to_string())?;
    let refresh_token = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    let expires_at_ms = v
        .get("expires_in")
        .and_then(|x| x.as_i64())
        .filter(|n| *n > 0)
        .map(|n| chrono::Utc::now().timestamp_millis() + n * 1000);
    let email = v
        .pointer("/account/email_address")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(ExchangeResult {
        access_token,
        refresh_token,
        expires_at_ms,
        email,
    })
}

async fn post_token(body: Value) -> Result<(u16, String), String> {
    let resp = http::client()
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/plain, */*")
        .header("User-Agent", OAUTH_UA)
        .json(&body)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    Ok((status, text))
}

/// 用回调页展示的授权码（`code` 或 `code#state`）换取凭据组，消费 begin 保存的 PKCE 上下文。
pub async fn exchange_code(raw_code: &str) -> Result<ExchangeResult, String> {
    let pending = PENDING
        .lock()
        .map_err(|_| "state_lock_poisoned".to_string())?
        .take()
        .ok_or_else(|| "oauth_not_started".to_string())?;
    let raw = raw_code.trim().trim_matches('"').trim();
    if raw.is_empty() {
        return Err("oauth_code_empty".into());
    }
    // 回调页展示的授权码为 code#state；state 片段存在时以它为准
    let mut parts = raw.splitn(2, '#');
    let code = parts.next().unwrap_or_default().trim().to_string();
    let cb_state = parts.next().map(str::trim).unwrap_or_default().to_string();
    if code.is_empty() {
        return Err("oauth_code_empty".into());
    }
    let state = if cb_state.is_empty() { pending.state } else { cb_state };
    let body = json!({
        "grant_type": "authorization_code",
        "code": code,
        "redirect_uri": REDIRECT_URI,
        "client_id": CLIENT_ID,
        "code_verifier": pending.verifier,
        "state": state,
    });
    let (status, text) = post_token(body).await?;
    if (400..500).contains(&status) {
        return Err(format!("oauth_exchange_denied: {}", http::truncate(&text, 200)));
    }
    if !(200..300).contains(&status) {
        return Err(format!("oauth_exchange_http_{status}"));
    }
    let v: Value =
        serde_json::from_str(&text).map_err(|_| "oauth_invalid_response".to_string())?;
    parse_token_response(&v)
}

pub enum ClaudeRefreshOutcome {
    /// 2xx 且拿到了新 access_token。
    Success {
        access_token: String,
        refresh_token: Option<String>,
        expires_at_ms: Option<i64>,
    },
    /// HTTP 4xx（如 invalid_grant）：refresh token 已失效，非致命。
    Denied { body: String },
}

/// 用 refresh_token 换取新 access_token。传输错误与 5xx 返回 Err（调用方保留旧状态）；
/// 4xx 返回 Denied。不携带 scope，沿用原授权范围（token 来源不一，显式收窄反而会被拒）。
pub async fn request_claude_refresh(refresh_token: &str) -> Result<ClaudeRefreshOutcome, String> {
    let body = json!({
        "client_id": CLIENT_ID,
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
    });
    let (status, text) = post_token(body).await?;
    if (400..500).contains(&status) {
        return Ok(ClaudeRefreshOutcome::Denied {
            body: http::truncate(&text, 200),
        });
    }
    if !(200..300).contains(&status) {
        return Err(format!("refresh_http_{status}"));
    }
    let v: Value =
        serde_json::from_str(&text).map_err(|_| "refresh_invalid_response".to_string())?;
    let parsed = parse_token_response(&v).map_err(|_| "refresh_missing_access_token".to_string())?;
    Ok(ClaudeRefreshOutcome::Success {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        expires_at_ms: parsed.expires_at_ms,
    })
}

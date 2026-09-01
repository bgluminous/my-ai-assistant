use base64::Engine;
use std::time::Duration;

pub const UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";

/// 依据代理配置构建 reqwest 客户端（系统 TLS，避免 rustls 指纹导致 Cursor Cookie 被拒）。
/// - system：默认行为（读取系统/环境变量代理）
/// - direct：显式禁用任何代理
/// - custom：使用指定地址（支持 http/https/socks5，可含账号密码）
pub fn build_client(cfg: &crate::proxy::ProxyConfig) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .user_agent(UA)
        .timeout(Duration::from_secs(40));
    builder = match cfg.mode.as_str() {
        "custom" => {
            let url = cfg.url.trim();
            match reqwest::Proxy::all(url) {
                Ok(proxy) => builder.proxy(proxy),
                Err(_) => builder,
            }
        }
        "direct" => builder.no_proxy(),
        _ => builder,
    };
    builder
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

pub fn client() -> reqwest::Client {
    build_client(&crate::proxy::current())
}

/// Cursor 的 User Token 常以 `user_xxx::<jwt>` 形式出现，粘贴时 `::` 可能被 URL 编码为 `%3A%3A`。
pub fn normalize_cursor_token(raw: &str) -> String {
    raw.trim()
        .replace("%3A%3A", "::")
        .replace("%3a%3a", "::")
}

/// 取 `user_xxx::<jwt>` 中 `::` 之后的 JWT 部分；若无分隔符则原样返回。
pub fn jwt_part(token: &str) -> &str {
    match token.find("::") {
        Some(idx) => &token[idx + 2..],
        None => token,
    }
}

/// 解码 JWT 的 payload 段为 JSON（不校验签名，仅用于读取本地声明）。
pub fn decode_jwt_payload(jwt: &str) -> Option<serde_json::Value> {
    let mut parts = jwt.trim().split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let cleaned = payload.trim_end_matches('=');
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cleaned)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

//! accounts 模块的单元测试（由 accounts.rs 以 `#[path]` 挂载为 `crate::accounts::tests`）。

use super::*;

#[test]
fn error_code_extraction_matches_codex_cli() {
    // RFC 6749 形态：error 为字符串，说明在 error_description
    let rfc = r#"{"error":"invalid_grant","error_description":"refresh token expired"}"#;
    assert_eq!(refresh_error_code(rfc).as_deref(), Some("invalid_grant"));
    assert_eq!(refresh_error_detail(rfc), "refresh token expired");
    // 旧形态：error 为对象，子码在 error.code
    let legacy = r#"{"error":{"code":"refresh_token_reused","message":"already used"}}"#;
    assert_eq!(refresh_error_code(legacy).as_deref(), Some("refresh_token_reused"));
    assert_eq!(refresh_error_detail(legacy), "already used");
    // 顶层 code
    assert_eq!(
        refresh_error_code(r#"{"code":"refresh_token_expired"}"#).as_deref(),
        Some("refresh_token_expired")
    );
    // 非 JSON：没有错误码，说明取截断原文
    assert!(refresh_error_code("Bad Gateway").is_none());
    assert_eq!(refresh_error_detail("  Bad Gateway  "), "Bad Gateway");
}

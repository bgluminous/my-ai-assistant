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

#[test]
fn dead_cursor_status_keeps_previous_profile() {
    let prev = json!({
        "alive": true,
        "name": "Alice",
        "email": "alice@example.com",
        "membershipType": "pro",
        "billingCycleStart": "2026-09-01T00:00:00Z",
        "billingCycleEnd": "2026-10-01T00:00:00Z",
    });

    // 会话失效：接口不返回身份与套餐，沿用上一次缓存的用户名、邮箱、套餐与计费周期，
    // 失效标记不受影响；额度等其它字段不沿用
    let mut dead = json!({
        "alive": false,
        "name": null,
        "email": null,
        "membershipType": null,
        "billingCycleStart": null,
        "billingCycleEnd": null,
        "plan": { "used": null },
    });
    carry_cursor_profile(&mut dead, Some(&prev));
    assert_eq!(dead["alive"], json!(false));
    assert_eq!(dead["name"], json!("Alice"));
    assert_eq!(dead["email"], json!("alice@example.com"));
    assert_eq!(dead["membershipType"], json!("pro"));
    assert_eq!(dead["billingCycleStart"], json!("2026-09-01T00:00:00Z"));
    assert_eq!(dead["billingCycleEnd"], json!("2026-10-01T00:00:00Z"));
    assert_eq!(dead["plan"]["used"], Value::Null);

    // 接口返回了新值时以新值为准
    let mut fresh = json!({
        "alive": true,
        "name": "Alice B",
        "email": "alice@example.com",
        "membershipType": "ultra",
    });
    carry_cursor_profile(&mut fresh, Some(&prev));
    assert_eq!(fresh["name"], json!("Alice B"));
    assert_eq!(fresh["membershipType"], json!("ultra"));

    // 首次刷新（没有历史状态）或历史里也没有可沿用的值：保持原样
    let mut first = json!({ "alive": false, "name": null, "email": null, "membershipType": null });
    carry_cursor_profile(&mut first, None);
    assert_eq!(first["name"], Value::Null);
    assert_eq!(first["membershipType"], Value::Null);
    carry_cursor_profile(&mut first, Some(&json!({ "alive": false, "name": "  ", "membershipType": "" })));
    assert_eq!(first["name"], Value::Null);
    assert_eq!(first["email"], Value::Null);
    assert_eq!(first["membershipType"], Value::Null);
}

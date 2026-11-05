//! accounts 模块的单元测试（由 accounts.rs 以 `#[path]` 挂载为 `crate::accounts::tests`）。

use super::*;

#[test]
fn subscription_refresh_keeps_cached_plan_on_failure_but_online_values_win() {
    let prev = json!({ "accountId": "same", "plan": "prolite", "planActiveUntil": "2026-12-01" });
    let mut stale_jwt = json!({ "accountId": "same", "alive": true, "plan": "plus" });
    carry_subscription_profile("codex", &mut stale_jwt, Some(&prev), true);
    assert_eq!(stale_jwt["plan"], "prolite");
    let mut fresh = json!({ "accountId": "same", "alive": true, "plan": "promax" });
    carry_subscription_profile("codex", &mut fresh, Some(&prev), false);
    assert_eq!(fresh["plan"], "promax");
    assert!(fresh.get("planActiveUntil").is_none());
    let mut denied = json!({ "alive": false, "refreshError": "expired" });
    carry_subscription_profile("codex", &mut denied, Some(&prev), false);
    assert_eq!(denied["plan"], "prolite");
    assert_eq!(denied["alive"], false);
    let mut another = json!({ "accountId": "other", "plan": "plus" });
    carry_subscription_profile("codex", &mut another, Some(&prev), true);
    assert_eq!(another["plan"], "plus");
}

#[test]
fn claude_cached_metadata_respects_family_changes_and_legacy_tiers() {
    let prev = json!({ "accountUuid": "same", "plan": "max", "rateLimitTier": "default_claude_max_20x" });
    let mut failed = json!({ "alive": false, "plan": null, "rateLimitTier": null });
    carry_subscription_profile("claude", &mut failed, Some(&prev), false);
    assert_eq!(failed["plan"], "max");
    assert_eq!(failed["rateLimitTier"], "default_claude_max_20x");
    let mut same = json!({ "alive": true, "plan": "claude_max" });
    carry_subscription_profile("claude", &mut same, Some(&prev), false);
    assert_eq!(same["rateLimitTier"], "default_claude_max_20x");
    for new_plan in ["pro", "team", "future_plan"] {
        let mut fresh = json!({ "alive": true, "plan": new_plan });
        carry_subscription_profile("claude", &mut fresh, Some(&prev), false);
        assert_eq!(fresh["plan"], new_plan);
        assert!(fresh.get("rateLimitTier").is_none());
    }
    let mut upgraded = json!({ "alive": true, "plan": "max", "rateLimitTier": "future_tier" });
    carry_subscription_profile("claude", &mut upgraded, Some(&prev), false);
    assert_eq!(upgraded["rateLimitTier"], "future_tier");
    let mut legacy = json!({ "alive": true, "plan": "max" });
    carry_subscription_profile("claude", &mut legacy, Some(&json!({ "plan": "default_claude_max_5x" })), false);
    assert_eq!(legacy["rateLimitTier"], "default_claude_max_5x");
    let mut empty = json!({ "alive": false });
    carry_subscription_profile("claude", &mut empty, None, false);
    assert!(empty.get("plan").is_none());
}

#[test]
fn cursor_api_filter_defaults_off_and_survives_serialization() {
    let mut account: Account = serde_json::from_value(json!({
        "id": "acc-test", "kind": "cursor", "token": "fixture"
    })).unwrap();
    assert!(!account.ignore_api_models);
    account.ignore_api_models = true;
    let stored = serde_json::to_value(&account).unwrap();
    assert_eq!(stored["ignoreApiModels"], true);
    let restored: Account = serde_json::from_value(stored).unwrap();
    assert!(restored.ignore_api_models);

    let legacy: AccountExportItem = serde_json::from_value(json!({ "token": "fixture" })).unwrap();
    assert!(!legacy.ignore_api_models);
    let exported: AccountExportItem = serde_json::from_value(json!({
        "token": "fixture", "ignoreApiModels": true
    })).unwrap();
    assert!(serde_json::from_value::<AccountExportItem>(serde_json::to_value(exported).unwrap())
        .unwrap().ignore_api_models);
}

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

//! cursor 模块的单元测试（由 cursor.rs 以 `#[path]` 挂载为 `crate::cursor::tests`）。

use super::{parse_event, parse_timestamp_ms};
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

#[test]
fn billing_kind_and_model_pool_jointly_determine_api_usage() {
    let cases = [
        ("claude-fable-5-1-thinking-xhigh", "INCLUDED_IN_PRO", Some(true)),
        ("gpt-5", "Included", Some(true)),
        ("gemini-2.5-pro", "INCLUDED_IN_PRO_PLUS", Some(true)),
        ("composer-1", "INCLUDED_IN_PRO", Some(true)),
        ("composer-2.5-fast", "INCLUDED_IN_PRO", Some(false)),
        ("cursor-grok-4.6-xhigh-fast", "INCLUDED_IN_PRO", Some(false)),
        ("grok-4.7-high", "INCLUDED_IN_PRO", Some(false)),
        ("Auto", "INCLUDED_IN_PRO", Some(false)),
        ("composer-2.5-fast", "USAGE_BASED", Some(true)),
        ("cursor-grok-4.6-xhigh-fast", "Usage-based", Some(true)),
        ("auto", "ON_DEMAND", Some(true)),
        ("claude-fable-5-1-thinking-xhigh", "USER_API_KEY", Some(false)),
        ("claude-fable-5-1-thinking-xhigh", "FREE_CREDIT", Some(false)),
        ("gpt-5", "ERRORED_NOT_CHARGED", Some(false)),
        ("api", "INCLUDED_IN_PRO", None),
        ("unknown", "INCLUDED_IN_PRO", None),
        ("composer-20", "INCLUDED_IN_PRO", None),
        ("gpt-5", "UNRECOGNIZED", None),
    ];
    for (model, kind, expected) in cases {
        for prefix in ["", "USAGE_EVENT_KIND_"] {
            let event = parse_event(&json!({
                "model": model,
                "kind": format!("{prefix}{kind}"),
                "isTokenBasedCall": true,
                "isChargeable": true,
                // 金额为零同样可能消耗 API 额度；不以金额判断。
                "chargedCents": 0,
                "cursorTokenFee": 0
            }));
            assert_eq!(event.billing.api_usage(&event.row.model), expected, "{model} {kind}");
        }
    }
}

#[test]
fn api_usage_distinguishes_legacy_requests_and_cursor_fee_for_byok() {
    let included = parse_event(&json!({
        "model": "gpt-5", "kind": "Included in Business",
        "isTokenBasedCall": false, "chargedCents": 25
    }));
    assert_eq!(included.billing.api_usage(&included.row.model), Some(false));
    let byok_fee = parse_event(&json!({
        "model": "gpt-5", "kind": "User API Key", "cursorTokenFee": 1.25
    }));
    assert_eq!(byok_fee.billing.api_usage(&byok_fee.row.model), Some(true));
    let missing = parse_event(&json!({"model":"gpt-5", "kind":"Included"}));
    assert_eq!(missing.billing.api_usage(&missing.row.model), None);
    let no_kind = parse_event(&json!({"model":"gpt-5", "isTokenBasedCall":true, "chargedCents":100}));
    assert_eq!(no_kind.billing.api_usage(&no_kind.row.model), None);
}

#[test]
fn event_parser_keeps_billing_metadata_and_token_rows() {
    let event = parse_event(&json!({
        "model": "claude-fable-5-1-thinking-xhigh",
        "kind": "USAGE_EVENT_KIND_INCLUDED_IN_PRO",
        "isTokenBasedCall": true,
        "cursorTokenFee": 0,
        "timestamp": "1750979225854",
        "chargedCents": 25,
        "tokenUsage": {
            "inputTokens": 10, "outputTokens": 20,
            "cacheReadTokens": 30, "cacheWriteTokens": 40
        }
    }));
    let row = event.row;
    assert_eq!(row.model, "claude-fable-5-1-thinking-xhigh");
    assert_eq!((row.input, row.output, row.cache_read, row.cache_write), (10.0, 20.0, 30.0, 40.0));
    assert_eq!(row.actual_cents, 25.0);
    assert_eq!(row.timestamp_ms, Some(1_750_979_225_854));
    assert_eq!(event.billing.kind, "USAGE_EVENT_KIND_INCLUDED_IN_PRO");
    assert_eq!(event.billing.is_token_based_call, Some(true));
    for value in [json!({}), json!({"model":""}), json!({"model":null})] {
        assert_eq!(parse_event(&value).row.model, "unknown");
    }
}

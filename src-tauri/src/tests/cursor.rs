//! cursor 模块的单元测试（由 cursor.rs 以 `#[path]` 挂载为 `crate::cursor::tests`）。

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

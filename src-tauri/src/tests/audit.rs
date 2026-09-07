//! audit 模块的单元测试（由 audit.rs 以 `#[path]` 挂载为 `crate::audit::tests`，可访问私有项）。

use super::*;

#[test]
fn every_registered_event_is_unique() {
    for (i, (id, ..)) in EVENTS.iter().enumerate() {
        assert!(
            !EVENTS[..i].iter().any(|(other, ..)| other == id),
            "事件重复登记：{id}"
        );
    }
}

#[test]
fn legacy_entry_without_level_gets_defaults() {
    let entry: AuditEntry = serde_json::from_str(
        r#"{"ts":1,"event":"account_refresh_failed","message":"x"}"#,
    )
    .unwrap();
    let row = AuditRow::from(entry);
    assert_eq!(row.level, Level::Error);
    assert_eq!(row.category, Category::State);
    assert_eq!(row.label, "刷新失败");
}

#[test]
fn stored_level_overrides_default() {
    let entry: AuditEntry = serde_json::from_str(
        r#"{"ts":1,"level":"info","category":"state","event":"account_state_changed","message":"x"}"#,
    )
    .unwrap();
    let row = AuditRow::from(entry);
    assert_eq!(row.level, Level::Info);
    assert_eq!(row.category, Category::State);
}

#[test]
fn level_and_category_serialize_as_lowercase_ids() {
    assert_eq!(serde_json::to_string(&Level::Warn).unwrap(), r#""warn""#);
    assert_eq!(
        serde_json::to_string(&Category::Credential).unwrap(),
        r#""credential""#
    );
}

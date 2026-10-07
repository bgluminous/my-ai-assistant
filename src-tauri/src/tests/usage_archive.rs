//! usage_archive 模块的单元测试（由 usage_archive.rs 以 `#[path]` 挂载为 `crate::usage_archive::tests`）。

use super::*;

fn meta(note: &str, note_auto: bool, email: Option<&str>, name: Option<&str>) -> DeletedMeta {
    DeletedMeta {
        deleted_at: 0,
        note: note.to_string(),
        note_auto,
        email: email.map(str::to_string),
        name: name.map(str::to_string),
        membership_type: None,
        ignore_api_models: false,
        api_filter_legacy: false,
    }
}

#[test]
fn deleted_note_manual_overrides_identity_and_blank_restores_auto() {
    let identity = Some("cursor:user_01ABC");
    let mut m = meta("alice@example.com", true, Some("alice@example.com"), Some("Alice"));
    assert_eq!(deleted_label(&m), "Alice");

    // 手填备注：显示时优先于用户名 / 邮箱，首尾空白去掉
    apply_deleted_note(&mut m, identity, "  工作号  ");
    assert!(!m.note_auto);
    assert_eq!(m.note, "工作号");
    assert_eq!(deleted_label(&m), "工作号");

    // 留空恢复自动备注：备注回到邮箱，显示回到用户名
    apply_deleted_note(&mut m, identity, "   ");
    assert!(m.note_auto);
    assert_eq!(m.note, "alice@example.com");
    assert_eq!(deleted_label(&m), "Alice");

    // 没有邮箱时自动备注取身份里的账号 ID；连身份也没有则为空，显示「未命名账户」
    let mut bare = meta("旧备注", false, None, None);
    apply_deleted_note(&mut bare, identity, "");
    assert!(bare.note_auto);
    assert_eq!(bare.note, "user_01ABC");
    assert_eq!(deleted_label(&bare), "user_01ABC");
    apply_deleted_note(&mut bare, None, "");
    assert_eq!(bare.note, "");
    assert_eq!(deleted_label(&bare), "未命名账户");
}

#[test]
fn membership_choice_normalizes_and_rejects_unknown() {
    // 登记的档位：去空白、统一小写
    assert_eq!(normalize_membership_choice(" Pro "), Ok(Some("pro".to_string())));
    assert_eq!(normalize_membership_choice("PRO_PLUS"), Ok(Some("pro_plus".to_string())));
    // 留空 = 清除为未知
    assert_eq!(normalize_membership_choice("   "), Ok(None));
    // 未登记的值不接受
    assert_eq!(normalize_membership_choice("platinum"), Err("invalid_membership".to_string()));
}

fn usage_event(model: &str, kind: &str, timestamp_ms: Option<i64>) -> cursor::UsageEvent {
    cursor::UsageEvent {
        row: TokenRow {
            model: model.into(), input: 1.0, output: 2.0, cache_read: 3.0, cache_write: 4.0,
            actual_cents: 25.0, timestamp_ms,
        },
        billing: cursor::UsageBilling {
            kind: kind.into(), is_token_based_call: Some(true), cursor_token_fee: Some(0.0),
        },
    }
}

#[test]
fn api_events_are_removed_from_archives_and_serialized_backups() {
    let included = json!({"kind":"INCLUDED_IN_PRO", "isTokenBasedCall":true, "cursorTokenFee":0});
    let on_demand = json!({"kind":"USAGE_BASED", "isTokenBasedCall":true, "cursorTokenFee":0});
    let byok = json!({"kind":"USER_API_KEY", "isTokenBasedCall":true, "cursorTokenFee":0});
    let data = json!({
        "version": ARCHIVE_VERSION,
        "accountId": "acc-test",
        "identity": "cursor:user_test",
        "syncedAt": 1_750_979_225_854_i64,
        "deleted": {"deletedAt": 1_750_979_225_855_i64, "note": "保留账户"},
        "events": [
            ["claude-fable-5-1-thinking-xhigh", 1000, 2000, 3000, 4000, 500, 1_750_979_225_854_i64, included],
            ["composer-2.5-fast", 100, 200, 300, 400, 50, null, on_demand],
            ["cursor-grok-4.6-xhigh-fast", 1, 2, 3, 4, 25, 1_750_979_225_854_i64, included],
            ["claude-fable-5-1-thinking-xhigh", 1, 1, 1, 1, 75, null, byok]
        ]
    });
    let disk: UsageArchive = serde_json::from_str(&data.to_string()).unwrap();
    let backup: UsageArchive = serde_json::from_value(data.clone()).unwrap();
    for mut archive in [disk, backup] {
        assert!(!account_api_filter(&archive).unwrap());
        let unfiltered = slice(&archive, None, None, false);
        assert_eq!(unfiltered.total_tokens, 11_014.0);
        assert_eq!(unfiltered.total_actual_usd, 6.5);
        let agg = slice(&archive, None, None, true);
        assert_eq!(agg.total_tokens, 14.0);
        assert_eq!(agg.total_actual_usd, 1.0);
        assert_eq!(agg.models.len(), 2);
        assert_eq!(agg.daily.iter().map(|day| day.tokens).sum::<f64>(), 10.0);
        let bounded = slice(&archive, Some(1_750_979_225_854), Some(1_750_979_225_854), true);
        assert_eq!(bounded.total_tokens, 10.0);
        assert_eq!(bounded.total_actual_usd, 0.25);
        assert_eq!(deleted_record(&archive).unwrap().events, 4);
        archive.deleted.as_mut().unwrap().ignore_api_models = true;
        assert!(prune_api_events(&mut archive).unwrap());
        assert!(!prune_api_events(&mut archive).unwrap());
        assert!(account_api_filter(&archive).unwrap());
        assert_eq!(deleted_record(&archive).unwrap().events, 2);
        let persisted = serde_json::to_value(&archive).unwrap();
        let expected: Vec<EventTuple> = serde_json::from_value(
            json!([data["events"][2], data["events"][3]]),
        ).unwrap();
        assert_eq!(persisted["events"], serde_json::to_value(expected).unwrap());
        assert_eq!(persisted["lastEventAt"], 1_750_979_225_854_i64);
        let restored: UsageArchive = serde_json::from_value(persisted).unwrap();
        assert!(account_api_filter(&restored).unwrap());
        // 关闭后不会凭空还原已经清理的 API 明细。
        assert_eq!(slice(&restored, None, None, false).total_tokens, 14.0);
        assert_eq!(restored.events.len(), 2);
    }
}

#[test]
fn legacy_seven_column_events_require_billing_before_pruning() {
    let data = json!({"version":2, "events":[["gpt-5",1,2,3,4,25,100]]});
    for mut archive in [
        serde_json::from_str::<UsageArchive>(&data.to_string()).unwrap(),
        serde_json::from_value::<UsageArchive>(data).unwrap(),
    ] {
        assert!(needs_billing(&archive));
        assert_eq!(slice(&archive, None, None, false).total_tokens, 10.0);
        let before = serde_json::to_value(&archive).unwrap();
        assert!(prune_api_events(&mut archive).is_err());
        assert_eq!(serde_json::to_value(&archive).unwrap(), before);
        let mut fetched = usage_event("gpt-5", "INCLUDED_IN_PRO", Some(100));
        fetched.row.actual_cents = 0.0; // 远端隐藏金额不能阻止计费信息匹配，也不能覆盖旧金额。
        hydrate_billing(&mut archive, &[fetched]).unwrap();
        assert_eq!(archive.events[0].5, 25.0);
        assert!(!needs_billing(&archive));
        assert!(prune_api_events(&mut archive).unwrap());
        assert!(archive.events.is_empty());
        assert_eq!(archive.last_event_at, Some(100));
    }
}

#[test]
fn missing_or_ambiguous_historical_billing_leaves_original_data_intact() {
    let mut archive: UsageArchive = serde_json::from_value(json!({
        "version":2, "events":[
            ["gpt-5",1,2,3,4,25,100], ["composer-2.5",1,2,3,4,25,200]
        ]
    })).unwrap();
    let before = serde_json::to_value(&archive).unwrap();
    let only_one = usage_event("gpt-5", "INCLUDED_IN_PRO", Some(100));
    assert!(hydrate_billing(&mut archive, &[only_one.clone()]).is_err());
    assert_eq!(serde_json::to_value(&archive).unwrap(), before);
    let different_pool = usage_event("gpt-5", "USER_API_KEY", Some(100));
    assert!(hydrate_billing(&mut archive, &[only_one, different_pool]).is_err());
    assert_eq!(serde_json::to_value(&archive).unwrap(), before);
    // 未知计费类型也不能被当成已成功迁移。
    let unknown = usage_event("gpt-5", "UNKNOWN_BILLING_KIND", Some(100));
    assert!(hydrate_billing(&mut archive, &[unknown]).is_err());
    assert_eq!(serde_json::to_value(&archive).unwrap(), before);
}

#[test]
fn api_only_and_empty_archives_remain_readable() {
    let mut archive = UsageArchive {synced_at:Some(100), ..Default::default()};
    merge_events(&mut archive, &[usage_event("gpt-5", "INCLUDED_IN_PRO", Some(200))], None, true).unwrap();
    assert!(archive.events.is_empty());
    assert_eq!(archive.last_event_at, Some(200));
    assert_eq!(archive.synced_at, Some(100));
    assert_eq!(slice(&archive, None, None, true).total_tokens, 0.0);
    assert_eq!(slice(&archive, None, None, false).total_actual_usd, 0.0);
    assert!(!prune_api_events(&mut archive).unwrap());
}

#[test]
fn sync_filters_api_events_without_losing_incremental_cursor() {
    let rows = [
        usage_event("composer-2.5", "INCLUDED_IN_PRO", Some(100)),
        usage_event("gpt-5", "INCLUDED_IN_PRO", Some(200)),
        usage_event("composer-2.5", "USAGE_BASED", None),
        usage_event("gpt-5", "USER_API_KEY", None),
    ];
    let mut filtered = UsageArchive::default();
    let mut other = UsageArchive::default();
    merge_events(&mut filtered, &rows, None, true).unwrap();
    merge_events(&mut other, &rows, None, false).unwrap();
    assert_eq!(filtered.events.len(), 2);
    assert_eq!(other.events.len(), 4);
    assert_eq!(filtered.last_event_at, Some(200));
    let next = [
        usage_event("cursor-grok-4.6-xhigh-fast", "USAGE_BASED", Some(300)),
        usage_event("composer-2.5", "INCLUDED_IN_PRO", Some(250)),
        usage_event("gpt-5", "FREE_CREDIT", None),
    ];
    merge_events(&mut filtered, &next, Some(150), true).unwrap();
    assert_eq!(filtered.events.len(), 3);
    assert_eq!(filtered.last_event_at, Some(300));
    assert!(filtered.events.iter().all(|e| e.7.as_ref().unwrap().api_usage(&e.0) == Some(false)));
    let once = serde_json::to_value(&filtered).unwrap();
    merge_events(&mut filtered, &next, Some(150), true).unwrap();
    assert_eq!(serde_json::to_value(&filtered).unwrap(), once);
    merge_events(&mut filtered, &next[..1], None, true).unwrap();
    assert!(filtered.events.is_empty());
    assert_eq!(filtered.last_event_at, Some(300));
    merge_events(&mut filtered, &next[..1], Some(250), false).unwrap();
    assert_eq!(filtered.events.len(), 1);
    assert_eq!(other.events.len(), 4);
}

#[test]
fn unknown_billing_does_not_partially_merge_or_prune() {
    let mut archive = UsageArchive::default();
    merge_events(&mut archive, &[usage_event("composer-2.5", "INCLUDED_IN_PRO", Some(100))], None, true).unwrap();
    let before = serde_json::to_value(&archive).unwrap();
    let unknown = [usage_event("gpt-5", "UNKNOWN_BILLING_KIND", Some(200))];
    assert!(merge_events(&mut archive, &unknown, Some(50), true).is_err());
    assert_eq!(serde_json::to_value(&archive).unwrap(), before);
}

fn deleted_archive(events: Vec<EventTuple>) -> UsageArchive {
    UsageArchive {
        version: ARCHIVE_VERSION,
        account_id: "acc-deleted-test".into(),
        identity: Some("cursor:user_deleted_test".into()),
        synced_at: Some(500),
        deleted: Some(meta("旧备注", false, None, None)),
        events,
        ..Default::default()
    }
}

#[test]
fn deleted_api_removal_is_one_way_and_keeps_note_editable() {
    let mut archive = deleted_archive(vec![
        to_tuple(&usage_event("gpt-5", "INCLUDED", Some(100))),
        to_tuple(&usage_event("composer-2.5", "INCLUDED", Some(200))),
        to_tuple(&usage_event("composer-2.5", "USAGE_BASED", Some(400))),
        to_tuple(&usage_event("gpt-5", "USER_API_KEY", Some(300))),
    ]);
    assert_eq!(apply_deleted_update(&mut archive, " 新备注 ", true).unwrap(), 2);
    let record = deleted_record(&archive).unwrap();
    assert!(record.ignore_api_models);
    assert!(!record.api_removal_approximate);
    assert!(record.api_removal_error.is_none());
    assert_eq!(record.note, "新备注");
    assert_eq!(record.events, 2);
    assert_eq!(archive.last_event_at, Some(400));
    assert_eq!(slice(&archive, None, None, true).total_tokens, 20.0);
    assert_eq!(slice(&archive, None, None, true).total_actual_usd, 0.5);
    assert_eq!(apply_deleted_update(&mut archive, "再次编辑", false).unwrap(), 0);
    assert!(archive.deleted.as_ref().unwrap().ignore_api_models);
    assert_eq!(archive.deleted.as_ref().unwrap().note, "再次编辑");
    assert_eq!(apply_deleted_update(&mut archive, "再次编辑", true).unwrap(), 0);
    assert_eq!(archive.events.len(), 2);
}

#[test]
fn deleted_legacy_removal_uses_model_only_when_billing_is_missing() {
    let mut legacy = to_tuple(&usage_event("gpt-5", "INCLUDED", Some(100)));
    legacy.7 = None;
    let mut own = to_tuple(&usage_event("composer-2.5", "INCLUDED", Some(200)));
    own.7 = None;
    let mut archive = deleted_archive(vec![
        legacy, own,
        to_tuple(&usage_event("gpt-5", "USER_API_KEY", Some(300))),
        to_tuple(&usage_event("composer-2.5", "ON_DEMAND", Some(400))),
    ]);
    let before = deleted_record(&archive).unwrap();
    assert!(before.api_removal_approximate);
    assert!(before.api_removal_error.is_none());
    assert_eq!(apply_deleted_update(&mut archive, "近似清理", true).unwrap(), 2);
    assert!(legacy_api_filter(&archive));
    assert_eq!(archive.events.len(), 2);
    assert!(archive.events[0].7.is_none()); // 不伪造计费信息。
    assert_eq!(archive.events[0].0, "composer-2.5");
    assert_eq!(archive.events[1].7.as_ref().unwrap().kind, "USER_API_KEY");
    let backup = serde_json::to_value(&archive).unwrap();
    let mut restored: UsageArchive = serde_json::from_value(backup).unwrap();
    assert!(restored.deleted.as_ref().unwrap().ignore_api_models);
    assert!(legacy_api_filter(&restored));
    assert!(!prune_api_events(&mut restored).unwrap());
    assert_eq!(slice(&restored, None, None, true).total_tokens, 20.0);
    assert_eq!(apply_deleted_update(&mut restored, "可改备注", false).unwrap(), 0);
    assert!(deleted_record(&restored).unwrap().api_removal_approximate);
}

#[test]
fn deleted_removal_rejects_unknown_billing_and_active_accounts_atomically() {
    for event in [
        EventTuple("unknown".into(), 1.0, 2.0, 3.0, 4.0, 25.0, Some(200), None),
        to_tuple(&usage_event("gpt-5", "UNKNOWN_KIND", Some(200))),
    ] {
        let mut archive = deleted_archive(vec![
            to_tuple(&usage_event("gpt-5", "INCLUDED", Some(100))), event,
        ]);
        let before = serde_json::to_value(&archive).unwrap();
        assert!(deleted_record(&archive).unwrap().api_removal_error.is_some());
        assert!(apply_deleted_update(&mut archive, "不得写入", true).is_err());
        assert_eq!(serde_json::to_value(&archive).unwrap(), before);
        // 不清理时，仍可改备注。
        assert_eq!(apply_deleted_update(&mut archive, "只改备注", false).unwrap(), 0);
    }
    let mut active = UsageArchive::default();
    let before = serde_json::to_value(&active).unwrap();
    assert_eq!(apply_deleted_update(&mut active, "", true), Err("not_deleted_account".into()));
    assert_eq!(serde_json::to_value(&active).unwrap(), before);
}

#[test]
fn deleted_api_only_archive_keeps_closed_marker_after_becoming_empty() {
    let mut archive = deleted_archive(vec![to_tuple(&usage_event("gpt-5", "INCLUDED", Some(200)))]);
    assert_eq!(apply_deleted_update(&mut archive, "", true).unwrap(), 1);
    assert!(archive.events.is_empty());
    let restored: UsageArchive = serde_json::from_value(serde_json::to_value(&archive).unwrap()).unwrap();
    assert!(restored.deleted.as_ref().unwrap().ignore_api_models);
    assert_eq!(deleted_record(&restored).unwrap().events, 0);
    assert_eq!(slice(&restored, None, None, true).total_tokens, 0.0);
    assert_eq!(restored.last_event_at, Some(200));
}

#[test]
fn backup_cannot_reenable_deleted_api_history() {
    let event = to_tuple(&usage_event("gpt-5", "INCLUDED", Some(200)));
    let mut local = deleted_archive(vec![event.clone()]);
    apply_deleted_update(&mut local, "", true).unwrap();
    let local_before = serde_json::to_value(&local).unwrap();
    let mut incoming = deleted_archive(vec![event]);
    preserve_deleted_api_filter(&mut incoming, &[local.clone()]).unwrap();
    assert!(incoming.deleted.as_ref().unwrap().ignore_api_models);
    assert!(incoming.events.is_empty());
    // 严格计费模式下导入缺失信息的旧备份应拒绝，不能偷偷改成近似清理。
    let mut legacy = deleted_archive(vec![EventTuple("gpt-5".into(), 1.0, 2.0, 3.0, 4.0, 25.0, Some(200), None)]);
    assert!(preserve_deleted_api_filter(&mut legacy, &[local.clone()]).is_err());
    assert_eq!(serde_json::to_value(&local).unwrap(), local_before);
}

#[test]
fn backup_preserves_legacy_policy_without_resurrecting_removed_events() {
    let events = vec![
        EventTuple("gpt-5".into(), 1.0, 2.0, 3.0, 4.0, 25.0, Some(100), None),
        EventTuple("auto".into(), 1.0, 2.0, 3.0, 4.0, 25.0, Some(200), None),
    ];
    let mut local = deleted_archive(events.clone());
    let mut incoming = deleted_archive(events);
    apply_deleted_update(&mut local, "", true).unwrap();
    preserve_deleted_api_filter(&mut incoming, &[local]).unwrap();
    assert!(incoming.deleted.as_ref().unwrap().ignore_api_models);
    assert!(legacy_api_filter(&incoming));
    assert_eq!(incoming.events.len(), 1);
    assert_eq!(incoming.events[0].0, "auto");
    assert_eq!(slice(&incoming, None, None, true).total_tokens, 10.0);
}

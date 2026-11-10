use super::*;

#[test]
fn opencode_failure_delete_rolls_back_archive_when_settings_or_sql_write_fails() {
    let mut db = open_path(Path::new(":memory:")).unwrap();
    let row = parse_event(&event("kept", 1000), "wrk_a").unwrap().unwrap();
    save_page(&mut db, "wrk_a", &[row]).unwrap();
    completed(&mut db, "wrk_a", 500, 2000).unwrap();
    db.execute("INSERT INTO checks VALUES('wrk_a','{}')", []).unwrap();
    let before = export_db(&db).unwrap();

    let result = remove_after_save_db(&mut db, "wrk_a", || Err::<(), _>("settings_write_failed".into()));
    assert_eq!(result.unwrap_err(), "settings_write_failed");
    assert_eq!(export_db(&db).unwrap(), before);
    assert_eq!(db.query_row("SELECT count(*) FROM checks", [], |r| r.get::<_, i64>(0)).unwrap(), 1);

    db.execute_batch("CREATE TRIGGER deny_delete BEFORE DELETE ON coverage BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
    let result: Result<(), String> = remove_after_save_db(&mut db, "wrk_a", || panic!("failed archive staging must not delete the account"));
    assert!(result.is_err());
    assert_eq!(export_db(&db).unwrap(), before);
    db.execute_batch("DROP TRIGGER deny_delete;").unwrap();

    assert_eq!(remove_after_save_db(&mut db, "wrk_a", || Ok(42)).unwrap(), 42);
    for table in ["events", "coverage", "sync", "checks"] {
        assert_eq!(db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }
}

#[test]
fn opencode_failure_delete_commit_reports_saved_account_and_keeps_archive() {
    let mut db = open_path(Path::new(":memory:")).unwrap();
    save_page(&mut db, "wrk_a", &[parse_event(&event("kept", 1000), "wrk_a").unwrap().unwrap()]).unwrap();
    let before = export_db(&db).unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON;
        CREATE TABLE fixture_ref(workspace TEXT, id TEXT,
        FOREIGN KEY(workspace,id) REFERENCES events(workspace,id) DEFERRABLE INITIALLY DEFERRED);
        INSERT INTO fixture_ref VALUES('wrk_a','kept');").unwrap();
    let persisted = std::cell::Cell::new(false);
    let result = remove_after_save_db(&mut db, "wrk_a", || { persisted.set(true); Ok(()) });
    assert_eq!(result.unwrap_err(), "opencode_archive_cleanup_failed_account_deleted");
    assert!(persisted.get());
    assert_eq!(export_db(&db).unwrap(), before);
}

#[test]
fn opencode_failure_backup_write_rolls_back_all_workspaces_and_can_retry() {
    let mut db = open_path(Path::new(":memory:")).unwrap();
    save_page(&mut db, "wrk_a", &[parse_event(&event("old", 900), "wrk_a").unwrap().unwrap()]).unwrap();
    completed(&mut db, "wrk_a", 800, 950).unwrap();
    let before = export_db(&db).unwrap();
    let row = parse_event(&event("new", 1000), "wrk_a").unwrap().unwrap();
    let backup = json!({"version":1,"workspaces":[
        {"workspaceId":"wrk_a","events":[row],"coverage":[[500,2000]],"syncedAt":2000},
        {"workspaceId":"wrk_b","events":[row],"coverage":[[500,2000]],"syncedAt":2000}
    ]});
    db.execute_batch("CREATE TRIGGER deny_restore BEFORE INSERT ON coverage WHEN NEW.workspace='wrk_b'
        BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(restore_archives(&mut db, parse_archives(&backup).unwrap()).is_err());
    assert_eq!(export_db(&db).unwrap(), before);
    db.execute_batch("DROP TRIGGER deny_restore;").unwrap();
    assert_eq!(restore_archives(&mut db, parse_archives(&backup).unwrap()).unwrap(), 2);
    assert_eq!(restore_archives(&mut db, parse_archives(&backup).unwrap()).unwrap(), 0);
    assert_eq!(query_db(&db, "wrk_a", None, None).unwrap()["agg"]["totalTokens"], 200.0);
    assert_eq!(query_db(&db, "wrk_b", None, None).unwrap()["agg"]["totalTokens"], 100.0);
}
fn event(id: &str, time: i64) -> Value {
    json!({"id":id,"workspaceID":"wrk_a","category":"inference","provider":"opencode",
    "product":"go","model":"fixture-model","startedAt":time,"inputTokens":10,"outputTokens":20,"reasoningTokens":15,
    "cacheReadTokens":30,"cacheWriteTokens":40,"cacheWrite1hTokens":12,"cost":2.5,
    "requestHeaders":{"Authorization":"secret"},"metadata":{"private":"secret"}})
}
#[test]
fn official_requests_are_minimized_without_double_counting_reasoning_or_cache_subtypes() {
    let row = parse_event(&event("req_a", 1000), "wrk_a")
        .unwrap()
        .unwrap();
    assert_eq!(
        row.input + row.output + row.cache_read + row.cache_write,
        100
    );
    assert_eq!(row.reported_cost_usd, Some(2.5));
    let encoded = serde_json::to_string(&row).unwrap();
    assert!(!encoded.contains("secret"));
    assert!(!encoded.contains("requestHeaders"));
    let mut other = event("req_b", 1000);
    other["provider"] = json!("custom");
    assert!(parse_event(&other, "wrk_a").unwrap().is_none());
    other = event("req_b", 1000);
    other["product"] = json!("byok");
    assert!(parse_event(&other, "wrk_a").unwrap().is_none());
    assert!(parse_event(&event("req_a", 1000), "wrk_other").is_err());
}
#[test]
fn pagination_retries_update_events_but_do_not_advance_checkpoint() {
    let mut db = open_path(Path::new(":memory:")).unwrap();
    let row = parse_event(&event("req_a", 1000), "wrk_a")
        .unwrap()
        .unwrap();
    save_page(&mut db, "wrk_a", &[row.clone(), row.clone()]).unwrap();
    assert_eq!(watermark(&db, "wrk_a").unwrap(), 0);
    let mut updated = row;
    updated.output = 25;
    save_page(&mut db, "wrk_a", &[updated]).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    completed(&mut db, "wrk_a", 500, 2000).unwrap();
    assert_eq!(watermark(&db, "wrk_a").unwrap(), 2000);
    let result = query_db(&db, "wrk_a", Some(500), Some(2000)).unwrap();
    assert_eq!(result["agg"]["totalTokens"], 105.0);
    assert_eq!(result["agg"]["totalActualUsd"], 0.0);
    assert_eq!(result["agg"]["totalReportedCostUsd"], 2.5);
    assert_eq!(result["agg"]["actualUnavailable"], true);
}
#[test]
fn overlapping_coverage_merges_but_offline_retention_gaps_remain_visible() {
    let mut db = open_path(Path::new(":memory:")).unwrap();
    completed(&mut db, "wrk_a", 100, 200).unwrap();
    completed(&mut db, "wrk_a", 150, 300).unwrap();
    completed(&mut db, "wrk_a", 400, 500).unwrap();
    assert_eq!(
        coverage(&db, "wrk_a").unwrap(),
        vec![(100, 300), (400, 500)]
    );
    let result = query_db(&db, "wrk_a", Some(50), Some(500)).unwrap();
    assert_eq!(result["agg"]["coverageGaps"].as_array().unwrap().len(), 2);
    assert!(query_db(&db, "wrk_a", Some(500), Some(50)).is_err());
}
#[test]
fn range_queries_are_disjoint_and_workspace_isolated() {
    let mut db = open_path(Path::new(":memory:")).unwrap();
    let one = parse_event(&event("req_a", 1000), "wrk_a")
        .unwrap()
        .unwrap();
    let two = parse_event(&event("req_b", 2000), "wrk_a")
        .unwrap()
        .unwrap();
    save_page(&mut db, "wrk_a", &[one, two]).unwrap();
    completed(&mut db, "wrk_a", 1, 3000).unwrap();
    assert_eq!(
        query_db(&db, "wrk_a", Some(1000), Some(1999)).unwrap()["agg"]["totalTokens"],
        100.0
    );
    assert_eq!(
        query_db(&db, "wrk_a", Some(2000), Some(3000)).unwrap()["agg"]["totalTokens"],
        100.0
    );
    assert_eq!(
        query_db(&db, "wrk_b", None, None).unwrap()["agg"]["totalTokens"],
        0.0
    );
}

#[tokio::test]
async fn interrupted_pagination_retries_late_records_idempotently_before_committing_watermark() {
    let mut db = open_path(Path::new(":memory:")).unwrap();
    let mut page = 0;
    let result = sync_pages(&mut db, "wrk_a", 500, 3000, |path| {
        page += 1;
        let result = if page == 1 {
            Ok(json!({"items":[event("first",1000)],"nextCursor":"next","until":3000}))
        } else {
            assert!(path.contains("cursor=next"));
            Err("opencode_network_error".into())
        };
        std::future::ready(result)
    })
    .await;
    assert!(result.is_err());
    assert_eq!(watermark(&db, "wrk_a").unwrap(), 0);
    assert!(coverage(&db, "wrk_a").unwrap().is_empty());
    page = 0;
    sync_pages(&mut db, "wrk_a", 500, 3000, |_| {
        page += 1;
        std::future::ready(Ok(if page == 1 {
            json!({"items":[event("first",1000)],"nextCursor":"next","until":3000})
        } else {
            json!({"items":[event("first",1000),event("late",1500)],"nextCursor":null,"until":3000})
        }))
    })
    .await
    .unwrap();
    assert_eq!(watermark(&db, "wrk_a").unwrap(), 3000);
    assert_eq!(
        query_db(&db, "wrk_a", None, None).unwrap()["agg"]["totalTokens"],
        200.0
    );
}
#[tokio::test]
async fn cursor_expiry_restarts_once_and_cursor_cycles_never_mark_complete() {
    let mut db = open_path(Path::new(":memory:")).unwrap();
    let mut page = 0;
    sync_pages(&mut db, "wrk_a", 100, 3000, |_| {
        page += 1;
        std::future::ready(match page {
            1 => Ok(json!({"items":[event("first",1000)],"nextCursor":"next","until":3000})),
            2 => Err("opencode_http_409".into()),
            _ => Ok(json!({"items":[event("first",1000)],"nextCursor":null,"until":3000})),
        })
    })
    .await
    .unwrap();
    assert_eq!(page, 3);
    let result = sync_pages(&mut db, "wrk_a", 200, 4000, |_| {
        std::future::ready(Ok(json!({"items":[],"nextCursor":"cycle","until":4000})))
    })
    .await;
    assert!(result.is_err());
    assert_eq!(watermark(&db, "wrk_a").unwrap(), 3000);
}
#[test]
fn initial_incremental_resync_and_offline_retention_use_correct_overlap() {
    let until = 100 * DAY;
    assert_eq!(sync_start(0, until, false), 70 * DAY);
    assert_eq!(sync_start(99 * DAY, until, false), 97 * DAY);
    assert_eq!(sync_start(99 * DAY, until, true), 70 * DAY);
    assert_eq!(sync_start(50 * DAY, until, false), 70 * DAY);
}
#[test]
fn backup_validation_rejects_private_fields_and_invalid_ranges_before_any_write() {
    let row = parse_event(&event("a", 1000), "wrk_a").unwrap().unwrap();
    let mut backup = json!({"version":1,"workspaces":[{"workspaceId":"wrk_a","events":[row],"coverage":[[500,2000]],"syncedAt":2000}]});
    validate_archives(&backup).unwrap();
    backup["workspaces"][0]["events"][0]["requestHeaders"] = json!({"secret":"forbidden"});
    assert!(validate_archives(&backup).is_err());
    backup["workspaces"][0]["events"][0]
        .as_object_mut()
        .unwrap()
        .remove("requestHeaders");
    backup["workspaces"][0]["coverage"] = json!([[2000, 500]]);
    assert!(validate_archives(&backup).is_err());
}
#[test]
fn zero_watermark_is_uncovered_and_reported_cost_follows_model_aliases() {
    let mut db = open_path(Path::new(":memory:")).unwrap();
    assert!(
        !query_db(&db, "wrk_a", Some(500), Some(2000)).unwrap()["agg"]["coverageGaps"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut row = parse_event(&event("a", 1000), "wrk_a").unwrap().unwrap();
    row.model = "gpt-5.5-high".into();
    save_page(&mut db, "wrk_a", &[row]).unwrap();
    let result = query_db(&db, "wrk_a", None, None).unwrap();
    assert_eq!(result["agg"]["models"][0]["reportedCostUsd"], 2.5);
    assert_eq!(result["agg"]["totalActualUsd"], 0.0);
}
#[test]
fn request_timestamps_follow_local_calendar_boundaries() {
    use chrono::{Local, TimeZone};
    let mut db = open_path(Path::new(":memory:")).unwrap();
    let midnight = Local
        .with_ymd_and_hms(2026, 11, 10, 0, 0, 0)
        .single()
        .unwrap()
        .timestamp_millis();
    let rows = [
        parse_event(&event("before", midnight - 1), "wrk_a")
            .unwrap()
            .unwrap(),
        parse_event(&event("after", midnight), "wrk_a")
            .unwrap()
            .unwrap(),
    ];
    save_page(&mut db, "wrk_a", &rows).unwrap();
    let result = query_db(&db, "wrk_a", Some(midnight - 1), Some(midnight)).unwrap();
    let dates: Vec<_> = result["agg"]["daily"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["date"].as_str().unwrap())
        .collect();
    assert_eq!(dates, vec!["2026-11-09", "2026-11-10"]);
    assert_eq!(result["agg"]["totalTokens"], 200.0);
}

#[test]
fn full_archive_backup_round_trip_merges_without_overwriting_newer_local_records() {
    let mut source = open_path(Path::new(":memory:")).unwrap();
    let row = parse_event(&event("a", 1000), "wrk_a").unwrap().unwrap();
    save_page(&mut source, "wrk_a", &[row.clone()]).unwrap();
    completed(&mut source, "wrk_a", 500, 2000).unwrap();
    let backup = export_db(&source).unwrap();
    let mut restored = open_path(Path::new(":memory:")).unwrap();
    for (workspace, events, ranges) in parse_archives(&backup).unwrap() {
        assert_eq!(
            restore_workspace(&mut restored, &workspace, events, ranges).unwrap(),
            1
        );
    }
    assert_eq!(export_db(&restored).unwrap(), backup);
    let mut newer = row;
    newer.output = 50;
    save_page(&mut restored, "wrk_a", &[newer]).unwrap();
    for (workspace, events, ranges) in parse_archives(&backup).unwrap() {
        assert_eq!(
            restore_workspace(&mut restored, &workspace, events, ranges).unwrap(),
            0
        );
    }
    assert_eq!(
        query_db(&restored, "wrk_a", None, None).unwrap()["agg"]["totalTokens"],
        130.0
    );
}
#[test]
fn official_summary_combines_disjoint_cache_ttls_without_reasoning_double_count() {
    let body = json!({"totalInputTokens":"10","totalOutputTokens":"20","totalCacheReadTokens":"30",
        "totalCacheWrite5mTokens":"25","totalCacheWrite1hTokens":"15","reasoningTokens":"10"});
    assert_eq!(summary_tokens(&body).unwrap(), 100);
    assert!(summary_tokens(&json!({})).is_err());
}

#[test]
fn absent_cost_stays_unknown_and_reasoning_is_archived_without_double_count() {
    let mut db = open_path(Path::new(":memory:")).unwrap();
    let mut raw = event("a", 1000);
    raw.as_object_mut().unwrap().remove("cost");
    raw["requestID"] = json!("request-a");
    let row = parse_event(&raw, "wrk_a").unwrap().unwrap();
    assert_eq!(row.reasoning, 15);
    assert_eq!(row.cache_write_1h, 12);
    assert_eq!(row.request_id.as_deref(), Some("request-a"));
    save_page(&mut db, "wrk_a", &[row]).unwrap();
    let agg = query_db(&db, "wrk_a", None, None).unwrap()["agg"].clone();
    assert!(agg["totalReportedCostUsd"].is_null());
    assert!(agg["models"][0]["reportedCostUsd"].is_null());
    assert_eq!(agg["totalTokens"], 100.0);
}

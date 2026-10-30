use super::progress::{apply, observed};
use super::*;

fn counts(events: &[ExchangeProgress]) -> (usize, usize) {
    let uploaded = events
        .iter()
        .filter_map(|e| match e {
            ExchangeProgress::Uploading { uploaded, .. } => Some(*uploaded),
            _ => None,
        })
        .last()
        .unwrap();
    let downloaded = events
        .iter()
        .filter_map(|e| match e {
            ExchangeProgress::Downloading { records, .. } => Some(*records),
            _ => None,
        })
        .last()
        .unwrap();
    (uploaded, downloaded)
}

#[test]
fn initial_incremental_and_idle_rounds_do_not_download_local_records() {
    let center = Center::filtering();
    let mut s = memory();
    for (total, expected) in [(10, (10, 0)), (20, (10, 0)), (20, (0, 0))] {
        add(&s, total);
        let (result, events) = observed(&center, &mut s, apply);
        result.unwrap();
        assert_eq!(counts(&events), expected);
        assert!(s.exclude_self_ready("p").unwrap());
        assert_eq!(s.records("p").unwrap().len(), total);
        assert_eq!(s.pending_count("p").unwrap(), 0);
    }
    assert_eq!(
        s.db.query_row("SELECT count(*) FROM remote_records", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(center.records.borrow().len(), 20);
}

#[test]
fn mixed_devices_keep_sources_and_only_download_the_other_devices_records() {
    let center = Center::filtering();
    let mut a = memory();
    let mut b = memory();
    run(&center, &mut a).unwrap();
    add(&b, 4);
    run(&center, &mut b).unwrap();
    add(&a, 10);
    let (result, events) = observed(&center, &mut a, apply);
    result.unwrap();
    assert_eq!(counts(&events), (10, 4));
    assert_eq!(a.records("p").unwrap().len(), 14);
    assert_eq!(
        store::aggregate_rows(
            a.records("p").unwrap(),
            None,
            &crate::session_scan::TimeRange::new(None, None, None)
        )
        .0
        .len(),
        10
    );
}

#[test]
fn clearing_local_cache_restores_own_history_without_reuploading_downloads() {
    let center = Center::filtering();
    let mut s = memory();
    add(&s, 10);
    run(&center, &mut s).unwrap();
    let own = s.device_id().unwrap();
    assert_eq!(s.clear_device("p", &own).unwrap(), 10);
    assert!(!s.exclude_self_ready("p").unwrap());
    assert!(s.records("p").unwrap().is_empty());
    let (result, events) = observed(&center, &mut s, apply);
    result.unwrap();
    assert_eq!(counts(&events), (0, 10));
    assert_eq!(s.records("p").unwrap().len(), 10);
    assert_eq!(s.pending_count("p").unwrap(), 0);
    assert!(s.exclude_self_ready("p").unwrap());
    let (result, events) = observed(&center, &mut s, apply);
    result.unwrap();
    assert_eq!(counts(&events), (0, 0));
}

struct TestDirectory(std::path::PathBuf);
impl Drop for TestDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn full_recovery_resumes_after_restart_and_keeps_own_remaining_pages() {
    let dir = TestDirectory(
        std::env::temp_dir().join(format!("maa-sync-mode-{}", store::random_id().unwrap())),
    );
    std::fs::create_dir(&dir.0).unwrap();
    let path = dir.0.join("usage.sqlite3");
    let center = Center::filtering();
    let mut s = store::Store::init(Connection::open(&path).unwrap()).unwrap();
    add(&s, 1203);
    run(&center, &mut s).unwrap();
    s.clear_device("p", &s.device_id().unwrap()).unwrap();
    let mut pages = 0;
    let (result, _) = observed(&center, &mut s, |s, action| {
        if matches!(&action, DbAction::Page(..)) {
            pages += 1;
            if pages == 2 {
                return Err("sync_store_failed:test".into());
            }
        }
        apply(s, action)
    });
    assert!(result.is_err());
    assert_eq!(s.records("p").unwrap().len(), 499);
    assert!(!s.exclude_self_ready("p").unwrap());
    let cursor = s.cursor("p").unwrap();
    drop(s);
    let mut s = store::Store::init(Connection::open(&path).unwrap()).unwrap();
    assert_eq!(s.cursor("p").unwrap(), cursor);
    let (result, events) = observed(&center, &mut s, apply);
    result.unwrap();
    assert_eq!(counts(&events), (0, 704));
    assert_eq!(s.records("p").unwrap().len(), 1203);
    assert!(s.exclude_self_ready("p").unwrap());
    drop(s);
}

#[test]
fn page_mode_failure_rolls_back_records_and_cursor_together() {
    let center = Center::filtering();
    let mut writer = memory();
    add(&writer, 4);
    run(&center, &mut writer).unwrap();
    let mut s = memory();
    s.db.execute_batch(
        "CREATE TRIGGER fail_mode BEFORE INSERT ON meta
        WHEN NEW.key LIKE 'download-mode:%' BEGIN SELECT RAISE(ABORT,'test'); END;",
    )
    .unwrap();
    assert!(run(&center, &mut s)
        .unwrap_err()
        .starts_with("sync_store_failed"));
    assert!(s.cursor("p").unwrap().is_none());
    assert!(!s.exclude_self_ready("p").unwrap());
    assert!(s.records("p").unwrap().is_empty());
    s.db.execute_batch("DROP TRIGGER fail_mode").unwrap();
    run(&center, &mut s).unwrap();
    assert_eq!(s.records("p").unwrap().len(), 4);
    assert!(s.exclude_self_ready("p").unwrap());
}

#[test]
fn capability_loss_resets_only_download_and_upgrade_enables_filter_again() {
    let center = Center::filtering();
    let mut s = memory();
    add(&s, 10);
    run(&center, &mut s).unwrap();
    center.info.borrow_mut().capabilities.clear();
    let (result, events) = observed(&center, &mut s, apply);
    result.unwrap();
    assert_eq!(counts(&events), (0, 10));
    assert_eq!(center.uploaded_records.get(), 10);
    assert!(!s.exclude_self_ready("p").unwrap());
    center
        .info
        .borrow_mut()
        .capabilities
        .push(EXCLUDE_DEVICE_RECORDS.into());
    let (result, events) = observed(&center, &mut s, apply);
    result.unwrap();
    assert_eq!(counts(&events), (0, 0));
    assert!(s.exclude_self_ready("p").unwrap());
}

#[test]
fn legacy_handshake_and_existing_pagination_remain_compatible() {
    let center = Center::new();
    let mut s = memory();
    add(&s, 10);
    let (result, events) = observed(&center, &mut s, apply);
    result.unwrap();
    assert_eq!(counts(&events), (10, 10));
    assert!(!s.exclude_self_ready("p").unwrap());
    center
        .info
        .borrow_mut()
        .capabilities
        .push(EXCLUDE_DEVICE_RECORDS.into());
    let (result, events) = observed(&center, &mut s, apply);
    result.unwrap();
    assert_eq!(counts(&events), (0, 0));
    assert!(s.exclude_self_ready("p").unwrap());
}

#[test]
fn upgrade_finishes_legacy_partial_page_before_excluding_self() {
    let center = Center::new();
    let mut writer = memory();
    add(&writer, 1203);
    run(&center, &mut writer).unwrap();
    let mut s = memory();
    add(&s, 10);
    *center.fail_page.borrow_mut() = true;
    assert_eq!(run(&center, &mut s).unwrap_err(), "sync_network");
    assert!(!s.exclude_self_ready("p").unwrap());
    assert!(s.cursor("p").unwrap().unwrap().starts_with("page:"));
    center
        .info
        .borrow_mut()
        .capabilities
        .push(EXCLUDE_DEVICE_RECORDS.into());
    let (result, events) = observed(&center, &mut s, apply);
    result.unwrap();
    assert_eq!(counts(&events), (0, 714));
    assert!(s.exclude_self_ready("p").unwrap());
    assert_eq!(s.records("p").unwrap().len(), 1213);
}

#[test]
fn reset_and_epoch_changes_require_full_recovery_before_filtering() {
    let center = Center::filtering();
    let mut s = memory();
    add(&s, 10);
    run(&center, &mut s).unwrap();
    s.reset_sync("p").unwrap();
    assert!(!s.exclude_self_ready("p").unwrap());
    let (result, events) = observed(&center, &mut s, apply);
    result.unwrap();
    assert_eq!(counts(&events), (10, 10));
    center.info.borrow_mut().epoch = "rebuilt".into();
    center.records.borrow_mut().clear();
    center.devices.borrow_mut().clear();
    center.changes.borrow_mut().clear();
    let (result, events) = observed(&center, &mut s, apply);
    result.unwrap();
    assert_eq!(counts(&events), (10, 0));
    assert!(s.exclude_self_ready("p").unwrap());
}

#[test]
fn http_negotiates_filtering_and_sends_optional_parameter() {
    let (transport, server) = http_once(http_ok(
        r#"{"protocolVersion":1,"spaceId":"s","epoch":"e","capabilities":["exclude-device-records-v1"]}"#,
    ));
    let h = tauri::async_runtime::block_on(transport.handshake()).unwrap();
    server.join().unwrap();
    assert_eq!(h.capabilities, [EXCLUDE_DEVICE_RECORDS]);
    let (transport, server) = http_once(http_ok(
        r#"{"spaceId":"s","epoch":"e","records":[],"devices":[],"nextCursor":"filtered","hasMore":true}"#,
    ));
    tauri::async_runtime::block_on(transport.changes(
        &h,
        Some("opaque+/?="),
        Some(&"a".repeat(32)),
    ))
    .unwrap();
    let request = server.join().unwrap();
    assert!(request.contains(&format!("excludeDeviceId={}", "a".repeat(32))));
    assert!(request.contains("cursor=opaque%2B%2F%3F%3D"));
}

#[test]
fn committed_upload_with_lost_response_retries_without_download_echo() {
    let center = Center::filtering();
    let mut s = memory();
    add(&s, 503);
    *center.lose_ack.borrow_mut() = true;
    assert_eq!(run(&center, &mut s).unwrap_err(), "sync_network");
    assert_eq!(s.pending_count("p").unwrap(), 503);
    let (result, events) = observed(&center, &mut s, apply);
    result.unwrap();
    assert_eq!(counts(&events), (503, 0));
    assert_eq!(center.records.borrow().len(), 503);
    assert_eq!(s.pending_count("p").unwrap(), 0);
}

struct EmptyContinuation(Center);
impl Transport for EmptyContinuation {
    async fn handshake(&self) -> Result<Handshake> {
        self.0.handshake().await
    }
    async fn push(&self, p: &Push) -> Result<Acknowledged> {
        self.0.push(p).await
    }
    async fn changes(
        &self,
        h: &Handshake,
        cursor: Option<&str>,
        exclude_device_id: Option<&str>,
    ) -> Result<Page> {
        Ok(Page {
            space_id: h.space_id.clone(),
            epoch: h.epoch.clone(),
            records: vec![],
            devices: vec![],
            next_cursor: if exclude_device_id.is_some() {
                cursor.unwrap().into()
            } else {
                "advanced".into()
            },
            has_more: true,
        })
    }
}

#[test]
fn empty_pages_require_filtering_and_a_new_cursor() {
    for enabled in [false, true] {
        let transport = EmptyContinuation(if enabled {
            Center::filtering()
        } else {
            Center::new()
        });
        let mut s = memory();
        s.prepare_profile("p", "space", "epoch").unwrap();
        s.apply_sync_page("p", &[], &[], "checkpoint:0", enabled)
            .unwrap();
        let own = Device {
            device_id: s.device_id().unwrap(),
            name: "test".into(),
        };
        let result =
            tauri::async_runtime::block_on(exchange_with_recovery(&transport, "p", own, |a| {
                apply_action(&mut s, "p", a)
            }));
        assert_eq!(result.unwrap_err(), "sync_protocol_invalid");
        assert_eq!(s.cursor("p").unwrap().as_deref(), Some("checkpoint:0"));
    }
}

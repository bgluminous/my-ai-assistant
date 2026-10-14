use super::*;

fn observed(
    center: &Center,
    store: &mut store::Store,
    mut apply: impl FnMut(&mut store::Store, DbAction) -> Result<DbReply>,
) -> (Result<()>, Vec<ExchangeProgress>) {
    let mut events = Vec::new();
    let device = Device {
        device_id: store.device_id().unwrap(),
        name: "progress-test".into(),
    };
    let result = tauri::async_runtime::block_on(exchange_recovery_progress(
        center,
        "p",
        device,
        |action| apply(store, action),
        |event| {
            events.push(event);
            Ok(())
        },
    ));
    (result, events)
}
fn apply(store: &mut store::Store, action: DbAction) -> Result<DbReply> {
    apply_action(store, "p", action)
}

#[test]
fn batches_report_confirmed_uploads_and_saved_pages_excluding_devices() {
    let center = Center::new();
    let mut s = memory();
    add(&s, 1203);
    let (result, events) = observed(&center, &mut s, apply);
    result.unwrap();
    for uploaded in [0, 500, 1000, 1203] {
        assert!(events.contains(&ExchangeProgress::Uploading {
            uploaded,
            total: 1203
        }));
    }
    assert_eq!(
        events.last(),
        Some(&ExchangeProgress::Downloading {
            records: 1203,
            pages: 3
        })
    );
    assert_eq!(s.pending_count("p").unwrap(), 0);
    assert!(s.success("p").unwrap().is_some());
}

#[test]
fn lost_ack_or_local_ack_failure_never_reports_uploaded_records() {
    for lose_response in [true, false] {
        let center = Center::new();
        *center.lose_ack.borrow_mut() = lose_response;
        let mut s = memory();
        add(&s, 503);
        let (result, events) = observed(&center, &mut s, |s, action| {
            if matches!(&action, DbAction::Ack(_)) {
                return Err("sync_store_failed:test".into());
            }
            apply(s, action)
        });
        assert!(result.is_err());
        assert!(!events
            .iter()
            .any(|e| matches!(e, ExchangeProgress::Uploading { uploaded, .. } if *uploaded > 0)));
        assert_eq!(s.pending_count("p").unwrap(), 503);
    }
}

#[test]
fn download_network_or_commit_failure_preserves_only_saved_page_counts() {
    for network_failure in [true, false] {
        let center = Center::new();
        *center.fail_page.borrow_mut() = network_failure;
        let mut s = memory();
        add(&s, 1003);
        let mut page = 0;
        let (result, events) = observed(&center, &mut s, |s, action| {
            if matches!(&action, DbAction::Page(_)) {
                page += 1;
                if page == 2 {
                    return Err("sync_store_failed:test".into());
                }
            }
            apply(s, action)
        });
        assert!(result.is_err());
        assert_eq!(
            events.last(),
            Some(&ExchangeProgress::Downloading {
                records: 499,
                pages: 1
            })
        );
        assert!(events.contains(&ExchangeProgress::Uploading {
            uploaded: 1003,
            total: 1003
        }));
        assert!(s.success("p").unwrap().is_none());
    }
}

#[test]
fn empty_sync_counts_no_device_as_a_record() {
    let (result, events) = observed(&Center::new(), &mut memory(), apply);
    result.unwrap();
    assert!(events.contains(&ExchangeProgress::Uploading {
        uploaded: 0,
        total: 0
    }));
    assert_eq!(
        events.last(),
        Some(&ExchangeProgress::Downloading {
            records: 0,
            pages: 1
        })
    );
}

#[test]
fn recovery_announces_restart_and_recomputes_upload_total() {
    let center = Center::new();
    let mut s = memory();
    add(&s, 503);
    run(&center, &mut s).unwrap();
    s.db.execute("UPDATE profiles SET cursor='invalid' WHERE id='p'", [])
        .unwrap();
    let (result, events) = observed(&center, &mut s, apply);
    result.unwrap();
    let restart = events
        .iter()
        .position(|e| *e == ExchangeProgress::Recovering)
        .unwrap();
    assert!(events[..restart].contains(&ExchangeProgress::Uploading {
        uploaded: 0,
        total: 0
    }));
    assert!(events[restart..].contains(&ExchangeProgress::Uploading {
        uploaded: 0,
        total: 503
    }));
    assert!(events[restart..].contains(&ExchangeProgress::Uploading {
        uploaded: 503,
        total: 503
    }));
}

#[test]
fn new_local_records_expand_this_rounds_denominator() {
    let center = Center::new();
    let mut s = memory();
    add(&s, 503);
    let (result, events) = observed(&center, &mut s, |s, action| {
        let ack = matches!(&action, DbAction::Ack(_));
        let reply = apply(s, action)?;
        if ack {
            add(s, 605);
        }
        Ok(reply)
    });
    result.unwrap();
    assert!(events.contains(&ExchangeProgress::Uploading {
        uploaded: 500,
        total: 605
    }));
    assert!(events.contains(&ExchangeProgress::Uploading {
        uploaded: 605,
        total: 605
    }));
}

#[test]
fn terminal_counts_survive_and_old_rounds_cannot_overwrite_new_progress() {
    let mut r = Runtime::default();
    let old = r.begin_progress();
    r.update_progress(old, |p| {
        p.exchange(ExchangeProgress::Uploading {
            uploaded: 500,
            total: 503,
        })
    })
    .unwrap();
    r.record_result(&Err("sync_network".into()));
    let failed = r.progress.as_ref().unwrap();
    assert_eq!(failed.phase, SyncPhase::Failed);
    assert_eq!(failed.uploaded_records, 500);
    assert!(failed.finished_at.is_some());
    let sequence = r.sequence;
    r.invalidate();
    assert!(r.progress.is_none());
    let new = r.begin_progress();
    assert!(r.sequence > sequence && new.id > old.id);
    assert_eq!(
        r.update_progress(old, |p| p.phase = SyncPhase::Completed),
        Err("sync_cancelled".into())
    );
    assert_eq!(r.progress.as_ref().unwrap().phase, SyncPhase::Waiting);
    r.update_progress(new, |p| {
        p.exchange(ExchangeProgress::Uploading {
            uploaded: 500,
            total: 503,
        });
        p.exchange(ExchangeProgress::Downloading {
            records: 499,
            pages: 1,
        });
        p.exchange(ExchangeProgress::Recovering);
    })
    .unwrap();
    let p = r.progress.as_ref().unwrap();
    assert_eq!(
        (p.uploaded_records, p.downloaded_records, p.downloaded_pages),
        (0, 0, 0)
    );
    assert_eq!(p.upload_total, None);
    r.record_result(&Ok(()));
    assert_eq!(r.progress.as_ref().unwrap().phase, SyncPhase::Completed);
    let json = serde_json::to_value(r.progress.as_ref().unwrap()).unwrap();
    assert!(json.get("roundId").is_some() && json.get("uploadedRecords").is_some());
}

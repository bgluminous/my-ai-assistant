use super::*;
const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
fn memory() -> Store {
    Store::init(Connection::open_in_memory().unwrap()).unwrap()
}
fn session(id: &str, parent: Option<&str>, time: i64) -> WireRecord {
    WireRecord::new(Record::Session {
        session_id: id.into(),
        forked_from_id: parent.map(str::to_string),
        created_at: Some(time),
    })
}
fn event(id: &str, time: i64, n: u64) -> WireRecord {
    WireRecord::new(Record::Usage {
        session_id: id.into(),
        timestamp_ms: Some(time),
        model: "gpt-test".into(),
        input_tokens: n,
        cache_read_tokens: 0,
        output_tokens: 1,
        total_usage: Some([n, 0, 1, 0, n + 1]),
        occurrence: 0,
    })
}
fn rows(items: Vec<(&str, WireRecord)>, device: Option<&str>, since: Option<i64>) -> Vec<TokenRow> {
    aggregate_rows(
        items.into_iter().map(|(d, r)| (d.into(), r)).collect(),
        device,
        &TimeRange::new(None, since, None),
    )
    .0
}
#[test]
fn copied_logs_and_forks_across_devices_count_once_before_filter() {
    let records = vec![
        (A, session("p", None, 0)),
        (A, event("p", 10, 100)),
        (B, session("c", Some("p"), 1000)),
        (B, event("c", 1000, 100)),
        (B, event("c", 2000, 150)),
        (B, event("p", 10, 100)),
    ];
    assert_eq!(rows(records.clone(), None, None).len(), 2);
    assert_eq!(rows(records.clone(), Some(B), Some(500)).len(), 1);
    assert_eq!(rows(records, Some(A), Some(500)).len(), 0);
}
#[test]
fn late_ancestor_corrects_lineage_and_timestamp() {
    let mut records = vec![
        (B, session("c", Some("p"), 1000)),
        (B, event("c", 1000, 100)),
    ];
    assert_eq!(rows(records.clone(), None, Some(500)).len(), 1);
    records.extend([
        (A, session("p", Some("root"), 0)),
        (A, event("p", 10, 100)),
        (A, session("root", None, 0)),
    ]);
    assert!(rows(records, None, Some(500)).is_empty());
}
#[test]
fn unrelated_sessions_and_counterless_occurrences_remain_distinct() {
    let one = WireRecord::new(Record::Usage {
        session_id: "old".into(),
        timestamp_ms: None,
        model: "unknown".into(),
        input_tokens: 1,
        cache_read_tokens: 0,
        output_tokens: 0,
        total_usage: None,
        occurrence: 0,
    });
    let mut two = one.record.clone();
    if let Record::Usage { occurrence, .. } = &mut two {
        *occurrence = 1;
    }
    assert_eq!(
        rows(
            vec![
                (A, event("a", 1, 100)),
                (B, event("b", 1, 100)),
                (A, one.clone()),
                (B, one),
                (A, WireRecord::new(two))
            ],
            None,
            None
        )
        .len(),
        4
    );
}
#[test]
fn page_validation_is_atomic_and_cursor_is_not_advanced() {
    let mut s = memory();
    s.prepare_profile("p", "space", "epoch").unwrap();
    let good = RemoteRecord {
        device_id: B.into(),
        item: event("a", 1, 100),
    };
    let mut bad = good.clone();
    bad.item.record_id = "bad".into();
    assert!(s.apply_page("p", &[good, bad], &[], "next").is_err());
    assert!(s.records("p").unwrap().is_empty());
    assert_eq!(s.cursor("p").unwrap(), None);
}
#[test]
fn sql_failure_rolls_back_records_and_cursor() {
    let mut s = memory();
    s.prepare_profile("p", "s", "e").unwrap();
    s.db.execute_batch("CREATE TRIGGER fail_cursor BEFORE UPDATE OF cursor ON profiles BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
    assert!(s
        .apply_page(
            "p",
            &[RemoteRecord {
                device_id: B.into(),
                item: event("a", 1, 10)
            }],
            &[],
            "next"
        )
        .is_err());
    assert!(s.records("p").unwrap().is_empty());
    assert_eq!(s.cursor("p").unwrap(), None);
}
#[test]
fn profiles_are_isolated_and_remote_records_never_become_pending() {
    let mut s = memory();
    s.prepare_profile("p", "s", "e").unwrap();
    s.apply_page(
        "p",
        &[RemoteRecord {
            device_id: B.into(),
            item: event("a", 1, 10),
        }],
        &[],
        "c",
    )
    .unwrap();
    assert_eq!(s.records("p").unwrap().len(), 1);
    assert!(s.records("other").unwrap().is_empty());
    assert_eq!(s.pending_count("p").unwrap(), 0);
    assert_eq!(s.pending_count("other").unwrap(), 0);
}
#[test]
fn clear_preserves_unacknowledged_local_usage_and_resets_download() {
    let mut s = memory();
    let own = s.device_id().unwrap();
    s.prepare_profile("p", "s", "e").unwrap();
    let a = event("a", 1, 10);
    let b = event("b", 2, 20);
    for r in [&a, &b] {
        s.db.execute(
            "INSERT INTO local_records VALUES (?1,?2)",
            params![r.record_id, serde_json::to_string(&r.record).unwrap()],
        )
        .unwrap();
    }
    s.acknowledge("p", &[a.clone()]).unwrap();
    s.apply_page(
        "p",
        &[RemoteRecord {
            device_id: own.clone(),
            item: a,
        }],
        &[],
        "c",
    )
    .unwrap();
    s.clear_device("p", &own).unwrap();
    assert_eq!(s.records("p").unwrap().len(), 1);
    assert_eq!(s.pending_count("p").unwrap(), 1);
    assert_eq!(s.cursor("p").unwrap(), None);
    assert_eq!(s.records("p").unwrap()[0].1.record_id, b.record_id);
}
#[test]
fn epoch_reset_requeues_local_history_but_preserves_remote_history() {
    let mut s = memory();
    s.prepare_profile("p", "s", "e").unwrap();
    let a = event("a", 1, 10);
    s.db.execute(
        "INSERT INTO local_records VALUES (?1,?2)",
        params![a.record_id, serde_json::to_string(&a.record).unwrap()],
    )
    .unwrap();
    s.acknowledge("p", &[a.clone()]).unwrap();
    s.apply_page(
        "p",
        &[RemoteRecord {
            device_id: B.into(),
            item: a,
        }],
        &[],
        "c",
    )
    .unwrap();
    s.prepare_profile("p", "s", "new").unwrap();
    assert_eq!(s.pending_count("p").unwrap(), 1);
    assert_eq!(s.records("p").unwrap().len(), 2);
    s.prepare_profile("p", "different-space", "new").unwrap();
    assert_eq!(s.records("p").unwrap().len(), 1);
}
#[test]
fn wire_roundtrip_and_hash_tampering() {
    let r = RemoteRecord {
        device_id: A.into(),
        item: event("session", 100, 42),
    };
    let json = serde_json::to_string(&r).unwrap();
    let read: RemoteRecord = serde_json::from_str(&json).unwrap();
    read.item.validate().unwrap();
    assert!(!json.contains("path"));
    assert!(!json.contains("tokenSecret"));
    let mut bad = read.item;
    if let Record::Usage { input_tokens, .. } = &mut bad.record {
        *input_tokens += 1;
    }
    assert!(bad.validate().is_err());
}

#[test]
fn canonical_hash_matches_protocol_cross_language_vectors() {
    assert_eq!(
        session("thread-1", None, 1791504000000).record_id,
        "fd3127c801e7b05afb5d900c6f091806b8e01e5f64a5b3ce70fe22fba89d5f67"
    );
    let r = Record::Usage {
        session_id: "会话/\"1".into(),
        timestamp_ms: None,
        model: "模型\\A".into(),
        input_tokens: 1,
        cache_read_tokens: 0,
        output_tokens: 0,
        total_usage: None,
        occurrence: 2,
    };
    assert_eq!(
        r.id(),
        "8301484be9dc31c5b241658355f3e68a5efde92ac2e716a158783cf1974dcb22"
    );
}
#[test]
fn malformed_lineage_cycle_is_deterministic() {
    let mut data = vec![
        (A, session("a", Some("b"), 0)),
        (B, session("b", Some("a"), 0)),
        (A, event("a", 1, 1)),
        (B, event("b", 1, 1)),
    ];
    let first = rows(data.clone(), None, None);
    data.reverse();
    let second = rows(data, None, None);
    assert_eq!(first.len(), second.len());
    assert_eq!(first[0].timestamp_ms, second[0].timestamp_ms);
}

#[test]
fn deep_lineages_and_cycle_descendants_share_the_same_root() {
    let mut data = Vec::new();
    for i in 0..150 {
        data.push((
            A.to_string(),
            session(
                &format!("s{i}"),
                (i > 0).then(|| format!("s{}", i - 1)).as_deref(),
                i,
            ),
        ));
        data.push((A.to_string(), event(&format!("s{i}"), i, 1)));
    }
    let range = TimeRange::new(None, None, None);
    assert_eq!(aggregate_rows(data, None, &range).0.len(), 1);
    assert_eq!(
        rows(
            vec![
                (A, session("x", Some("y"), 0)),
                (A, session("y", Some("x"), 0)),
                (B, session("a", Some("x"), 1)),
                (A, event("x", 1, 1)),
                (B, event("a", 2, 1)),
            ],
            None,
            None
        )
        .len(),
        1
    );
}

struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("maa-codex-store-test-{}", random_id().unwrap()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn real_logs_are_private_idempotent_and_survive_deletion_and_restart() {
    let temp = TempDir::new();
    let logs = temp.0.join("sessions");
    std::fs::create_dir(&logs).unwrap();
    let event = json!({"timestamp":"2026-01-01T01:00:00Z", "type":"event_msg", "payload":{
        "type":"token_count", "info":{"last_token_usage":{"input_tokens":12,"cached_input_tokens":2,"output_tokens":3}}
    }}).to_string();
    let text = [
        json!({"type":"session_meta","timestamp":"2026-01-01T00:00:00Z","payload":{"id":"shared-session","cwd":"PRIVATE_PATH"}}).to_string(),
        json!({"type":"turn_context","payload":{"model":"original-model-alias"}}).to_string(),
        json!({"type":"response_item","payload":{"content":"PRIVATE_BODY","token":"PRIVATE_CREDENTIAL"}}).to_string(),
        event.clone(), event,
    ].join("\n");
    std::fs::write(logs.join("original.jsonl"), &text).unwrap();
    std::fs::write(logs.join("copy.jsonl"), &text).unwrap();
    let db_path = temp.0.join("usage.sqlite3");
    let mut s = Store::init(Connection::open(&db_path).unwrap()).unwrap();
    let device = s.device_id().unwrap();
    let collect = || crate::codex::collect_sessions(Some(temp.0.to_string_lossy().into_owned())).0;
    assert_eq!(s.ingest(collect()).unwrap(), 3); // 一个会话，两条无累计值的重复明细。
    assert_eq!(s.ingest(collect()).unwrap(), 0);
    let records = s.pending("p").unwrap();
    let wire = serde_json::to_string(&records).unwrap();
    assert!(!wire.contains("PRIVATE_"));
    assert!(wire.contains("original-model-alias"));
    s.prepare_profile("p", "space", "epoch").unwrap();
    s.acknowledge("p", &records).unwrap();
    s.apply_page("p", &[], &[], "opaque-checkpoint").unwrap();
    std::fs::remove_dir_all(&logs).unwrap();
    assert!(collect().is_empty());
    drop(s);
    let mut reopened = Store::init(Connection::open(&db_path).unwrap()).unwrap();
    assert_eq!(reopened.device_id().unwrap(), device);
    assert_eq!(reopened.pending_count("p").unwrap(), 0);
    assert_eq!(
        reopened.cursor("p").unwrap().as_deref(),
        Some("opaque-checkpoint")
    );
    let (rows, _) = aggregate_rows(
        reopened.records("p").unwrap(),
        None,
        &TimeRange::new(None, None, None),
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.iter().map(|r| r.input).sum::<f64>(), 20.0);
    reopened.reset_sync("p").unwrap();
    assert_eq!(reopened.pending_count("p").unwrap(), 3);
    assert_eq!(reopened.cursor("p").unwrap(), None);
}

#[test]
fn legacy_file_identity_persists_but_is_not_shared_between_devices() {
    let legacy = ParsedFile {
        rows: vec![crate::codex::CachedRow {
            ts: None,
            key: None,
            row: TokenRow {
                model: "unknown".into(),
                input: 1.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                actual_cents: 0.0,
                timestamp_ms: None,
            },
        }],
        ..Default::default()
    };
    let mut a = memory();
    let mut b = memory();
    let files = || vec![("local-only-path".into(), legacy.clone())];
    assert_eq!(a.ingest(files()).unwrap(), 2);
    assert_eq!(a.ingest(files()).unwrap(), 0);
    b.ingest(files()).unwrap();
    assert_ne!(
        a.pending("p").unwrap()[0].record_id,
        b.pending("p").unwrap()[0].record_id
    );
}

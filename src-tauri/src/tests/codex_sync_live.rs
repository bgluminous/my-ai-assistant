//! Opt-in live server integration. The launcher owns TLS, isolated MySQL tables and all cleanup.
use super::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn trusted_client(ca: &[u8]) -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(ca).unwrap())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(40))
        .build()
        .unwrap()
}

struct Client {
    cfg: SyncConfig,
    http: HttpTransport,
    store: store::Store,
    db_path: PathBuf,
    ca: Vec<u8>,
}
impl Client {
    fn new(root: &Path, name: &str, base: &str, key: &str, ca: &[u8]) -> Self {
        let home = root.join(name);
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        let cfg = SyncConfig {
            server_url: base.into(),
            sealed_key: local_crypto::seal(key).unwrap(),
            source_home: home.to_string_lossy().into_owned(),
            device_name: name.into(),
            ..Default::default()
        };
        let mut http = HttpTransport::new(&cfg).unwrap();
        // Only this ignored test extends trust; production URL/redirect validation stays active.
        http.client = trusted_client(ca);
        let db_path = home.join("usage.sqlite3");
        let store = store::Store::init(Connection::open(&db_path).unwrap()).unwrap();
        Self {
            cfg,
            http,
            store,
            db_path,
            ca: ca.to_vec(),
        }
    }
    fn profile(&self) -> String {
        self.cfg.profile().unwrap()
    }
    fn own(&self) -> Device {
        Device {
            device_id: self.store.device_id().unwrap(),
            name: self.cfg.device_name.clone(),
        }
    }
    fn collect(&mut self) {
        let files = codex::collect_sessions(Some(self.cfg.source_home.clone())).0;
        self.store.ingest(files).unwrap();
    }
    async fn sync(&mut self) -> Result<()> {
        self.sync_counts().await.map(|_| ())
    }
    async fn sync_counts(&mut self) -> Result<(usize, usize)> {
        self.collect();
        let profile = self.profile();
        let own = self.own();
        let mut counts = (0, 0);
        exchange_recovery_progress(
            &self.http,
            &profile,
            own,
            |a| apply_action(&mut self.store, &profile, a),
            |event| {
                match event {
                    ExchangeProgress::Uploading { uploaded, .. } => counts.0 = uploaded,
                    ExchangeProgress::Downloading { records, .. } => counts.1 = records,
                    ExchangeProgress::Recovering => counts = (0, 0),
                    _ => {}
                }
                Ok(())
            },
        )
        .await?;
        Ok(counts)
    }
    fn rows(&self, device: Option<&str>, since: Option<i64>) -> Vec<pricing::TokenRow> {
        store::aggregate_rows(
            self.store.records(&self.profile()).unwrap(),
            device,
            &TimeRange::new(None, since, None),
        )
        .0
    }
    fn key(&mut self, key: &str) {
        self.cfg.sealed_key = local_crypto::seal(key).unwrap();
        self.http = HttpTransport::new(&self.cfg).unwrap();
        self.http.client = trusted_client(&self.ca);
    }
    fn reopen(&mut self) {
        let replacement = memory();
        drop(std::mem::replace(&mut self.store, replacement));
        self.store = store::Store::init(Connection::open(&self.db_path).unwrap()).unwrap();
    }
}

fn log_text(id: &str, parent: Option<&str>, timestamp: &str) -> String {
    [
        json!({"type":"session_meta","timestamp":timestamp,"payload":{
            "id":id,"forked_from_id":parent,"cwd":"PRIVATE_PATH"}}),
        json!({"type":"turn_context","payload":{"model":"原始模型/Ω"}}),
        json!({"type":"response_item","payload":{"content":"PRIVATE_BODY","token":"PRIVATE_CREDENTIAL"}}),
        json!({"timestamp":timestamp,"type":"event_msg","payload":{"type":"token_count","info":{
            "last_token_usage":{"input_tokens":12,"cached_input_tokens":2,"output_tokens":3},
            "total_token_usage":{"input_tokens":12,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":0,"total_tokens":15}
        }}}),
    ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n")
}
fn write_log(client: &Client, filename: &str, text: &str) {
    std::fs::write(
        Path::new(&client.cfg.source_home)
            .join("sessions")
            .join(filename),
        text,
    )
    .unwrap();
}

struct Harness {
    http: reqwest::Client,
    base: String,
    control_key: String,
}
impl Harness {
    async fn admin(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Value {
        let mut request = self
            .http
            .request(
                method,
                format!("{}/admin/api/codex-usage/{path}", self.base),
            )
            .header("X-Auth-Token", "admin-test-key");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let json: Value = response.json().await.unwrap();
        assert!(json["code"] == 0, "admin operation failed");
        json["data"].clone()
    }
    async fn account(&self, id: &str) -> Value {
        self.admin(reqwest::Method::GET, &format!("accounts/{id}"), None)
            .await
    }
    async fn create(&self, name: &str) -> Value {
        self.admin(
            reqwest::Method::POST,
            "accounts",
            Some(json!({"name":name,"remark":null,"enabled":true})),
        )
        .await
    }
    async fn control(&self, body: Value) {
        let origin = self.base.strip_suffix("/maa").unwrap();
        let response = self
            .http
            .post(format!("{origin}/_test/control"))
            .bearer_auth(&self.control_key)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::OK,
            "test control failed"
        );
    }
}

struct DuringPage<'a> {
    reader: &'a HttpTransport,
    writer: &'a HttpTransport,
    device: Device,
    late: WireRecord,
    inserted: Cell<bool>,
}
impl Transport for DuringPage<'_> {
    async fn handshake(&self) -> Result<Handshake> {
        self.reader.handshake().await
    }
    async fn push(&self, p: &Push) -> Result<Acknowledged> {
        self.reader.push(p).await
    }
    async fn changes(
        &self,
        h: &Handshake,
        cursor: Option<&str>,
        exclude_device_id: Option<&str>,
    ) -> Result<Page> {
        let page = self.reader.changes(h, cursor, exclude_device_id).await?;
        if page.has_more && !self.inserted.replace(true) {
            let ack = self
                .writer
                .push(&Push {
                    protocol_version: 1,
                    space_id: h.space_id.clone(),
                    epoch: h.epoch.clone(),
                    device: self.device.clone(),
                    records: vec![self.late.clone()],
                })
                .await?;
            assert_eq!(ack.acknowledged_ids, vec![self.late.record_id.clone()]);
        }
        Ok(page)
    }
}

#[test]
#[ignore = "Run scripts/codex-sync-tests/server-integration.mjs with an isolated development database"]
fn live_server_interoperability() {
    tauri::async_runtime::block_on(async {
        let base = std::env::var("MAA_TEST_SERVER_URL").unwrap();
        let root = PathBuf::from(std::env::var("MAA_TEST_ROOT").unwrap());
        assert!(root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("maa-server-integration-"));
        let ca = std::fs::read(std::env::var("MAA_TEST_CA").unwrap()).unwrap();
        let harness = Harness {
            http: trusted_client(&ca),
            base: base.clone(),
            control_key: std::env::var("MAA_TEST_CONTROL_KEY").unwrap(),
        };
        let created = harness.create("MAA 双机联调").await;
        let filter_account = harness.create("MAA 过滤联调").await;
        let filter_key = filter_account["key"].as_str().unwrap();
        let mut fa = Client::new(&root, "FilterA", &base, filter_key, &ca);
        let mut fb = Client::new(&root, "FilterB", &base, filter_key, &ca);
        add(&fa.store, 10);
        assert_eq!(fa.sync_counts().await.unwrap(), (10, 0));
        assert_eq!(fa.sync_counts().await.unwrap(), (0, 0));
        add(&fb.store, 4);
        assert_eq!(fb.sync_counts().await.unwrap(), (4, 10));
        add(&fa.store, 20);
        assert_eq!(fa.sync_counts().await.unwrap(), (10, 4));
        harness
            .control(json!({"action":"legacy-mode","enabled":true}))
            .await;
        assert_eq!(fa.sync_counts().await.unwrap(), (0, 24));
        assert!(!fa.store.exclude_self_ready(&fa.profile()).unwrap());
        harness
            .control(json!({"action":"legacy-mode","enabled":false}))
            .await;
        assert_eq!(fa.sync_counts().await.unwrap(), (0, 0));
        assert!(fa.store.exclude_self_ready(&fa.profile()).unwrap());
        fa.store
            .clear_device(&fa.profile(), &fa.own().device_id)
            .unwrap();
        assert_eq!(fa.sync_counts().await.unwrap(), (0, 24));
        assert_eq!(fa.store.pending_count(&fa.profile()).unwrap(), 0);
        println!("PASS: first upload 10/download 0, mixed upload 10/download 4, legacy fallback, upgrade and own-cache restore");
        let account = created["account"]["id"].as_str().unwrap();
        let key = created["key"].as_str().unwrap();
        assert_eq!(key.len(), 43);
        let mut a = Client::new(&root, "A", &base, key, &ca);
        let mut b = Client::new(&root, "B", &base, key, &ca);
        let mut reader = Client::new(&root, "Reader", &base, key, &ca);
        assert_ne!(a.own().device_id, b.own().device_id);
        assert!(HttpTransport::new(&SyncConfig::default()).is_err());
        assert_eq!(
            HttpTransport::new(&a.cfg)
                .unwrap()
                .handshake()
                .await
                .unwrap_err(),
            "sync_network"
        );
        a.http.handshake().await.unwrap().validate().unwrap();
        assert_eq!(harness.account(account).await["deviceCount"], 0);
        reader.sync().await.unwrap();
        assert!(reader.rows(None, None).is_empty());
        assert_eq!(harness.account(account).await["deviceCount"], 1);
        println!("PASS: HTTPS trust and /maa prefix; handshake does not register; empty client registers and downloads");

        let child = log_text("child", Some("parent"), "2026-01-02T01:00:00Z");
        write_log(&b, "child.jsonl", &child);
        b.sync().await.unwrap();
        reader.sync().await.unwrap();
        let cutoff = 1_767_312_000_000; // 2026-01-02T00:00:00Z
        assert_eq!(reader.rows(None, Some(cutoff)).len(), 1);
        let parent = log_text("parent", None, "2026-01-01T01:00:00Z");
        write_log(&a, "parent.jsonl", &parent);
        a.sync().await.unwrap();
        write_log(&b, "copy.jsonl", &parent);
        b.sync().await.unwrap();
        reader.sync().await.unwrap();
        a.sync().await.unwrap();
        for client in [&a, &reader] {
            let rows = client.rows(None, None);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].input, 10.0);
            assert_eq!(rows[0].cache_read, 2.0);
            assert_eq!(rows[0].output, 3.0);
            assert!(client.rows(None, Some(cutoff)).is_empty());
        }
        assert_eq!(reader.rows(Some(&a.own().device_id), None).len(), 1);
        assert_eq!(reader.rows(Some(&b.own().device_id), None).len(), 1);
        println!("PASS: copied logs, cross-device forks and late ancestors deduplicate before date/device filtering");

        add(&a.store, 503);
        harness
            .control(json!({"action":"drop-push-response"}))
            .await;
        assert_eq!(a.sync().await.unwrap_err(), "sync_network");
        assert_eq!(a.store.pending_count(&a.profile()).unwrap(), 503);
        a.sync().await.unwrap();
        let before = harness.account(account).await;
        a.sync().await.unwrap();
        let after = harness.account(account).await;
        assert_eq!(before["recordCount"], after["recordCount"]);
        assert_eq!(before["sourceCount"], after["sourceCount"]);
        assert_eq!(a.store.pending_count(&a.profile()).unwrap(), 0);
        assert_eq!(a.rows(None, None).len(), 504);
        println!("PASS: >500-record upload batches; lost committed response retries without duplicate records or sources");

        reader.store.reset_sync(&reader.profile()).unwrap();
        harness.control(json!({"action":"drop-second-page"})).await;
        assert_eq!(reader.sync().await.unwrap_err(), "sync_network");
        assert!(reader.store.cursor(&reader.profile()).unwrap().is_some());
        let id = reader.own().device_id;
        reader.reopen();
        assert_eq!(reader.own().device_id, id);
        harness.control(json!({"action":"restart"})).await;
        reader.sync().await.unwrap();
        assert_eq!(reader.rows(None, None).len(), 504);
        assert_eq!(reader.store.pending_count(&reader.profile()).unwrap(), 0);
        println!(
            "PASS: interrupted pagination resumes after reopening SQLite and restarting the server"
        );

        let late = WireRecord::new(store::Record::Usage {
            session_id: "late-event".into(),
            timestamp_ms: None,
            model: "gpt-test".into(),
            input_tokens: 5,
            cache_read_tokens: 0,
            output_tokens: 1,
            total_usage: None,
            occurrence: 0,
        });
        b.store
            .db
            .execute(
                "INSERT INTO local_records VALUES (?1,?2)",
                params![late.record_id, serde_json::to_string(&late.record).unwrap()],
            )
            .unwrap();
        let mut snapshot = Client::new(&root, "Snapshot", &base, key, &ca);
        let transport = DuringPage {
            reader: &snapshot.http,
            writer: &b.http,
            device: b.own(),
            late,
            inserted: Cell::new(false),
        };
        let profile = snapshot.profile();
        let identity = transport.handshake().await.unwrap();
        apply_action(
            &mut snapshot.store,
            &profile,
            DbAction::Prepare(identity.clone()),
        )
        .unwrap();
        // 核对单个固定上界窗口；正常首次同步随后还会开始上传后的增量窗口。
        download_pages(
            &transport,
            &identity,
            None,
            true,
            &mut |action| apply_action(&mut snapshot.store, &profile, action),
            &mut |_| Ok(()),
            &mut (0, 0),
        )
        .await
        .unwrap();
        assert!(transport.inserted.get());
        assert_eq!(snapshot.rows(None, None).len(), 504);
        snapshot.sync().await.unwrap();
        assert_eq!(snapshot.rows(None, None).len(), 505);
        println!("PASS: concurrent new upload stays outside the active page bound and appears in the next increment");

        b.cfg.device_name = "重命名的设备 B".into();
        b.sync().await.unwrap();
        a.sync().await.unwrap();
        assert!(a
            .store
            .devices(&a.profile(), "A")
            .unwrap()
            .iter()
            .any(|d| d.device_id == b.own().device_id && d.name == b.cfg.device_name));
        assert!(
            a.store
                .clear_device(&a.profile(), &b.own().device_id)
                .unwrap()
                > 0
        );
        a.sync().await.unwrap();
        assert_eq!(a.rows(None, None).len(), 505);
        std::fs::remove_file(Path::new(&a.cfg.source_home).join("sessions/parent.jsonl")).unwrap();
        a.reopen();
        a.sync().await.unwrap();
        assert_eq!(a.rows(None, None).len(), 505);
        println!("PASS: device rename, per-device cache restore and history retained after deleting source logs");

        let before = harness.account(account).await;
        harness
            .admin(
                reqwest::Method::PUT,
                &format!("accounts/{account}"),
                Some(json!({"name":"MAA 双机联调","remark":null,"enabled":false})),
            )
            .await;
        let result = a.sync().await;
        assert_eq!(result.as_ref().unwrap_err(), "sync_forbidden");
        let mut state = Runtime::default();
        state.record_result(&result);
        assert!(state.paused);
        assert_eq!(a.rows(None, None).len(), 505);
        harness
            .admin(
                reqwest::Method::PUT,
                &format!("accounts/{account}"),
                Some(json!({"name":"MAA 双机联调","remark":null,"enabled":true})),
            )
            .await;
        let result = a.sync().await;
        result.as_ref().unwrap();
        state.record_result(&result);
        assert!(!state.paused);
        let identity = a.http.handshake().await.unwrap();
        let reset = harness
            .admin(
                reqwest::Method::POST,
                &format!("accounts/{account}/reset-key"),
                None,
            )
            .await;
        let new_key = reset["key"].as_str().unwrap();
        assert_eq!(b.sync().await.unwrap_err(), "sync_unauthorized");
        let old_profile = a.profile();
        a.key(new_key);
        b.key(new_key);
        assert_ne!(a.profile(), old_profile);
        assert!(a.store.cursor(&a.profile()).unwrap().is_none());
        assert!(a.store.pending_count(&a.profile()).unwrap() > 0);
        a.sync().await.unwrap();
        b.sync().await.unwrap();
        let new_identity = a.http.handshake().await.unwrap();
        assert_eq!(new_identity.space_id, identity.space_id);
        assert_eq!(new_identity.epoch, identity.epoch);
        let after = harness.account(account).await;
        assert_eq!(before["recordCount"], after["recordCount"]);
        assert_eq!(before["sourceCount"], after["sourceCount"]);
        println!("PASS: real admin disable/enable and key reset produce 403/401; new key preserves space, epoch and history");

        let other = harness.create("隔离账户").await;
        let other_key = other["key"].as_str().unwrap();
        reader.key(other_key);
        assert!(reader.rows(None, None).is_empty());
        reader.sync().await.unwrap();
        assert!(reader.rows(None, None).is_empty());
        a.key(other_key);
        a.sync().await.unwrap();
        let other_stats = harness
            .account(other["account"]["id"].as_str().unwrap())
            .await;
        assert_eq!(other_stats["sourceCount"], 505); // A's parent session/event and 503 local events only.
        assert_eq!(other_stats["recordCount"], 505);
        assert_eq!(a.rows(None, None).len(), 504); // B's late event cannot be forwarded.
        a.key(new_key);
        a.sync().await.unwrap();
        assert_eq!(a.rows(None, None).len(), 505);
        println!("PASS: switching accounts isolates caches and never uploads records downloaded from another device");

        harness
            .control(json!({"action":"epoch","accountId":account}))
            .await;
        a.sync().await.unwrap();
        assert_ne!(a.http.handshake().await.unwrap().epoch, identity.epoch);
        assert_eq!(a.rows(None, None).len(), 505);
        a.store
            .db
            .execute(
                "UPDATE profiles SET cursor='invalid' WHERE id=?1",
                [&a.profile()],
            )
            .unwrap();
        a.sync().await.unwrap();
        assert_eq!(a.rows(None, None).len(), 505);
        assert_eq!(a.store.pending_count(&a.profile()).unwrap(), 0);
        println!("PASS: server epoch change and real 409 invalid-cursor recovery re-confirm local history");

        let mut failed = Client::new(&root, "FailedWrite", &base, new_key, &ca);
        add(&failed.store, 1);
        let before = harness.account(account).await;
        harness
            .control(json!({"action":"fail-write","deviceId":failed.own().device_id}))
            .await;
        assert_eq!(failed.sync().await.unwrap_err(), "sync_server");
        assert_eq!(failed.store.pending_count(&failed.profile()).unwrap(), 1);
        // 上传前已提交完整恢复，上传失败不能撤销下载检查点或模式。
        assert!(failed.store.cursor(&failed.profile()).unwrap().is_some());
        assert!(failed.store.exclude_self_ready(&failed.profile()).unwrap());
        let after = harness.account(account).await;
        for field in ["recordCount", "sourceCount", "deviceCount"] {
            assert_eq!(before[field], after[field]);
        }
        harness.control(json!({"action":"restore-write"})).await;
        assert_eq!(failed.sync_counts().await.unwrap(), (1, 0));
        assert_eq!(failed.store.pending_count(&failed.profile()).unwrap(), 0);
        println!("PASS: actual MySQL write failure rolls back device and source registration; retry succeeds");

        let mut local_failure = Client::new(&root, "LocalFailure", &base, new_key, &ca);
        local_failure.store.db.execute_batch("CREATE TRIGGER fail_page BEFORE INSERT ON remote_records BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
        assert!(local_failure
            .sync()
            .await
            .unwrap_err()
            .starts_with("sync_store_failed"));
        assert!(local_failure
            .store
            .cursor(&local_failure.profile())
            .unwrap()
            .is_none());
        assert!(local_failure.rows(None, None).is_empty());
        assert!(!local_failure
            .store
            .exclude_self_ready(&local_failure.profile())
            .unwrap());
        local_failure
            .store
            .db
            .execute_batch("DROP TRIGGER fail_page")
            .unwrap();
        local_failure.sync().await.unwrap();
        assert_eq!(local_failure.rows(None, None).len(), 505);
        println!("PASS: local SQLite failure commits neither page nor cursor, then downloads the page again");

        let audits = harness
            .admin(
                reqwest::Method::GET,
                &format!("audits?accountId={account}&size=100"),
                None,
            )
            .await;
        let text = audits.to_string();
        for secret in [
            key,
            new_key,
            "PRIVATE_BODY",
            "PRIVATE_PATH",
            "PRIVATE_CREDENTIAL",
        ] {
            assert!(!text.contains(secret), "audit leaked test input");
        }
        assert!(audits["total"].as_u64().unwrap() > 0);
        println!("PASS: administration audit exposes counts and outcomes without keys or private log payloads");
    });
}

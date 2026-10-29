use super::*;
use rusqlite::{params, Connection};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
#[path = "codex_sync_live.rs"]
mod live;
#[path = "codex_sync_progress.rs"]
mod progress;
struct Center {
    info: RefCell<Handshake>,
    records: RefCell<BTreeMap<(String, String), RemoteRecord>>,
    devices: RefCell<BTreeMap<String, Device>>,
    changes: RefCell<Vec<Change>>,
    lose_ack: RefCell<bool>,
    fail_page: RefCell<bool>,
    reject_changes: Cell<bool>,
    handshakes: Cell<usize>,
    uploaded_records: Cell<usize>,
}
#[derive(Clone)]
enum Change {
    Record(RemoteRecord),
    Device(Device),
}
impl Center {
    fn new() -> Self {
        Self {
            info: RefCell::new(Handshake {
                protocol_version: 1,
                space_id: "space".into(),
                epoch: "epoch".into(),
            }),
            records: RefCell::new(BTreeMap::new()),
            devices: RefCell::new(BTreeMap::new()),
            changes: RefCell::new(Vec::new()),
            lose_ack: RefCell::new(false),
            fail_page: RefCell::new(false),
            reject_changes: Cell::new(false),
            handshakes: Cell::new(0),
            uploaded_records: Cell::new(0),
        }
    }
}
impl Transport for Center {
    async fn handshake(&self) -> Result<Handshake> {
        self.handshakes.set(self.handshakes.get() + 1);
        Ok(self.info.borrow().clone())
    }
    async fn push(&self, p: &Push) -> Result<Acknowledged> {
        assert!(serde_json::to_vec(p).unwrap().len() <= MAX_BYTES);
        assert!(p.records.len() <= MAX_RECORDS);
        let info = self.info.borrow();
        if p.space_id != info.space_id || p.epoch != info.epoch {
            return Err("sync_reset_required".into());
        }
        self.uploaded_records
            .set(self.uploaded_records.get() + p.records.len());
        let changed = self
            .devices
            .borrow()
            .get(&p.device.device_id)
            .is_none_or(|old| old.name != p.device.name);
        if changed {
            self.devices
                .borrow_mut()
                .insert(p.device.device_id.clone(), p.device.clone());
            self.changes
                .borrow_mut()
                .push(Change::Device(p.device.clone()));
        }
        for item in &p.records {
            let r = RemoteRecord {
                device_id: p.device.device_id.clone(),
                item: item.clone(),
            };
            let key = (r.device_id.clone(), item.record_id.clone());
            if !self.records.borrow().contains_key(&key) {
                self.records.borrow_mut().insert(key, r.clone());
                self.changes.borrow_mut().push(Change::Record(r));
            }
        }
        if std::mem::take(&mut *self.lose_ack.borrow_mut()) {
            return Err("sync_network".into());
        }
        Ok(Acknowledged {
            space_id: p.space_id.clone(),
            epoch: p.epoch.clone(),
            acknowledged_ids: p.records.iter().map(|r| r.record_id.clone()).collect(),
        })
    }
    async fn changes(&self, h: &Handshake, cursor: Option<&str>) -> Result<Page> {
        let info = self.info.borrow();
        if self.reject_changes.get()
            || cursor == Some("invalid")
            || h.space_id != info.space_id
            || h.epoch != info.epoch
        {
            return Err("sync_reset_required".into());
        }
        let parts: Vec<_> = cursor.unwrap_or("checkpoint:0").split(':').collect();
        let start = parts[1].parse::<usize>().unwrap();
        let upper = if parts[0] == "page" {
            parts[2].parse().unwrap()
        } else {
            self.changes.borrow().len()
        };
        if start > 0 && std::mem::take(&mut *self.fail_page.borrow_mut()) {
            return Err("sync_network".into());
        }
        let next = (start + MAX_RECORDS).min(upper);
        let mut records = Vec::new();
        let mut devices = Vec::new();
        for change in &self.changes.borrow()[start..next] {
            match change {
                Change::Record(r) => records.push(r.clone()),
                Change::Device(d) => devices.push(d.clone()),
            }
        }
        Ok(Page {
            space_id: info.space_id.clone(),
            epoch: info.epoch.clone(),
            records,
            devices,
            next_cursor: if next < upper {
                format!("page:{next}:{upper}")
            } else {
                format!("checkpoint:{next}")
            },
            has_more: next < upper,
        })
    }
}
fn memory() -> store::Store {
    store::Store::init(Connection::open_in_memory().unwrap()).unwrap()
}
fn add(s: &store::Store, n: usize) {
    for i in 0..n {
        let r = store::Record::Usage {
            session_id: format!("s{i}"),
            timestamp_ms: Some(i as i64),
            model: "gpt-test".into(),
            input_tokens: 10,
            cache_read_tokens: 2,
            output_tokens: 3,
            total_usage: None,
            occurrence: 0,
        };
        s.db.execute(
            "INSERT OR IGNORE INTO local_records VALUES (?1,?2)",
            params![r.id(), serde_json::to_string(&r).unwrap()],
        )
        .unwrap();
    }
}
fn run(c: &Center, s: &mut store::Store) -> Result<()> {
    let d = Device {
        device_id: s.device_id()?,
        name: "test".into(),
    };
    tauri::async_runtime::block_on(exchange_with_recovery(c, "p", d, |a| {
        apply_action(s, "p", a)
    }))
}
#[test]
fn two_clients_full_incremental_and_retries_are_idempotent() {
    let center = Center::new();
    let mut a = memory();
    let mut b = memory();
    add(&a, 503);
    *center.lose_ack.borrow_mut() = true;
    assert!(run(&center, &mut a).is_err());
    assert_eq!(a.pending_count("p").unwrap(), 503);
    run(&center, &mut a).unwrap();
    assert_eq!(center.records.borrow().len(), 503);
    assert_eq!(a.pending_count("p").unwrap(), 0);
    *center.fail_page.borrow_mut() = true;
    assert!(run(&center, &mut b).is_err());
    assert_eq!(b.cursor("p").unwrap().as_deref(), Some("page:500:505"));
    run(&center, &mut b).unwrap();
    assert_eq!(b.records("p").unwrap().len(), 503);
    assert_eq!(b.pending_count("p").unwrap(), 0);
    run(&center, &mut a).unwrap();
    run(&center, &mut b).unwrap();
    assert_eq!(center.records.borrow().len(), 503);
    add(&a, 504);
    run(&center, &mut a).unwrap();
    run(&center, &mut b).unwrap();
    assert_eq!(b.records("p").unwrap().len(), 504);
    let d = a.device_id().unwrap();
    b.clear_device("p", &d).unwrap();
    assert!(b.records("p").unwrap().is_empty());
    run(&center, &mut b).unwrap();
    assert_eq!(b.records("p").unwrap().len(), 504);
    // 同一批日志复制到 B 后，两台设备保留来源关系，但合计仍只算一次。
    add(&b, 504);
    run(&center, &mut b).unwrap();
    run(&center, &mut a).unwrap();
    let all = TimeRange::new(None, None, None);
    assert_eq!(
        store::aggregate_rows(a.records("p").unwrap(), None, &all)
            .0
            .len(),
        504
    );
    assert_eq!(
        store::aggregate_rows(b.records("p").unwrap(), None, &all)
            .0
            .len(),
        504
    );
}
#[test]
fn disabled_defaults_and_https_only() {
    let c = SyncConfig::default();
    assert!(!c.configured());
    assert!(c.use_proxy);
    assert_eq!(c.profile().unwrap(), "");
    assert!(HttpTransport::new(&c).is_err());
    for url in [
        "http://example.com",
        "https://user:pass@example.com",
        "https://example.com/?token=x",
        "https://example.com/#x",
    ] {
        assert!(normalize_url(url).is_err());
    }
    assert_eq!(
        normalize_url(" https://example.com/base/ ").unwrap(),
        "https://example.com/base"
    );
}
#[test]
fn key_keep_replace_clear_and_backup_compatible_default() {
    let input = |key: Option<String>| ConfigInput {
        server_url: String::new(),
        key,
        use_proxy: true,
        source_home: String::new(),
        device_name: String::new(),
    };
    let c = config_from(input(Some("secret".into())), &SyncConfig::default()).unwrap();
    assert_eq!(c.key().unwrap(), "secret");
    let kept = config_from(input(None), &c).unwrap();
    assert_eq!(kept.sealed_key, c.sealed_key);
    assert!(config_from(input(Some(String::new())), &c)
        .unwrap()
        .sealed_key
        .is_empty());
    let old: settings::Settings = serde_json::from_str("{}").unwrap();
    assert!(!old.codex_usage_sync.configured());
    assert!(old.codex_usage_sync.use_proxy);
    assert_eq!(old.interval_minutes, 0);
    for interval in [0, 15, 1440] {
        let old: settings::Settings = serde_json::from_value(serde_json::json!({
            "intervalMinutes": interval,
            "codexUsageSync": { "intervalMinutes": 7 }
        }))
        .unwrap();
        assert_eq!(old.interval_minutes, interval);
        let restored = validate_saved_config(old.codex_usage_sync).unwrap();
        assert!(restored.use_proxy);
        assert!(serde_json::to_value(restored)
            .unwrap()
            .get("intervalMinutes")
            .is_none());
    }
    let legacy_input: ConfigInput = serde_json::from_value(serde_json::json!({
        "serverUrl": "", "key": null, "intervalMinutes": 7, "sourceHome": "", "deviceName": ""
    }))
    .unwrap();
    assert!(legacy_input.use_proxy);
    assert!(
        config_from(legacy_input, &SyncConfig::default())
            .unwrap()
            .use_proxy
    );
}

#[test]
fn proxy_preference_round_trips_without_changing_profile() {
    let original = account_config("proxy-test-key");
    let profile = original.profile().unwrap();
    for use_proxy in [false, true] {
        let input: ConfigInput = serde_json::from_value(serde_json::json!({
            "serverUrl": original.server_url, "key": null, "useProxy": use_proxy,
            "sourceHome": "", "deviceName": ""
        }))
        .unwrap();
        let config = config_from(input, &original).unwrap();
        assert_eq!(config.use_proxy, use_proxy);
        assert_eq!(config.sealed_key, original.sealed_key);
        let saved = serde_json::to_value(config).unwrap();
        assert_eq!(saved["useProxy"], use_proxy);
        let loaded = validate_saved_config(serde_json::from_value(saved).unwrap()).unwrap();
        assert_eq!(loaded.use_proxy, use_proxy);
        assert_eq!(loaded.profile().unwrap(), profile);
    }
}

#[test]
fn backup_carries_connection_but_preserves_target_machine_fields() {
    let original = SyncConfig {
        server_url: "https://sync.example.test".into(),
        sealed_key: local_crypto::seal("backup-key").unwrap(),
        use_proxy: false,
        source_home: "source-path".into(),
        device_name: "source-name".into(),
    };
    let portable = original.portable();
    assert!(portable.source_home.is_empty());
    assert!(portable.device_name.is_empty());
    assert_eq!(portable.key().unwrap(), "backup-key");
    assert!(!portable.use_proxy);
    let target = SyncConfig {
        source_home: "target-path".into(),
        device_name: "target-name".into(),
        ..Default::default()
    };
    let restored = validate_saved_config(
        serde_json::from_value(serde_json::to_value(portable).unwrap()).unwrap(),
    )
    .unwrap()
    .restore_local_fields(&target);
    assert_eq!(restored.source_home, target.source_home);
    assert_eq!(restored.device_name, target.device_name);
    assert_eq!(restored.server_url, original.server_url);
    assert!(!restored.use_proxy);
    let wire = serde_json::to_value(restored).unwrap();
    assert!(wire.get("deviceId").is_none());
    assert!(wire.get("cursor").is_none());
    assert!(wire.get("intervalMinutes").is_none());
    assert!(validate_saved_config(SyncConfig {
        server_url: "http://example.test".into(),
        ..original
    })
    .is_err());
}
#[test]
fn bad_protocol_and_hash_are_rejected() {
    assert!(Handshake {
        protocol_version: 2,
        space_id: "s".into(),
        epoch: "e".into()
    }
    .validate()
    .is_err());
    assert!(Handshake {
        protocol_version: 1,
        space_id: String::new(),
        epoch: "e".into()
    }
    .validate()
    .is_err());
}

#[test]
fn pagination_keeps_its_upper_bound_and_device_names_are_incremental() {
    let c = Center::new();
    let mut a = memory();
    add(&a, 503);
    run(&c, &mut a).unwrap();
    let h = tauri::async_runtime::block_on(c.handshake()).unwrap();
    let first = tauri::async_runtime::block_on(c.changes(&h, None)).unwrap();
    assert!(first.has_more);
    add(&a, 510);
    run(&c, &mut a).unwrap();
    let second = tauri::async_runtime::block_on(c.changes(&h, Some(&first.next_cursor))).unwrap();
    assert!(!second.has_more);
    assert_eq!(first.records.len() + second.records.len(), 503);
    let third = tauri::async_runtime::block_on(c.changes(&h, Some(&second.next_cursor))).unwrap();
    assert_eq!(third.records.len(), 7);
    let d = Device {
        device_id: a.device_id().unwrap(),
        name: "renamed".into(),
    };
    tauri::async_runtime::block_on(exchange(&c, "p", d, |action| {
        apply_action(&mut a, "p", action)
    }))
    .unwrap();
    let change = tauri::async_runtime::block_on(c.changes(&h, Some(&third.next_cursor))).unwrap();
    assert!(change.records.is_empty());
    assert_eq!(change.devices[0].name, "renamed");
}

#[test]
fn empty_remote_is_successful_and_upload_ack_never_advances_cursor() {
    let c = Center::new();
    let mut s = memory();
    run(&c, &mut s).unwrap();
    assert!(s.records("p").unwrap().is_empty());
    assert!(s.success("p").unwrap().is_some());
    let cursor = s.cursor("p").unwrap();
    add(&s, 1);
    let pending = s.pending("p").unwrap();
    s.acknowledge("p", &pending).unwrap();
    assert_eq!(s.cursor("p").unwrap(), cursor);
}

struct BadAck(Center);
impl Transport for BadAck {
    async fn handshake(&self) -> Result<Handshake> {
        self.0.handshake().await
    }
    async fn push(&self, p: &Push) -> Result<Acknowledged> {
        let mut ack = self.0.push(p).await?;
        ack.acknowledged_ids.clear();
        Ok(ack)
    }
    async fn changes(&self, _: &Handshake, _: Option<&str>) -> Result<Page> {
        panic!("invalid ack must stop download")
    }
}
#[test]
fn incomplete_ack_and_configuration_change_cannot_commit_progress() {
    let mut s = memory();
    add(&s, 1);
    let d = Device {
        device_id: s.device_id().unwrap(),
        name: "test".into(),
    };
    let result =
        tauri::async_runtime::block_on(exchange(&BadAck(Center::new()), "p", d.clone(), |a| {
            apply_action(&mut s, "p", a)
        }));
    assert_eq!(result.unwrap_err(), "sync_protocol_invalid");
    assert_eq!(s.pending_count("p").unwrap(), 1);
    assert_eq!(s.cursor("p").unwrap(), None);
    let generation = runtime().lock().unwrap().generation;
    let result = tauri::async_runtime::block_on(exchange(&Center::new(), "p", d, |a| {
        if matches!(a, DbAction::Ack(_)) {
            update_configuration(|| Ok(())).unwrap();
        }
        checked(generation, || apply_action(&mut s, "p", a))
    }));
    assert_eq!(result.unwrap_err(), "sync_cancelled");
    assert_eq!(s.pending_count("p").unwrap(), 1);
    assert_eq!(s.cursor("p").unwrap(), None);
}

#[test]
fn concurrent_triggers_execute_one_round() {
    tauri::async_runtime::block_on(async {
        for result in [Ok(()), Err("sync_network".into())] {
            let mut state = Runtime::default();
            let (release, pending) = tokio::sync::oneshot::channel();
            let expected = result.clone();
            let owner = state.start(
                async move {
                    pending.await.unwrap();
                    result
                },
                || {},
            );
            let mut waiters = Vec::new();
            for _ in 0..10 {
                waiters.push(state.start(async { panic!("duplicate round") }, || {}));
            }
            // 其它窗口必须等待真实结果；原 IPC 等待者取消也不取消后台任务。
            assert!(
                tokio::time::timeout(Duration::from_millis(10), wait_for_round(owner))
                    .await
                    .is_err()
            );
            release.send(()).unwrap();
            for done in waiters {
                assert_eq!(wait_for_round(done).await, expected);
            }
            assert!(state.join().is_none());
            assert_eq!(
                wait_for_round(state.start(async { Ok(()) }, || {})).await,
                Ok(())
            );
        }
    });
}

#[test]
fn new_configuration_gets_its_own_round_and_old_waiters_keep_their_result() {
    tauri::async_runtime::block_on(async {
        let mut state = Runtime::default();
        let (release, pending) = tokio::sync::oneshot::channel();
        let old = state.start(
            async move {
                pending.await.unwrap();
                Err("sync_cancelled".into())
            },
            || {},
        );
        state.generation += 1;
        let fresh = state.start(async { Ok(()) }, || {});
        assert_eq!(wait_for_round(fresh).await, Ok(()));
        release.send(()).unwrap();
        assert_eq!(wait_for_round(old).await.unwrap_err(), "sync_cancelled");
        assert!(state.join().is_none());
    });
}

#[test]
fn unified_schedule_and_viewing_freshness_respect_pause_and_backoff() {
    let mut state = Runtime::default();
    let now = 1_000_000;
    assert!(!state.should_sync(SyncTrigger::Interval, 0, None, now));
    for interval in [5, 15, 30, 60, 360, 720, 1440] {
        assert!(state.should_sync(SyncTrigger::Interval, interval, Some(now), now));
    }
    assert!(state.should_sync(SyncTrigger::Viewing, 0, None, now));
    assert!(!state.should_sync(SyncTrigger::Viewing, 0, Some(now - 299_999), now));
    assert!(state.should_sync(SyncTrigger::Viewing, 0, Some(now - 300_000), now));
    for error in ["sync_network", "sync_forbidden"] {
        state.record_result(&Err(error.into()));
        assert!(!state.should_sync(SyncTrigger::Interval, 5, None, now));
        assert!(!state.should_sync(SyncTrigger::Viewing, 0, None, now));
        assert!(state.should_sync(SyncTrigger::Manual, 0, Some(now), now));
        state.next = Some(Instant::now() - Duration::from_secs(1));
        assert_eq!(
            state.should_sync(SyncTrigger::Interval, 5, None, now),
            !state.paused
        );
        state.record_result(&Ok(()));
        assert!(state.next.is_none());
    }
}

// 单连接、自动关闭的本机 HTTP 替身。只在测试中绕过 HTTPS URL 构造，生产校验保持不变。
fn http_once(response: String) -> (HttpTransport, std::thread::JoinHandle<String>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        // 路由断言失败时也能退出，避免等待一个永远不会经过此监听器的请求。
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "HTTP fixture received no request"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("HTTP fixture accept failed: {e}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        loop {
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            request.extend_from_slice(&buf[..n]);
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                let len = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .map(|n| n.parse::<usize>().unwrap())
                    .unwrap_or(0);
                if request.len() >= end + 4 + len {
                    break;
                }
            }
        }
        let _ = socket.write_all(response.as_bytes());
        String::from_utf8(request).unwrap()
    });
    let t = HttpTransport {
        client: reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        base: format!("http://{addr}/prefix"),
        key: "test-only-shared-key".into(),
    };
    (t, server)
}
fn http_ok(body: &str) -> String {
    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

fn proxy_client(use_proxy: bool, mode: &str, url: &str) -> reqwest::Client {
    HttpTransport::client_builder(
        use_proxy,
        &crate::proxy::ProxyConfig {
            mode: mode.into(),
            url: url.into(),
        },
    )
    .unwrap()
    .timeout(Duration::from_secs(3))
    .build()
    .unwrap()
}

#[test]
fn http_custom_proxy_failure_can_be_bypassed() {
    let (mut t, origin) = http_once(http_ok(
        r#"{"protocolVersion":1,"spaceId":"s","epoch":"e"}"#,
    ));
    let (proxy, proxy_server) = http_once(
        "HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
    );
    let proxy_url = reqwest::Url::parse(&proxy.base)
        .unwrap()
        .origin()
        .ascii_serialization();
    t.client = proxy_client(true, "custom", &proxy_url);
    assert_eq!(
        tauri::async_runtime::block_on(t.handshake()).unwrap_err(),
        "sync_protocol_invalid"
    );
    assert!(proxy_server.join().unwrap().starts_with(&format!(
        "GET {}/api/v1/codex-usage/handshake HTTP/1.1",
        t.base
    )));

    t.client = proxy_client(false, "custom", &proxy_url);
    tauri::async_runtime::block_on(t.handshake())
        .unwrap()
        .validate()
        .unwrap();
    assert!(origin
        .join()
        .unwrap()
        .starts_with("GET /prefix/api/v1/codex-usage/handshake HTTP/1.1"));
}

#[test]
fn http_direct_mode_ignores_invalid_custom_proxy() {
    let invalid = crate::proxy::ProxyConfig {
        mode: "custom".into(),
        url: "http://[".into(),
    };
    assert!(matches!(
        HttpTransport::client_builder(true, &invalid),
        Err(e) if e == "sync_invalid_proxy"
    ));
    for (use_proxy, mode) in [(false, "custom"), (true, "direct"), (false, "direct")] {
        let (mut t, server) = http_once(http_ok(
            r#"{"protocolVersion":1,"spaceId":"s","epoch":"e"}"#,
        ));
        t.client = proxy_client(use_proxy, mode, &invalid.url);
        tauri::async_runtime::block_on(t.handshake())
            .unwrap()
            .validate()
            .unwrap();
        server.join().unwrap();
    }
}

#[test]
fn http_system_proxy_is_followed_or_bypassed() {
    const CHILD: &str = "MAA_TEST_SYNC_SYSTEM_PROXY";
    if std::env::var_os(CHILD).is_some() {
        let (mut t, server) = http_once(http_ok(
            r#"{"protocolVersion":1,"spaceId":"s","epoch":"e"}"#,
        ));
        t.client = proxy_client(true, "system", "");
        assert_eq!(
            tauri::async_runtime::block_on(t.handshake()).unwrap_err(),
            "sync_protocol_invalid"
        );
        t.client = proxy_client(false, "system", "");
        tauri::async_runtime::block_on(t.handshake())
            .unwrap()
            .validate()
            .unwrap();
        server.join().unwrap();
        let (mut t, server) = http_once(http_ok(
            r#"{"protocolVersion":1,"spaceId":"s","epoch":"e"}"#,
        ));
        t.client = proxy_client(true, "direct", "");
        tauri::async_runtime::block_on(t.handshake())
            .unwrap()
            .validate()
            .unwrap();
        server.join().unwrap();
        return;
    }

    // 环境变量只传给独立测试进程，避免 reqwest 缓存或并行测试相互影响。
    let (proxy, server) = http_once(
        "HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
    );
    let proxy_url = reqwest::Url::parse(&proxy.base)
        .unwrap()
        .origin()
        .ascii_serialization();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "codex_sync::tests::http_system_proxy_is_followed_or_bypassed",
            "--nocapture",
        ])
        .env(CHILD, "1");
    for name in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        child.env(name, &proxy_url);
    }
    child.env("NO_PROXY", "").env("no_proxy", "");
    let output = child.output().unwrap();
    let request = server.join().unwrap();
    assert!(
        output.status.success(),
        "proxy subprocess failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(request.starts_with("GET http://127.0.0.1:"));
}

#[test]
fn http_contract_auth_paths_payload_and_opaque_cursor() {
    let (t, server) = http_once(http_ok(
        r#"{"protocolVersion":1,"spaceId":"s","epoch":"e"}"#,
    ));
    let h = tauri::async_runtime::block_on(t.handshake()).unwrap();
    h.validate().unwrap();
    let request = server.join().unwrap();
    assert!(request.starts_with("GET /prefix/api/v1/codex-usage/handshake HTTP/1.1"));
    assert!(request
        .to_ascii_lowercase()
        .contains("authorization: bearer test-only-shared-key\r\n"));
    let (t, server) = http_once(http_ok(
        r#"{"spaceId":"s","epoch":"e","acknowledgedIds":[]}"#,
    ));
    let push = Push {
        protocol_version: 1,
        space_id: "s".into(),
        epoch: "e".into(),
        device: Device {
            device_id: "a".repeat(32),
            name: "设备".into(),
        },
        records: vec![],
    };
    tauri::async_runtime::block_on(t.push(&push)).unwrap();
    let request = server.join().unwrap();
    assert!(request.starts_with("POST /prefix/api/v1/codex-usage/push HTTP/1.1"));
    let body: serde_json::Value =
        serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body, serde_json::to_value(push).unwrap());
    let (t, server) = http_once(http_ok(
        r#"{"spaceId":"s","epoch":"e","records":[],"devices":[],"nextCursor":"opaque","hasMore":false}"#,
    ));
    tauri::async_runtime::block_on(t.changes(&h, Some("opaque+/?="))).unwrap();
    let request = server.join().unwrap();
    assert!(request.contains("cursor=opaque%2B%2F%3F%3D"));
    let target = request.split_whitespace().nth(1).unwrap();
    let url = reqwest::Url::parse(&format!("https://example.test{target}")).unwrap();
    let query: BTreeMap<_, _> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(
        query,
        BTreeMap::from([
            ("spaceId".into(), "s".into()),
            ("epoch".into(), "e".into()),
            ("limit".into(), "500".into()),
            ("cursor".into(), "opaque+/?=".into()),
        ])
    );
}

#[test]
fn http_rejects_auth_errors_redirects_versions_and_oversized_data() {
    for (status, expected) in [
        (400, "sync_protocol_invalid"),
        (401, "sync_unauthorized"),
        (403, "sync_forbidden"),
        (409, "sync_reset_required"),
        (429, "sync_rate_limited"),
        (503, "sync_server"),
        (302, "sync_protocol_invalid"),
        (413, "sync_protocol_invalid"),
    ] {
        let body = r#"{"code":"error","message":"untrusted-error-body"}"#;
        let (t, server) = http_once(format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nRetry-After: 60\r\nLocation: https://example.invalid/\r\nConnection: close\r\n\r\n{body}", body.len()));
        assert_eq!(
            tauri::async_runtime::block_on(t.handshake()).unwrap_err(),
            expected
        );
        server.join().unwrap();
    }
    let (t, server) = http_once(http_ok(
        r#"{"protocolVersion":2,"spaceId":"s","epoch":"e"}"#,
    ));
    assert_eq!(
        tauri::async_runtime::block_on(t.handshake())
            .unwrap()
            .validate()
            .unwrap_err(),
        "sync_protocol_version"
    );
    server.join().unwrap();
    let (t, server) = http_once(http_ok(&" ".repeat(MAX_BYTES + 1)));
    assert_eq!(
        tauri::async_runtime::block_on(t.handshake()).unwrap_err(),
        "sync_response_too_large"
    );
    server.join().unwrap();
}

struct Account {
    center: Center,
    enabled: Cell<bool>,
    key: RefCell<String>,
}

#[test]
fn http_upload_keeps_null_fields_raw_strings_and_decimal_integer_boundaries() {
    let session = store::Record::Session {
        session_id: "会话/\"Ω\"".into(),
        forked_from_id: None,
        created_at: None,
    };
    let usage = store::Record::Usage {
        session_id: "会话/\"Ω\"".into(),
        timestamp_ms: Some(8_210_266_876_799_999),
        model: " GPT-example ".into(),
        input_tokens: 9_007_199_254_740_991,
        cache_read_tokens: 0,
        output_tokens: 0,
        total_usage: None,
        occurrence: u32::MAX,
    };
    session.validate().unwrap();
    usage.validate().unwrap();
    let payload = Push {
        protocol_version: 1,
        space_id: "account".into(),
        epoch: "epoch".into(),
        device: Device {
            device_id: "a".repeat(32),
            name: "设备 A".into(),
        },
        records: vec![WireRecord::new(session), WireRecord::new(usage)],
    };
    let reply = serde_json::json!({
        "spaceId": "account", "epoch": "epoch",
        "acknowledgedIds": payload.records.iter().map(|r| &r.record_id).collect::<Vec<_>>()
    });
    let (t, server) = http_once(http_ok(&reply.to_string()));
    tauri::async_runtime::block_on(t.push(&payload)).unwrap();
    let request = server.join().unwrap();
    let body = request.split_once("\r\n\r\n").unwrap().1;
    let json: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(json.as_object().unwrap().len(), 5);
    assert_eq!(
        json["device"],
        serde_json::json!({ "deviceId": "a".repeat(32), "name": "设备 A" })
    );
    assert_eq!(
        json["records"][0]["record"],
        serde_json::json!({
            "kind": "session", "sessionId": "会话/\"Ω\"", "forkedFromId": null, "createdAt": null
        })
    );
    assert_eq!(
        json["records"][1]["record"],
        serde_json::json!({
            "kind": "usage", "sessionId": "会话/\"Ω\"", "timestampMs": 8_210_266_876_799_999i64,
            "model": " GPT-example ", "inputTokens": 9_007_199_254_740_991u64,
            "cacheReadTokens": 0, "outputTokens": 0, "totalUsage": null, "occurrence": 4_294_967_295u64
        })
    );
    assert!(body.contains("\"timestampMs\":8210266876799999"));
    assert!(body.contains("\"inputTokens\":9007199254740991"));
    assert!(body.contains("\"occurrence\":4294967295"));
}

impl Account {
    fn new(space: &str, key: &str) -> Self {
        let center = Center::new();
        center.info.borrow_mut().space_id = space.into();
        Self {
            center,
            enabled: Cell::new(true),
            key: RefCell::new(key.into()),
        }
    }
}
struct AccountTransport<'a> {
    account: &'a Account,
    key: String,
}
impl AccountTransport<'_> {
    fn authorize(&self) -> Result<()> {
        if self.key != *self.account.key.borrow() {
            return Err("sync_unauthorized".into());
        }
        if !self.account.enabled.get() {
            return Err("sync_forbidden".into());
        }
        Ok(())
    }
}
impl Transport for AccountTransport<'_> {
    async fn handshake(&self) -> Result<Handshake> {
        self.authorize()?;
        self.account.center.handshake().await
    }
    async fn push(&self, payload: &Push) -> Result<Acknowledged> {
        self.authorize()?;
        self.account.center.push(payload).await
    }
    async fn changes(&self, h: &Handshake, cursor: Option<&str>) -> Result<Page> {
        self.authorize()?;
        self.account.center.changes(h, cursor).await
    }
}
fn account_config(key: &str) -> SyncConfig {
    SyncConfig {
        server_url: "https://sync.example.test/maa".into(),
        sealed_key: local_crypto::seal(key).unwrap(),
        ..Default::default()
    }
}
fn sync_account(account: &Account, s: &mut store::Store, cfg: &SyncConfig) -> Result<()> {
    let profile = cfg.profile()?;
    let transport = AccountTransport {
        account,
        key: cfg.key()?,
    };
    let own = Device {
        device_id: s.device_id()?,
        name: "test-device".into(),
    };
    tauri::async_runtime::block_on(exchange_with_recovery(&transport, &profile, own, |a| {
        apply_action(s, &profile, a)
    }))
}

#[test]
fn account_disable_rotation_and_switch_keep_history_and_isolate_progress() {
    let account = Account::new("account-a", "first-key");
    let config = account_config("first-key");
    let profile = config.profile().unwrap();
    let mut a = memory();
    let mut b = memory();
    let a_id = a.device_id().unwrap();
    let b_id = b.device_id().unwrap();
    assert_ne!(a_id, b_id);
    add(&a, 1);
    add(&b, 2);
    sync_account(&account, &mut a, &config).unwrap();
    sync_account(&account, &mut b, &config).unwrap();
    sync_account(&account, &mut a, &config).unwrap();
    assert_eq!(a.records(&profile).unwrap().len(), 3);

    account.enabled.set(false);
    add(&a, 3);
    let cursor = a.cursor(&profile).unwrap();
    let last_success = a.success(&profile).unwrap();
    let uploaded = account.center.uploaded_records.get();
    let result = sync_account(&account, &mut a, &config);
    assert_eq!(result.as_ref().unwrap_err(), "sync_forbidden");
    assert_eq!(a.cursor(&profile).unwrap(), cursor);
    assert_eq!(a.success(&profile).unwrap(), last_success);
    assert_eq!(a.pending_count(&profile).unwrap(), 2);
    assert_eq!(account.center.uploaded_records.get(), uploaded);
    assert_eq!(a.records(&profile).unwrap().len(), 5);
    let mut state = Runtime::default();
    state.record_result(&result);
    assert!(state.paused);

    account.enabled.set(true);
    let result = sync_account(&account, &mut a, &config);
    result.as_ref().unwrap();
    state.record_result(&result);
    assert!(!state.paused);
    assert!(state.error.is_none());
    assert_eq!(a.pending_count(&profile).unwrap(), 0);
    let info = account.center.info.borrow().clone();
    *account.key.borrow_mut() = "rotated-key".into();
    for client in [&mut a, &mut b] {
        assert_eq!(
            sync_account(&account, client, &config).unwrap_err(),
            "sync_unauthorized"
        );
    }
    let rotated = account_config("rotated-key");
    let next_profile = rotated.profile().unwrap();
    assert_ne!(profile, next_profile);
    assert!(a.cursor(&next_profile).unwrap().is_none());
    assert_eq!(a.pending_count(&next_profile).unwrap(), 3);
    assert_eq!(a.records(&next_profile).unwrap().len(), 3);
    let uploaded = account.center.uploaded_records.get();
    sync_account(&account, &mut a, &rotated).unwrap();
    assert_eq!(account.center.uploaded_records.get(), uploaded + 3);
    sync_account(&account, &mut b, &rotated).unwrap();
    assert_eq!(account.center.records.borrow().len(), 5);
    assert_eq!(a.records(&next_profile).unwrap().len(), 5);
    assert_eq!(b.records(&next_profile).unwrap().len(), 5);
    assert_eq!(a.pending_count(&next_profile).unwrap(), 0);
    assert_eq!(account.center.info.borrow().space_id, info.space_id);
    assert_eq!(account.center.info.borrow().epoch, info.epoch);
    assert_eq!(a.device_id().unwrap(), a_id);
    assert_eq!(b.device_id().unwrap(), b_id);
    let all = TimeRange::new(None, None, None);
    assert_eq!(
        store::aggregate_rows(a.records(&next_profile).unwrap(), None, &all)
            .0
            .len(),
        3
    );

    let other = Account::new("account-b", "other-key");
    let other_config = account_config("other-key");
    let other_profile = other_config.profile().unwrap();
    assert!(a.cursor(&other_profile).unwrap().is_none());
    assert_eq!(a.pending_count(&other_profile).unwrap(), 3);
    assert_eq!(a.records(&other_profile).unwrap().len(), 3);
    sync_account(&other, &mut a, &other_config).unwrap();
    // 仅上传本机采集的三条，来自 B 的两条不能转传到另一账户。
    assert_eq!(other.center.records.borrow().len(), 3);
    assert!(other
        .center
        .records
        .borrow()
        .values()
        .all(|r| r.device_id == a_id));
    assert_eq!(a.devices(&other_profile, "test-device").unwrap().len(), 1);
    assert_eq!(a.records(&next_profile).unwrap().len(), 5);
}

#[test]
fn handshake_does_not_register_device_and_empty_sync_does() {
    let account = Account::new("account", "test-key");
    let config = account_config("test-key");
    let transport = AccountTransport {
        account: &account,
        key: config.key().unwrap(),
    };
    tauri::async_runtime::block_on(transport.handshake())
        .unwrap()
        .validate()
        .unwrap();
    assert!(account.center.devices.borrow().is_empty());
    assert!(account.center.changes.borrow().is_empty());
    let mut client = memory();
    sync_account(&account, &mut client, &config).unwrap();
    assert_eq!(account.center.devices.borrow().len(), 1);
    assert!(account.center.records.borrow().is_empty());
    assert!(client
        .success(&config.profile().unwrap())
        .unwrap()
        .is_some());
}

#[test]
fn invalid_cursor_recovers_once_and_epoch_reset_reuploads_local_history() {
    let center = Center::new();
    let mut client = memory();
    add(&client, 2);
    run(&center, &mut client).unwrap();
    client
        .db
        .execute("UPDATE profiles SET cursor='invalid' WHERE id='p'", [])
        .unwrap();
    let handshakes = center.handshakes.get();
    let uploaded = center.uploaded_records.get();
    run(&center, &mut client).unwrap();
    assert_eq!(center.handshakes.get(), handshakes + 2);
    assert_eq!(center.uploaded_records.get(), uploaded + 2);
    assert_eq!(center.records.borrow().len(), 2);
    assert_ne!(client.cursor("p").unwrap().as_deref(), Some("invalid"));

    // 模拟恢复落后备份后更换 epoch，本机已确认的原始存档须重新上传。
    center.info.borrow_mut().epoch = "restored-epoch".into();
    center.records.borrow_mut().clear();
    center.devices.borrow_mut().clear();
    center.changes.borrow_mut().clear();
    run(&center, &mut client).unwrap();
    assert_eq!(center.records.borrow().len(), 2);
    assert_eq!(client.pending_count("p").unwrap(), 0);
    assert_eq!(client.records("p").unwrap().len(), 2);

    center.reject_changes.set(true);
    let handshakes = center.handshakes.get();
    assert_eq!(
        run(&center, &mut client).unwrap_err(),
        "sync_reset_required"
    );
    assert_eq!(center.handshakes.get(), handshakes + 2);
    assert_eq!(client.records("p").unwrap().len(), 2);
}

#[test]
fn failed_reset_transaction_stops_recovery_without_losing_cursor_or_history() {
    let center = Center::new();
    let mut client = memory();
    add(&client, 1);
    run(&center, &mut client).unwrap();
    client
        .db
        .execute_batch(
            "UPDATE profiles SET cursor='invalid' WHERE id='p';
        CREATE TRIGGER fail_reset BEFORE DELETE ON acks BEGIN SELECT RAISE(ABORT,'test'); END;",
        )
        .unwrap();
    let handshakes = center.handshakes.get();
    assert!(run(&center, &mut client)
        .unwrap_err()
        .starts_with("sync_store_failed"));
    assert_eq!(center.handshakes.get(), handshakes + 1);
    assert_eq!(client.cursor("p").unwrap().as_deref(), Some("invalid"));
    assert_eq!(client.pending_count("p").unwrap(), 0);
    assert_eq!(client.records("p").unwrap().len(), 1);
}

struct OversizedPage(Center);
impl Transport for OversizedPage {
    async fn handshake(&self) -> Result<Handshake> {
        self.0.handshake().await
    }
    async fn push(&self, p: &Push) -> Result<Acknowledged> {
        self.0.push(p).await
    }
    async fn changes(&self, h: &Handshake, cursor: Option<&str>) -> Result<Page> {
        let mut page = self.0.changes(h, cursor).await?;
        page.devices.push(page.devices[0].clone());
        assert!(page.devices.len() <= MAX_RECORDS && page.records.len() <= MAX_RECORDS);
        assert_eq!(page.devices.len() + page.records.len(), MAX_RECORDS + 1);
        Ok(page)
    }
}
#[test]
fn combined_page_limit_rejects_the_whole_page_before_advancing_cursor() {
    let center = Center::new();
    let mut writer = memory();
    add(&writer, 499);
    run(&center, &mut writer).unwrap();
    let mut reader = memory();
    let own = Device {
        device_id: reader.device_id().unwrap(),
        name: "reader".into(),
    };
    let result = tauri::async_runtime::block_on(exchange_with_recovery(
        &OversizedPage(center),
        "p",
        own,
        |a| apply_action(&mut reader, "p", a),
    ));
    assert_eq!(result.unwrap_err(), "sync_protocol_invalid");
    assert!(reader.cursor("p").unwrap().is_none());
    assert!(reader.success("p").unwrap().is_none());
    assert!(reader.records("p").unwrap().is_empty());
}

#[test]
fn authentication_pauses_and_transient_failures_back_off_until_success() {
    let mut state = Runtime::default();
    for code in [
        "sync_unauthorized",
        "sync_forbidden",
        "sync_protocol_invalid",
    ] {
        state.record_result(&Err(code.into()));
        assert!(state.paused);
        assert_eq!(state.error.as_deref(), Some(code));
        state.record_result(&Ok(()));
        assert!(!state.paused);
        assert!(state.error.is_none());
        assert_eq!(state.failures, 0);
    }
    for (code, seconds) in [
        ("sync_network", 300),
        ("sync_server", 600),
        ("sync_rate_limited", 1200),
        ("sync_rate_limited", 1800),
        ("sync_network", 1800),
    ] {
        let before = Instant::now();
        state.record_result(&Err(code.into()));
        let after = Instant::now();
        assert!(!state.paused);
        let next = state.next.unwrap();
        assert!(next >= before + Duration::from_secs(seconds));
        assert!(next <= after + Duration::from_secs(seconds));
    }
    state.record_result(&Ok(()));
    assert!(state.error.is_none());
    assert_eq!(state.failures, 0);
}

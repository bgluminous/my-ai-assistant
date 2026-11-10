use crate::{accounts, opencode, opencode_local, settings, AppHandle};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn credential(workspace: &str) -> opencode::Credential {
    opencode::Credential {
        server: opencode::SERVER.into(),
        workspace_id: workspace.into(),
        workspace_name: "Fixture".into(),
        email: "fixture@example.invalid".into(),
        user_id: "fixture".into(),
        access: format!("access-{workspace}"),
        refresh: format!("refresh-{workspace}"),
        expires_at: chrono::Utc::now().timestamp_millis() - 1,
    }
}
fn add(id: &str, workspace: &str) -> String {
    let blob = credential(workspace).seal().unwrap();
    settings::mutate(|s| {
        s.accounts.push(accounts::Account {
            id: id.into(),
            kind: "opencode".into(),
            token: workspace.into(),
            note: String::new(),
            opencode_auth: Some(blob.clone()),
            status: None,
        });
        Ok(())
    })
    .unwrap();
    blob
}

#[tokio::test]
async fn independent_sessions_refresh_once_per_account_and_native_activation_uses_only_native_credentials(
) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let requests = calls.clone();
    opencode::tests::set_transport(Some(Arc::new(move |method, path, secret| {
        requests
            .lock()
            .unwrap()
            .push((method.to_string(), path.to_string()));
        if method == "POST" && path == "/auth/device/code" {
            assert!(serde_json::from_str::<Value>(secret)
                .unwrap()
                .get("note")
                .is_none());
            return Ok((
                200,
                json!({"device_code":"device-fixture","user_code":"TEST",
            "verification_uri_complete":"/console/device?user_code=TEST-CODE&client_id=opencode-cli","expires_in":60,"interval":1}),
            ));
        }
        if method == "POST" {
            let body: Value = serde_json::from_str(secret).unwrap();
            let workspace = body["refresh_token"]
                .as_str()
                .and_then(|s| s.strip_prefix("refresh-"))
                .unwrap_or("wrk_authorized");
            return Ok((
                200,
                json!({"access_token":format!("new-{workspace}"),"refresh_token":format!("rotated-{workspace}"),
                "expires_in":3600,"org_id":workspace}),
            ));
        }
        let workspace = secret.strip_prefix("new-").unwrap();
        Ok((
            200,
            if path == "/api/user" {
                json!({"email":"fixture@example.invalid"})
            } else {
                json!({"org":{"id":workspace,"name":"Fixture"},"userId":"fixture","hasGoSubscription":true})
            },
        ))
    })));
    let original = add("refresh-a", "wrk_a");
    add("refresh-b", "wrk_b");
    let (a, b, c) = tokio::join!(
        opencode::credential(&AppHandle, "refresh-a", None),
        opencode::credential(&AppHandle, "refresh-a", None),
        opencode::credential(&AppHandle, "refresh-b", None)
    );
    assert_eq!(a.unwrap().access, b.unwrap().access);
    assert_eq!(c.unwrap().access, "new-wrk_b");
    assert_eq!(
        calls.lock().unwrap().len(),
        2,
        "one exchange per account despite concurrent reads"
    );
    assert_ne!(
        accounts::account_snapshot("refresh-a")
            .unwrap()
            .opencode_auth
            .unwrap(),
        original
    );
    // 401 重试请求引用已被替换的 access token 时直接读取新副本，不再次轮换。
    opencode::credential(&AppHandle, "refresh-a", Some("access-wrk_a"))
        .await
        .unwrap();
    assert_eq!(calls.lock().unwrap().len(), 2);

    let start =
        opencode::opencode_oauth_start(Some("wrk_authorized".into()), Some("工作账户".into()))
            .await
            .unwrap();
    assert_eq!(
        start["url"],
        "https://opencode.ai/console/device?user_code=TEST-CODE&client_id=opencode-cli"
    );
    let id = start["id"].as_str().unwrap().to_string();
    assert_eq!(
        opencode::opencode_oauth_poll(AppHandle, id.clone())
            .await
            .unwrap()["state"],
        "pending"
    );
    tokio::time::sleep(std::time::Duration::from_millis(1050)).await;
    let result = opencode::opencode_oauth_poll(AppHandle, id).await.unwrap();
    assert_eq!(result["state"], "complete");
    assert_eq!(
        settings::read(|s| s
            .accounts
            .iter()
            .find(|a| a.token == "wrk_authorized")
            .unwrap()
            .note
            .clone())
        .unwrap(),
        "工作账户"
    );
    let count = settings::read(|s| s.accounts.len()).unwrap();
    let start =
        opencode::opencode_oauth_start(Some("wrk_authorized".into()), Some("工作账户".into()))
            .await
            .unwrap();
    let id = start["id"].as_str().unwrap().to_string();
    tokio::time::sleep(std::time::Duration::from_millis(1050)).await;
    let (result, ()) = tokio::join!(
        opencode::opencode_oauth_poll(AppHandle, id.clone()),
        async {
            opencode::opencode_oauth_cancel(id).unwrap();
        }
    );
    assert_eq!(result.unwrap_err(), "opencode_oauth_cancelled");
    assert_eq!(settings::read(|s| s.accounts.len()).unwrap(), count);
    opencode::tests::set_transport(None);

    native_contract().await;
}

struct TempDb(std::path::PathBuf);
impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
async fn native_contract() {
    let _fixture = opencode_local::tests::native_fixture_lock().lock().await;
    let file = TempDb(std::env::temp_dir().join(format!(
        "maa-opencode-fixture-{}-{}.sqlite",
        std::process::id(),
        accounts::new_id()
    )));
    let db = rusqlite::Connection::open(&file.0).unwrap();
    db.execute_batch("CREATE TABLE credential(id TEXT PRIMARY KEY,integration_id TEXT,value TEXT,active INTEGER,time_created INTEGER,time_updated INTEGER);").unwrap();
    let native_auth = |workspace: &str| {
        json!({"type":"oauth","methodID":"device","access":"native-access-never-replaced",
        "refresh":"native-refresh-never-replaced","expires":1,"metadata":{"server":opencode::SERVER,"orgID":workspace,"orgName":"Fixture"}}).to_string()
    };
    for (id, workspace, active) in [("cred_a", "wrk_a", 1), ("cred_b", "wrk_b", 0)] {
        db.execute(
            "INSERT INTO credential VALUES(?1,'opencode',?2,?3,1,1)",
            rusqlite::params![id, native_auth(workspace), active],
        )
        .unwrap();
    }
    settings::mutate(|s| {
        s.opencode_client.db_path = file.0.to_string_lossy().into();
        Ok(())
    })
    .unwrap();
    let db_path = file.0.clone();
    let state = Arc::new(Mutex::new("pending".to_string()));
    let status = state.clone();
    let activations = Arc::new(Mutex::new(Vec::new()));
    let activated = activations.clone();
    opencode_local::tests::set_runner(Some(Arc::new(move |args| {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        if args == ["--version"] {
            return Ok("2.1.0".into());
        }
        assert_eq!(args[0], "api");
        let response = match (args[1], args[2]) {
            ("get", "/api/credential") => {
                let mut s = db.prepare("SELECT id,value FROM credential").unwrap();
                json!(s.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).unwrap()
                    .map(|r|{let (id,value)=r.unwrap();json!({"id":id,"integrationID":"opencode","value":serde_json::from_str::<Value>(&value).unwrap()})}).collect::<Vec<_>>())
            }
            ("post", path) if path.ends_with("/activate") => {
                let id = path
                    .strip_prefix("/api/credential/")
                    .unwrap()
                    .strip_suffix("/activate")
                    .unwrap();
                activated.lock().unwrap().push(id.to_string());
                db.execute("UPDATE credential SET active=(id=?1)", [id])
                    .unwrap();
                Value::Null
            }
            ("post", "/api/integration/opencode/connect/oauth") => {
                assert_eq!(args[3], "--data");
                assert_eq!(
                    serde_json::from_str::<Value>(args[4]).unwrap()["methodID"],
                    "device"
                );
                json!({"attemptID":"attempt_fixture","url":"https://opencode.ai/console/device?code=TEST","instructions":"Fixture"})
            }
            ("get", "/api/integration/opencode/connect/oauth/attempt_fixture") => {
                json!({"status":*status.lock().unwrap()})
            }
            ("delete", "/api/integration/opencode/connect/oauth/attempt_fixture") => Value::Null,
            _ => panic!("unexpected native API route"),
        };
        Ok(response.to_string())
    })));
    assert_eq!(opencode_local::read_identities().unwrap().len(), 2);
    let app_blob = accounts::account_snapshot("refresh-b")
        .unwrap()
        .opencode_auth;
    assert_eq!(
        opencode_local::opencode_switch_local("refresh-b".into())
            .await
            .unwrap()["state"],
        "complete"
    );
    assert_eq!(*activations.lock().unwrap(), vec!["cred_b"]);
    assert_eq!(
        accounts::account_snapshot("refresh-b")
            .unwrap()
            .opencode_auth,
        app_blob
    );
    assert_eq!(
        db.query_row("SELECT value FROM credential WHERE id='cred_b'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        native_auth("wrk_b")
    );

    add("new-native", "wrk_native_new");
    assert_eq!(
        opencode_local::opencode_switch_local("new-native".into())
            .await
            .unwrap()["state"],
        "pending"
    );
    assert!(opencode_local::opencode_switch_local("refresh-a".into())
        .await
        .is_err());
    assert_eq!(
        opencode_local::opencode_client_oauth_poll("attempt_fixture".into(), false)
            .await
            .unwrap()["state"],
        "pending"
    );
    // 浏览器选错工作区；模拟官方 OAuth 创建并激活后，适配器必须回滚原登录。
    db.execute(
        "INSERT INTO credential VALUES('cred_wrong','opencode',?1,1,2,2)",
        [native_auth("wrk_wrong")],
    )
    .unwrap();
    db.execute("UPDATE credential SET active=0 WHERE id!='cred_wrong'", [])
        .unwrap();
    *state.lock().unwrap() = "complete".into();
    assert_eq!(
        opencode_local::opencode_client_oauth_poll("attempt_fixture".into(), false)
            .await
            .unwrap_err(),
        "opencode_workspace_mismatch"
    );
    assert_eq!(
        db.query_row("SELECT id FROM credential WHERE active=1", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "cred_b"
    );
    *state.lock().unwrap() = "pending".into();
    opencode_local::opencode_switch_local("new-native".into())
        .await
        .unwrap();
    assert_eq!(
        opencode_local::opencode_client_oauth_poll("attempt_fixture".into(), true)
            .await
            .unwrap()["state"],
        "cancelled"
    );
    assert!(
        opencode_local::opencode_client_oauth_poll("attempt_fixture".into(), true)
            .await
            .is_ok()
    );
    opencode_local::opencode_switch_local("new-native".into())
        .await
        .unwrap();
    db.execute(
        "INSERT INTO credential VALUES('cred_new','opencode',?1,0,3,3)",
        [native_auth("wrk_native_new")],
    )
    .unwrap();
    *state.lock().unwrap() = "complete".into();
    assert_eq!(
        opencode_local::opencode_client_oauth_poll("attempt_fixture".into(), false)
            .await
            .unwrap()["state"],
        "complete"
    );
    assert_eq!(
        db.query_row("SELECT id FROM credential WHERE active=1", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "cred_new"
    );
    opencode_local::tests::set_runner(None);
    settings::mutate(|s| {
        s.opencode_client.db_path.clear();
        Ok(())
    })
    .unwrap();
    drop(db);
}

#[test]
fn desktop_command_futures_are_send() {
    fn send<T: Send>(_: T) {}
    send(opencode::opencode_oauth_poll(AppHandle, "fixture".into()));
    send(opencode_local::opencode_switch_local("fixture".into()));
    send(crate::opencode_store::opencode_usage_fetch(
        AppHandle,
        "fixture".into(),
        None,
        None,
        None,
        None,
    ));
}

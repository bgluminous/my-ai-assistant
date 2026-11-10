use super::*;

// 原生授权夹具会使用同一进程内的 attempt/CLI 替身，与独立测试包中的原生流程串行。
pub(crate) fn native_fixture_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[tokio::test]
async fn client_config_writes_guard_native_start_pending_cancel_retry_and_persistence() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let _fixture = native_fixture_lock().lock().await;
    let mut writes = 0;
    // 授权尚未登记 attempt，但原生操作已开始时也必须拒绝换路径。
    let starting = native_lock().lock().await;
    assert_eq!(with_client_config_update(|| { writes += 1; Ok(()) }).unwrap_err(), "opencode_client_auth_pending");
    assert_eq!(opencode_client_set("invalid-fixture-path".into(), None).unwrap_err(), "opencode_client_auth_pending");
    assert_eq!(writes, 0);
    drop(starting);

    with_client_config_update(|| {
        assert!(native_lock().try_lock().is_err(), "persistence must hold the native lock");
        writes += 1;
        Ok(())
    }).unwrap();
    assert_eq!(writes, 1);
    assert!(native_lock().try_lock().is_ok());
    assert!(with_client_config_update(|| Err::<(), _>("fixture-write-failed".into())).is_err());
    assert!(native_lock().try_lock().is_ok(), "a failed write must release the guard");

    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            set_runner(None);
            attempts().lock().unwrap().remove("attempt_path_guard");
        }
    }
    let _cleanup = Cleanup;
    attempts().lock().unwrap().insert("attempt_path_guard".into(), NativeAttempt {
        workspace: "wrk_guard".into(), previous: None,
        deadline: chrono::Utc::now().timestamp_millis() + 60_000,
    });
    let cancels = std::sync::Arc::new(AtomicUsize::new(0));
    let calls = cancels.clone();
    set_runner(Some(std::sync::Arc::new(move |args| {
        assert!(args.contains(&"delete"));
        assert!(args.contains(&"/api/integration/opencode/connect/oauth/attempt_path_guard"));
        if calls.fetch_add(1, Ordering::SeqCst) == 0 { return Err("opencode_cli_failed".into()); }
        Ok(json!({"status":"cancelled"}).to_string())
    })));
    let mut paths = OpenCodeClientConfig { exe_path: "original-cli".into(), db_path: "original-db".into() };
    // 与备份共用的写入边界：待完成和取消失败时都不能执行任何设置/账户合并。
    for cancel_failed in [false, true] {
        if cancel_failed {
            assert_eq!(opencode_client_oauth_poll("attempt_path_guard".into(), true).await.unwrap_err(), "opencode_cli_failed");
        }
        assert_eq!(with_client_config_update(|| {
            writes += 1;
            paths.exe_path = "backup-cli".into();
            paths.db_path = "backup-db".into();
            Ok(())
        }).unwrap_err(), "opencode_client_auth_pending");
        assert_eq!(opencode_client_set("invalid-fixture-path".into(), None).unwrap_err(), "opencode_client_auth_pending");
        assert_eq!(writes, 1);
        assert_eq!(paths.exe_path, "original-cli");
        assert_eq!(paths.db_path, "original-db");
        assert!(attempts().lock().unwrap().contains_key("attempt_path_guard"));
    }
    assert_eq!(opencode_client_oauth_poll("attempt_path_guard".into(), true).await.unwrap()["state"], "cancelled");
    assert_eq!(cancels.load(Ordering::SeqCst), 2);
    with_client_config_update(|| {
        paths.exe_path = "backup-cli".into(); paths.db_path = "backup-db".into();
        Ok(())
    }).unwrap();
    assert_eq!(paths.exe_path, "backup-cli");
    assert_eq!(paths.db_path, "backup-db");
}
#[test]
fn import_exposes_only_identity_and_rejects_foreign_or_key_credentials() {
    let credential = json!({"type":"oauth","access":"secret","refresh":"secret",
        "metadata":{"server":"https://opencode.ai/console","orgID":"wrk_a","orgName":"Team","email":"fixture@example.invalid"}});
    let found = identity("cred_a", &credential).unwrap();
    let serialized = serde_json::to_string(&found).unwrap();
    assert!(!serialized.contains("secret"));
    assert!(!serialized.contains("refresh"));
    assert_eq!(found.workspace_id, "wrk_a");
    let mut wrong = credential.clone();
    wrong["metadata"]["server"] = json!("https://example.invalid");
    assert!(identity("cred_a", &wrong).is_none());
    wrong = credential.clone();
    wrong["type"] = json!("key");
    assert!(identity("cred_a", &wrong).is_none());
    wrong = credential;
    wrong["metadata"]["server"] = json!("https://console.opencode.ai");
    assert!(identity("cred_a", &wrong).is_some());
}
#[test]
fn old_settings_default_to_auto_detection() {
    let c: OpenCodeClientConfig = serde_json::from_str("{}").unwrap();
    assert!(c.exe_path.is_empty());
    assert!(c.db_path.is_empty());
}

pub(crate) type Runner = std::sync::Arc<dyn Fn(&[&str]) -> Result<String, String> + Send + Sync>;
fn runner() -> &'static Mutex<Option<Runner>> {
    static R: OnceLock<Mutex<Option<Runner>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(None))
}
pub(crate) fn set_runner(value: Option<Runner>) {
    *runner().lock().unwrap() = value;
}
pub(crate) fn run_fixture(args: &[&str]) -> Option<Result<String, String>> {
    let f = runner().lock().unwrap().clone()?;
    Some(f(args))
}

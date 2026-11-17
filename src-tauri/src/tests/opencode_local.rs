use super::*;

// 原生授权夹具会使用同一进程内的 attempt/CLI 替身，与独立测试包中的原生流程串行。
pub(crate) fn native_fixture_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[tokio::test]
async fn client_configuration_holds_the_native_guard_during_persistence() {
    let _fixture = native_fixture_lock().lock().await;
    let guard=native_lock().lock().await;
    assert!(with_client_config_update(||Ok(())).is_err());
    drop(guard);
    with_client_config_update(|| { assert!(native_lock().try_lock().is_err()); Ok(()) }).unwrap();
    assert!(with_client_config_update(||Err::<(),_>("failed".into())).is_err());
    assert!(native_lock().try_lock().is_ok());
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
    assert!(c.desktop_exe_path.is_empty());
    assert!(c.desktop_db_path.is_empty());
}

#[test]
fn independent_databases_import_distinct_workspaces_without_duplicate_logins() {
    let root=tempfile::tempdir().unwrap();
    let files=[root.path().join("cli.db"),root.path().join("desktop.db")];
    for (index,path) in files.iter().enumerate() {
        let db=Connection::open(path).unwrap();
        db.execute_batch("CREATE TABLE credential(id TEXT, integration_id TEXT, value TEXT, active INTEGER, time_updated INTEGER)").unwrap();
        for workspace in ["wrk_shared",if index==0 {"wrk_cli"} else {"wrk_desktop"}] {
            let value=json!({"type":"oauth","metadata":{"server":opencode::SERVER,"orgID":workspace}}).to_string();
            db.execute("INSERT INTO credential VALUES(?1,'opencode',?2,1,1)",rusqlite::params![workspace,value]).unwrap();
        }
    }
    let identities=read_identity_databases(&files).unwrap();
    assert_eq!(identities.len(),3);
    assert!(identities.iter().any(|i|i.workspace_id=="wrk_desktop"));
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

fn launch_result() -> &'static Mutex<Option<bool>> {
    static RESULT: OnceLock<Mutex<Option<bool>>>=OnceLock::new();
    RESULT.get_or_init(||Mutex::new(None))
}
pub(crate) fn set_launch_fixture(value: Option<bool>) { *launch_result().lock().unwrap()=value; }
pub(crate) fn launch_fixture() -> Option<bool> { *launch_result().lock().unwrap() }

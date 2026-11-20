use super::*;

#[test]
fn account_order_backup_maps_new_and_existing_identities_before_local_only_accounts() {
    let mut local: Settings = serde_json::from_value(json!({
        "accounts":[{"id":"local-only","kind":"opencode","token":"wrk_local"},
            {"id":"local-shared","kind":"opencode","token":"wrk_shared"}],
        "accountOrder":["local-only","local-shared"]
    })).unwrap();
    let backup = json!({"accounts":[
        {"id":"old-shared","kind":"opencode","token":"wrk_shared"},
        {"id":"old-new","kind":"opencode","token":"wrk_new"}],
        "accountOrder":["old-new","old-shared"]});
    let first = merge_settings(&mut local, serde_json::from_value(backup.clone()).unwrap(), false);
    let expected = vec![first.imported_ids[0].clone(), "local-shared".into(), "local-only".into()];
    assert_eq!(local.account_order.as_ref(), Some(&expected));
    let retry = merge_settings(&mut local, serde_json::from_value(backup).unwrap(), false);
    assert!(retry.imported_ids.is_empty());
    assert_eq!(local.account_order.as_ref(), Some(&expected));
}

#[test]
fn opencode_failure_backup_restore_includes_paths_and_remains_idempotent() {
    let mut current: Settings = serde_json::from_value(json!({
        "opencodeClient":{"exePath":"old-cli","dbPath":"old-db"},
        "accounts":[{"id":"local","kind":"opencode","token":"wrk_existing","note":"local","refreshIntervalMinutes":15}]
    })).unwrap();
    let incoming: Settings = serde_json::from_value(json!({
        "opencodeClient":{"exePath":"restored-cli","dbPath":"restored-db"},
        "accounts":[
            {"id":"backup-existing","kind":"opencode","token":"wrk_existing","note":"backup"},
            {"id":"backup-new","kind":"opencode","token":"wrk_new","note":"new","refreshIntervalMinutes":5,"autoRefreshToken":false}
        ]
    })).unwrap();
    // Use the actual export representation and import normalization before merging.
    let exported = serde_json::to_value(settings::persisted_form(&incoming)).unwrap();
    let restored = settings::sanitize_loaded(serde_json::from_value(exported.clone()).unwrap());
    let result = merge_settings(&mut current, restored, false);
    assert_eq!(current.opencode_client.exe_path, "restored-cli");
    assert_eq!(current.opencode_client.db_path, "restored-db");
    assert_eq!(result.imported_ids.len(), 1);
    assert_eq!(result.skipped_exists, 1);
    assert_eq!(current.accounts[0].id, "local");
    assert_eq!(current.accounts[0].note, "local");
    assert_eq!(current.accounts[0].refresh_interval_minutes, Some(15));
    assert_eq!(current.accounts[1].refresh_interval_minutes, Some(5));
    assert!(current.accounts[0].auto_refresh_token);
    assert!(!current.accounts[1].auto_refresh_token);

    let restored = settings::sanitize_loaded(serde_json::from_value(exported).unwrap());
    let retry = merge_settings(&mut current, restored, false);
    assert!(retry.imported_ids.is_empty());
    assert_eq!(retry.skipped_exists, 2);
    assert_eq!(current.accounts.len(), 2);
    assert!(!current.accounts[1].auto_refresh_token);

    let old: Settings = serde_json::from_value(json!({})).unwrap();
    merge_settings(&mut current, old, false);
    assert!(current.opencode_client.exe_path.is_empty());
    assert!(current.opencode_client.db_path.is_empty());
}

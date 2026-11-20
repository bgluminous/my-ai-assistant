//! accounts 模块的单元测试（由 accounts.rs 以 `#[path]` 挂载为 `crate::accounts::tests`）。

use super::*;

#[test]
fn account_order_migrates_cards_and_preserves_manual_positions_after_refresh() {
    let mut rows: Vec<Account> = serde_json::from_value(json!([
        {"id":"free","kind":"cursor","token":"a","status":{"membershipType":"Free"}},
        {"id":"work","kind":"opencode","token":"wrk_a"},
        {"id":"later","kind":"cursor","token":"b","status":{"membershipType":"Pro","billingCycleEnd":"2026-12-01"}},
        {"id":"soon","kind":"cursor","token":"c","status":{"membershipType":"Pro","billingCycleEnd":"2026-11-22T00:00:00Z"}},
        {"id":"unknown","kind":"cursor","token":"d"}
    ])).unwrap();
    let mut order = crate::account_order::legacy(&["opencode".into()], &rows);
    assert_eq!(order, ["work", "soon", "later", "unknown", "free"]);
    crate::account_order::move_before(&mut order, "free", Some("work")).unwrap();
    crate::account_order::move_before(&mut order, "later", None).unwrap();
    rows[1].status = Some(json!({"plan":"free"}));
    rows.retain(|a| a.id != "unknown");
    rows.push(serde_json::from_value(json!({"id":"added","kind":"claude","token":"e"})).unwrap());
    let saved = crate::account_order::complete(&order, &rows);
    assert_eq!(saved, ["free", "work", "soon", "later", "added"]);
    let mut stale = saved.clone();
    assert!(crate::account_order::move_before(&mut stale, "added", Some("removed")).is_err());
    assert_eq!(stale, saved);
    let settings = settings::Settings { accounts: rows, account_order: Some(saved.clone()), ..Default::default() };
    let restored: settings::Settings = serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
    assert_eq!(view(&restored.accounts_file()).account_order, Some(saved));
}

#[test]
fn renewal_setting_defaults_on_and_survives_account_export_view_and_backup() {
    for kind in ["cursor", "codex", "claude", "opencode"] {
        let mut account: Account = serde_json::from_value(json!({
            "id":"renewal-fixture", "kind":kind, "token":"fixture"
        })).unwrap();
        assert!(account.auto_refresh_token);
        account.auto_refresh_token = false;
        let stored = serde_json::to_value(&account).unwrap();
        assert!(!serde_json::from_value::<Account>(stored.clone()).unwrap().auto_refresh_token);
        let exported: AccountExportItem = serde_json::from_value(stored).unwrap();
        assert!(!serde_json::from_value::<AccountExportItem>(serde_json::to_value(exported).unwrap())
            .unwrap().auto_refresh_token);
        assert_eq!(serde_json::to_value(account_view(&account)).unwrap()["autoRefreshToken"], false);
        let mut data = crate::settings::Settings::default();
        data.accounts.push(account);
        let restored: crate::settings::Settings = serde_json::from_value(
            serde_json::to_value(crate::settings::persisted_form(&data)).unwrap()
        ).unwrap();
        assert!(!restored.accounts[0].auto_refresh_token);
    }
    let legacy: AccountExportItem = serde_json::from_value(json!({"token":"fixture"})).unwrap();
    assert!(legacy.auto_refresh_token);
}

#[tokio::test]
async fn provider_queues_are_shared_fifo_and_independent() {
    let queue = refresh_queue("cursor").unwrap();
    let held = queue.lock().await;
    assert!(refresh_queue("cursor").unwrap().try_lock().is_err());
    for kind in ["codex", "claude", "opencode"] {
        assert!(refresh_queue(kind).unwrap().try_lock().is_ok());
    }
    assert!(refresh_queue("unknown").is_err());
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let (finished, mut finishes) = tokio::sync::mpsc::unbounded_channel();
    let mut jobs = Vec::new();
    for index in 0..2 {
        let started = started.clone();
        let finished = finished.clone();
        jobs.push(tokio::spawn(async move {
            started.send(index).unwrap();
            let _guard = refresh_queue("cursor").unwrap().lock().await;
            finished.send(index).unwrap();
        }));
        // 当前线程 runtime：收到开始通知时，任务已在被占用的锁上挂起。
        assert_eq!(starts.recv().await, Some(index));
    }
    assert!(finishes.try_recv().is_err());
    drop(held);
    assert_eq!(finishes.recv().await, Some(0));
    assert_eq!(finishes.recv().await, Some(1));
    for job in jobs { job.await.unwrap(); }
}

#[test]
fn subscription_refresh_keeps_cached_plan_on_failure_but_online_values_win() {
    let prev = json!({ "accountId": "same", "plan": "prolite", "planActiveUntil": "2026-12-01" });
    let mut stale_jwt = json!({ "accountId": "same", "alive": true, "plan": "plus" });
    carry_subscription_profile("codex", &mut stale_jwt, Some(&prev), true);
    assert_eq!(stale_jwt["plan"], "prolite");
    let mut fresh = json!({ "accountId": "same", "alive": true, "plan": "promax" });
    carry_subscription_profile("codex", &mut fresh, Some(&prev), false);
    assert_eq!(fresh["plan"], "promax");
    assert!(fresh.get("planActiveUntil").is_none());
    let mut denied = json!({ "alive": false, "refreshError": "expired" });
    carry_subscription_profile("codex", &mut denied, Some(&prev), false);
    assert_eq!(denied["plan"], "prolite");
    assert_eq!(denied["alive"], false);
    let mut another = json!({ "accountId": "other", "plan": "plus" });
    carry_subscription_profile("codex", &mut another, Some(&prev), true);
    assert_eq!(another["plan"], "plus");
}

#[test]
fn claude_cached_metadata_respects_family_changes_and_legacy_tiers() {
    let prev = json!({ "accountUuid": "same", "plan": "max", "rateLimitTier": "default_claude_max_20x" });
    let mut failed = json!({ "alive": false, "plan": null, "rateLimitTier": null });
    carry_subscription_profile("claude", &mut failed, Some(&prev), false);
    assert_eq!(failed["plan"], "max");
    assert_eq!(failed["rateLimitTier"], "default_claude_max_20x");
    assert_eq!(failed["accountUuid"], "same");
    let mut same = json!({ "alive": true, "plan": "claude_max" });
    carry_subscription_profile("claude", &mut same, Some(&prev), false);
    assert_eq!(same["rateLimitTier"], "default_claude_max_20x");
    assert_eq!(same["accountUuid"], "same");
    let mut another = json!({ "alive": true, "accountUuid": "another" });
    carry_subscription_profile("claude", &mut another, Some(&prev), false);
    assert_eq!(another["accountUuid"], "another");
    assert!(another.get("plan").is_none());
    for new_plan in ["pro", "team", "future_plan"] {
        let mut fresh = json!({ "alive": true, "plan": new_plan });
        carry_subscription_profile("claude", &mut fresh, Some(&prev), false);
        assert_eq!(fresh["plan"], new_plan);
        assert!(fresh.get("rateLimitTier").is_none());
    }
    let mut upgraded = json!({ "alive": true, "plan": "max", "rateLimitTier": "future_tier" });
    carry_subscription_profile("claude", &mut upgraded, Some(&prev), false);
    assert_eq!(upgraded["rateLimitTier"], "future_tier");
    let mut legacy = json!({ "alive": true, "plan": "max" });
    carry_subscription_profile("claude", &mut legacy, Some(&json!({ "plan": "default_claude_max_5x" })), false);
    assert_eq!(legacy["rateLimitTier"], "default_claude_max_5x");
    let mut empty = json!({ "alive": false });
    carry_subscription_profile("claude", &mut empty, None, false);
    assert!(empty.get("plan").is_none());
}

#[test]
fn cursor_api_filter_defaults_off_and_survives_serialization() {
    let mut account: Account = serde_json::from_value(json!({
        "id": "acc-test", "kind": "cursor", "token": "fixture"
    })).unwrap();
    assert!(!account.ignore_api_models);
    account.ignore_api_models = true;
    let stored = serde_json::to_value(&account).unwrap();
    assert_eq!(stored["ignoreApiModels"], true);
    let restored: Account = serde_json::from_value(stored).unwrap();
    assert!(restored.ignore_api_models);

    let legacy: AccountExportItem = serde_json::from_value(json!({ "token": "fixture" })).unwrap();
    assert!(!legacy.ignore_api_models);
    let exported: AccountExportItem = serde_json::from_value(json!({
        "token": "fixture", "ignoreApiModels": true
    })).unwrap();
    assert!(serde_json::from_value::<AccountExportItem>(serde_json::to_value(exported).unwrap())
        .unwrap().ignore_api_models);
}

#[test]
fn error_code_extraction_matches_codex_cli() {
    // RFC 6749 形态：error 为字符串，说明在 error_description
    let rfc = r#"{"error":"invalid_grant","error_description":"refresh token expired"}"#;
    assert_eq!(refresh_error_code(rfc).as_deref(), Some("invalid_grant"));
    assert_eq!(refresh_error_detail(rfc), "refresh token expired");
    // 旧形态：error 为对象，子码在 error.code
    let legacy = r#"{"error":{"code":"refresh_token_reused","message":"already used"}}"#;
    assert_eq!(refresh_error_code(legacy).as_deref(), Some("refresh_token_reused"));
    assert_eq!(refresh_error_detail(legacy), "already used");
    // 顶层 code
    assert_eq!(
        refresh_error_code(r#"{"code":"refresh_token_expired"}"#).as_deref(),
        Some("refresh_token_expired")
    );
    // 非 JSON：没有错误码，说明取截断原文
    assert!(refresh_error_code("Bad Gateway").is_none());
    assert_eq!(refresh_error_detail("  Bad Gateway  "), "Bad Gateway");
}

#[test]
fn refresh_interval_legacy_accounts_inherit_and_explicit_values_survive_exports() {
    for kind in ["cursor", "codex", "claude", "opencode"] {
        let legacy = json!({ "id": "fixture", "kind": kind, "token": "fixture" });
        let account: Account = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(account.refresh_interval_minutes, None);
        assert!(serde_json::to_value(&account).unwrap().get("refreshIntervalMinutes").is_none());
        for interval in [Value::Null, json!(0), json!(5), json!(1440)] {
            let mut value = legacy.clone();
            value["refreshIntervalMinutes"] = interval.clone();
            let account: Account = serde_json::from_value(value).unwrap();
            let stored = serde_json::to_value(&account).unwrap();
            let restored: Account = serde_json::from_value(stored.clone()).unwrap();
            let exported: AccountExportItem = serde_json::from_value(stored).unwrap();
            let imported: AccountExportItem = serde_json::from_value(serde_json::to_value(exported).unwrap()).unwrap();
            let expected = interval.as_u64().map(|value| value as u32);
            assert_eq!(restored.refresh_interval_minutes, expected);
            assert_eq!(imported.refresh_interval_minutes, expected);
        }
    }
    let old_export: AccountExportItem = serde_json::from_value(json!({ "token": "fixture" })).unwrap();
    assert_eq!(old_export.refresh_interval_minutes, None);
}

#[test]
fn refresh_interval_survives_backup_settings_and_account_view() {
    let mut data = crate::settings::Settings::default();
    data.interval_minutes = 15;
    data.accounts = serde_json::from_value(json!([
        { "id": "inherited", "kind": "cursor", "token": "fixture" },
        { "id": "disabled", "kind": "claude", "token": "fixture", "refreshIntervalMinutes": 0 },
        { "id": "custom", "kind": "opencode", "token": "wrk_fixture", "refreshIntervalMinutes": 5 }
    ])).unwrap();
    let persisted = crate::settings::persisted_form(&data);
    let restored = crate::settings::sanitize_loaded(
        serde_json::from_value(serde_json::to_value(persisted).unwrap()).unwrap()
    );
    assert_eq!(restored.interval_minutes, 15);
    assert_eq!(restored.accounts.iter().map(|account| account.refresh_interval_minutes).collect::<Vec<_>>(),
        vec![None, Some(0), Some(5)]);
    assert_eq!(serde_json::to_value(account_view(&restored.accounts[2])).unwrap()["refreshIntervalMinutes"], 5);
}

#[test]
fn dead_cursor_status_keeps_previous_profile() {
    let prev = json!({
        "alive": true,
        "name": "Alice",
        "email": "alice@example.com",
        "membershipType": "pro",
        "billingCycleStart": "2026-09-01T00:00:00Z",
        "billingCycleEnd": "2026-10-01T00:00:00Z",
    });

    // 会话失效：接口不返回身份与套餐，沿用上一次缓存的用户名、邮箱、套餐与计费周期，
    // 失效标记不受影响；额度等其它字段不沿用
    let mut dead = json!({
        "alive": false,
        "name": null,
        "email": null,
        "membershipType": null,
        "billingCycleStart": null,
        "billingCycleEnd": null,
        "plan": { "used": null },
    });
    carry_cursor_profile(&mut dead, Some(&prev));
    assert_eq!(dead["alive"], json!(false));
    assert_eq!(dead["name"], json!("Alice"));
    assert_eq!(dead["email"], json!("alice@example.com"));
    assert_eq!(dead["membershipType"], json!("pro"));
    assert_eq!(dead["billingCycleStart"], json!("2026-09-01T00:00:00Z"));
    assert_eq!(dead["billingCycleEnd"], json!("2026-10-01T00:00:00Z"));
    assert_eq!(dead["plan"]["used"], Value::Null);

    // 接口返回了新值时以新值为准
    let mut fresh = json!({
        "alive": true,
        "name": "Alice B",
        "email": "alice@example.com",
        "membershipType": "ultra",
    });
    carry_cursor_profile(&mut fresh, Some(&prev));
    assert_eq!(fresh["name"], json!("Alice B"));
    assert_eq!(fresh["membershipType"], json!("ultra"));

    // 首次刷新（没有历史状态）或历史里也没有可沿用的值：保持原样
    let mut first = json!({ "alive": false, "name": null, "email": null, "membershipType": null });
    carry_cursor_profile(&mut first, None);
    assert_eq!(first["name"], Value::Null);
    assert_eq!(first["membershipType"], Value::Null);
    carry_cursor_profile(&mut first, Some(&json!({ "alive": false, "name": "  ", "membershipType": "" })));
    assert_eq!(first["name"], Value::Null);
    assert_eq!(first["email"], Value::Null);
    assert_eq!(first["membershipType"], Value::Null);
}

fn auth_fixture(id: &str) -> Value {
    json!({
        "tokens": {"access_token": format!("fixture-access-{id}"), "refresh_token": format!("fixture-refresh-{id}"),
                   "account_id": id, "id_token": format!("fixture-id-{id}")},
        "last_refresh": "2026-11-18T12:00:00Z", "extension": {"owner": id, "counter": 9_007_199_254_740_993_u64}
    })
}

#[test]
fn adding_full_auth_preserves_encrypted_credentials_extensions_and_optional_fields() {
    let mut auth = auth_fixture("add");
    auth["extension"]["precise"] = serde_json::from_str::<Value>("0.123456789012345678901").unwrap();
    for with_refresh in [true, false] {
        if !with_refresh { auth["tokens"].as_object_mut().unwrap().remove("refresh_token"); }
        let input: NewAccount = serde_json::from_value(json!({
            "kind":"codex", "note":" imported ", "codexAuthText":auth.to_string(),
            "token":"ignored-token", "refreshToken":"ignored-refresh"
        })).unwrap();
        let account = prepare_new_account(input).unwrap();
        assert_eq!(account.note, "imported");
        assert!(!account.note_auto);
        assert_eq!(account.token, "fixture-access-add");
        assert_eq!(account.refresh_token.as_deref(), with_refresh.then_some("fixture-refresh-add"));
        assert_eq!(codex_auth_json(&account).unwrap(), auth);
        assert!(account.codex_auth.is_some()); // 完整副本使 accounts_add 跳过换票分支。
        let mut stored = vec![account];
        dehydrate_accounts(&mut stored);
        let encoded = serde_json::to_string(&stored).unwrap();
        assert!(!encoded.contains("fixture-access"));
        assert!(!encoded.contains("fixture-refresh"));
        assert!(!encoded.contains("fixture-id"));
        let mut reopened: Vec<Account> = serde_json::from_str(&encoded).unwrap();
        hydrate_accounts(&mut reopened);
        assert_eq!(codex_auth_json(&reopened[0]).unwrap(), auth);
        assert!(is_duplicate_account(&reopened, "codex", "fixture-access-add", None));
    }
    let input: NewAccount = serde_json::from_value(json!({"kind":"codex", "codexAuthText":auth.to_string()})).unwrap();
    assert!(prepare_new_account(input).unwrap().note_auto);
}

#[test]
fn adding_auth_rejects_invalid_shapes_and_other_providers_without_falling_back_to_token() {
    for (text, error) in [
        ("[]", "codex_auth_object"),
        ("null", "codex_auth_object"),
        (r#"{"tokens":{"access_token":" "}}"#, "codex_auth_access_token"),
        (r#"{"access_token":"flat"}"#, "codex_auth_access_token"),
        (r#"{"tokens":{"access_token":42}}"#, "codex_auth_access_token"),
        (r#"{"tokens":{"access_token":"a","refresh_token":42}}"#, "codex_auth_refresh_token"),
    ] {
        let input = serde_json::from_value(json!({"kind":"codex", "codexAuthText":text, "token":"fallback"})).unwrap();
        assert_eq!(prepare_new_account(input).unwrap_err(), error);
    }
    let input = serde_json::from_value(json!({"kind":"codex", "codexAuthText":"{\n\"secret\":}"})).unwrap();
    let error = prepare_new_account(input).unwrap_err();
    assert!(error.starts_with("codex_auth_json:2:"));
    assert!(!error.contains("secret"));
    for kind in ["cursor", "claude", "opencode"] {
        let input = serde_json::from_value(json!({"kind":kind, "codexAuthText":auth_fixture("a").to_string()})).unwrap();
        assert_eq!(prepare_new_account(input).unwrap_err(), "not_codex_account");
    }
    let missing = serde_json::from_value(json!({"kind":"codex"})).unwrap();
    assert_eq!(prepare_new_account(missing).unwrap_err(), "empty_token");
    let legacy = serde_json::from_value(json!({"kind":"codex", "token":" manual ", "refreshToken":" refresh "})).unwrap();
    let account = prepare_new_account(legacy).unwrap();
    assert_eq!(account.token, "manual");
    assert_eq!(account.refresh_token.as_deref(), Some("refresh"));
    assert!(account.codex_auth.is_none()); // Token 模式仍由 accounts_add 尝试补齐凭据。
}

fn fixture_accounts() -> AccountsFile {
    let mut data: AccountsFile = serde_json::from_value(json!({
        "accounts":[{"id":"a","kind":"codex","note":"A","token":""},
                    {"id":"b","kind":"codex","note":"B","token":""},
                    {"id":"c","kind":"cursor","note":"C","token":"fixture-c"}],
        "accountOrder":["b","c","a"]
    }))
    .unwrap();
    for a in &mut data.accounts[..2] {
        let auth = auth_fixture(&a.id);
        set_codex_auth(a, &auth).unwrap();
    }
    data
}

fn auth_change(revision: &str, value: Value) -> AccountEdit {
    AccountEdit {
        note: Some("edited".into()),
        update_credentials: true,
        token: String::new(),
        refresh_token: None,
        new_codex_auth: Some(value),
        expected_auth_revision: Some(revision.to_owned()),
        interval: Some(Some(15)),
        auto_refresh_token: Some(false),
        ignore_api_models: None,
    }
}

#[test]
fn full_auth_atomic_edit_preserves_extensions_and_explicit_removals_after_reopen() {
    let mut data = fixture_accounts();
    let mut draft = auth_fixture("a");
    let tokens = draft["tokens"].as_object_mut().unwrap();
    tokens.remove("refresh_token");
    tokens.remove("id_token");
    tokens.insert("access_token".into(), json!("fixture-new-access"));
    let revision = auth_edit::revision(&data.accounts[0]);
    let fields = auth_change(&revision, draft.clone())
        .apply(&mut data, "a", None)
        .unwrap();
    assert_eq!(
        fields,
        [
            "Auth 凭据",
            "Token",
            "Refresh Token",
            "备注",
            "同步间隔",
            "自动续期 Token"
        ]
    );
    let a = &data.accounts[0];
    assert_eq!(a.token, "fixture-new-access");
    assert!(a.refresh_token.is_none());
    assert_eq!(a.refresh_interval_minutes, Some(15));
    assert!(!a.auto_refresh_token);
    assert_eq!(a.note, "edited");
    assert_eq!(
        auth_edit::saved_value(&data.accounts[1]).unwrap(),
        auth_fixture("b")
    );
    assert_ne!(auth_edit::revision(a), revision);
    let public = serde_json::to_value(account_view(a)).unwrap();
    assert!(public.get("codexAuth").is_none());
    assert!(!public.to_string().contains("extension"));
    let persisted = settings::persisted_form(&settings::Settings {
        accounts: data.accounts,
        ..Default::default()
    });
    let encoded = serde_json::to_string(&persisted).unwrap();
    assert!(!encoded.contains("fixture-new-access"));
    let mut reopened: settings::Settings = serde_json::from_str(&encoded).unwrap();
    hydrate_accounts(&mut reopened.accounts);
    assert_eq!(
        auth_edit::saved_value(&reopened.accounts[0]).unwrap(),
        draft
    );
    assert!(reopened.accounts[0].refresh_token.is_none());
}

#[test]
fn auth_conflicts_duplicates_and_invalid_access_do_not_commit_other_edits() {
    let mut data = fixture_accounts();
    let stale = auth_edit::revision(&data.accounts[0]);
    let mut renewed = auth_fixture("a");
    renewed["tokens"]["id_token"] = json!("fixture-renewed-id");
    set_codex_auth(&mut data.accounts[0], &renewed).unwrap();
    let current = auth_edit::revision(&data.accounts[0]);
    for (rev, auth, error) in [
        (&stale, auth_fixture("a"), "codex_auth_conflict"),
        (&current, auth_fixture("b"), "duplicate_account"),
        (
            &current,
            json!({"tokens":{"access_token":"  "}}),
            "codex_auth_access_token",
        ),
    ] {
        assert_eq!(
            auth_change(rev, auth)
                .apply(&mut data, "a", None)
                .unwrap_err(),
            error
        );
        assert_eq!(data.accounts[0].note, "A");
        assert_eq!(auth_edit::saved_value(&data.accounts[0]).unwrap(), renewed);
    }
    let metadata = AccountEdit {
        update_credentials: false,
        new_codex_auth: None,
        ..auth_change(&stale, Value::Null)
    };
    metadata.apply(&mut data, "a", None).unwrap();
    assert_eq!(data.accounts[0].note, "edited");
    assert_eq!(auth_edit::revision(&data.accounts[0]), current);
}

#[test]
fn account_files_export_selected_ids_and_roundtrip_latest_auth_in_display_order() {
    let mut data = fixture_accounts();
    let mut edited = auth_fixture("b");
    edited["custom"] = json!(["one", {"extra":true}]);
    let revision = auth_edit::revision(&data.accounts[1]);
    auth_change(&revision, edited.clone())
        .apply(&mut data, "b", None)
        .unwrap();
    assert_eq!(
        export_accounts(&data, Some("removed")).unwrap_err(),
        "not_found"
    );
    let dir = tempfile::tempdir().unwrap();
    for (id, names) in [
        (None, vec!["edited", "C", "A"]),
        (Some("b"), vec!["edited"]),
    ] {
        let file = export_accounts(&data, id).unwrap();
        assert_eq!(file.version, 2);
        assert!(file.kind.is_none());
        let path = dir.path().join("accounts.json");
        crate::codex_auth_store::atomic_write(&path, &serde_json::to_vec(&file).unwrap()).unwrap();
        let encoded = std::fs::read_to_string(path).unwrap();
        assert!(!encoded.contains("fixture-access-b"));
        if id.is_some() {
            assert!(!encoded.contains(data.accounts[0].codex_auth.as_ref().unwrap()));
        }
        let mut target = AccountsFile::default();
        let (rows, skipped) =
            merge_account_file(&mut target, parse_account_file(&encoded).unwrap()).unwrap();
        assert!(skipped.is_empty());
        assert_eq!(
            rows.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(),
            names
        );
        assert_ne!(target.accounts[0].id, "b");
        assert_eq!(auth_edit::saved_value(&target.accounts[0]).unwrap(), edited);
        if id.is_none() {
            assert_eq!(target.accounts[1].kind, "cursor");
            assert_eq!(
                auth_edit::saved_value(&target.accounts[2]).unwrap(),
                auth_fixture("a")
            );
        }
        let (reimported, duplicates) =
            merge_account_file(&mut target, parse_account_file(&encoded).unwrap()).unwrap();
        assert!(reimported.is_empty());
        assert_eq!(duplicates.len(), target.accounts.len());
        assert!(duplicates.iter().all(|r| r.reason == "exists"));
    }
}

#[test]
fn legacy_and_mixed_import_append_in_file_order_and_trust_encrypted_auth_identity() {
    let mut data = fixture_accounts();
    let old = json!({"format":EXPORT_FORMAT,"version":1,"kind":"codex","exportedAt":0,"accounts":[
        {"note":"B again","token":"stale-flat-b","codexAuth":data.accounts[1].codex_auth},
        {"note":"Partial","token":"fixture-partial"}
    ]});
    let (rows, skipped) = merge_account_file(
        &mut data,
        parse_account_file(&format!("\u{feff}{old}")).unwrap(),
    )
    .unwrap();
    assert_eq!(skipped[0].reason, "exists");
    assert_eq!(rows.len(), 1);
    let partial = data.accounts.last().unwrap();
    assert_eq!(
        auth_edit::saved_value(partial).unwrap()["tokens"]["access_token"],
        "fixture-partial"
    );
    let mut file = serde_json::to_value(export_accounts(&data, None).unwrap()).unwrap();
    let new_auth = auth_fixture("new");
    file["accounts"] = json!([
        {"kind":"claude","note":"First","token":"fixture-claude"},
        {"kind":"codex","note":"Invalid","token":"wrong-fallback","codexAuth":"broken"},
        {"kind":"codex","note":"Second","token":"fixture-access-a","codexAuth":local_crypto::seal(&new_auth.to_string()).unwrap()}
    ]);
    let (added, skipped) =
        merge_account_file(&mut data, parse_account_file(&file.to_string()).unwrap()).unwrap();
    assert_eq!(skipped[0].reason, "invalid");
    assert_eq!(
        added.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(),
        ["First", "Second"]
    );
    let order =
        crate::account_order::complete(data.account_order.as_ref().unwrap(), &data.accounts);
    assert_eq!(&order[..3], ["b", "c", "a"]);
    assert_eq!(
        &order[3..],
        [rows[0].id.clone(), added[0].id.clone(), added[1].id.clone()]
    );
    assert_eq!(data.accounts.last().unwrap().token, "fixture-access-new");
}

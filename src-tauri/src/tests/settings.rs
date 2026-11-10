//! 设置载入与备份格式的回归测试，使用生产环境的设置结构和归一化路径。

use super::*;

#[test]
fn opencode_failure_settings_mutation_keeps_memory_until_disk_save_succeeds() {
    let mut current: Settings = serde_json::from_value(serde_json::json!({
        "accounts": [{"id":"a","kind":"opencode","token":"wrk_a","note":"kept"}],
        "intervalMinutes":15
    })).unwrap();
    let before = serde_json::to_value(&current).unwrap();
    let result = mutate_saved(&mut current, |candidate| {
        candidate.accounts.clear();
        candidate.interval_minutes = 5;
        Ok(())
    }, |_| Err("settings_write_failed".into()));
    assert_eq!(result.unwrap_err(), "settings_write_failed");
    assert_eq!(serde_json::to_value(&current).unwrap(), before);

    let result: Result<(), String> = mutate_saved(&mut current, |candidate| {
        candidate.accounts.clear();
        Err("change_failed".into())
    }, |_| panic!("a failed change must never be persisted"));
    assert_eq!(result.unwrap_err(), "change_failed");
    assert_eq!(serde_json::to_value(&current).unwrap(), before);

    let saved = std::cell::RefCell::new(None);
    mutate_saved(&mut current, |candidate| {
        candidate.accounts.clear();
        Ok(())
    }, |candidate| {
        *saved.borrow_mut() = Some(serde_json::to_value(candidate).unwrap());
        Ok(())
    }).unwrap();
    assert!(current.accounts.is_empty());
    assert_eq!(saved.into_inner().unwrap(), serde_json::to_value(current).unwrap());
}

#[test]
fn official_urls_migrate_but_custom_sources_and_cached_prices_survive() {
    let legacy = "https://inf.xil.to/kv/MAA-Price-Table";
    let custom = "https://example.org/own-prices.json";
    for url in [
        "",
        "   ",
        legacy,
        crate::pricing::DEFAULT_UPDATE_URL,
        custom,
    ] {
        let data: Settings = serde_json::from_value(serde_json::json!({
            "pricingRemote":{"url":url,"fetchedAtMs":123,"table":{"note":"cached", "models":{"custom":{"input":2.0}}}},
            "pricing":{"note":"my note", "models":{"custom":{"input":7.0}}}
        })).unwrap();
        let loaded = sanitize_loaded(data);
        assert_eq!(
            loaded.pricing_remote.url,
            if url == custom { custom } else { "" }
        );
        assert_eq!(loaded.pricing_remote.fetched_at_ms, Some(123));
        assert_eq!(loaded.pricing_remote.table.models["custom"].input, 2.0);
        assert_eq!(loaded.pricing.models["custom"].input, 7.0);
        assert_eq!(loaded.pricing.note.as_deref(), Some("my note"));
        assert!(loaded.app_update.enabled);
    }
}

#[test]
fn old_backup_defaults_and_update_preferences_round_trip() {
    let mut data: Settings = serde_json::from_str("{}").unwrap();
    assert!(data.app_update.enabled);
    data.app_update.enabled = false;
    let exported = persisted_form(&data);
    let imported: Settings =
        serde_json::from_slice(&serde_json::to_vec(&exported).unwrap()).unwrap();
    assert!(!sanitize_loaded(imported).app_update.enabled);
}

//! 设置载入与备份格式的回归测试，使用生产环境的设置结构和归一化路径。

use super::*;

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

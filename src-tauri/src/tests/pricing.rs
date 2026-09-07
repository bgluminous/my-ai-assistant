//! pricing 模块的单元测试（由 pricing.rs 以 `#[path]` 挂载为 `crate::pricing::tests`）：
//! 按日 / 按小时聚合，以及默认价、在线价、用户改价三层价格表的叠加与差分。

use super::*;

// ---------------------------------------------------------------------------
// 聚合：按日 / 按模型 / 按小时
// ---------------------------------------------------------------------------

/// 测试用：按默认（今天 + 昨天）小时聚合。
fn aggregate_and_price(rows: Vec<TokenRow>, table: &PricingTable) -> UsageAggregate {
    aggregate_and_price_for(rows, table, None)
}

fn row(model: &str, tokens: f64, ms: Option<i64>) -> TokenRow {
    TokenRow {
        model: model.to_string(),
        input: tokens,
        output: 0.0,
        cache_read: 0.0,
        cache_write: 0.0,
        actual_cents: 0.0,
        timestamp_ms: ms,
    }
}

fn ms(rfc3339: &str) -> i64 {
    DateTime::parse_from_rfc3339(rfc3339).unwrap().timestamp_millis()
}

#[test]
fn same_day_merges() {
    let table = defaults();
    let a = ms("2026-06-15T12:00:00Z");
    let b = ms("2026-06-15T13:00:00Z");
    let agg = aggregate_and_price(vec![row("gpt-5", 100.0, Some(a)), row("gpt-5", 50.0, Some(b))], &table);
    assert_eq!(agg.daily.len(), 1);
    assert_eq!(agg.daily[0].tokens, 150.0);
    assert_eq!(agg.total_tokens, 150.0);
}

#[test]
fn different_days_split() {
    let table = defaults();
    let a = ms("2026-06-15T12:00:00Z");
    let b = ms("2026-06-22T12:00:00Z");
    let agg = aggregate_and_price(vec![row("gpt-5", 100.0, Some(a)), row("gpt-5", 50.0, Some(b))], &table);
    assert_eq!(agg.daily.len(), 2);
    assert!(agg.daily[0].date < agg.daily[1].date);
    assert_eq!(agg.daily[0].tokens, 100.0);
    assert_eq!(agg.daily[1].tokens, 50.0);
}

#[test]
fn same_day_models_split() {
    let table = defaults();
    let a = ms("2026-06-15T12:00:00Z");
    let agg = aggregate_and_price(
        vec![
            row("gpt-5", 100.0, Some(a)),
            row("claude-4-sonnet", 40.0, Some(a)),
        ],
        &table,
    );
    assert_eq!(agg.daily.len(), 1);
    assert_eq!(agg.daily[0].models.len(), 2);
    assert_eq!(agg.daily[0].models[0].model, "gpt-5");
    assert_eq!(agg.daily[0].models[0].tokens, 100.0);
    assert_eq!(agg.daily[0].models[1].model, "claude-4-sonnet");
    assert_eq!(agg.daily[0].models[1].tokens, 40.0);
}

#[test]
fn effort_levels_merge_into_base_model() {
    let table = defaults();
    let a = ms("2026-06-15T12:00:00Z");
    let agg = aggregate_and_price(
        vec![
            row("claude-4.5-sonnet-thinking", 100.0, Some(a)),
            row("claude-4.5-sonnet", 50.0, Some(a)),
        ],
        &table,
    );
    assert_eq!(agg.models.len(), 1);
    assert_eq!(agg.models[0].model, "claude-4.5-sonnet");
    assert_eq!(agg.models[0].total_tokens, 150.0);
    assert_eq!(agg.daily.len(), 1);
    assert_eq!(agg.daily[0].models.len(), 1);
    assert_eq!(agg.daily[0].models[0].tokens, 150.0);
}

#[test]
fn missing_timestamp_excluded_from_daily() {
    let table = defaults();
    let a = ms("2026-06-15T12:00:00Z");
    let agg = aggregate_and_price(vec![row("gpt-5", 100.0, Some(a)), row("gpt-5", 50.0, None)], &table);
    assert_eq!(agg.daily.len(), 1);
    assert_eq!(agg.daily[0].tokens, 100.0);
    assert_eq!(agg.total_tokens, 150.0);
}

#[test]
fn hourly_buckets_only_for_today_and_yesterday() {
    use chrono::Timelike;
    let table = defaults();
    let now = Local::now();
    let now_ms = now.timestamp_millis();
    // 昨天同一时刻：按日历日回退，取该日中午避开夏令时切换的边界小时
    let yesterday_noon = now
        .date_naive()
        .pred_opt()
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
        .and_local_timezone(Local)
        .single()
        .unwrap();
    let yesterday_ms = yesterday_noon.timestamp_millis();
    let old = ms("2026-06-15T12:00:00Z");
    let agg = aggregate_and_price(
        vec![
            row("gpt-5", 100.0, Some(now_ms)),
            row("gpt-5", 30.0, Some(yesterday_ms)),
            row("gpt-5", 50.0, Some(old)),
        ],
        &table,
    );
    // 今天与昨天的行进入 hourly（日期升序：昨天在前），更早的日期不进入
    assert_eq!(agg.hourly.len(), 2);
    let y = &agg.hourly[0];
    assert_eq!(y.date, yesterday_noon.format("%Y-%m-%d").to_string());
    assert_eq!(y.hour, 12);
    assert_eq!(y.tokens, 30.0);
    let h = &agg.hourly[1];
    assert_eq!(h.date, now.format("%Y-%m-%d").to_string());
    assert_eq!(h.hour, now.hour());
    assert_eq!(h.tokens, 100.0);
}

// ---------------------------------------------------------------------------
// 价格表分层：内置默认 → 在线更新 → 用户改价
// ---------------------------------------------------------------------------

fn price(input: f64, output: f64) -> ModelPrice {
    ModelPrice {
        input,
        output,
        cache_read: 0.0,
        cache_write: 0.0,
    }
}

fn table(note: Option<&str>, models: &[(&str, ModelPrice)]) -> PricingTable {
    PricingTable {
        note: note.map(str::to_string),
        models: models
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    }
}

fn entry(key: &str, p: &ModelPrice) -> PricingEntry {
    PricingEntry {
        key: key.to_string(),
        input: p.input,
        output: p.output,
        cache_read: p.cache_read,
        cache_write: p.cache_write,
        source: String::new(),
    }
}

#[test]
fn remote_layer_overrides_defaults_and_adds_models() {
    let d = defaults();
    let remote = table(
        Some("远端说明"),
        &[("gpt-5", price(9.0, 90.0)), ("brand-new", price(1.0, 2.0))],
    );
    let base = base_of(remote);
    assert_eq!(base.note.as_deref(), Some("远端说明"));
    assert!(price_eq(base.models.get("gpt-5").unwrap(), &price(9.0, 90.0)));
    assert!(base.models.contains_key("brand-new"));
    // 未覆盖的模型保持默认价，总数 = 默认 + 新增
    assert!(price_eq(
        base.models.get("gpt-4o").unwrap(),
        d.models.get("gpt-4o").unwrap()
    ));
    assert_eq!(base.models.len(), d.models.len() + 1);
}

#[test]
fn base_of_empty_equals_defaults() {
    assert!(tables_eq(&base_of(PricingTable::default()), &defaults()));
}

#[test]
fn remote_keys_lowercased() {
    let base = base_of(table(None, &[("GPT-5", price(9.0, 90.0))]));
    assert!(price_eq(base.models.get("gpt-5").unwrap(), &price(9.0, 90.0)));
}

#[test]
fn diff_overrides_keeps_only_changes() {
    let base = base_of(table(None, &[("remote-model", price(9.0, 90.0))]));
    let unchanged = base.models.get("gpt-5").unwrap().clone();
    let entries = vec![
        entry("gpt-5", &unchanged),               // 与默认一致 → 丢弃
        entry("remote-model", &price(9.0, 90.0)), // 与在线层一致 → 丢弃
        entry("my-custom", &price(1.0, 2.0)),     // 基础层没有 → 保留
        entry("  GPT-5 ", &price(0.5, 0.5)),      // 改价（键归一化）→ 保留
    ];
    let overrides = diff_overrides(&base, &entries).unwrap();
    assert_eq!(overrides.len(), 2);
    assert!(overrides.contains_key("my-custom"));
    assert!(price_eq(overrides.get("gpt-5").unwrap(), &price(0.5, 0.5)));
}

#[test]
fn diff_overrides_rejects_invalid_numbers() {
    let bad = ModelPrice {
        input: f64::NAN,
        ..ModelPrice::default()
    };
    assert!(diff_overrides(&defaults(), &[entry("x", &bad)]).is_err());
    let neg = ModelPrice {
        output: -1.0,
        ..ModelPrice::default()
    };
    assert!(diff_overrides(&defaults(), &[entry("y", &neg)]).is_err());
}

#[test]
fn update_check_equality_semantics() {
    // 远端内容与内置默认一致 → 视为无更新
    assert!(tables_eq(&base_of(defaults()), &base_of(PricingTable::default())));
    // 远端改价 → 有更新；应用（缓存同表）后再查 → 无更新
    let mut fetched = defaults();
    fetched.models.insert("gpt-5".into(), price(9.0, 90.0));
    assert!(!tables_eq(&base_of(fetched.clone()), &base_of(PricingTable::default())));
    assert!(tables_eq(&base_of(fetched.clone()), &base_of(fetched)));
}

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use tauri::AppHandle;

use crate::settings;

const DEFAULT_JSON: &str = include_str!("../resources/pricing.default.json");

/// 每百万 token 的美元单价。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelPrice {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default, rename = "cacheRead", alias = "cache_read")]
    pub cache_read: f64,
    #[serde(default, rename = "cacheWrite", alias = "cache_write")]
    pub cache_write: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PricingTable {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub models: HashMap<String, ModelPrice>,
}

impl PricingTable {
    pub fn lowercased(self) -> Self {
        let models = self
            .models
            .into_iter()
            .map(|(k, v)| (k.to_lowercase(), v))
            .collect();
        PricingTable {
            note: self.note,
            models,
        }
    }
}

pub fn defaults() -> PricingTable {
    serde_json::from_str::<PricingTable>(DEFAULT_JSON)
        .unwrap_or_default()
        .lowercased()
}

/// 加载“默认表 + 用户覆盖表”。
pub fn load() -> PricingTable {
    let mut table = defaults();
    if let Ok(user) = settings::read(|s| s.pricing.clone()) {
        let user = user.lowercased();
        if user.note.is_some() {
            table.note = user.note;
        }
        for (k, v) in user.models {
            table.models.insert(k, v);
        }
    }
    table
}

/// 聚合前的单条 token 记录（Cursor 逐事件 / Codex 逐轮）。
#[derive(Debug, Clone)]
pub struct TokenRow {
    pub model: String,
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    /// 实扣金额（分）。计划内用量或未知时为 0。
    pub actual_cents: f64,
    /// 事件时间（unix 毫秒）。缺失则计入模型合计，不进入按日序列。
    pub timestamp_ms: Option<i64>,
}

/// 某日某个模型的合计（供托盘「今日模型」等切片）。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DailyModelUsage {
    pub model: String,
    pub tokens: f64,
    pub equivalent_usd: f64,
}

/// 按本地自然日合计的用量（供前端堆叠柱图）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyUsage {
    pub date: String,
    pub tokens: f64,
    pub equivalent_usd: f64,
    pub actual_usd: f64,
    #[serde(default)]
    pub models: Vec<DailyModelUsage>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub model: String,
    pub priced: bool,
    pub priced_as: Option<String>,
    pub events: u64,
    pub input_tokens: f64,
    pub output_tokens: f64,
    pub cache_read_tokens: f64,
    pub cache_write_tokens: f64,
    pub total_tokens: f64,
    pub actual_usd: f64,
    pub equivalent_usd: f64,
    /// 按分类拆分的等价费用（美元）。
    pub input_usd: f64,
    pub output_usd: f64,
    pub cache_read_usd: f64,
    pub cache_write_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageAggregate {
    pub models: Vec<ModelUsage>,
    pub total_equivalent_usd: f64,
    pub total_actual_usd: f64,
    pub total_tokens: f64,
    pub priced_models: usize,
    pub unpriced_models: usize,
    pub unpriced_tokens: f64,
    /// 全部模型按分类合计的等价费用（美元）。
    pub total_input_usd: f64,
    pub total_output_usd: f64,
    pub total_cache_read_usd: f64,
    pub total_cache_write_usd: f64,
    /// 按本地自然日合计，日期升序。无时间戳的行不在此列。
    pub daily: Vec<DailyUsage>,
}

fn local_ymd(ms: i64) -> Option<String> {
    DateTime::from_timestamp_millis(ms).map(|dt| dt.with_timezone(&Local).format("%Y-%m-%d").to_string())
}

fn row_equivalent_usd(row: &TokenRow, table: &PricingTable) -> f64 {
    match crate::model_match::resolve(table, &row.model) {
        Some((_, price)) => {
            row.input * price.input / 1_000_000.0
                + row.output * price.output / 1_000_000.0
                + row.cache_read * price.cache_read / 1_000_000.0
                + row.cache_write * price.cache_write / 1_000_000.0
        }
        None => 0.0,
    }
}

pub fn aggregate_and_price(rows: Vec<TokenRow>, table: &PricingTable) -> UsageAggregate {
    let mut map: BTreeMap<String, ModelUsage> = BTreeMap::new();
    let mut daily_map: BTreeMap<String, DailyUsage> = BTreeMap::new();
    let mut daily_models: BTreeMap<String, BTreeMap<String, DailyModelUsage>> = BTreeMap::new();
    for r in rows {
        let entry = map.entry(r.model.clone()).or_insert_with(|| ModelUsage {
            model: r.model.clone(),
            priced: false,
            priced_as: None,
            events: 0,
            input_tokens: 0.0,
            output_tokens: 0.0,
            cache_read_tokens: 0.0,
            cache_write_tokens: 0.0,
            total_tokens: 0.0,
            actual_usd: 0.0,
            equivalent_usd: 0.0,
            input_usd: 0.0,
            output_usd: 0.0,
            cache_read_usd: 0.0,
            cache_write_usd: 0.0,
        });
        entry.events += 1;
        entry.input_tokens += r.input;
        entry.output_tokens += r.output;
        entry.cache_read_tokens += r.cache_read;
        entry.cache_write_tokens += r.cache_write;
        entry.actual_usd += r.actual_cents / 100.0;

        if let Some(date) = r.timestamp_ms.and_then(local_ymd) {
            let tokens = r.input + r.output + r.cache_read + r.cache_write;
            let equivalent_usd = row_equivalent_usd(&r, table);
            let actual_usd = r.actual_cents / 100.0;
            let day = daily_map.entry(date.clone()).or_insert_with_key(|k| DailyUsage {
                date: k.clone(),
                tokens: 0.0,
                equivalent_usd: 0.0,
                actual_usd: 0.0,
                models: Vec::new(),
            });
            day.tokens += tokens;
            day.equivalent_usd += equivalent_usd;
            day.actual_usd += actual_usd;
            let model_entry = daily_models
                .entry(date)
                .or_default()
                .entry(r.model.clone())
                .or_insert_with(|| DailyModelUsage {
                    model: r.model.clone(),
                    tokens: 0.0,
                    equivalent_usd: 0.0,
                });
            model_entry.tokens += tokens;
            model_entry.equivalent_usd += equivalent_usd;
        }
    }

    let mut total_equivalent_usd = 0.0;
    let mut total_actual_usd = 0.0;
    let mut total_tokens = 0.0;
    let mut priced_models = 0usize;
    let mut unpriced_models = 0usize;
    let mut unpriced_tokens = 0.0;
    let mut total_input_usd = 0.0;
    let mut total_output_usd = 0.0;
    let mut total_cache_read_usd = 0.0;
    let mut total_cache_write_usd = 0.0;
    let mut models: Vec<ModelUsage> = Vec::with_capacity(map.len());

    for (_, mut m) in map {
        m.total_tokens =
            m.input_tokens + m.output_tokens + m.cache_read_tokens + m.cache_write_tokens;
        if let Some((key, price)) = crate::model_match::resolve(table, &m.model) {
            m.priced = true;
            m.priced_as = Some(key.to_string());
            m.input_usd = m.input_tokens * price.input / 1_000_000.0;
            m.output_usd = m.output_tokens * price.output / 1_000_000.0;
            m.cache_read_usd = m.cache_read_tokens * price.cache_read / 1_000_000.0;
            m.cache_write_usd = m.cache_write_tokens * price.cache_write / 1_000_000.0;
            m.equivalent_usd =
                m.input_usd + m.output_usd + m.cache_read_usd + m.cache_write_usd;
            priced_models += 1;
        } else {
            unpriced_models += 1;
            unpriced_tokens += m.total_tokens;
        }
        total_equivalent_usd += m.equivalent_usd;
        total_actual_usd += m.actual_usd;
        total_tokens += m.total_tokens;
        total_input_usd += m.input_usd;
        total_output_usd += m.output_usd;
        total_cache_read_usd += m.cache_read_usd;
        total_cache_write_usd += m.cache_write_usd;
        models.push(m);
    }

    models.sort_by(|a, b| {
        b.equivalent_usd
            .partial_cmp(&a.equivalent_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                b.total_tokens
                    .partial_cmp(&a.total_tokens)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });

    UsageAggregate {
        models,
        total_equivalent_usd,
        total_actual_usd,
        total_tokens,
        priced_models,
        unpriced_models,
        unpriced_tokens,
        total_input_usd,
        total_output_usd,
        total_cache_read_usd,
        total_cache_write_usd,
        daily: daily_map
            .into_iter()
            .map(|(date, mut day)| {
                let mut models: Vec<DailyModelUsage> = daily_models
                    .remove(&date)
                    .unwrap_or_default()
                    .into_values()
                    .collect();
                models.sort_by(|a, b| {
                    b.tokens
                        .partial_cmp(&a.tokens)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(
                            b.equivalent_usd
                                .partial_cmp(&a.equivalent_usd)
                                .unwrap_or(std::cmp::Ordering::Equal),
                        )
                });
                day.models = models;
                day
            })
            .collect(),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PricingStatus {
    pub path: String,
    pub models: usize,
    pub note: Option<String>,
}

/// 前端可编辑的单条模型价格（camelCase 与前端约定一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PricingEntry {
    pub key: String,
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default, alias = "cache_read")]
    pub cache_read: f64,
    #[serde(default, alias = "cache_write")]
    pub cache_write: f64,
    /// 是否相对内置默认表有改动（新增或改价）。仅用于展示，保存时忽略。
    #[serde(default)]
    pub custom: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PricingView {
    pub path: String,
    pub note: Option<String>,
    pub count: usize,
    pub models: Vec<PricingEntry>,
}

fn price_eq(a: &ModelPrice, b: &ModelPrice) -> bool {
    let eq = |x: f64, y: f64| (x - y).abs() < 1e-9;
    eq(a.input, b.input) && eq(a.output, b.output) && eq(a.cache_read, b.cache_read)
        && eq(a.cache_write, b.cache_write)
}

/// 读取“默认 + 用户覆盖”的有效价格表，按键排序返回，供应用内编辑。
#[tauri::command]
pub fn pricing_get(_app: AppHandle) -> Result<PricingView, String> {
    settings::ensure_loaded()?;
    let defaults = defaults();
    let effective = load();
    let mut models: Vec<PricingEntry> = effective
        .models
        .iter()
        .map(|(k, v)| {
            let custom = match defaults.models.get(k) {
                Some(d) => !price_eq(d, v),
                None => true,
            };
            PricingEntry {
                key: k.clone(),
                input: v.input,
                output: v.output,
                cache_read: v.cache_read,
                cache_write: v.cache_write,
                custom,
            }
        })
        .collect();
    models.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(PricingView {
        path: settings::path_display(),
        note: effective.note,
        count: models.len(),
        models,
    })
}

/// 保存用户价格表：只把“与默认不同 / 默认表没有”的条目写入用户文件（保持精简，
/// 未改动的模型仍随默认表更新）。数值需为有限非负数。
#[tauri::command]
pub fn pricing_save(
    _app: AppHandle,
    models: Vec<PricingEntry>,
    note: Option<String>,
) -> Result<PricingStatus, String> {
    let defaults = defaults();
    let mut overrides = HashMap::new();
    for m in &models {
        let key = m.key.trim().to_lowercase();
        if key.is_empty() {
            continue;
        }
        for val in [m.input, m.output, m.cache_read, m.cache_write] {
            if !val.is_finite() || val < 0.0 {
                return Err(format!("invalid_price:{key}"));
            }
        }
        let candidate = ModelPrice {
            input: m.input,
            output: m.output,
            cache_read: m.cache_read,
            cache_write: m.cache_write,
        };
        // 与默认完全一致的条目无需写入（避免锁死默认表后续更新）。
        if defaults.models.get(&key).map(|d| price_eq(d, &candidate)).unwrap_or(false) {
            continue;
        }
        overrides.insert(key, candidate);
    }

    let note = note.and_then(|n| {
        let n = n.trim().to_string();
        if n.is_empty() { None } else { Some(n) }
    });
    settings::mutate(|s| {
        s.pricing = PricingTable {
            note,
            models: overrides,
        };
        Ok(())
    })?;

    let table = load();
    Ok(PricingStatus {
        path: settings::path_display(),
        models: table.models.len(),
        note: table.note,
    })
}

/// 清空用户价格覆盖，恢复为内置默认表（不删除 settings.json）。
#[tauri::command]
pub fn pricing_reset(app: AppHandle) -> Result<PricingView, String> {
    settings::mutate(|s| {
        s.pricing = PricingTable::default();
        Ok(())
    })?;
    pricing_get(app)
}

#[cfg(test)]
mod daily_tests {
    use super::*;

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
    fn missing_timestamp_excluded_from_daily() {
        let table = defaults();
        let a = ms("2026-06-15T12:00:00Z");
        let agg = aggregate_and_price(vec![row("gpt-5", 100.0, Some(a)), row("gpt-5", 50.0, None)], &table);
        assert_eq!(agg.daily.len(), 1);
        assert_eq!(agg.daily[0].tokens, 100.0);
        assert_eq!(agg.total_tokens, 150.0);
    }
}

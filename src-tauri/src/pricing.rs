use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use crate::audit;
use crate::settings;

const DEFAULT_JSON: &str = include_str!("../resources/pricing.default.json");

/// 在线价格表默认更新地址（用户未自定义地址时使用）。
pub const DEFAULT_UPDATE_URL: &str = "https://inf.xil.to/kv/MAA-Price-Table";

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

/// 在线价格表缓存层：「在线更新」成功后写入 settings.json，
/// 生效顺序位于内置默认表之上、用户覆盖之下。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PricingRemote {
    /// 更新地址；为空时使用 DEFAULT_UPDATE_URL。
    #[serde(default)]
    pub url: String,
    /// 上次成功更新时间（unix 毫秒）。None 表示从未在线更新。
    #[serde(default)]
    pub fetched_at_ms: Option<i64>,
    /// 已应用的在线表。
    #[serde(default)]
    pub table: PricingTable,
}

pub fn defaults() -> PricingTable {
    serde_json::from_str::<PricingTable>(DEFAULT_JSON)
        .unwrap_or_default()
        .lowercased()
}

/// 把一层表叠加到 base 上：note 为 Some 时覆盖，models 同键覆盖。
fn overlay(base: &mut PricingTable, layer: PricingTable) {
    if layer.note.is_some() {
        base.note = layer.note;
    }
    for (k, v) in layer.models {
        base.models.insert(k, v);
    }
}

/// 基础层 = 内置默认表 + 给定在线表（纯函数，便于测试）。
fn base_of(remote: PricingTable) -> PricingTable {
    let mut table = defaults();
    overlay(&mut table, remote.lowercased());
    table
}

/// 基础层 = 内置默认表 + 已缓存的在线表（用户覆盖判定与保存都以此为基准）。
pub fn base() -> PricingTable {
    base_of(settings::read(|s| s.pricing_remote.table.clone()).unwrap_or_default())
}

/// 加载“默认表 + 在线表 + 用户覆盖表”。
pub fn load() -> PricingTable {
    let (remote, user) = settings::read(|s| (s.pricing_remote.table.clone(), s.pricing.clone()))
        .unwrap_or_default();
    let mut table = base_of(remote);
    overlay(&mut table, user.lowercased());
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

/// 聚合执行日（本地时区）内某小时的合计，供「今天」跨度的 24 小时柱图。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HourlyUsage {
    /// 本地日 YYYY-MM-DD；跨零点后前端据此丢弃旧数据。
    pub date: String,
    /// 本地小时 0-23。
    pub hour: u32,
    pub tokens: f64,
    pub equivalent_usd: f64,
    pub actual_usd: f64,
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
    /// 按小时合计，按日期、小时升序，仅含有数据的小时。查询区间不超过两天时覆盖区间内的日期，
    /// 否则只记聚合执行日的今天与昨天（供托盘与单日视图的 24h 柱图）；前端按 date 取所需的那一天。
    pub hourly: Vec<HourlyUsage>,
}

/// 时间戳 → 本地（日期, 小时）。
fn local_ymd_hour(ms: i64) -> Option<(String, u32)> {
    use chrono::Timelike;
    DateTime::from_timestamp_millis(ms).map(|dt| {
        let local = dt.with_timezone(&Local);
        (local.format("%Y-%m-%d").to_string(), local.hour())
    })
}

/// 单条记录的计价：命中的价格表键与按该单价折算的等价费用（美元）；未定价为 None。
pub fn price_row<'a>(row: &TokenRow, table: &'a PricingTable) -> Option<(&'a str, f64)> {
    let (key, price) = crate::model_match::resolve(table, &row.model)?;
    let usd = row.input * price.input / 1_000_000.0
        + row.output * price.output / 1_000_000.0
        + row.cache_read * price.cache_read / 1_000_000.0
        + row.cache_write * price.cache_write / 1_000_000.0;
    Some((key, usd))
}

fn row_equivalent_usd(row: &TokenRow, table: &PricingTable) -> f64 {
    price_row(row, table).map_or(0.0, |(_, usd)| usd)
}

/// 按小时聚合最多覆盖的区间长度（毫秒）：不超过两天的查询才给出 24 小时分布。
const HOURLY_MAX_SPAN_MS: i64 = 2 * 86_400_000 + 60_000;

/// 统计区间 [start_ms, end_ms]（unix 毫秒，闭区间）内需要按小时聚合的本地日期列表。
/// 任一端缺失或跨度超过两天返回 None（调用方退回默认的今天 + 昨天）。
pub fn hourly_dates_for(start_ms: Option<i64>, end_ms: Option<i64>) -> Option<Vec<String>> {
    let (start, end) = (start_ms?, end_ms?);
    if end < start || end - start > HOURLY_MAX_SPAN_MS {
        return None;
    }
    let first = DateTime::from_timestamp_millis(start)?.with_timezone(&Local).date_naive();
    let last = DateTime::from_timestamp_millis(end)?.with_timezone(&Local).date_naive();
    let mut dates = Vec::new();
    let mut day = first;
    while day <= last && dates.len() < 4 {
        dates.push(day.format("%Y-%m-%d").to_string());
        day = day.succ_opt()?;
    }
    Some(dates)
}

/// 默认按小时聚合的日期：今天与昨天（昨天用日历日回退而非减 24 小时，夏令时切换日也不会算错）。
fn default_hourly_dates() -> Vec<String> {
    let today = Local::now().date_naive();
    let mut dates = vec![today.format("%Y-%m-%d").to_string()];
    if let Some(y) = today.pred_opt() {
        dates.push(y.format("%Y-%m-%d").to_string());
    }
    dates
}

/// 聚合并折算等价费用。hourly_dates 指定需要按小时聚合的本地日期（任意日期，最多几天），
/// None 时退回今天 + 昨天。
pub fn aggregate_and_price_for(
    rows: Vec<TokenRow>,
    table: &PricingTable,
    hourly_dates: Option<&[String]>,
) -> UsageAggregate {
    let mut map: BTreeMap<String, ModelUsage> = BTreeMap::new();
    let mut daily_map: BTreeMap<String, DailyUsage> = BTreeMap::new();
    let mut daily_models: BTreeMap<String, BTreeMap<String, DailyModelUsage>> = BTreeMap::new();
    // 按小时合计：(date, hour) -> (tokens, equivalent_usd, actual_usd)，只记 hourly_dates 里的日期
    let hourly_dates: Vec<String> = match hourly_dates {
        Some(list) => list.to_vec(),
        None => default_hourly_dates(),
    };
    let mut hourly_map: BTreeMap<(String, u32), (f64, f64, f64)> = BTreeMap::new();
    for mut r in rows {
        // 同一模型的不同思考 / 效率等级并入一行统计（价格表精确收录的名字保持独立）
        r.model = crate::model_match::display_key(table, &r.model);
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

        if let Some((date, hour)) = r.timestamp_ms.and_then(local_ymd_hour) {
            let tokens = r.input + r.output + r.cache_read + r.cache_write;
            let equivalent_usd = row_equivalent_usd(&r, table);
            let actual_usd = r.actual_cents / 100.0;
            if hourly_dates.contains(&date) {
                let slot = hourly_map
                    .entry((date.clone(), hour))
                    .or_insert((0.0, 0.0, 0.0));
                slot.0 += tokens;
                slot.1 += equivalent_usd;
                slot.2 += actual_usd;
            }
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
        hourly: hourly_map
            .into_iter()
            .map(|((date, hour), (tokens, equivalent_usd, actual_usd))| HourlyUsage {
                date,
                hour,
                tokens,
                equivalent_usd,
                actual_usd,
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
    /// 价格来源："default"（内置）/ "remote"（在线表）/ "custom"（用户改动）。
    /// 仅用于展示，保存时忽略。
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PricingView {
    pub path: String,
    pub note: Option<String>,
    pub count: usize,
    pub models: Vec<PricingEntry>,
    /// 用户保存的更新地址（空 = 使用默认地址）。
    pub remote_url: String,
    /// 内置默认更新地址（供前端展示 placeholder）。
    pub remote_default_url: String,
    /// 上次在线更新时间（unix 毫秒）；None 表示从未更新。
    pub remote_fetched_at_ms: Option<i64>,
    /// 在线表缓存层的条目数。
    pub remote_models: usize,
}

fn price_eq(a: &ModelPrice, b: &ModelPrice) -> bool {
    let eq = |x: f64, y: f64| (x - y).abs() < 1e-9;
    eq(a.input, b.input) && eq(a.output, b.output) && eq(a.cache_read, b.cache_read)
        && eq(a.cache_write, b.cache_write)
}

/// 读取“默认 + 在线 + 用户覆盖”的有效价格表，按键排序返回，供应用内编辑。
#[tauri::command]
pub fn pricing_get() -> Result<PricingView, String> {
    settings::ensure_loaded()?;
    let defaults = defaults();
    let base = base();
    let effective = load();
    let remote = settings::read(|s| s.pricing_remote.clone())?;
    let mut models: Vec<PricingEntry> = effective
        .models
        .iter()
        .map(|(k, v)| {
            // 与基础层（默认+在线）不同 → 用户改动；否则与内置默认不同 → 来自在线表。
            let source = if base.models.get(k).map(|b| price_eq(b, v)) != Some(true) {
                "custom"
            } else if defaults.models.get(k).map(|d| price_eq(d, v)) != Some(true) {
                "remote"
            } else {
                "default"
            };
            PricingEntry {
                key: k.clone(),
                input: v.input,
                output: v.output,
                cache_read: v.cache_read,
                cache_write: v.cache_write,
                source: source.to_string(),
            }
        })
        .collect();
    models.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(PricingView {
        path: settings::path_display(),
        note: effective.note,
        count: models.len(),
        models,
        remote_url: remote.url,
        remote_default_url: DEFAULT_UPDATE_URL.to_string(),
        remote_fetched_at_ms: remote.fetched_at_ms,
        remote_models: remote.table.models.len(),
    })
}

/// 计算相对基础层的用户覆盖：丢弃与基础层一致的条目，数值需为有限非负数。
fn diff_overrides(
    base: &PricingTable,
    models: &[PricingEntry],
) -> Result<HashMap<String, ModelPrice>, String> {
    let mut overrides = HashMap::new();
    for m in models {
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
        // 与基础层（默认+在线）完全一致的条目无需写入（避免锁死后续更新）。
        if base.models.get(&key).map(|d| price_eq(d, &candidate)).unwrap_or(false) {
            continue;
        }
        overrides.insert(key, candidate);
    }
    Ok(overrides)
}

/// 保存用户价格表：只把“与基础层（默认+在线）不同 / 基础层没有”的条目写入用户文件
/// （保持精简，未改动的模型仍随默认表与在线表更新）。数值需为有限非负数。
#[tauri::command]
pub fn pricing_save(models: Vec<PricingEntry>, note: Option<String>) -> Result<PricingStatus, String> {
    settings::ensure_loaded()?;
    let overrides = diff_overrides(&base(), &models)?;

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

/// 清空用户价格覆盖与在线表缓存，恢复为内置默认表（不删除 settings.json）。
#[tauri::command]
pub fn pricing_reset() -> Result<PricingView, String> {
    settings::mutate(|s| {
        s.pricing = PricingTable::default();
        s.pricing_remote = PricingRemote::default();
        Ok(())
    })?;
    pricing_get()
}

// ---------- 在线更新 ----------

/// 两张表内容一致（note 相同、models 键集相同且各价格相等）。
fn tables_eq(a: &PricingTable, b: &PricingTable) -> bool {
    a.note == b.note
        && a.models.len() == b.models.len()
        && a
            .models
            .iter()
            .all(|(k, v)| b.models.get(k).map(|o| price_eq(v, o)).unwrap_or(false))
}

/// 解析更新地址：None → 用已保存地址；空白或等于默认地址 → 默认地址（保存为空，
/// 以便默认地址将来变化时自动跟随）。返回（请求地址, 落盘地址）。
fn resolve_update_url(url: Option<String>) -> Result<(String, String), String> {
    let raw = match url {
        Some(u) => u.trim().to_string(),
        None => settings::read(|s| s.pricing_remote.url.trim().to_string())?,
    };
    if raw.is_empty() || raw == DEFAULT_UPDATE_URL {
        return Ok((DEFAULT_UPDATE_URL.to_string(), String::new()));
    }
    if !raw.starts_with("http://") && !raw.starts_with("https://") {
        return Err("invalid_update_url".into());
    }
    Ok((raw.clone(), raw))
}

/// 拉取并校验在线价格表，返回小写化后的表。
async fn fetch_remote_table(url: &str) -> Result<PricingTable, String> {
    let resp = crate::http::client()
        .get(url)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("fetch_failed: {e}"))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("http_status_{status}"));
    }
    let text = resp.text().await.map_err(|e| format!("read_failed: {e}"))?;
    let table = serde_json::from_str::<PricingTable>(&text)
        .map_err(|e| format!("parse_failed: {e}"))?
        .lowercased();
    if table.models.is_empty() {
        return Err("remote_table_empty".into());
    }
    for (k, p) in &table.models {
        for val in [p.input, p.output, p.cache_read, p.cache_write] {
            if !val.is_finite() || val < 0.0 {
                return Err(format!("invalid_price:{k}"));
            }
        }
    }
    Ok(table)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PricingUpdateCheck {
    /// 应用远端表后基础层是否会变化。
    pub has_update: bool,
    /// 远端表条目数。
    pub models: usize,
    /// 实际请求的地址。
    pub url: String,
}

/// 拉取远端表并与当前基础层（默认+已缓存在线表）比较，判断是否有更新。不写盘。
#[tauri::command]
pub async fn pricing_update_check(url: Option<String>) -> Result<PricingUpdateCheck, String> {
    settings::ensure_loaded()?;
    let (fetch_url, _) = resolve_update_url(url)?;
    let fetched = fetch_remote_table(&fetch_url).await?;
    let models = fetched.models.len();
    let has_update = !tables_eq(&base_of(fetched), &base());
    Ok(PricingUpdateCheck {
        has_update,
        models,
        url: fetch_url,
    })
}

/// 拉取远端表并写入在线缓存层（用户覆盖保持不变），返回最新视图。
#[tauri::command]
pub async fn pricing_update_apply(url: Option<String>) -> Result<PricingView, String> {
    settings::ensure_loaded()?;
    let (fetch_url, store_url) = resolve_update_url(url)?;
    let fetched = fetch_remote_table(&fetch_url).await?;
    let count = fetched.models.len();
    settings::mutate(|s| {
        s.pricing_remote = PricingRemote {
            url: store_url,
            fetched_at_ms: Some(Utc::now().timestamp_millis()),
            table: fetched,
        };
        Ok(())
    })?;
    audit::log(
        "pricing_update",
        format!("在线价格表已更新：{count} 个模型（{fetch_url}）"),
        None,
    );
    pricing_get()
}

#[cfg(test)]
#[path = "tests/pricing.rs"]
mod tests;

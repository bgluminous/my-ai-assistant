use crate::pricing::{ModelPrice, PricingTable};

/// 分发方/路由前缀：Cursor 会把部分第三方模型上报为 `cursor-grok-4.6-high-fast` 这类名字，
/// 剥离前缀后再按官方模型名匹配价格。
const PROVIDER_PREFIXES: &[&str] = &["cursor-"];

const EFFORT_SUFFIXES: &[&str] = &[
    "-xhigh-thinking",
    "-high-thinking",
    "-medium-thinking",
    "-low-thinking",
    "-minimal-thinking",
    "-max-thinking",
    "-thinking",
    "-reasoning",
    "-xhigh-fast",
    "-high-fast",
    "-xhigh",
    "-high",
    "-medium",
    "-low",
    "-minimal",
    "-max",
    "-fast",
    "-latest",
];

/// 把 Cursor / Codex 上报的原始模型名归一到价格表里的键。
/// 先用原始名走一遍匹配流程；失败后剥离已知分发前缀（如 cursor-）再试一遍。
pub fn resolve<'a>(table: &'a PricingTable, raw: &str) -> Option<(&'a str, &'a ModelPrice)> {
    let key = raw.trim().to_lowercase();
    if key.is_empty() {
        return None;
    }
    if let Some(hit) = resolve_one(table, &key) {
        return Some(hit);
    }
    for prefix in PROVIDER_PREFIXES {
        if let Some(stripped) = key.strip_prefix(prefix) {
            if !stripped.is_empty() {
                if let Some(hit) = resolve_one(table, stripped) {
                    return Some(hit);
                }
            }
        }
    }
    None
}

/// 纯数字且不超过 3 位的段视为版本小数段（`5-1` 的 `1`、`4-20` 的 `20`）；
/// 更长的纯数字段（`2025`、`20251001`）视为日期 / 快照段，不算版本小数。
fn is_version_fragment(segment: &str) -> bool {
    !segment.is_empty() && segment.len() <= 3 && segment.bytes().all(|b| b.is_ascii_digit())
}

fn ends_with_digit(s: &str) -> bool {
    s.bytes().last().is_some_and(|b| b.is_ascii_digit())
}

/// 单个候选名的匹配流程：精确匹配 -> 去掉思考/努力度后缀 -> 逐段去尾 -> 前缀匹配兜底。
/// 去尾与兜底均带版本截断防护，避免 5.1 / 5-1 这类小版本被按 5 计价。
fn resolve_one<'a>(table: &'a PricingTable, key: &str) -> Option<(&'a str, &'a ModelPrice)> {
    if let Some(hit) = get(table, key) {
        return Some(hit);
    }

    let mut base = key.to_string();
    let mut changed = true;
    while changed {
        changed = false;
        for suffix in EFFORT_SUFFIXES {
            if base.len() > suffix.len() && base.ends_with(suffix) {
                base.truncate(base.len() - suffix.len());
                changed = true;
            }
        }
    }
    if let Some(hit) = get(table, &base) {
        return Some(hit);
    }

    let mut segments: Vec<&str> = base.split('-').collect();
    while segments.len() > 1 {
        let removed = segments.pop().unwrap_or_default();
        let candidate = segments.join("-");
        // 版本截断防护：候选以数字结尾且刚移除的是短数字段时，截点落在 `5-1`
        // 这类版本号中间（claude-fable-5-1 不得按 claude-fable-5 计价），跳过该候选；
        // 移除日期 / 快照段（gpt-5-2025-08-07 的 2025）则照常回退。
        if is_version_fragment(removed) && ends_with_digit(&candidate) {
            continue;
        }
        if let Some(hit) = get(table, &candidate) {
            return Some(hit);
        }
    }

    // 最后兜底：价格表里若有某键是候选名的前缀，则采用它（取最长前缀）。
    // 键后必须是 `-` 且不落在版本号中间：claude-fable-5 不得命中
    // claude-fable-5.1-* / claude-fable-5-1-*，但 gpt-4o 仍可命中 gpt-4o-2024-08-06。
    let mut best: Option<(&str, &ModelPrice)> = None;
    for (k, v) in &table.models {
        if k.is_empty() || !key.starts_with(k.as_str()) {
            continue;
        }
        let rest = &key[k.len()..];
        if !rest.is_empty() {
            let Some(after) = rest.strip_prefix('-') else {
                continue;
            };
            let next_segment = after.split('-').next().unwrap_or_default();
            if ends_with_digit(k) && is_version_fragment(next_segment) {
                continue;
            }
        }
        match best {
            Some((bk, _)) if bk.len() >= k.len() => {}
            _ => best = Some((k.as_str(), v)),
        }
    }
    best
}

fn get<'a>(table: &'a PricingTable, key: &str) -> Option<(&'a str, &'a ModelPrice)> {
    table.models.get_key_value(key).map(|(k, v)| (k.as_str(), v))
}

/// 模型名里数字之间的 - / _ 归一为点号：claude-fable-5-1 / claude-fable-5_1 → claude-fable-5.1，
/// claude-3-5-sonnet → claude-3.5-sonnet。无可归一处返回 None。
/// 仅当点号形式确实存在于价格表时才会被采用（见 prefer_dotted），
/// 因此 grok-code-fast-1、gpt-5-2025-08-07 这类名字不受影响。
fn dotted_alias(key: &str) -> Option<String> {
    let chars: Vec<char> = key.chars().collect();
    let mut out = String::with_capacity(key.len());
    let mut changed = false;
    for i in 0..chars.len() {
        let c = chars[i];
        if (c == '-' || c == '_')
            && i > 0
            && i + 1 < chars.len()
            && chars[i - 1].is_ascii_digit()
            && chars[i + 1].is_ascii_digit()
        {
            out.push('.');
            changed = true;
        } else {
            out.push(c);
        }
    }
    if changed { Some(out) } else { None }
}

/// 若点号规范形式在价格表中存在，优先用它作为展示 / 计价键
/// （表里 5.1 与 5-1 互为别名同价时，统一展示为 5.1）。
fn prefer_dotted(table: &PricingTable, key: String) -> String {
    if let Some(dotted) = dotted_alias(&key) {
        if table.models.contains_key(&dotted) {
            return dotted;
        }
    }
    key
}

/// 聚合展示用的模型名归一：同一模型的不同思考 / 效率等级并入一行。
/// 价格表精确收录的名字（如 grok-4-fast、grok-code-fast-1 这类独立计价的真实模型）
/// 原样保留；其余剥离分发前缀与思考 / 效率后缀，剥离途中一旦命中价格表键即停在该键，
/// 保证归并结果与计价口径一致。日期 / 快照段（gpt-5-2025-08-07）不在剥离之列，保持独立。
/// 命中的键与未命中的兜底名都会尝试点号规范（5-1 / 5_1 → 5.1，仅当点号键在表内）。
pub fn display_key(table: &PricingTable, raw: &str) -> String {
    let mut key = raw.trim().to_lowercase();
    if key.is_empty() {
        return "unknown".into();
    }
    if table.models.contains_key(&key) {
        return prefer_dotted(table, key);
    }
    for prefix in PROVIDER_PREFIXES {
        if let Some(stripped) = key.strip_prefix(prefix) {
            if !stripped.is_empty() {
                key = stripped.to_string();
                if table.models.contains_key(&key) {
                    return prefer_dotted(table, key);
                }
                break;
            }
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for suffix in EFFORT_SUFFIXES {
            if key.len() > suffix.len() && key.ends_with(suffix) {
                key.truncate(key.len() - suffix.len());
                if table.models.contains_key(&key) {
                    return prefer_dotted(table, key);
                }
                changed = true;
            }
        }
    }
    // 全程未命中：点号规范形式在表内则采用（claude-3-5-sonnet → claude-3.5-sonnet）
    if let Some(dotted) = dotted_alias(&key) {
        if table.models.contains_key(&dotted) {
            return dotted;
        }
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> PricingTable {
        crate::pricing::defaults()
    }

    #[test]
    fn cursor_prefixed_grok_models_resolve() {
        let t = table();
        // Cursor 上报的带 cursor- 前缀、含 effort 后缀的 grok 模型名都应能归一到价格表
        for (raw, expect) in [
            ("cursor-grok-4.6-high-fast", "grok-4.6"),
            ("cursor-grok-4.6-high", "grok-4.6"),
            ("cursor-grok-4.6-xhigh-fast", "grok-4.6"),
            ("cursor-grok-4.5-high", "grok-4.5"),
            ("cursor-grok-4.5-high-fast", "grok-4.5"),
        ] {
            let hit = resolve(&t, raw);
            assert!(hit.is_some(), "{raw} 应命中价格表");
            assert_eq!(hit.unwrap().0, expect, "{raw} 归一目标不对");
        }
    }

    #[test]
    fn plain_models_still_resolve() {
        let t = table();
        assert_eq!(resolve(&t, "grok-4.6").unwrap().0, "grok-4.6");
        assert!(resolve(&t, "gpt-5-codex-high").is_some());
    }

    #[test]
    fn fable_5_1_never_priced_as_fable_5() {
        let t = table();
        // 点号 / 连字符（官方 API ID）两种写法、带 effort 后缀与 cursor- 前缀都应命中 5.1
        for (raw, expect) in [
            ("claude-fable-5.1", "claude-fable-5.1"),
            ("claude-fable-5.1-thinking", "claude-fable-5.1"),
            ("claude-fable-5-1", "claude-fable-5-1"),
            ("claude-fable-5-1-thinking-max", "claude-fable-5-1"),
            ("cursor-claude-fable-5-1-high-thinking", "claude-fable-5-1"),
        ] {
            let hit = resolve(&t, raw);
            assert!(hit.is_some(), "{raw} 应命中价格表");
            assert_eq!(hit.unwrap().0, expect, "{raw} 归一目标不对");
        }
        // fable 5 本体不受影响
        assert_eq!(resolve(&t, "claude-fable-5").unwrap().0, "claude-fable-5");
        assert_eq!(resolve(&t, "claude-fable-5-thinking-max").unwrap().0, "claude-fable-5");
    }

    #[test]
    fn unknown_minor_version_stays_unpriced() {
        let t = table();
        // 表里没有的小版本不得回退按大版本计价，应保持“未定价”
        // （grok 系列除外：表里有裸 grok 兜底键，属有意设计）
        for raw in ["claude-fable-5.2", "claude-fable-5-2-thinking", "gpt-5.1", "claude-sonnet-5.1"] {
            assert!(resolve(&t, raw).is_none(), "{raw} 不应错配到旧版本价格");
        }
    }

    #[test]
    fn display_key_merges_effort_levels() {
        let t = table();
        // 思考 / 效率等级并入基础模型（含分发前缀与大小写归一）
        assert_eq!(display_key(&t, "claude-4.5-sonnet-thinking"), "claude-4.5-sonnet");
        assert_eq!(display_key(&t, "cursor-grok-4.6-high-fast"), "grok-4.6");
        assert_eq!(display_key(&t, "GPT-5-High"), "gpt-5");
        assert_eq!(display_key(&t, "gpt-5-codex-xhigh"), "gpt-5-codex");
    }

    #[test]
    fn display_key_keeps_exact_table_models() {
        let t = table();
        // 价格表精确收录的 fast 系列是独立计价模型，不得并入基础名
        assert_eq!(display_key(&t, "grok-4-fast"), "grok-4-fast");
        assert_eq!(display_key(&t, "grok-4.1-fast"), "grok-4.1-fast");
        assert_eq!(display_key(&t, "grok-code-fast-1"), "grok-code-fast-1");
        // 剥离途中命中表键即停：只去掉 -high，保留独立的 grok-4-fast
        assert_eq!(display_key(&t, "grok-4-fast-high"), "grok-4-fast");
    }

    #[test]
    fn display_key_prefers_dotted_minor_versions() {
        let t = table();
        // 连字符 / 下划线的小版本写法统一为点号形式展示与计价（表内存在点号键时）
        assert_eq!(display_key(&t, "claude-fable-5-1"), "claude-fable-5.1");
        assert_eq!(display_key(&t, "claude-fable-5_1"), "claude-fable-5.1");
        assert_eq!(display_key(&t, "claude-fable-5_1-thinking"), "claude-fable-5.1");
        assert_eq!(display_key(&t, "cursor-claude-fable-5-1-high-thinking"), "claude-fable-5.1");
        assert_eq!(display_key(&t, "claude-3-5-sonnet"), "claude-3.5-sonnet");
        // 点号形式不在表内的名字保持原样（fast 系列 / 日期快照不受影响）
        assert_eq!(display_key(&t, "grok-code-fast-1"), "grok-code-fast-1");
        assert_eq!(display_key(&t, "gpt-5-2025-08-07"), "gpt-5-2025-08-07");
    }

    #[test]
    fn display_key_unpriced_and_snapshots() {
        let t = table();
        // 未收录模型同样合并思考等级；日期快照不剥离，保持独立
        assert_eq!(display_key(&t, "my-model-xhigh-thinking"), "my-model");
        assert_eq!(display_key(&t, "gpt-5-2025-08-07"), "gpt-5-2025-08-07");
        assert_eq!(display_key(&t, "unknown"), "unknown");
    }

    #[test]
    fn date_snapshot_suffix_still_resolves() {
        let t = table();
        // 日期 / 快照后缀不是版本小数，仍应回退到基础键
        for (raw, expect) in [
            ("gpt-5-2025-08-07", "gpt-5"),
            ("o3-2025-04-16", "o3"),
            ("claude-sonnet-5-20260115", "claude-sonnet-5"),
        ] {
            assert_eq!(resolve(&t, raw).unwrap().0, expect, "{raw} 归一目标不对");
        }
    }
}

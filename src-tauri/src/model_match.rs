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

/// 段是否形如版本号（`5` / `4.8` / `4.20`）：按点拆开后逐段满足版本小数段规则，
/// 因此日期段（`20260528`）不会被当成版本。
fn is_version_segment(segment: &str) -> bool {
    !segment.is_empty() && segment.split('.').all(is_version_fragment)
}

/// Anthropic 系「版本-家族」词序互换写法：claude-4.8-opus ↔ claude-opus-4.8、
/// claude-5-sonnet ↔ claude-sonnet-5。仅当 claude- 后恰有一个版本段且位于首 / 尾时
/// 生成互换形式；版本段居中说明还挂着别的段（如未剥净的后缀），不在此处理。
fn claude_reordered(key: &str) -> Option<String> {
    let tail = key.strip_prefix("claude-")?;
    let segments: Vec<&str> = tail.split('-').collect();
    if segments.len() < 2 || segments.iter().filter(|s| is_version_segment(s)).count() != 1 {
        return None;
    }
    let last = segments.len() - 1;
    let mut reordered: Vec<&str> = Vec::with_capacity(segments.len());
    if is_version_segment(segments[0]) {
        reordered.extend_from_slice(&segments[1..]);
        reordered.push(segments[0]);
    } else if is_version_segment(segments[last]) {
        reordered.push(segments[last]);
        reordered.extend_from_slice(&segments[..last]);
    } else {
        return None;
    }
    Some(format!("claude-{}", reordered.join("-")))
}

/// 带等价写法归一的查表：原样 → 点号规范（5-1 / 5_1 → 5.1）→ claude 系词序互换。
/// 候选只用于查表，不在表内时无副作用。因此同一模型在价格表里只需收录一个规范名，
/// 点号 / 连字符 / 下划线与词序变体都会归到该键，无需成对维护别名条目。
fn get_normalized<'a>(table: &'a PricingTable, key: &str) -> Option<(&'a str, &'a ModelPrice)> {
    if let Some(hit) = get(table, key) {
        return Some(hit);
    }
    let dotted = dotted_alias(key);
    if let Some(d) = &dotted {
        if let Some(hit) = get(table, d) {
            return Some(hit);
        }
    }
    if let Some(swapped) = claude_reordered(dotted.as_deref().unwrap_or(key)) {
        if let Some(hit) = get(table, &swapped) {
            return Some(hit);
        }
    }
    None
}

fn ends_with_digit(s: &str) -> bool {
    s.bytes().last().is_some_and(|b| b.is_ascii_digit())
}

/// 单个候选名的匹配流程：精确匹配（含等价写法归一）-> 去掉思考/努力度后缀 ->
/// 逐段去尾 -> 前缀匹配兜底。去尾与兜底均带版本截断防护，
/// 避免 5.1 / 5-1 这类小版本被按 5 计价。
fn resolve_one<'a>(table: &'a PricingTable, key: &str) -> Option<(&'a str, &'a ModelPrice)> {
    if let Some(hit) = get_normalized(table, key) {
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
    if let Some(hit) = get_normalized(table, &base) {
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
        if let Some(hit) = get_normalized(table, &candidate) {
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
/// 仅当点号形式确实存在于价格表时才会被采用（见 get_normalized / prefer_dotted），
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

/// 聚合展示用的模型名归一：同一模型的不同思考 / 效率等级并入一行，
/// 点号 / 连字符 / 下划线与「版本-家族」词序变体归并为表内规范名
/// （claude-opus-4-8 / claude-opus-4.8 → claude-4.8-opus，同一模型不再拆行统计），
/// 保证归并结果与计价口径一致。价格表精确收录的名字（如 grok-4-fast、grok-code-fast-1
/// 这类独立计价的真实模型）原样保留；剥离分发前缀与思考 / 效率后缀途中一旦命中表键
/// 即停在该键。日期 / 快照段（gpt-5-2025-08-07）不在剥离之列，保持独立；
/// 全程未命中的名字保留剥离后的原样。
pub fn display_key(table: &PricingTable, raw: &str) -> String {
    let mut key = raw.trim().to_lowercase();
    if key.is_empty() {
        return "unknown".into();
    }
    if let Some((k, _)) = get_normalized(table, &key) {
        return prefer_dotted(table, k.to_string());
    }
    for prefix in PROVIDER_PREFIXES {
        if let Some(stripped) = key.strip_prefix(prefix) {
            if !stripped.is_empty() {
                key = stripped.to_string();
                if let Some((k, _)) = get_normalized(table, &key) {
                    return prefer_dotted(table, k.to_string());
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
                if let Some((k, _)) = get_normalized(table, &key) {
                    return prefer_dotted(table, k.to_string());
                }
                changed = true;
            }
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
        // 点号 / 连字符（官方 API ID）两种写法、带 effort 后缀与 cursor- 前缀
        // 都应命中 5.1（连字符写法自动归一到表内唯一的点号键）
        for (raw, expect) in [
            ("claude-fable-5.1", "claude-fable-5.1"),
            ("claude-fable-5.1-thinking", "claude-fable-5.1"),
            ("claude-fable-5-1", "claude-fable-5.1"),
            ("claude-fable-5-1-thinking-max", "claude-fable-5.1"),
            ("cursor-claude-fable-5-1-high-thinking", "claude-fable-5.1"),
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
    fn opus_4_8_all_writings_resolve() {
        let t = table();
        // Cursor 版本在前式、API 家族在前式（点号 / 连字符）、日期快照、带前缀后缀
        // 都应归一到表内唯一的规范键 claude-4.8-opus
        for (raw, expect) in [
            ("claude-4.8-opus", "claude-4.8-opus"),
            ("claude-opus-4.8", "claude-4.8-opus"),
            ("claude-opus-4-8", "claude-4.8-opus"),
            ("claude-opus-4-8-20260528", "claude-4.8-opus"),
            ("cursor-claude-opus-4-8-high-thinking", "claude-4.8-opus"),
        ] {
            let hit = resolve(&t, raw);
            assert!(hit.is_some(), "{raw} 应命中价格表");
            assert_eq!(hit.unwrap().0, expect, "{raw} 归一目标不对");
        }
        // API 连字符写法展示同样归并到规范键（曾原样显示为 claude-opus-4-8）
        assert_eq!(display_key(&t, "claude-opus-4-8"), "claude-4.8-opus");
        assert_eq!(display_key(&t, "claude-opus-4-8-thinking"), "claude-4.8-opus");
    }

    #[test]
    fn gpt_5_5_family_resolves() {
        let t = table();
        assert_eq!(resolve(&t, "gpt-5.5").unwrap().0, "gpt-5.5");
        assert_eq!(resolve(&t, "gpt-5.5-high").unwrap().0, "gpt-5.5");
        assert_eq!(resolve(&t, "gpt-5.5-2026-04-23").unwrap().0, "gpt-5.5");
        // pro 独立计价，不得回退按基础款计价；连字符写法归一到点号键
        assert_eq!(resolve(&t, "gpt-5.5-pro").unwrap().0, "gpt-5.5-pro");
        assert_eq!(resolve(&t, "gpt-5-5").unwrap().0, "gpt-5.5");
        assert_eq!(display_key(&t, "gpt-5-5"), "gpt-5.5");
        assert_eq!(display_key(&t, "gpt-5.5-xhigh"), "gpt-5.5");
    }

    #[test]
    fn anthropic_api_style_ids_normalize_to_table_keys() {
        let t = table();
        // sonnet / haiku / opus 的官方 API ID（家族在前、连字符、可带日期快照）
        // 归一到表内规范键（版本在前的 Cursor 风格）
        assert_eq!(resolve(&t, "claude-sonnet-4-6").unwrap().0, "claude-4.6-sonnet");
        assert_eq!(resolve(&t, "claude-sonnet-4-5-20250929").unwrap().0, "claude-4.5-sonnet");
        assert_eq!(display_key(&t, "claude-sonnet-4-6"), "claude-4.6-sonnet");
        assert_eq!(display_key(&t, "claude-haiku-4-5"), "claude-4.5-haiku");
        assert_eq!(display_key(&t, "cursor-claude-opus-4-7-max-thinking"), "claude-4.7-opus");
        // 词序变体归并为同一表键，同一模型不再拆成两行统计
        assert_eq!(display_key(&t, "claude-5-sonnet"), "claude-sonnet-5");
        assert_eq!(display_key(&t, "claude-5-opus"), "claude-opus-5");
        assert_eq!(display_key(&t, "claude-haiku-4.5"), "claude-4.5-haiku");
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

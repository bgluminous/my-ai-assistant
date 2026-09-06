use crate::pricing::{ModelPrice, PricingTable};

/// 分发方/路由前缀：Cursor 会把自家 Grok 上报为 `cursor-grok-4.6-high-fast` 这类名字，
/// 剥离前缀后再按表内模型名匹配价格。
const PROVIDER_PREFIXES: &[&str] = &["cursor-"];

/// 思考 / 努力度等修饰后缀：同一模型的不同等级同价，剥掉后按基础模型计价。
/// 不含 `-fast`——Fast 模式是单独计价的变体，见 [`FAST_SUFFIX`]。
const EFFORT_SUFFIXES: &[&str] = &[
    "-xhigh-thinking",
    "-high-thinking",
    "-medium-thinking",
    "-low-thinking",
    "-minimal-thinking",
    "-max-thinking",
    "-thinking",
    "-reasoning",
    "-xhigh",
    "-high",
    "-medium",
    "-low",
    "-minimal",
    "-max",
    "-latest",
];

/// Fast 模式变体标记。Cursor / OpenAI / Anthropic 的 Fast 模式单独计价（通常为标准价 2 倍或更高），
/// 上报名以 `-fast` 结尾且位于 effort 后缀之后：`cursor-grok-4.6-high-fast`、`gpt-5-high-fast`、
/// `claude-opus-5-thinking-high-fast`、`composer-2.5-fast`。
/// 价格表里 Fast 变体的键 = 基础模型规范键 + `-fast`（如 grok-4.6-fast、claude-4.8-opus-fast）。
/// 本体名字就带 fast 的独立模型（grok-4-fast、grok-code-fast-1）靠精确收录优先命中，不受影响。
const FAST_SUFFIX: &str = "-fast";

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

/// 从末尾剥掉一个修饰后缀。返回 Some(true) 表示剥掉的是 Fast 标记，Some(false) 表示 effort 后缀，
/// None 表示没有可剥的后缀。列表里的复合后缀（-high-thinking）排在单段之前，保证一次剥整段。
fn strip_one_modifier(key: &mut String) -> Option<bool> {
    if key.len() > FAST_SUFFIX.len() && key.ends_with(FAST_SUFFIX) {
        key.truncate(key.len() - FAST_SUFFIX.len());
        return Some(true);
    }
    for suffix in EFFORT_SUFFIXES {
        if key.len() > suffix.len() && key.ends_with(suffix) {
            key.truncate(key.len() - suffix.len());
            return Some(false);
        }
    }
    None
}

/// 表内 `key-fast` 条目。
fn fast_of<'a>(table: &'a PricingTable, key: &str) -> Option<(&'a str, &'a ModelPrice)> {
    get(table, &format!("{key}{FAST_SUFFIX}"))
}

/// Fast 感知的查表：带 Fast 标记时先按 `candidate-fast` 的等价写法直接查（覆盖 grok-4.1-fast 这类
/// 只收录了带 fast 名字的独立模型），再解析基础模型的规范键 K 并优先取表内的 `K-fast`；
/// 表内没有 Fast 条目时回退到 K，按标准价折算（展示层会保留 -fast 名字并把 priced_as 标为 K，
/// 让这种低估可见）。不带 Fast 标记时等同于普通查表。
fn lookup<'a>(table: &'a PricingTable, candidate: &str, fast: bool) -> Option<(&'a str, &'a ModelPrice)> {
    if fast {
        if let Some(hit) = get_normalized(table, &format!("{candidate}{FAST_SUFFIX}")) {
            return Some(hit);
        }
    }
    let (k, price) = get_normalized(table, candidate)?;
    if fast {
        if let Some(hit) = fast_of(table, k) {
            return Some(hit);
        }
    }
    Some((k, price))
}

/// 单个候选名的匹配流程：精确匹配（含等价写法归一）-> 逐个剥掉 Fast 标记 / 思考 / 努力度后缀，
/// 每剥一层查一次表 -> 逐段去尾 -> 前缀匹配兜底。去尾与兜底均带版本截断防护，
/// 避免 5.1 / 5-1 这类小版本被按 5 计价；全程带着 Fast 标记，命中基础键后优先取其 Fast 条目。
fn resolve_one<'a>(table: &'a PricingTable, key: &str) -> Option<(&'a str, &'a ModelPrice)> {
    if let Some(hit) = get_normalized(table, key) {
        return Some(hit);
    }

    let mut base = key.to_string();
    let mut fast = false;
    while let Some(was_fast) = strip_one_modifier(&mut base) {
        fast |= was_fast;
        if let Some(hit) = lookup(table, &base, fast) {
            return Some(hit);
        }
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
        if let Some(hit) = lookup(table, &candidate, fast) {
            return Some(hit);
        }
    }

    // 最后兜底：价格表里若有某键是（已剥修饰的）候选名的前缀，则采用它（取最长前缀）。
    // 键后必须是 `-` 且不落在版本号中间：claude-fable-5 不得命中
    // claude-fable-5.1-* / claude-fable-5-1-*，但 gpt-4o 仍可命中 gpt-4o-2024-08-06。
    let mut best: Option<(&str, &ModelPrice)> = None;
    for (k, v) in &table.models {
        if k.is_empty() || !base.starts_with(k.as_str()) {
            continue;
        }
        let rest = &base[k.len()..];
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
    if fast {
        if let Some((bk, _)) = best {
            if let Some(hit) = fast_of(table, bk) {
                return Some(hit);
            }
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

/// 聚合展示用的模型名归一：同一模型的不同思考 / 努力度等级并入一行，
/// 点号 / 连字符 / 下划线与「版本-家族」词序变体归并为表内规范名
/// （claude-opus-4-8 / claude-opus-4.8 → claude-4.8-opus，同一模型不再拆行统计），
/// 保证归并结果与计价口径一致。价格表精确收录的名字（如 grok-4-fast、grok-code-fast-1
/// 这类独立计价的真实模型）原样保留；剥离分发前缀与修饰后缀途中一旦命中表键即停在该键。
/// Fast 变体始终独立成行：命中基础键 K 后展示为 `K-fast`（表内有该条目则按 Fast 价，
/// 没有则 resolve 回退到 K 的标准价、priced_as 显示 K），不与标准版合并。
/// 日期 / 快照段（gpt-5-2025-08-07）不在剥离之列，保持独立；全程未命中的名字保留
/// 剥离后的原样（带 Fast 标记的补回 -fast）。
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
    let mut fast = false;
    while let Some(was_fast) = strip_one_modifier(&mut key) {
        fast |= was_fast;
        if let Some((k, _)) = lookup(table, &key, fast) {
            let k = prefer_dotted(table, k.to_string());
            return if fast && !k.ends_with(FAST_SUFFIX) {
                format!("{k}{FAST_SUFFIX}")
            } else {
                k
            };
        }
    }
    if fast {
        key.push_str(FAST_SUFFIX);
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> PricingTable {
        crate::pricing::defaults()
    }

    /// Fast 标记的三种情形：表内有 Fast 条目 → 落到「规范键 + -fast」；表内没有（厂商无 Fast 档）→
    /// 独立成行但按基础模型标准价计价（priced_as = 基础键）；本体名字就带 fast 的独立模型不受影响。
    #[test]
    fn fast_marker_resolution() {
        let t = table();
        // (上报名, 计价键, 展示名)
        for (raw, priced_as, shown) in [
            // effort / thinking 后缀之后的 -fast；Anthropic API 风格先归一为规范键再取 Fast 条目
            ("gpt-5-high-fast", "gpt-5-fast", "gpt-5-fast"),
            ("gpt-5.6-sol-medium-fast", "gpt-5.6-sol-fast", "gpt-5.6-sol-fast"),
            ("claude-opus-5-thinking-high-fast", "claude-opus-5-fast", "claude-opus-5-fast"),
            ("claude-opus-4-8-thinking-max-fast", "claude-4.8-opus-fast", "claude-4.8-opus-fast"),
            ("cursor-composer-2.5-fast", "composer-2.5-fast", "composer-2.5-fast"),
            // xAI 无 Fast 档、Anthropic 不支持 Opus 4.7 fast：不得并入标准版一行，但按标准价
            ("cursor-grok-4.6-xhigh-fast", "grok-4.6", "grok-4.6-fast"),
            ("cursor-grok-4.5-high-fast", "grok-4.5", "grok-4.5-fast"),
            ("claude-opus-4-7-thinking-fast", "claude-4.7-opus", "claude-4.7-opus-fast"),
            // 本体带 fast 的独立模型：精确收录优先，只剥 effort 后缀（grok-4.1 无基础键也不得落到裸 grok）
            ("grok-4-fast-high", "grok-4-fast", "grok-4-fast"),
            ("grok-4.1-fast-reasoning", "grok-4.1-fast", "grok-4.1-fast"),
        ] {
            assert_eq!(resolve(&t, raw).unwrap().0, priced_as, "{raw} 计价键不对");
            assert_eq!(display_key(&t, raw), shown, "{raw} 展示名不对");
            // 聚合层先 display_key 再 resolve：展示名送回 resolve 必须得到同一计价键
            assert_eq!(resolve(&t, shown).unwrap().0, priced_as, "{shown} 与 {raw} 计价不一致");
        }
        // 未收录的模型保留 Fast 标记且仍为未定价
        assert_eq!(display_key(&t, "my-model-xhigh-fast"), "my-model-fast");
        assert!(resolve(&t, "my-model-fast").is_none());
        // Fast 条目必须比标准版贵，防止写成同价
        for (base, fast) in [("gpt-5", "gpt-5-fast"), ("claude-opus-5", "claude-opus-5-fast"), ("composer-2.5", "composer-2.5-fast")] {
            let (b, f) = (resolve(&t, base).unwrap().1, resolve(&t, fast).unwrap().1);
            assert!(f.input > b.input && f.output > b.output, "{fast} 应比 {base} 贵");
        }
    }

    /// 等价价的口径是厂商官方 API 价目而非 Cursor 售价。只固化两者不同、容易被 Cursor 账单带偏的几处，
    /// 其余价格以 pricing.default.json 为唯一事实来源，不在测试里重复。
    #[test]
    fn vendor_official_prices_pinned() {
        let t = table();
        let check = |raw: &str, expect: (f64, f64, f64, f64)| {
            let p = resolve(&t, raw).unwrap_or_else(|| panic!("{raw} 未命中")).1;
            assert_eq!((p.input, p.output, p.cache_read, p.cache_write), expect, "{raw} 单价与厂商官方价目不符");
        };
        check("cursor-grok-4.5-high", (2.0, 6.0, 0.3, 0.0)); // xAI：缓存读 $0.30（Cursor 收 $0.5）
        check("cursor-grok-4.6-xhigh-fast", (2.0, 6.0, 0.5, 0.0)); // xAI 无 Fast 档（Cursor 收 2 倍）
        check("gpt-5.6-sol-medium", (4.0, 20.0, 0.4, 5.0)); // OpenAI 官方促销价（至少到 2026-11-21）
        check("claude-opus-5-thinking-high-fast", (10.0, 50.0, 1.0, 12.5)); // Anthropic fast mode 2 倍，缓存倍率叠加
        check("claude-sonnet-5", (2.0, 10.0, 0.2, 2.5)); // Anthropic：$2/$10 已定为永久价
        check("claude-fable-5-thinking-max", (10.0, 50.0, 1.0, 12.5)); // Max 是 effort 等级，无溢价
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
        for raw in [
            "claude-fable-5.2",
            "claude-fable-5-2-thinking",
            "gpt-5.1",
            "gpt-6.1",
            "gpt-6-1-astra",
            "claude-sonnet-5.1",
            "composer-2",
            "composer-2-fast",
        ] {
            assert!(resolve(&t, raw).is_none(), "{raw} 不应错配到旧版本价格");
        }
    }

    #[test]
    fn display_key_merges_effort_levels() {
        let t = table();
        // 思考 / 效率等级并入基础模型（含分发前缀与大小写归一）
        assert_eq!(display_key(&t, "claude-4.5-sonnet-thinking"), "claude-4.5-sonnet");
        assert_eq!(display_key(&t, "cursor-grok-4.6-high"), "grok-4.6");
        assert_eq!(display_key(&t, "cursor-grok-4.6-xhigh"), "grok-4.6");
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
        // 表键本身含 effort 词（codex-max）时精确匹配优先，不被当成后缀剥成 gpt-5.1-codex
        assert_eq!(display_key(&t, "gpt-5.1-codex-max-high"), "gpt-5.1-codex-max");
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
        assert_eq!(display_key(&t, "kimi-k2-7-code"), "kimi-k2.7-code");
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

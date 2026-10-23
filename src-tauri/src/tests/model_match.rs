//! model_match 模块的单元测试（由 model_match.rs 以 `#[path]` 挂载为 `crate::model_match::tests`）：
//! 上报模型名到价格表键的归一（Fast 标记、思考等级、版本写法、日期快照）与展示名合并。

use super::*;

fn table() -> PricingTable {
    crate::pricing::defaults()
}

/// Fast 标记的三种情形：表内有 Fast 条目 → 落到「规范键 + -fast」；表内没有对应 Fast 条目 →
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
        // 未收录对应 Fast 价：独立成行但按标准价；Grok 的 -fast 不能直接视为 API Priority
        ("cursor-grok-4.7-high-fast", "grok-4.7", "grok-4.7-fast"),
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
    check("cursor-grok-4.6-xhigh-fast", (2.0, 6.0, 0.5, 0.0)); // Cursor Fast 不等同于 xAI API Priority
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
fn opus_5_5_never_priced_as_opus_5() {
    let t = table();
    // Cursor 上报的连字符写法、版本在前式、effort / Fast 后缀与 cursor- 前缀都应命中 5.5，
    // 展示名同样归并到点号规范键（Opus 5.5 比 Opus 5 便宜，截成 5 会高估）
    for (raw, expect) in [
        ("claude-opus-5-5", "claude-opus-5.5"),
        ("claude-opus-5-5-thinking-high", "claude-opus-5.5"),
        ("claude-opus-5-5-max", "claude-opus-5.5"),
        ("claude-5.5-opus-high-thinking", "claude-opus-5.5"),
        ("cursor-claude-opus-5-5-thinking-xhigh", "claude-opus-5.5"),
        ("claude-opus-5-5-fast", "claude-opus-5.5-fast"),
        ("claude-opus-5-5-thinking-high-fast", "claude-opus-5.5-fast"),
    ] {
        assert_eq!(resolve(&t, raw).map(|h| h.0), Some(expect), "{raw} 计价键不对");
        assert_eq!(display_key(&t, raw), expect, "{raw} 展示名不对");
    }
    // 日期快照回退到 5.5 计价；opus 5 本体不受影响
    assert_eq!(resolve(&t, "claude-opus-5-5-20260922").unwrap().0, "claude-opus-5.5");
    assert_eq!(resolve(&t, "claude-opus-5-thinking-high").unwrap().0, "claude-opus-5");
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

/// 子型号必须命中各自价格，不能因后缀或版本写法退回较贵的基础型号。
#[test]
fn added_model_variants_keep_their_own_prices() {
    let t = table();
    for (raw, expected) in [
        ("gpt-4.1-nano", "gpt-4.1-nano"),
        ("cursor-GPT-4-1-Nano-xhigh", "gpt-4.1-nano"),
        ("GPT-5_1-high", "gpt-5.1"),
        ("Chat-Latest-high", "chat-latest"),
        ("Gemini-3-5-Flash-Lite-high", "gemini-3.5-flash-lite"),
        ("gemini-3_1-flash-lite", "gemini-3.1-flash-lite"),
        ("gemini-4-argon", "gemini-4-argon"),
        ("glm-5.3", "glm-5.3"),
        ("GLM-5-3-Flash-high", "glm-5.3-flash"),
        ("glm-5_3-flashx-xhigh", "glm-5.3-flashx"),
        ("claude-sonnet-5-5-thinking", "claude-sonnet-5.5"),
        ("claude-5.5-haiku-high", "claude-haiku-5.5"),
        ("DeepSeek-Flash", "deepseek-flash"),
        ("minimax-m3-high", "minimax-m3"),
        ("MiniMax-M3.1-Flash-high", "minimax-m3.1-flash"),
        ("Mistral-Medium-3-5-high", "mistral-medium-3-5"),
        ("mistral-small-2603", "mistral-small-2603"),
        ("codestral-2508", "codestral-2508"),
    ] {
        assert_eq!(resolve(&t, raw).map(|hit| hit.0), Some(expected), "{raw} 计价键不对");
        let shown = display_key(&t, raw);
        assert_eq!(shown, expected, "{raw} 展示名不对");
        assert_eq!(resolve(&t, &shown).map(|hit| hit.0), Some(expected));
    }
}

#[test]
fn unknown_minor_version_stays_unpriced() {
    let t = table();
    // 表里没有的小版本不得回退按大版本计价，应保持“未定价”
    // （grok 系列除外：表里有裸 grok 兜底键，属有意设计）
    for raw in [
        "claude-fable-5.2",
        "claude-fable-5-2-thinking",
        "gpt-5.7",
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

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
#[path = "tests/model_match.rs"]
mod tests;

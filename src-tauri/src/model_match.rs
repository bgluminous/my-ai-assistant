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

/// 单个候选名的匹配流程：精确匹配 -> 去掉思考/努力度后缀 -> 逐段去尾 -> 前缀匹配兜底。
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
        segments.pop();
        let candidate = segments.join("-");
        if let Some(hit) = get(table, &candidate) {
            return Some(hit);
        }
    }

    // 最后兜底：价格表里若有某键是候选名的前缀，则采用它（取最长前缀）。
    let mut best: Option<(&str, &ModelPrice)> = None;
    for (k, v) in &table.models {
        if !k.is_empty() && key.starts_with(k.as_str()) {
            match best {
                Some((bk, _)) if bk.len() >= k.len() => {}
                _ => best = Some((k.as_str(), v)),
            }
        }
    }
    best
}

fn get<'a>(table: &'a PricingTable, key: &str) -> Option<(&'a str, &'a ModelPrice)> {
    table.models.get_key_value(key).map(|(k, v)| (k.as_str(), v))
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
}

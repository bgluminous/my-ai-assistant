use crate::accounts::Account;
use chrono::{DateTime, NaiveDate};
use std::collections::HashSet;

/// 保留已有行的相对位置，按写入顺序追加新账户并清除失效 ID。
pub fn complete(preferred: &[String], accounts: &[Account]) -> Vec<String> {
    let mut remaining: HashSet<&str> = accounts.iter().map(|a| a.id.as_str()).collect();
    preferred
        .iter()
        .chain(accounts.iter().map(|a| &a.id))
        .filter(|id| remaining.remove(id.as_str()))
        .cloned()
        .collect()
}

/// 首次升级时按旧卡片和套餐顺序展开，相同排序键维持原先的先后位置。
pub fn legacy(groups: &[String], accounts: &[Account]) -> Vec<String> {
    let mut kinds: Vec<&str> = groups.iter().map(String::as_str).collect();
    kinds.extend(["cursor", "codex", "claude", "opencode"]);
    let mut rows: Vec<_> = accounts.iter().collect();
    rows.sort_by_cached_key(|a| {
        let kind = kinds
            .iter()
            .position(|k| *k == a.kind)
            .unwrap_or(kinds.len());
        let status = a.status.as_ref();
        let field = if a.kind == "cursor" {
            "membershipType"
        } else {
            "plan"
        };
        let plan = status.and_then(|s| s[field].as_str()).unwrap_or("").trim();
        let date = if a.kind == "codex" {
            "planActiveUntil"
        } else {
            "billingCycleEnd"
        };
        let end = status.and_then(|s| s[date].as_str()).and_then(|value| {
            DateTime::parse_from_rfc3339(value)
                .ok()
                .map(|d| d.timestamp_millis())
                .or_else(|| {
                    NaiveDate::parse_from_str(value, "%Y-%m-%d")
                        .ok()?
                        .and_hms_opt(0, 0, 0)
                        .map(|d| d.and_utc().timestamp_millis())
                })
        });
        let rank = if plan.eq_ignore_ascii_case("free") {
            (3, 0)
        } else if let Some(time) = end.filter(|_| a.kind != "claude") {
            (0, time)
        } else if plan.is_empty() {
            (2, 0)
        } else {
            (1, 0)
        };
        (kind, rank)
    });
    rows.into_iter().map(|a| a.id.clone()).collect()
}

pub fn move_before(order: &mut Vec<String>, id: &str, before: Option<&str>) -> Result<(), String> {
    let from = order
        .iter()
        .position(|key| key == id)
        .ok_or("account_order_changed")?;
    let target = match before {
        Some(key) => order
            .iter()
            .position(|item| item == key)
            .ok_or("account_order_changed")?,
        None => order.len(),
    };
    let item = order.remove(from);
    order.insert(if from < target { target - 1 } else { target }, item);
    Ok(())
}

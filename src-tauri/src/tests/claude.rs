//! claude 模块的单元测试（由 claude.rs 以 `#[path]` 挂载为 `crate::claude::tests`）：
//! 本机 Claude Code 会话日志的去重口径与会话 id 解析。

use super::*;
use serde_json::json;

#[test]
fn profile_distinguishes_plan_family_limit_tier_and_seat() {
    for tier in ["default_claude_max_5x", "default_claude_max_20x"] {
        let plan = SubscriptionPlan::from_json(&json!({
            "subscription_type": "pro",
            "organization": { "organization_type": "claude_max", "rate_limit_tier": tier }
        }));
        assert_eq!(plan.plan.as_deref(), Some("max"));
        assert_eq!(plan.rate_limit_tier.as_deref(), Some(tier));
        assert!(plan.seat_tier.is_none());
    }
    let team = SubscriptionPlan::from_json(&json!({ "organization": {
        "organization_type": "claude_team", "rate_limit_tier": "default_claude_max_5x", "seat_tier": "team_premium"
    }}));
    assert_eq!(team.plan.as_deref(), Some("team"));
    assert_eq!(team.seat_tier.as_deref(), Some("team_premium"));
    // 没有套餐类型时，不把组织限额猜成个人 Max。
    let partial = SubscriptionPlan::from_json(&json!({ "rate_limit_tier": "default_claude_max_5x" }));
    assert!(partial.plan.is_none());
}

#[test]
fn live_profile_updates_tier_and_clears_incompatible_old_fields() {
    let mut plan = SubscriptionPlan::from_json(&json!({ "subscriptionType": "max", "rateLimitTier": "default_claude_max_5x" }));
    plan.apply_profile(SubscriptionPlan::from_json(&json!({ "organization": {
        "organization_type": "claude_max", "rate_limit_tier": "default_claude_max_20x"
    }})));
    assert_eq!(plan.rate_limit_tier.as_deref(), Some("default_claude_max_20x"));
    plan.apply_profile(SubscriptionPlan::from_json(&json!({ "organization": { "organization_type": "claude_team" } })));
    assert_eq!(plan.plan.as_deref(), Some("team"));
    assert!(plan.rate_limit_tier.is_none());
    assert!(plan.seat_tier.is_none());
    plan.apply_profile(SubscriptionPlan::default());
    assert_eq!(plan.plan.as_deref(), Some("team"));
}

fn cr(
    session: &str,
    msg: &str,
    req: &str,
    sidechain: bool,
    ts_ms: i64,
    input: f64,
    cache_read: f64,
) -> ClaudeRow {
    ClaudeRow {
        ts: DateTime::from_timestamp_millis(ts_ms).map(|d| d.with_timezone(&Utc)),
        session: session.into(),
        message_id: msg.into(),
        request_id: req.into(),
        sidechain,
        row: TokenRow {
            model: "claude-sonnet-4-5".into(),
            input,
            output: 1.0,
            cache_read,
            cache_write: 0.0,
            actual_cents: 0.0,
            timestamp_ms: Some(ts_ms),
        },
    }
}

fn total(rows: &[TokenRow]) -> f64 {
    rows.iter().map(row_total).sum()
}

#[test]
fn streaming_duplicates_keep_largest() {
    let mut d = Dedupe::new();
    d.push(cr("s", "m1", "r1", false, 1000, 100.0, 0.0));
    d.push(cr("s", "m1", "r1", false, 2000, 100.0, 500.0));
    assert_eq!(d.rows.len(), 1);
    assert_eq!(d.rows[0].cache_read, 500.0);
}

#[test]
fn distinct_sessions_with_same_message_id_kept() {
    let mut d = Dedupe::new();
    d.push(cr("s1", "m1", "r1", false, 1000, 100.0, 0.0));
    d.push(cr("s2", "m1", "r1", false, 1000, 300.0, 0.0));
    assert_eq!(d.rows.len(), 2);
    assert_eq!(total(&d.rows), 402.0);
}

#[test]
fn sidechain_replay_prefers_parent() {
    let mut d = Dedupe::new();
    d.push(cr("s", "m1", "r-parent", false, 1000, 0.0, 20.0));
    // sidechain 用新 requestId 重放父消息（同 timestamp）：不得重复计数
    d.push(cr("s", "m1", "r-replay", true, 1000, 0.0, 50_000.0));
    // sidechain 自己的回答是独立消息，照常保留
    d.push(cr("s", "m2", "r-side", true, 2000, 0.0, 700.0));
    assert_eq!(d.rows.len(), 2);
    assert_eq!(d.rows[0].cache_read, 20.0);
    assert_eq!(d.rows[1].cache_read, 700.0);
}

#[test]
fn rows_without_message_id_not_deduped() {
    let mut d = Dedupe::new();
    d.push(cr("s", "", "", false, 1000, 10.0, 0.0));
    d.push(cr("s", "", "", false, 1000, 10.0, 0.0));
    assert_eq!(d.rows.len(), 2);
}

#[test]
fn session_from_path_variants() {
    assert_eq!(
        session_from_path(Path::new("/h/.claude/projects/proj-a/sess-1.jsonl")),
        "sess-1"
    );
    assert_eq!(
        session_from_path(Path::new("/h/.claude/projects/proj-a/sess-1/chat.jsonl")),
        "sess-1"
    );
    assert_eq!(
        session_from_path(Path::new(
            "/h/.claude/projects/proj-a/sess-1/subagents/worker.jsonl"
        )),
        "sess-1"
    );
}

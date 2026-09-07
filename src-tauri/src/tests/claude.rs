//! claude 模块的单元测试（由 claude.rs 以 `#[path]` 挂载为 `crate::claude::tests`）：
//! 本机 Claude Code 会话日志的去重口径与会话 id 解析。

use super::*;

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

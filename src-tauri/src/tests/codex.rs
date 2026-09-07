//! codex 模块的单元测试（由 codex.rs 以 `#[path]` 挂载为 `crate::codex::tests`）：
//! 本机会话日志的跨文件去重，以及订阅接口日期字段的解析。

use super::*;
use serde_json::json;

// ---------------------------------------------------------------------------
// 会话日志去重：同一谱系内累计计数器相同的记录只计一次
// ---------------------------------------------------------------------------

fn key(n: i64) -> UsageKey {
    UsageKey {
        input: n,
        cached: 0,
        output: 1,
        reasoning: 0,
        total: n + 1,
    }
}

fn row(ts_ms: i64, key: Option<UsageKey>, input: f64) -> CachedRow {
    let ts = DateTime::from_timestamp_millis(ts_ms);
    CachedRow {
        ts,
        key,
        row: TokenRow {
            model: "gpt-5".into(),
            input,
            output: 1.0,
            cache_read: 0.0,
            cache_write: 0.0,
            actual_cents: 0.0,
            timestamp_ms: Some(ts_ms),
        },
    }
}

fn file(id: &str, forked_from: Option<&str>, created_ms: i64, rows: Vec<CachedRow>) -> ParsedFile {
    ParsedFile {
        thread_id: Some(id.into()),
        forked_from_id: forked_from.map(str::to_string),
        created: DateTime::from_timestamp_millis(created_ms),
        had_meta: true,
        rows,
    }
}

fn all() -> TimeRange {
    TimeRange::new(None, None, None)
}

fn inputs(rows: &[TokenRow]) -> Vec<f64> {
    rows.iter().map(|r| r.input).collect()
}

#[test]
fn repeated_report_in_same_file_counted_once() {
    // 累计值没变的第二条是重复上报，哪怕中间隔着别的记录
    let f = file(
        "a",
        None,
        0,
        vec![
            row(1, Some(key(100)), 100.0),
            row(2, Some(key(100)), 100.0),
            row(3, Some(key(250)), 150.0),
            row(4, Some(key(100)), 100.0),
        ],
    );
    assert_eq!(inputs(&dedupe_rows(vec![f], &all())), vec![100.0, 150.0]);
}

#[test]
fn fork_copies_skipped_and_parent_timestamps_kept() {
    let parent = file(
        "p",
        None,
        0,
        vec![row(10, Some(key(100)), 100.0), row(20, Some(key(300)), 200.0)],
    );
    // 分叉复制件时间戳被改写到分叉时刻，之后是子任务自己的消耗
    let child = file(
        "c",
        Some("p"),
        1_000,
        vec![
            row(1_000, Some(key(100)), 100.0),
            row(1_000, Some(key(300)), 200.0),
            row(1_500, Some(key(350)), 50.0),
        ],
    );
    // 目录遍历顺序不保证父先于子
    let rows = dedupe_rows(vec![child, parent], &all());
    assert_eq!(inputs(&rows), vec![100.0, 200.0, 50.0]);
    assert_eq!(rows[0].timestamp_ms, Some(10));
    assert_eq!(rows[1].timestamp_ms, Some(20));
}

#[test]
fn dedupe_happens_before_range_filter() {
    let parent = file("p", None, 0, vec![row(10, Some(key(100)), 100.0)]);
    let child = file(
        "c",
        Some("p"),
        5_000,
        vec![row(5_000, Some(key(100)), 100.0), row(6_000, Some(key(140)), 40.0)],
    );
    // 范围只覆盖分叉之后：父记录不在范围内，它的复制件也不得被算进来
    let range = TimeRange::new(None, Some(4_000), None);
    assert_eq!(inputs(&dedupe_rows(vec![parent, child], &range)), vec![40.0]);
}

#[test]
fn siblings_of_missing_parent_share_copy_once() {
    let a = file(
        "a",
        Some("gone"),
        1_000,
        vec![row(1_000, Some(key(100)), 100.0), row(1_100, Some(key(130)), 30.0)],
    );
    let b = file(
        "b",
        Some("gone"),
        2_000,
        vec![row(2_000, Some(key(100)), 100.0), row(2_100, Some(key(170)), 70.0)],
    );
    assert_eq!(inputs(&dedupe_rows(vec![b, a], &all())), vec![100.0, 30.0, 70.0]);
}

#[test]
fn nested_fork_resolves_to_same_lineage() {
    let root = file("r", None, 0, vec![row(10, Some(key(100)), 100.0)]);
    let child = file(
        "c",
        Some("r"),
        1_000,
        vec![row(1_000, Some(key(100)), 100.0), row(1_100, Some(key(130)), 30.0)],
    );
    let grandchild = file(
        "g",
        Some("c"),
        2_000,
        vec![
            row(2_000, Some(key(100)), 100.0),
            row(2_000, Some(key(130)), 30.0),
            row(2_100, Some(key(150)), 20.0),
        ],
    );
    assert_eq!(
        inputs(&dedupe_rows(vec![grandchild, child, root], &all())),
        vec![100.0, 30.0, 20.0]
    );
}

#[test]
fn unrelated_threads_with_equal_counters_both_counted() {
    let a = file("a", None, 0, vec![row(10, Some(key(100)), 100.0)]);
    let b = file("b", None, 1, vec![row(20, Some(key(100)), 100.0)]);
    assert_eq!(inputs(&dedupe_rows(vec![a, b], &all())), vec![100.0, 100.0]);
}

#[test]
fn rows_without_counter_not_deduped() {
    let f = file("a", None, 0, vec![row(1, None, 100.0), row(2, None, 100.0)]);
    assert_eq!(inputs(&dedupe_rows(vec![f], &all())), vec![100.0, 100.0]);
}

#[test]
fn usage_key_reads_integers_and_floats() {
    let v = json!({
        "input_tokens": 17305,
        "cached_input_tokens": 9984.0,
        "output_tokens": 227,
        "reasoning_output_tokens": 98,
        "total_tokens": 17532
    });
    assert_eq!(
        usage_key(&v),
        Some(UsageKey {
            input: 17305,
            cached: 9984,
            output: 227,
            reasoning: 98,
            total: 17532
        })
    );
    assert_eq!(usage_key(&Value::Null), None);
}

// ---------------------------------------------------------------------------
// 订阅接口日期字段：RFC3339 / unix 秒 / unix 毫秒
// ---------------------------------------------------------------------------

#[test]
fn reads_rfc3339_string() {
    let v = json!({ "active_until": "2026-06-10T02:52:15Z" });
    assert_eq!(
        json_date(&v, &["active_until"]).as_deref(),
        Some("2026-06-10T02:52:15Z")
    );
}

#[test]
fn reads_unix_seconds() {
    let v = json!({ "chatgpt_subscription_active_until": 1781059935 });
    let got = json_date(&v, &["chatgpt_subscription_active_until"]).unwrap();
    assert!(got.starts_with("2026-06-10T"));
}

#[test]
fn reads_unix_millis() {
    let v = json!({ "active_until": 1781059935000i64 });
    let got = json_date(&v, &["active_until"]).unwrap();
    assert!(got.starts_with("2026-06-10T"));
}

#[test]
fn skips_missing_or_empty() {
    let v = json!({ "active_until": "" });
    assert_eq!(json_date(&v, &["active_until", "activeUntil"]), None);
    assert_eq!(json_date(&json!({}), &["active_until"]), None);
}

//! 本地会话日志（Codex CLI 与 Claude Code 的 `*.jsonl`）扫描的共用部分：
//! 时间戳 / 数字字段解析、jsonl 文件筛选、扫描时间范围，以及按「mtime + 大小」
//! 复用解析结果的文件缓存。各来源的行格式解析仍留在自己的模块里。

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;
use walkdir::DirEntry;

/// RFC3339 时间戳 → UTC；解析失败为 None。
pub fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// JSON 对象的数字字段，缺失或非数字按 0 计。
pub fn json_f64(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(|x| x.as_f64()).unwrap_or(0.0)
}

/// 目录遍历条目是否为 `.jsonl` 文件（扩展名不分大小写）。
pub fn is_jsonl_file(entry: &DirEntry) -> bool {
    entry.file_type().is_file()
        && entry
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("jsonl"))
}

/// 扫描的时间范围。起点：since_ms（unix 毫秒，可表达「本地自然日 0 点」这类固定时刻）
/// 优先于 days 滚动窗口，两者皆无则不设起点；终点 until_ms 可选、开区间。
/// 没有时间戳的行不受范围限制（计入合计，不进入按日序列）。
pub struct TimeRange {
    cutoff: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
}

impl TimeRange {
    pub fn new(days: Option<i64>, since_ms: Option<i64>, until_ms: Option<i64>) -> Self {
        let cutoff = match since_ms {
            Some(ms) => DateTime::from_timestamp_millis(ms),
            None => days.map(|d| Utc::now() - Duration::days(d.max(0))),
        };
        TimeRange {
            cutoff,
            until: until_ms.and_then(DateTime::from_timestamp_millis),
        }
    }

    pub fn contains(&self, ts: Option<DateTime<Utc>>) -> bool {
        let Some(ts) = ts else {
            return true;
        };
        if self.cutoff.is_some_and(|cut| ts < cut) {
            return false;
        }
        !self.until.is_some_and(|until| ts >= until)
    }
}

struct CachedFile<T> {
    mtime: SystemTime,
    len: u64,
    value: T,
}

/// 会话文件解析缓存：同一路径的 mtime 与大小都没变就复用上次的解析结果，
/// 避免每次扫描都全量重解析。缓存的是不带时间过滤的原始行，与扫描范围无关。
pub struct FileCache<T> {
    entries: Mutex<HashMap<PathBuf, CachedFile<T>>>,
}

impl<T: Clone> FileCache<T> {
    pub fn new() -> Self {
        FileCache {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// 命中缓存直接返回，否则用 `parse` 解析该文件并写入缓存。
    pub fn get_or_parse(&self, entry: &DirEntry, parse: impl FnOnce(&Path) -> T) -> T {
        let path = entry.path().to_path_buf();
        let (mtime, len) = entry
            .metadata()
            .ok()
            .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()))
            .unwrap_or((SystemTime::UNIX_EPOCH, 0));
        if let Ok(cache) = self.entries.lock() {
            if let Some(hit) = cache
                .get(&path)
                .filter(|f| f.mtime == mtime && f.len == len)
            {
                return hit.value.clone();
            }
        }
        let value = parse(&path);
        if let Ok(mut cache) = self.entries.lock() {
            cache.insert(
                path,
                CachedFile {
                    mtime,
                    len,
                    value: value.clone(),
                },
            );
        }
        value
    }
}

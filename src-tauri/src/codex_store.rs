//! GPT 用量存档与 v1 线格式。原日志路径只用于本地旧日志身份表，不进入同步记录。
use crate::{codex::ParsedFile, paths, pricing::TokenRow, session_scan::TimeRange};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};

pub const MAX_RECORDS: usize = 500;
pub const MAX_BYTES: usize = 1_048_576;
const MAX_TOKEN: u64 = 9_007_199_254_740_991;
pub type Result<T> = std::result::Result<T, String>;

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn random_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|_| "sync_random_failed")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// v1 的同步白名单；不加入路径、对话、凭据或价格。跨语言哈希向量见 tests/codex_store.rs。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Record {
    #[serde(rename_all = "camelCase")]
    Session {
        session_id: String,
        forked_from_id: Option<String>,
        created_at: Option<i64>,
    },
    #[serde(rename_all = "camelCase")]
    Usage {
        session_id: String,
        timestamp_ms: Option<i64>,
        model: String,
        // 输入不含缓存；输出已含推理 Token，不能再次累加推理值。
        input_tokens: u64,
        cache_read_tokens: u64,
        output_tokens: u64,
        // 原日志累计快照：[input, cached_input, output, reasoning_output, total]，不得重算。
        total_usage: Option<[u64; 5]>,
        // 有累计快照时为 0；否则区分内容相同但确实重复发生的事件。
        occurrence: u32,
    },
}

impl Record {
    pub fn session_id(&self) -> &str {
        match self {
            Self::Session { session_id, .. } | Self::Usage { session_id, .. } => session_id,
        }
    }
    /// v1 哈希固定为下列有序数组的紧凑 UTF-8 JSON：整数、无 BOM 或换行、不转义斜杠与非 ASCII。
    /// 字符串不归一化，未知值保留 null；设备不参与哈希，相同内容的来源另外保存。
    pub fn id(&self) -> String {
        let value = match self {
            Self::Session {
                session_id,
                forked_from_id,
                created_at,
            } => json!([1, "session", session_id, forked_from_id, created_at]),
            Self::Usage {
                session_id,
                timestamp_ms,
                model,
                input_tokens,
                cache_read_tokens,
                output_tokens,
                total_usage,
                occurrence,
            } => json!([
                1,
                "usage",
                session_id,
                timestamp_ms,
                model,
                input_tokens,
                cache_read_tokens,
                output_tokens,
                total_usage,
                occurrence
            ]),
        };
        hash(value.to_string().as_bytes())
    }
    pub fn validate(&self) -> Result<()> {
        let string_ok =
            |s: &str| !s.trim().is_empty() && s.len() <= 512 && !s.chars().any(char::is_control);
        let time_ok = |t: &Option<i64>| {
            t.is_none_or(|ms| ms >= 0 && DateTime::from_timestamp_millis(ms).is_some())
        };
        if !string_ok(self.session_id()) {
            return Err("sync_invalid_record".into());
        }
        let valid = match self {
            Self::Session {
                forked_from_id,
                created_at,
                ..
            } => forked_from_id.as_deref().is_none_or(string_ok) && time_ok(created_at),
            Self::Usage {
                timestamp_ms,
                model,
                input_tokens,
                cache_read_tokens,
                output_tokens,
                total_usage,
                occurrence,
                ..
            } => {
                time_ok(timestamp_ms)
                    && string_ok(model)
                    && [*input_tokens, *cache_read_tokens, *output_tokens]
                        .iter()
                        .all(|n| *n <= MAX_TOKEN)
                    && total_usage
                        .as_ref()
                        .is_none_or(|a| a.iter().all(|n| *n <= MAX_TOKEN) && *occurrence == 0)
            }
        };
        if valid {
            Ok(())
        } else {
            Err("sync_invalid_record".into())
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireRecord {
    pub record_id: String,
    pub record: Record,
}
impl WireRecord {
    #[cfg(test)]
    pub fn new(record: Record) -> Self {
        Self {
            record_id: record.id(),
            record,
        }
    }
    pub fn validate(&self) -> Result<()> {
        self.record.validate()?;
        if self.record_id != self.record.id() {
            return Err("sync_invalid_record".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteRecord {
    pub device_id: String,
    pub item: WireRecord,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Device {
    pub device_id: String,
    pub name: String,
}
impl Device {
    pub fn validate(&self) -> Result<()> {
        if self.device_id.len() != 32
            || !self
                .device_id
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            || self.name.trim().is_empty()
            || self.name.len() > 128
            || self.name.chars().any(char::is_control)
        {
            return Err("sync_invalid_device".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    pub device_id: String,
    pub name: String,
    pub local: bool,
    pub events: usize,
}

pub struct Store {
    pub db: Connection,
}
impl Store {
    pub fn init(db: Connection) -> Result<Self> {
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(db_err)?;
        db.execute_batch("PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS local_sources (path TEXT PRIMARY KEY, session_id TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS local_records (id TEXT PRIMARY KEY, body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS profiles (id TEXT PRIMARY KEY, space TEXT, epoch TEXT, cursor TEXT, success INTEGER);
            CREATE TABLE IF NOT EXISTS acks (profile TEXT NOT NULL, id TEXT NOT NULL, PRIMARY KEY(profile,id));
            CREATE TABLE IF NOT EXISTS remote_records (profile TEXT NOT NULL, device TEXT NOT NULL, id TEXT NOT NULL, body TEXT NOT NULL, PRIMARY KEY(profile,device,id));
            CREATE TABLE IF NOT EXISTS devices (profile TEXT NOT NULL, id TEXT NOT NULL, name TEXT NOT NULL, PRIMARY KEY(profile,id));").map_err(db_err)?;
        let version: Option<String> = db
            .query_row("SELECT value FROM meta WHERE key='schema'", [], |r| {
                r.get(0)
            })
            .optional()
            .map_err(db_err)?;
        if version.as_deref().is_some_and(|v| v != "1") {
            return Err("sync_store_version".into());
        }
        db.execute("INSERT OR IGNORE INTO meta VALUES ('schema','1')", [])
            .map_err(db_err)?;
        let id = random_id()?;
        db.execute("INSERT OR IGNORE INTO meta VALUES ('device',?1)", [&id])
            .map_err(db_err)?;
        Ok(Self { db })
    }
    pub fn device_id(&self) -> Result<String> {
        self.db
            .query_row("SELECT value FROM meta WHERE key='device'", [], |r| {
                r.get(0)
            })
            .map_err(db_err)
    }
    pub fn ingest(&mut self, files: Vec<(String, ParsedFile)>) -> Result<usize> {
        let device = self.device_id()?;
        let tx = self.db.transaction().map_err(db_err)?;
        let mut added = 0;
        for (path, file) in files {
            let id = match file.thread_id {
                Some(id) => id,
                None => {
                    let fresh = format!("legacy:{device}:{}", random_id()?);
                    tx.execute(
                        "INSERT OR IGNORE INTO local_sources VALUES (?1,?2)",
                        params![path, fresh],
                    )
                    .map_err(db_err)?;
                    tx.query_row(
                        "SELECT session_id FROM local_sources WHERE path=?1",
                        [&path],
                        |r| r.get(0),
                    )
                    .map_err(db_err)?
                }
            };
            let mut records = vec![Record::Session {
                session_id: id.clone(),
                forked_from_id: file.forked_from_id,
                created_at: file.created.map(|t| t.timestamp_millis()),
            }];
            let mut occurrences = HashMap::<String, u32>::new();
            for row in file.rows {
                let numbers = [row.row.input, row.row.cache_read, row.row.output];
                if numbers.iter().any(|n| {
                    !n.is_finite() || *n < 0.0 || *n > MAX_TOKEN as f64 || n.fract() != 0.0
                }) {
                    continue;
                }
                let total = row
                    .key
                    .map(|k| [k.input, k.cached, k.output, k.reasoning, k.total]);
                if total.is_some_and(|a| a.iter().any(|n| *n < 0 || *n as u64 > MAX_TOKEN)) {
                    continue;
                }
                let mut record = Record::Usage {
                    session_id: id.clone(),
                    timestamp_ms: row.ts.map(|t| t.timestamp_millis()),
                    model: row.row.model,
                    input_tokens: numbers[0] as u64,
                    cache_read_tokens: numbers[1] as u64,
                    output_tokens: numbers[2] as u64,
                    total_usage: total.map(|a| a.map(|n| n as u64)),
                    occurrence: 0,
                };
                if total.is_none() {
                    let n = occurrences.entry(record.id()).or_default();
                    if let Record::Usage { occurrence, .. } = &mut record {
                        *occurrence = *n;
                    }
                    *n += 1;
                }
                records.push(record);
            }
            for record in records {
                if record.validate().is_err() {
                    continue;
                }
                added += tx
                    .execute(
                        "INSERT OR IGNORE INTO local_records VALUES (?1,?2)",
                        params![
                            record.id(),
                            serde_json::to_string(&record).map_err(json_err)?
                        ],
                    )
                    .map_err(db_err)?;
            }
        }
        tx.commit().map_err(db_err)?;
        Ok(added)
    }
    pub fn pending(&self, profile: &str) -> Result<Vec<WireRecord>> {
        let mut stmt = self.db.prepare("SELECT id,body FROM local_records WHERE NOT EXISTS (SELECT 1 FROM acks WHERE profile=?1 AND acks.id=local_records.id) ORDER BY id LIMIT 500").map_err(db_err)?;
        let rows = stmt
            .query_map([profile], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(db_err)?;
        rows.map(|r| {
            let (record_id, body) = r.map_err(db_err)?;
            Ok(WireRecord {
                record_id,
                record: serde_json::from_str(&body).map_err(json_err)?,
            })
        })
        .collect()
    }
    pub fn pending_count(&self, profile: &str) -> Result<usize> {
        self.db.query_row("SELECT count(*) FROM local_records WHERE NOT EXISTS (SELECT 1 FROM acks WHERE profile=?1 AND acks.id=local_records.id)", [profile], |r| r.get::<_, i64>(0)).map(|n| n as usize).map_err(db_err)
    }
    pub fn prepare_profile(&mut self, profile: &str, space: &str, epoch: &str) -> Result<()> {
        let old: Option<(Option<String>, Option<String>)> = self
            .db
            .query_row(
                "SELECT space,epoch FROM profiles WHERE id=?1",
                [profile],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(db_err)?;
        if old == Some((Some(space.into()), Some(epoch.into()))) {
            return Ok(());
        }
        let tx = self.db.transaction().map_err(db_err)?;
        tx.execute("DELETE FROM acks WHERE profile=?1", [profile])
            .map_err(db_err)?;
        // 空间身份变化不得把旧空间缓存混入新空间。单纯服务重建则保留历史，重新确认本机记录。
        if old
            .as_ref()
            .is_some_and(|(s, _)| s.as_deref() != Some(space))
        {
            tx.execute("DELETE FROM remote_records WHERE profile=?1", [profile])
                .map_err(db_err)?;
            tx.execute("DELETE FROM devices WHERE profile=?1", [profile])
                .map_err(db_err)?;
        }
        tx.execute("INSERT INTO profiles VALUES (?1,?2,?3,NULL,NULL) ON CONFLICT(id) DO UPDATE SET space=excluded.space,epoch=excluded.epoch,cursor=NULL,success=NULL", params![profile, space, epoch]).map_err(db_err)?;
        tx.commit().map_err(db_err)
    }
    pub fn acknowledge(&mut self, profile: &str, items: &[WireRecord]) -> Result<()> {
        let tx = self.db.transaction().map_err(db_err)?;
        for item in items {
            tx.execute(
                "INSERT OR IGNORE INTO acks VALUES (?1,?2)",
                params![profile, item.record_id],
            )
            .map_err(db_err)?;
        }
        tx.commit().map_err(db_err)
    }
    pub fn cursor(&self, profile: &str) -> Result<Option<String>> {
        self.db
            .query_row("SELECT cursor FROM profiles WHERE id=?1", [profile], |r| {
                r.get(0)
            })
            .optional()
            .map(|v| v.flatten())
            .map_err(db_err)
    }
    pub fn success(&self, profile: &str) -> Result<Option<i64>> {
        self.db
            .query_row("SELECT success FROM profiles WHERE id=?1", [profile], |r| {
                r.get(0)
            })
            .optional()
            .map(|v| v.flatten())
            .map_err(db_err)
    }
    pub fn finish(&self, profile: &str) -> Result<()> {
        self.db
            .execute(
                "UPDATE profiles SET success=?2 WHERE id=?1",
                params![profile, Utc::now().timestamp_millis()],
            )
            .map(|_| ())
            .map_err(db_err)
    }
    pub fn reset_sync(&mut self, profile: &str) -> Result<()> {
        let tx = self.db.transaction().map_err(db_err)?;
        tx.execute("DELETE FROM acks WHERE profile=?1", [profile])
            .map_err(db_err)?;
        tx.execute("UPDATE profiles SET cursor=NULL WHERE id=?1", [profile])
            .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }
    pub fn apply_page(
        &mut self,
        profile: &str,
        records: &[RemoteRecord],
        devices: &[Device],
        cursor: &str,
    ) -> Result<usize> {
        // 先验证整页，再开事务；畸形页不能推进游标或留下半页数据。
        for d in devices {
            d.validate()?;
        }
        for r in records {
            r.item.validate()?;
            Device {
                device_id: r.device_id.clone(),
                name: "device".into(),
            }
            .validate()?;
        }
        // 内容相同也保留每台设备的来源；记录、设备和游标同一事务提交，失败后可重读本页。
        let tx = self.db.transaction().map_err(db_err)?;
        let mut added = 0;
        for r in records {
            added += tx
                .execute(
                    "INSERT OR IGNORE INTO remote_records VALUES (?1,?2,?3,?4)",
                    params![
                        profile,
                        r.device_id,
                        r.item.record_id,
                        serde_json::to_string(&r.item.record).map_err(json_err)?
                    ],
                )
                .map_err(db_err)?;
        }
        for d in devices {
            tx.execute("INSERT INTO devices VALUES (?1,?2,?3) ON CONFLICT(profile,id) DO UPDATE SET name=excluded.name", params![profile,d.device_id,d.name]).map_err(db_err)?;
        }
        tx.execute(
            "UPDATE profiles SET cursor=?2 WHERE id=?1",
            params![profile, cursor],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        Ok(added)
    }
    pub fn records(&self, profile: &str) -> Result<Vec<(String, WireRecord)>> {
        let own = self.device_id()?;
        let mut stmt = self.db.prepare("SELECT ?2,id,body FROM local_records UNION SELECT device,id,body FROM remote_records WHERE profile=?1").map_err(db_err)?;
        let rows = stmt
            .query_map(params![profile, own], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(db_err)?;
        rows.map(|r| {
            let (device, record_id, body) = r.map_err(db_err)?;
            Ok((
                device,
                WireRecord {
                    record_id,
                    record: serde_json::from_str(&body).map_err(json_err)?,
                },
            ))
        })
        .collect()
    }
    pub fn devices(&self, profile: &str, name: &str) -> Result<Vec<DeviceView>> {
        let own = self.device_id()?;
        let mut names = BTreeMap::new();
        let mut stmt = self
            .db
            .prepare("SELECT id,name FROM devices WHERE profile=?1")
            .map_err(db_err)?;
        for r in stmt
            .query_map([profile], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(db_err)?
        {
            let (id, name) = r.map_err(db_err)?;
            names.insert(id, name);
        }
        names.insert(own.clone(), name.into());
        let mut events: HashMap<String, BTreeSet<String>> = HashMap::new();
        for (d, r) in self.records(profile)? {
            names
                .entry(d.clone())
                .or_insert_with(|| format!("MAA-{}", &d[..8.min(d.len())]));
            if matches!(r.record, Record::Usage { .. }) {
                events.entry(d).or_default().insert(r.record_id);
            }
        }
        Ok(names
            .into_iter()
            .map(|(device_id, name)| DeviceView {
                local: device_id == own,
                events: events.get(&device_id).map_or(0, BTreeSet::len),
                device_id,
                name,
            })
            .collect())
    }
    pub fn clear_device(&mut self, profile: &str, device: &str) -> Result<usize> {
        let own = self.device_id()?;
        let tx = self.db.transaction().map_err(db_err)?;
        let mut removed = tx.execute("DELETE FROM remote_records WHERE profile=?1 AND device=?2 AND json_extract(body,'$.kind')='usage'",params![profile,device]).map_err(db_err)?;
        if device == own {
            removed += tx.execute("DELETE FROM local_records WHERE json_extract(body,'$.kind')='usage' AND id IN (SELECT id FROM acks WHERE profile=?1)",[profile]).map_err(db_err)?;
        }
        // 保留会话关系和未上传明细。下次同步从头重放，不能跳过刚被清理的历史。
        tx.execute("UPDATE profiles SET cursor=NULL WHERE id=?1", [profile])
            .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        Ok(removed)
    }
}

fn db_err(e: rusqlite::Error) -> String {
    format!("sync_store_failed: {e}")
}
fn json_err(_: serde_json::Error) -> String {
    "sync_invalid_record".into()
}
pub fn with_store<T>(f: impl FnOnce(&mut Store) -> Result<T>) -> Result<T> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "sync_lock_failed")?;
    let dir = paths::app_dir().ok_or("home_dir_unavailable")?;
    std::fs::create_dir_all(&dir).map_err(|_| "sync_store_failed")?;
    let db = Connection::open(dir.join("codex-usage.sqlite3")).map_err(db_err)?;
    f(&mut Store::init(db)?)
}

/// 全量关系图先确定规范事件与全部来源设备，再过滤设备和时间。父记录晚到也可修正分叉时间。
pub fn aggregate_rows(
    records: Vec<(String, WireRecord)>,
    device: Option<&str>,
    range: &TimeRange,
) -> (Vec<TokenRow>, usize) {
    let mut sessions = BTreeMap::<String, (Option<String>, Option<i64>)>::new();
    for (_, r) in &records {
        if let Record::Session {
            session_id,
            forked_from_id,
            created_at,
        } = &r.record
        {
            let entry = sessions.entry(session_id.clone()).or_default();
            if let Some(parent) = forked_from_id {
                if entry.0.as_ref().is_none_or(|p| parent < p) {
                    entry.0 = Some(parent.clone());
                }
            }
            if let Some(created) = created_at {
                if entry.1.is_none_or(|t| *created < t) {
                    entry.1 = Some(*created);
                }
            }
        }
    }
    // 缓存完整谱系，深分叉不截断；引用环统一使用环内最小 ID，环外子节点仍归于该根。
    let mut lineages = HashMap::<String, (String, usize)>::new();
    for (_, r) in &records {
        let mut current = r.record.session_id().to_string();
        let mut path = Vec::<String>::new();
        let mut positions = HashMap::<String, usize>::new();
        let (root, mut depth) = loop {
            if let Some(known) = lineages.get(&current) {
                break known.clone();
            }
            if let Some(&start) = positions.get(&current) {
                let root = path[start..].iter().min().unwrap().clone();
                for id in path.drain(start..) {
                    lineages.insert(id, (root.clone(), 0));
                }
                break (root, 0);
            }
            let Some(parent) = sessions.get(&current).and_then(|s| s.0.as_ref()) else {
                lineages.insert(current.clone(), (current.clone(), 0));
                break (current, 0);
            };
            positions.insert(current.clone(), path.len());
            path.push(current);
            current = parent.clone();
        };
        for id in path.into_iter().rev() {
            depth += 1;
            lineages.insert(id, (root.clone(), depth));
        }
    }
    let lineage = |id: &str| &lineages[id];
    let mut events: Vec<_> = records
        .into_iter()
        .filter(|(_, r)| matches!(r.record, Record::Usage { .. }))
        .collect();
    events.sort_by_key(|(d, r)| {
        let id = r.record.session_id();
        let time = match &r.record {
            Record::Usage { timestamp_ms, .. } => *timestamp_ms,
            _ => None,
        };
        (
            lineage(id).1,
            sessions.get(id).and_then(|s| s.1).unwrap_or(i64::MAX),
            time.unwrap_or(i64::MAX),
            r.record_id.clone(),
            d.clone(),
        )
    });
    let mut canonical = BTreeMap::<String, (TokenRow, BTreeSet<String>, String)>::new();
    for (d, r) in events {
        if let Record::Usage {
            session_id,
            timestamp_ms,
            model,
            input_tokens,
            cache_read_tokens,
            output_tokens,
            total_usage,
            ..
        } = r.record
        {
            let key = total_usage
                .map(|t| format!("{}:{t:?}", lineage(&session_id).0))
                .unwrap_or(r.record_id);
            canonical
                .entry(key)
                .or_insert_with(|| {
                    (
                        TokenRow {
                            model,
                            input: input_tokens as f64,
                            cache_read: cache_read_tokens as f64,
                            output: output_tokens as f64,
                            cache_write: 0.0,
                            actual_cents: 0.0,
                            timestamp_ms,
                        },
                        BTreeSet::new(),
                        session_id,
                    )
                })
                .1
                .insert(d);
        }
    }
    let mut rows = Vec::new();
    let mut selected_sessions = BTreeSet::new();
    for (_, (row, devices, id)) in canonical {
        if device.is_none_or(|d| devices.contains(d))
            && range.contains(row.timestamp_ms.and_then(DateTime::from_timestamp_millis))
        {
            selected_sessions.insert(id);
            rows.push(row);
        }
    }
    (rows, selected_sessions.len())
}

#[cfg(test)]
#[path = "tests/codex_store.rs"]
mod tests;

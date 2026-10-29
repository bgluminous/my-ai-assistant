//! 中心同步客户端。后台只传规范化用量，不传登录凭据、路径或原会话文件。
use crate::{
    audit, codex,
    codex_store::{
        self as store, Device, DeviceView, RemoteRecord, Result, WireRecord, MAX_BYTES, MAX_RECORDS,
    },
    local_crypto, pricing,
    session_scan::TimeRange,
    settings,
};
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SyncConfig {
    pub server_url: String,
    pub sealed_key: String,
    pub use_proxy: bool,
    pub source_home: String,
    pub device_name: String,
}
impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            server_url: String::new(),
            sealed_key: String::new(),
            use_proxy: default_use_proxy(),
            source_home: String::new(),
            device_name: String::new(),
        }
    }
}
fn default_use_proxy() -> bool {
    true
}
impl SyncConfig {
    pub fn portable(&self) -> Self {
        Self {
            source_home: String::new(),
            device_name: String::new(),
            ..self.clone()
        }
    }
    pub fn restore_local_fields(mut self, current: &Self) -> Self {
        self.source_home = current.source_home.clone();
        self.device_name = current.device_name.clone();
        self
    }
    fn key(&self) -> Result<String> {
        local_crypto::open(&self.sealed_key)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "sync_key_required".into())
    }
    fn configured(&self) -> bool {
        !self.server_url.is_empty() && !self.sealed_key.is_empty()
    }
    fn profile(&self) -> Result<String> {
        if !self.configured() {
            return Ok(String::new());
        }
        Ok(store::hash(
            serde_json::to_string(&[&self.server_url, &self.key()?])
                .map_err(|_| "sync_invalid_config")?
                .as_bytes(),
        ))
    }
}
pub fn current() -> Result<SyncConfig> {
    settings::ensure_loaded()?;
    settings::read(|s| s.codex_usage_sync.clone())
}
fn device(cfg: &SyncConfig) -> Result<Device> {
    store::with_store(|s| {
        let id = s.device_id()?;
        let name = if cfg.device_name.trim().is_empty() {
            format!("MAA-{}-{}", std::env::consts::OS, &id[..8])
        } else {
            cfg.device_name.clone()
        };
        Ok(Device {
            device_id: id,
            name,
        })
    })
}
pub fn normalize_url(text: &str) -> Result<String> {
    if text.trim().is_empty() {
        return Ok(String::new());
    }
    let url = reqwest::Url::parse(text.trim()).map_err(|_| "sync_invalid_url")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("sync_invalid_url".into());
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigInput {
    pub server_url: String,
    /// None 保留，空串清除，非空替换。读取设置永不返回密钥或密文。
    pub key: Option<String>,
    #[serde(default = "default_use_proxy")]
    pub use_proxy: bool,
    pub source_home: String,
    pub device_name: String,
}
fn config_from(input: ConfigInput, old: &SyncConfig) -> Result<SyncConfig> {
    let sealed_key = match input.key {
        None => old.sealed_key.clone(),
        Some(k) if k.trim().is_empty() => String::new(),
        Some(k) => {
            let k = k.trim();
            if k.len() > 4096 || !k.bytes().all(|b| (33..=126).contains(&b)) {
                return Err("sync_invalid_key".into());
            }
            local_crypto::seal(k)?
        }
    };
    validate_saved_config(SyncConfig {
        server_url: normalize_url(&input.server_url)?,
        sealed_key,
        use_proxy: input.use_proxy,
        source_home: input.source_home.trim().into(),
        device_name: input.device_name.trim().into(),
    })
}
/// 备份导入也使用同一校验；旧配置中的独立周期由 serde 忽略。
pub fn validate_saved_config(mut cfg: SyncConfig) -> Result<SyncConfig> {
    cfg.server_url = normalize_url(&cfg.server_url)?;
    if cfg.device_name.len() > 128 || cfg.device_name.chars().any(char::is_control) {
        return Err("sync_invalid_device".into());
    }
    if !cfg.server_url.is_empty() || !cfg.sealed_key.is_empty() {
        let key = cfg.key()?;
        if key.len() > 4096 || !key.bytes().all(|b| (33..=126).contains(&b)) {
            return Err("sync_invalid_key".into());
        }
    }
    Ok(cfg)
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigView {
    server_url: String,
    has_key: bool,
    use_proxy: bool,
    source_home: String,
    device_id: String,
    device_name: String,
}
fn config_view(cfg: &SyncConfig) -> Result<ConfigView> {
    let d = device(cfg)?;
    Ok(ConfigView {
        server_url: cfg.server_url.clone(),
        has_key: !cfg.sealed_key.is_empty(),
        use_proxy: cfg.use_proxy,
        source_home: cfg.source_home.clone(),
        device_id: d.device_id,
        device_name: d.name,
    })
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum SyncTrigger {
    #[default]
    Manual,
    Interval,
    Viewing,
}

type RoundReceiver = tokio::sync::watch::Receiver<Option<Result<()>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncPhase {
    Waiting,
    Scanning,
    Storing,
    Connecting,
    Uploading,
    Downloading,
    Recovering,
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgress {
    pub round_id: u64,
    pub phase: SyncPhase,
    pub uploaded_records: usize,
    pub upload_total: Option<usize>,
    pub downloaded_records: usize,
    pub downloaded_pages: usize,
    pub finished_at: Option<i64>,
}

#[derive(Clone, Copy)]
struct RoundKey {
    generation: u64,
    id: u64,
}

#[derive(Default)]
struct Runtime {
    generation: u64,
    sequence: u64,
    round_id: u64,
    progress: Option<SyncProgress>,
    active: Option<(u64, RoundReceiver)>,
    paused: bool,
    error: Option<String>,
    failures: usize,
    next: Option<Instant>,
}
impl Runtime {
    fn begin_progress(&mut self) -> RoundKey {
        self.round_id += 1;
        self.sequence += 1;
        self.error = None;
        self.progress = Some(SyncProgress {
            round_id: self.round_id,
            phase: SyncPhase::Waiting,
            uploaded_records: 0,
            upload_total: None,
            downloaded_records: 0,
            downloaded_pages: 0,
            finished_at: None,
        });
        RoundKey {
            generation: self.generation,
            id: self.round_id,
        }
    }
    fn update_progress(
        &mut self,
        key: RoundKey,
        change: impl FnOnce(&mut SyncProgress),
    ) -> Result<bool> {
        if self.generation != key.generation {
            return Err("sync_cancelled".into());
        }
        let p = self
            .progress
            .as_mut()
            .filter(|p| p.round_id == key.id)
            .ok_or("sync_cancelled")?;
        let before = p.clone();
        change(p);
        let changed = *p != before;
        if changed {
            self.sequence += 1;
        }
        Ok(changed)
    }
    fn invalidate(&mut self) {
        self.generation += 1;
        self.sequence += 1;
        self.progress = None;
    }
    fn join(&self) -> Option<RoundReceiver> {
        self.active
            .as_ref()
            .filter(|(generation, done)| {
                *generation == self.generation
                    && done.borrow().is_none()
                    && done.has_changed().is_ok()
            })
            .map(|(_, done)| done.clone())
    }
    fn start(
        &mut self,
        work: impl std::future::Future<Output = Result<()>> + Send + 'static,
        finished: impl FnOnce() + Send + 'static,
    ) -> RoundReceiver {
        if let Some(done) = self.join() {
            return done;
        }
        let (sender, done) = tokio::sync::watch::channel(None);
        self.active = Some((self.generation, done.clone()));
        // 同步任务独立于 IPC 等待者；关闭窗口不会取消写盘或使其它入口提前返回。
        tauri::async_runtime::spawn(async move {
            sender.send_replace(Some(work.await));
            finished();
        });
        done
    }
    fn should_sync(
        &self,
        trigger: SyncTrigger,
        interval: u32,
        last: Option<i64>,
        now: i64,
    ) -> bool {
        if trigger == SyncTrigger::Manual {
            return true;
        }
        if self.paused || self.next.is_some_and(|t| Instant::now() < t) {
            return false;
        }
        match trigger {
            SyncTrigger::Interval => interval > 0,
            SyncTrigger::Viewing => last.is_none_or(|at| now.saturating_sub(at) >= 5 * 60_000),
            SyncTrigger::Manual => true,
        }
    }
    fn record_result(&mut self, result: &Result<()>) {
        self.sequence += 1;
        if let Some(p) = self.progress.as_mut() {
            p.phase = if result.is_ok() {
                SyncPhase::Completed
            } else {
                SyncPhase::Failed
            };
            p.finished_at = Some(chrono::Utc::now().timestamp_millis());
        }
        match result {
            Ok(()) => {
                self.error = None;
                self.failures = 0;
                self.paused = false;
                self.next = None;
            }
            Err(e) => {
                self.paused = matches!(
                    e.as_str(),
                    "sync_unauthorized"
                        | "sync_forbidden"
                        | "sync_protocol_version"
                        | "sync_protocol_invalid"
                        | "sync_invalid_record"
                        | "sync_invalid_device"
                        | "sync_invalid_url"
                        | "sync_key_required"
                        | "sync_response_too_large"
                );
                // v1 使用固定退避阶梯；429 的 Retry-After 不改变 30 分钟上限。
                self.next = Some(
                    Instant::now()
                        + Duration::from_secs([300, 600, 1200, 1800][self.failures.min(3)]),
                );
                self.failures = self.failures.saturating_add(1);
                self.error = Some(e.clone());
            }
        }
    }
}
fn runtime() -> &'static Mutex<Runtime> {
    static R: OnceLock<Mutex<Runtime>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Runtime::default()))
}
fn gate() -> &'static tokio::sync::Mutex<()> {
    static G: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    G.get_or_init(|| tokio::sync::Mutex::new(()))
}
fn checked<T>(generation: u64, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let r = runtime().lock().map_err(|_| "sync_lock_failed")?;
    if r.generation != generation {
        return Err("sync_cancelled".into());
    }
    f()
}
pub fn notify_changed(app: &AppHandle, reason: &str) {
    let _ = app.emit("codex-usage-changed", serde_json::json!({"reason":reason}));
}
pub fn update_configuration<T>(update: impl FnOnce() -> Result<T>) -> Result<T> {
    // 配置写入与响应落库共用短锁，包括备份导入，消除更新配置到作废旧请求之间的窗口。
    let mut r = runtime().lock().map_err(|_| "sync_lock_failed")?;
    let result = update();
    // settings::mutate 写盘失败时内存可能已变化，因此错误分支也必须作废旧请求。
    r.invalidate();
    r.error = None;
    r.paused = false;
    r.failures = 0;
    r.next = None;
    result
}
pub fn configuration_changed(app: &AppHandle) {
    emit_status(app);
    notify_changed(app, "config");
    // 保存与备份恢复后仅同步一次，不再维护独立周期或失败重试定时器。
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = synchronize(&app, SyncTrigger::Manual).await;
    });
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub sequence: u64,
    pub state: String,
    pub last_success_at: Option<i64>,
    pub pending_records: usize,
    pub error: Option<String>,
    pub progress: Option<SyncProgress>,
}
#[tauri::command]
pub fn codex_usage_sync_status() -> Result<SyncStatus> {
    let _operation = crate::operations::begin()?;
    status_snapshot()
}
fn status_snapshot() -> Result<SyncStatus> {
    // 与配置更新、写盘统一按 runtime → settings/store 的顺序取锁。
    let r = runtime().lock().map_err(|_| "sync_lock_failed")?;
    let cfg = current()?;
    let profile = cfg.profile()?;
    let (pending, last) =
        store::with_store(|s| Ok((s.pending_count(&profile)?, s.success(&profile)?)))?;
    let state = if !cfg.configured() {
        "disabled"
    } else if r.join().is_some() && r.progress.as_ref().is_none_or(|p| p.finished_at.is_none()) {
        "running"
    } else if r.paused {
        "paused"
    } else if r.error.is_some() {
        "error"
    } else {
        "idle"
    };
    Ok(SyncStatus {
        sequence: r.sequence,
        state: state.into(),
        last_success_at: last,
        pending_records: pending,
        error: r.error.clone(),
        progress: r.progress.clone(),
    })
}
fn emit_status(app: &AppHandle) {
    // 构造快照之后已释放所有锁；进度通知不触发统计缓存失效。
    if let Ok(snapshot) = status_snapshot() {
        let _ = app.emit("codex-usage-sync-status", snapshot);
    }
}
fn report_progress(
    app: &AppHandle,
    key: RoundKey,
    change: impl FnOnce(&mut SyncProgress),
) -> Result<()> {
    let changed = runtime()
        .lock()
        .map_err(|_| "sync_lock_failed")?
        .update_progress(key, change)?;
    if changed {
        emit_status(app);
    }
    Ok(())
}
#[tauri::command]
pub fn codex_usage_sync_get() -> Result<ConfigView> {
    let _operation = crate::operations::begin()?;
    config_view(&current()?)
}
#[tauri::command]
pub fn codex_usage_sync_set(app: AppHandle, config: ConfigInput) -> Result<ConfigView> {
    let _operation = crate::operations::begin()?;
    let next = config_from(config, &current()?)?;
    // 与响应落库共用短锁：设置一变，旧请求不能继续确认或写入任何空间。
    update_configuration(|| {
        settings::mutate(|s| {
            s.codex_usage_sync = next.clone();
            Ok(())
        })
    })?;
    audit::log(
        "codex_usage_sync_config",
        "更新 GPT 用量同步设置".into(),
        None,
    );
    configuration_changed(&app);
    config_view(&next)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Handshake {
    pub protocol_version: u32,
    pub space_id: String,
    pub epoch: String,
}
impl Handshake {
    fn validate(&self) -> Result<()> {
        if self.protocol_version != 1 {
            return Err("sync_protocol_version".into());
        }
        if [&self.space_id, &self.epoch]
            .iter()
            .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
        {
            return Err("sync_protocol_invalid".into());
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Push {
    pub protocol_version: u32,
    pub space_id: String,
    pub epoch: String,
    pub device: Device,
    pub records: Vec<WireRecord>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Acknowledged {
    pub space_id: String,
    pub epoch: String,
    pub acknowledged_ids: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub space_id: String,
    pub epoch: String,
    pub records: Vec<RemoteRecord>,
    pub devices: Vec<Device>,
    // 不解析服务端游标：中间页固定本轮上界，末页检查点供下一轮捕获新的上界。
    pub next_cursor: String,
    pub has_more: bool,
}

// 泛型传输边界让测试替身与真实 HTTP 走同一同步状态机，无需正式服务端。
pub trait Transport {
    async fn handshake(&self) -> Result<Handshake>;
    async fn push(&self, payload: &Push) -> Result<Acknowledged>;
    async fn changes(&self, h: &Handshake, cursor: Option<&str>) -> Result<Page>;
}
struct HttpTransport {
    client: reqwest::Client,
    base: String,
    key: String,
}
impl HttpTransport {
    fn new(cfg: &SyncConfig) -> Result<Self> {
        let base = normalize_url(&cfg.server_url)?;
        if base.is_empty() {
            return Err("sync_not_configured".into());
        }
        let key = cfg.key()?;
        let builder = Self::client_builder(cfg.use_proxy, &crate::proxy::current())?;
        Ok(Self {
            client: builder.build().map_err(|_| "sync_network")?,
            base,
            key,
        })
    }
    fn client_builder(
        use_proxy: bool,
        proxy: &crate::proxy::ProxyConfig,
    ) -> Result<reqwest::ClientBuilder> {
        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(40))
            .redirect(reqwest::redirect::Policy::none());
        // 显式直连优先于全局设置，也禁用系统与环境变量代理。
        if !use_proxy {
            return Ok(builder.no_proxy());
        }
        Ok(match proxy.mode.as_str() {
            "direct" => builder.no_proxy(),
            "custom" => {
                builder.proxy(reqwest::Proxy::all(&proxy.url).map_err(|_| "sync_invalid_proxy")?)
            }
            _ => builder,
        })
    }
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        // v1 的记录语义与内容哈希必须兼容；改变字段、计数口径或删除语义需使用新协议路径。
        self.client
            .request(method, format!("{}/api/v1/codex-usage/{path}", self.base))
            .bearer_auth(&self.key)
            .header("Accept", "application/json")
    }
    async fn response<T: serde::de::DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
    ) -> Result<T> {
        let mut res = req.send().await.map_err(|_| "sync_network")?;
        let status = res.status().as_u16();
        if status != 200 {
            return Err(match status {
                401 => "sync_unauthorized",
                403 => "sync_forbidden",
                409 => "sync_reset_required",
                429 => "sync_rate_limited",
                500..=599 => "sync_server",
                _ => "sync_protocol_invalid",
            }
            .into());
        }
        if res.content_length().is_some_and(|n| n > MAX_BYTES as u64) {
            return Err("sync_response_too_large".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(|_| "sync_network")? {
            if bytes.len() + chunk.len() > MAX_BYTES {
                return Err("sync_response_too_large".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| "sync_protocol_invalid".into())
    }
}
impl Transport for HttpTransport {
    async fn handshake(&self) -> Result<Handshake> {
        self.response(self.request(reqwest::Method::GET, "handshake"))
            .await
    }
    async fn push(&self, payload: &Push) -> Result<Acknowledged> {
        self.response(self.request(reqwest::Method::POST, "push").json(payload))
            .await
    }
    async fn changes(&self, h: &Handshake, cursor: Option<&str>) -> Result<Page> {
        let mut req = self.request(reqwest::Method::GET, "changes").query(&[
            ("spaceId", h.space_id.as_str()),
            ("epoch", h.epoch.as_str()),
            ("limit", "500"),
        ]);
        if let Some(c) = cursor {
            req = req.query(&[("cursor", c)]);
        }
        self.response(req).await
    }
}

/// DB 回调包含代次检查；测试可注入内存库。每页事务与每次确认独立提交，失败从断点恢复。
#[cfg(test)]
pub async fn exchange<T: Transport>(
    transport: &T,
    profile: &str,
    device: Device,
    db: impl FnMut(DbAction) -> Result<DbReply>,
) -> Result<()> {
    exchange_progress(transport, profile, device, db, &mut |_| Ok(())).await
}

#[derive(Clone, Debug, PartialEq)]
enum ExchangeProgress {
    Connecting,
    Uploading { uploaded: usize, total: usize },
    Downloading { records: usize, pages: usize },
    Recovering,
}
impl SyncProgress {
    fn exchange(&mut self, event: ExchangeProgress) {
        match event {
            ExchangeProgress::Connecting => self.phase = SyncPhase::Connecting,
            ExchangeProgress::Uploading { uploaded, total } => {
                self.phase = SyncPhase::Uploading;
                self.uploaded_records = uploaded;
                self.upload_total = Some(total);
            }
            ExchangeProgress::Downloading { records, pages } => {
                self.phase = SyncPhase::Downloading;
                self.downloaded_records = records;
                self.downloaded_pages = pages;
            }
            ExchangeProgress::Recovering => {
                self.phase = SyncPhase::Recovering;
                self.uploaded_records = 0;
                self.upload_total = None;
                self.downloaded_records = 0;
                self.downloaded_pages = 0;
            }
        }
    }
}
async fn exchange_progress<T: Transport>(
    transport: &T,
    profile: &str,
    device: Device,
    mut db: impl FnMut(DbAction) -> Result<DbReply>,
    progress: &mut impl FnMut(ExchangeProgress) -> Result<()>,
) -> Result<()> {
    progress(ExchangeProgress::Connecting)?;
    let h = transport.handshake().await?;
    h.validate()?;
    db(DbAction::Prepare(h.clone()))?;
    let mut sent_metadata = false;
    let mut uploaded = 0;
    loop {
        let DbReply::Count(pending) = db(DbAction::PendingCount)? else {
            return Err("sync_protocol_invalid".into());
        };
        let total = uploaded + pending;
        progress(ExchangeProgress::Uploading { uploaded, total })?;
        let DbReply::Pending(mut records) = db(DbAction::Pending)? else {
            return Err("sync_protocol_invalid".into());
        };
        if records.is_empty() && sent_metadata {
            break;
        }
        // 首轮空 push 仍登记设备或改名；handshake（含“测试连接”）只验证身份。
        let had_records = !records.is_empty();
        let mut payload = Push {
            protocol_version: 1,
            space_id: h.space_id.clone(),
            epoch: h.epoch.clone(),
            device: device.clone(),
            records: Vec::new(),
        };
        for r in records.drain(..) {
            payload.records.push(r);
            if serde_json::to_vec(&payload)
                .map_err(|_| "sync_invalid_record")?
                .len()
                > MAX_BYTES
            {
                payload.records.pop();
                break;
            }
        }
        if had_records && payload.records.is_empty() {
            return Err("sync_invalid_record".into());
        }
        // 服务端整批持久提交后才确认，包含已存在的 ID 且每个恰好一次；响应丢失可幂等重传。
        let ack = transport.push(&payload).await?;
        if ack.space_id != h.space_id || ack.epoch != h.epoch {
            return Err("sync_reset_required".into());
        }
        let mut expected: Vec<_> = payload
            .records
            .iter()
            .map(|r| r.record_id.clone())
            .collect();
        expected.sort();
        let mut actual = ack.acknowledged_ids;
        actual.sort();
        if actual != expected {
            return Err("sync_protocol_invalid".into());
        }
        let count = payload.records.len();
        db(DbAction::Ack(payload.records))?;
        uploaded += count;
        progress(ExchangeProgress::Uploading {
            uploaded,
            total: total.max(uploaded),
        })?;
        sent_metadata = true;
    }
    // 上传确认不能推进下载游标；服务端读取不消费数据，各客户端分别保存检查点。
    let mut downloaded = 0;
    let mut pages = 0;
    progress(ExchangeProgress::Downloading {
        records: 0,
        pages: 0,
    })?;
    let DbReply::Cursor(mut cursor) = db(DbAction::Cursor)? else {
        return Err("sync_protocol_invalid".into());
    };
    let mut seen = std::collections::HashSet::new();
    if let Some(c) = &cursor {
        seen.insert(c.clone());
    }
    loop {
        let page = transport.changes(&h, cursor.as_deref()).await?;
        if page.space_id != h.space_id || page.epoch != h.epoch {
            return Err("sync_reset_required".into());
        }
        if page.records.len() + page.devices.len() > MAX_RECORDS
            || page.next_cursor.is_empty()
            || page.next_cursor.len() > 4096
            || (page.has_more
                && (page.records.is_empty() && page.devices.is_empty()
                    || !seen.insert(page.next_cursor.clone())))
        {
            return Err("sync_protocol_invalid".into());
        }
        cursor = Some(page.next_cursor.clone());
        let more = page.has_more;
        let count = page.records.len();
        db(DbAction::Page(page))?;
        downloaded += count;
        pages += 1;
        progress(ExchangeProgress::Downloading {
            records: downloaded,
            pages,
        })?;
        if !more {
            break;
        }
    }
    let _ = profile; // 空间只由上层的固定 DB 回调选择，服务端不能控制本地存储命名空间。
    db(DbAction::Finish)?;
    Ok(())
}

/// 409 最多恢复一次；本机原始记录保留，清除确认和游标后重新握手。
#[cfg(test)]
async fn exchange_with_recovery<T: Transport>(
    transport: &T,
    profile: &str,
    device: Device,
    db: impl FnMut(DbAction) -> Result<DbReply>,
) -> Result<()> {
    exchange_recovery_progress(transport, profile, device, db, |_| Ok(())).await
}
async fn exchange_recovery_progress<T: Transport>(
    transport: &T,
    profile: &str,
    device: Device,
    mut db: impl FnMut(DbAction) -> Result<DbReply>,
    mut progress: impl FnMut(ExchangeProgress) -> Result<()>,
) -> Result<()> {
    let result =
        exchange_progress(transport, profile, device.clone(), &mut db, &mut progress).await;
    if result.as_ref().err().map(String::as_str) == Some("sync_reset_required") {
        db(DbAction::Reset)?;
        progress(ExchangeProgress::Recovering)?;
        return exchange_progress(transport, profile, device, db, &mut progress).await;
    }
    result
}
pub enum DbAction {
    Prepare(Handshake),
    Reset,
    Pending,
    PendingCount,
    Ack(Vec<WireRecord>),
    Cursor,
    Page(Page),
    Finish,
}
pub enum DbReply {
    Done,
    Pending(Vec<WireRecord>),
    Count(usize),
    Cursor(Option<String>),
}
pub fn apply_action(s: &mut store::Store, profile: &str, action: DbAction) -> Result<DbReply> {
    match action {
        DbAction::Prepare(h) => s.prepare_profile(profile, &h.space_id, &h.epoch)?,
        DbAction::Reset => s.reset_sync(profile)?,
        DbAction::Pending => return Ok(DbReply::Pending(s.pending(profile)?)),
        DbAction::PendingCount => return Ok(DbReply::Count(s.pending_count(profile)?)),
        DbAction::Ack(records) => s.acknowledge(profile, &records)?,
        DbAction::Cursor => return Ok(DbReply::Cursor(s.cursor(profile)?)),
        DbAction::Page(p) => {
            s.apply_page(profile, &p.records, &p.devices, &p.next_cursor)?;
        }
        DbAction::Finish => s.finish(profile)?,
    }
    Ok(DbReply::Done)
}

async fn wait_for_round(mut done: RoundReceiver) -> Result<()> {
    loop {
        if let Some(result) = done.borrow().clone() {
            return result;
        }
        done.changed().await.map_err(|_| "sync_task_failed")?;
    }
}

async fn synchronize(app: &AppHandle, trigger: SyncTrigger) -> Result<()> {
    if crate::operations::required() {
        return Err("update_required".into());
    }
    let done = {
        let mut r = runtime().lock().map_err(|_| "sync_lock_failed")?;
        if let Some(done) = r.join() {
            done
        } else {
            let cfg = current()?;
            let interval = settings::read(|s| s.interval_minutes)?;
            let last = store::with_store(|s| s.success(&cfg.profile()?))?;
            if !r.should_sync(
                trigger,
                interval,
                last,
                chrono::Utc::now().timestamp_millis(),
            ) {
                return Ok(());
            }
            let key = r.begin_progress();
            let app = app.clone();
            let finished_app = app.clone();
            r.start(
                async move {
                    // 新配置可排队，但旧请求退出前不并发执行另一轮网络交换。
                    let _gate = gate().lock().await;
                    let result = async {
                        checked(key.generation, || Ok(()))?;
                        let _operation = crate::operations::begin()?;
                        sync_round(&app, &cfg, key).await
                    }
                    .await;
                    let result = {
                        let mut r = runtime().lock().map_err(|_| "sync_lock_failed")?;
                        if r.generation != key.generation || r.round_id != key.id {
                            Err("sync_cancelled".into())
                        } else {
                            r.record_result(&result);
                            result
                        }
                    };
                    if result.as_ref().err().map(String::as_str) != Some("sync_cancelled") {
                        audit::log(
                            if result.is_ok() {
                                "codex_usage_sync_ok"
                            } else {
                                "codex_usage_sync_failed"
                            },
                            match &result {
                                Ok(()) => "GPT 用量同步完成".into(),
                                Err(e) => format!("GPT 用量同步失败：{e}"),
                            },
                            None,
                        );
                        // 网络失败时本机采集仍可能已入库，同样通知界面读取存档。
                        notify_changed(&app, "sync");
                    }
                    result
                },
                move || {
                    emit_status(&finished_app);
                },
            )
        }
    };
    emit_status(app);
    wait_for_round(done).await
}
async fn sync_round(app: &AppHandle, cfg: &SyncConfig, key: RoundKey) -> Result<()> {
    report_progress(app, key, |p| p.phase = SyncPhase::Scanning)?;
    let home = cfg.source_home.clone();
    let (files, _) =
        tauri::async_runtime::spawn_blocking(move || codex::collect_sessions(Some(home)))
            .await
            .map_err(|_| "sync_scan_failed")?;
    report_progress(app, key, |p| p.phase = SyncPhase::Storing)?;
    checked(key.generation, || store::with_store(|s| s.ingest(files)))?;
    if !cfg.configured() {
        return Ok(());
    }
    let profile = cfg.profile()?;
    let transport = HttpTransport::new(&cfg)?;
    let own = device(&cfg)?;
    exchange_recovery_progress(
        &transport,
        &profile,
        own,
        |a| {
            checked(key.generation, || {
                store::with_store(|s| apply_action(s, &profile, a))
            })
        },
        |event| report_progress(app, key, |p| p.exchange(event)),
    )
    .await
}
#[tauri::command]
pub async fn codex_usage_sync_now(
    app: AppHandle,
    trigger: Option<SyncTrigger>,
) -> Result<SyncStatus> {
    let _operation = crate::operations::begin()?;
    synchronize(&app, trigger.unwrap_or_default()).await?;
    codex_usage_sync_status()
}
#[tauri::command]
pub async fn codex_usage_sync_test(config: ConfigInput) -> Result<Handshake> {
    let _operation = crate::operations::begin()?;
    let cfg = config_from(config, &current()?)?;
    let h = HttpTransport::new(&cfg)?.handshake().await?;
    h.validate()?;
    Ok(h)
}
pub async fn query(
    days: Option<i64>,
    since_ms: Option<i64>,
    until_ms: Option<i64>,
    home: Option<String>,
    device_id: Option<String>,
    scan: bool,
) -> Result<codex::CodexScan> {
    let (cfg, generation) = {
        let r = runtime().lock().map_err(|_| "sync_lock_failed")?;
        (current()?, r.generation)
    };
    tauri::async_runtime::spawn_blocking(move || {
        let (files, roots) = if scan {
            codex::collect_sessions(home.or(Some(cfg.source_home.clone())))
        } else {
            (Vec::new(), Vec::new())
        };
        let count = files.len();
        let profile = cfg.profile()?;
        let records = checked(generation, || {
            store::with_store(|s| {
                if scan {
                    s.ingest(files)?;
                }
                s.records(&profile)
            })
        })?;
        let (rows, sessions) = store::aggregate_rows(
            records,
            device_id.as_deref(),
            &TimeRange::new(days, since_ms, until_ms),
        );
        let hourly = pricing::hourly_dates_for(since_ms, until_ms.map(|u| u - 1));
        Ok(codex::CodexScan {
            aggregate: pricing::aggregate_and_price_for(rows, &pricing::load(), hourly.as_deref()),
            files_scanned: count,
            sessions,
            roots,
            days,
        })
    })
    .await
    .map_err(|_| "sync_scan_failed")?
}
#[tauri::command]
pub async fn codex_usage_query(
    days: Option<i64>,
    since_ms: Option<i64>,
    until_ms: Option<i64>,
    device_id: Option<String>,
) -> Result<codex::CodexScan> {
    let _operation = crate::operations::begin()?;
    query(days, since_ms, until_ms, None, device_id, false).await
}
#[tauri::command]
pub fn codex_usage_devices() -> Result<Vec<DeviceView>> {
    let _operation = crate::operations::begin()?;
    let cfg = current()?;
    let own = device(&cfg)?;
    store::with_store(|s| s.devices(&cfg.profile()?, &own.name))
}
#[tauri::command]
pub fn codex_usage_cache_clear(app: AppHandle, device_id: String) -> Result<usize> {
    let _operation = crate::operations::begin()?;
    let n = {
        let mut r = runtime().lock().map_err(|_| "sync_lock_failed")?;
        let cfg = current()?;
        let n = store::with_store(|s| s.clear_device(&cfg.profile()?, &device_id))?;
        r.invalidate();
        n
    };
    audit::log(
        "codex_usage_cache_clear",
        "清理当前 MAA 的 GPT 设备用量缓存（中心数据保留）".into(),
        None,
    );
    notify_changed(&app, "clear");
    emit_status(&app);
    Ok(n)
}

#[cfg(test)]
#[path = "tests/codex_sync.rs"]
mod tests;

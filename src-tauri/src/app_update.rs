//! 应用更新：索引负责发现版本，官方 updater 负责签名验证与安装。
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::Notify;
use crate::{operations, settings, update_protocol::{self as protocol, Release}};

type Result<T> = std::result::Result<T, Failure>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences { pub enabled: bool }
impl Default for Preferences { fn default() -> Self { Self { enabled: true } } }

#[derive(Debug, Clone, Serialize)]
pub struct Failure { pub code: String, pub message: String }
impl Failure {
    fn new(code: &str) -> Self {
        let message = match code {
            "timeout" => "连接超时，请检查网络或代理后重试。",
            "network" => "无法连接更新源，请检查网络或代理；也可以前往 GitHub Releases。",
            "not_published" => "该平台尚无可读取的版本索引，请前往 GitHub Releases 查看。",
            "http" => "更新源暂时无法访问，请稍后重试或前往 GitHub Releases。",
            "manifest_missing" => "该版本更新清单尚不可用，请稍后重试或前往对应的 GitHub Release。",
            "stale_index" => "更新源返回了较旧的索引，已保留已知的必需更新。请稍后重试。",
            "invalid_json" | "index_identity" | "index_version" | "index_url" | "manifest_url" | "manifest_invalid" => "更新信息校验失败，本次不会安装。请重试或前往 GitHub Releases 核对版本。",
            "signature" => "更新包签名校验失败，已停止安装。请重试或从 GitHub Releases 获取安装包。",
            "download" => "下载更新包失败，请检查网络或代理后重试。",
            "install" => "安装更新失败，用户数据保持不变。请重试或前往 GitHub Releases 手动安装。",
            "update_busy" => "仍有业务操作未完成，已取消安装。请稍后重试。",
            "manual_only" => "当前运行方式需要手动更新，请前往 GitHub Releases 下载。",
            "no_update" => "请先检查并选择可用的新版本。",
            "unsupported_platform" => "当前平台暂不支持在线更新，请前往 GitHub Releases 查看。",
            "cancelled" => "已取消下载，当前版本保持不变。",
            "open_failed" => "无法打开浏览器，请复制页面中的 GitHub 地址。",
            "settings" => "保存更新设置失败，请检查用户数据目录权限。",
            _ => "更新操作失败，请重试或前往 GitHub Releases。",
        };
        Self { code: code.into(), message: message.into() }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub revision: u64,
    pub phase: String,
    pub current_version: String,
    pub current_changelog: Option<String>,
    pub current_changelog_error: Option<Failure>,
    pub latest_version: Option<String>,
    pub changelog: String,
    pub force_update: bool,
    pub checked_at_ms: Option<i64>,
    pub error: Option<Failure>,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub can_install: bool,
    pub manual_reason: Option<String>,
    pub releases_url: String,
    pub automatic: bool,
}
impl Default for View {
    fn default() -> Self {
        Self { revision: 0, phase: "idle".into(), current_version: env!("CARGO_PKG_VERSION").into(),
            current_changelog: None, current_changelog_error: None,
            latest_version: None, changelog: String::new(), force_update: false, checked_at_ms: None,
            error: None, downloaded_bytes: 0, total_bytes: None, can_install: false,
            manual_reason: None, releases_url: protocol::RELEASES.into(), automatic: true }
    }
}
#[derive(Default)]
struct Runtime { view: View, release: Option<Release>, cancel: Option<Arc<Notify>> }
fn runtime() -> &'static Mutex<Runtime> {
    static STATE: OnceLock<Mutex<Runtime>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(Runtime::default()))
}
fn action() -> &'static tokio::sync::Mutex<()> {
    static ACTION: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    ACTION.get_or_init(|| tokio::sync::Mutex::new(()))
}
fn automatic() -> bool { settings::read(|s| s.app_update.enabled).unwrap_or(true) }
fn snapshot() -> View {
    let mut view = runtime().lock().unwrap_or_else(|e| e.into_inner()).view.clone();
    view.automatic = automatic();
    view
}
fn change(app: &AppHandle, f: impl FnOnce(&mut Runtime)) -> View {
    let view = {
        let mut r = runtime().lock().unwrap_or_else(|e| e.into_inner());
        f(&mut r);
        r.view.revision += 1;
        r.view.automatic = automatic();
        r.view.clone()
    };
    let _ = app.emit("app-update-state", &view);
    view
}
fn fail(app: &AppHandle, failure: Failure) -> Failure {
    change(app, |r| {
        r.view.phase = if failure.code == "cancelled" { "cancelled" } else { "error" }.into();
        r.view.error = Some(failure.clone());
        r.cancel = None;
    });
    failure
}

fn platform() -> Result<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) { Ok("windows-x64") }
    else if cfg!(all(target_os = "macos", target_arch = "aarch64")) { Ok("macos-arm64") }
    else { Err(Failure::new("unsupported_platform")) }
}
fn install_target() -> std::result::Result<&'static str, &'static str> {
    if cfg!(debug_assertions) { return Err("开发环境仅检查更新，请使用正式安装版更新。"); }
    #[cfg(windows)]
    {
        use tauri::utils::{config::BundleType, platform::bundle_type};
        return match bundle_type() {
            Some(BundleType::Nsis) => Ok("windows-x86_64-nsis"),
            Some(BundleType::Msi) => Ok("windows-x86_64-msi"),
            _ => Err("当前为便携版，请从 GitHub Releases 下载并手动替换程序。"),
        };
    }
    #[cfg(target_os = "macos")]
    {
        let exe = tauri::utils::platform::current_exe().map_err(|_| "无法确定应用安装位置，请手动更新。")?;
        if exe.starts_with("/Volumes") || exe.to_string_lossy().contains("/AppTranslocation/") {
            return Err("请先将应用移到 Applications 后重新打开，再使用自动更新。");
        }
        let bundle = exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app"))
            .ok_or("当前不是已安装的 macOS 应用，请手动更新。")?;
        let parent = bundle.parent().ok_or("应用目录不可写，请手动更新。")?;
        let name = crate::codex_store::random_id().map_err(|_| "无法检查应用目录，请手动更新。")?;
        let probe = parent.join(format!(".maa-update-{name}.tmp"));
        std::fs::OpenOptions::new().write(true).create_new(true).open(&probe)
            .map_err(|_| "应用目录不可写，请从 GitHub Releases 手动安装。")?;
        std::fs::remove_file(&probe).map_err(|_| "应用目录不可写，请手动更新。")?;
        return Ok("darwin-aarch64");
    }
    #[allow(unreachable_code)]
    Err("当前平台需要手动更新。")
}
fn artifact(version: &str, target: &str) -> String {
    let name = match target {
        "windows-x86_64-nsis" => format!("my-ai-assistant_{version}_x64-setup.exe"),
        "windows-x86_64-msi" => format!("my-ai-assistant_{version}_x64_en-US.msi"),
        _ => "my-ai-assistant.app.tar.gz".into(),
    };
    protocol::asset_url(version, &name)
}
fn client() -> Result<update_http::Client> {
    let proxy = crate::proxy::current();
    let mut b = update_http::Client::builder().user_agent("my-ai-assistant-updater")
        .timeout(Duration::from_secs(20)).https_only(true);
    match proxy.mode.as_str() {
        "direct" => b = b.no_proxy(),
        "custom" => b = b.proxy(update_http::Proxy::all(&proxy.url).map_err(|_| Failure::new("network"))?),
        _ => {}
    }
    b.build().map_err(|_| Failure::new("network"))
}
fn network_error(e: update_http::Error) -> Failure {
    Failure::new(if e.is_timeout() { "timeout" } else { "network" })
}
fn install_error(error: tauri_plugin_updater::Error) -> Failure {
    if let tauri_plugin_updater::Error::PreviousAppNotRestored(path) = error {
        return Failure { code: "install_restore".into(), message: format!(
            "安装失败，旧应用保留在 {}。请将其移回原安装位置，或前往 GitHub Releases 手动安装；用户数据保持不变。",
            path.display()) };
    }
    Failure::new("install")
}
async fn fetch_index(platform: &str, version: Option<&str>) -> Result<Release> {
    let file = version.map(|v| format!("versions/{v}.json")).unwrap_or_else(|| "latest.json".into());
    let url = format!("{}/projects/my-ai-assistant/{platform}/{file}", protocol::REGISTRY);
    let release = fetch_index_with(client()?, &url, platform).await?;
    if version.is_some_and(|v| v != release.version) { return Err(Failure::new("index_version")); }
    Ok(release)
}
async fn fetch_index_with(client: update_http::Client, url: &str, platform: &str) -> Result<Release> {
    let mut response = client.get(url).header("Cache-Control", "no-cache").send().await.map_err(network_error)?;
    if response.status().as_u16() == 404 { return Err(Failure::new("not_published")); }
    if !response.status().is_success() { return Err(Failure::new("http")); }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network_error)? {
        if bytes.len() + chunk.len() > 512 * 1024 { return Err(Failure::new("invalid_json")); }
        bytes.extend_from_slice(&chunk);
    }
    let release: Release = serde_json::from_slice(&bytes).map_err(|_| Failure::new("invalid_json"))?;
    release.validate(platform).map_err(Failure::new)?;
    Ok(release)
}
fn update_current_changelog(view: &mut View, latest: Option<&Release>, current: Result<Release>) {
    // latest 与当前版本相同时复用其日志，避免版本快照暂不可读时丢失内容。
    let notes = match latest.filter(|release| release.version == view.current_version) {
        Some(release) => Ok(release.changelog.clone().unwrap_or_default()),
        None => current.and_then(|release| {
            if release.version != view.current_version { return Err(Failure::new("index_version")); }
            Ok(release.changelog.unwrap_or_default())
        }),
    };
    match notes {
        Ok(notes) => { view.current_changelog = Some(notes); view.current_changelog_error = None; }
        Err(error) => { view.current_changelog_error = Some(error); }
    }
}
async fn prepare(app: &AppHandle, release: &Release, target: &str) -> Result<Update> {
    let endpoint = protocol::manifest_url(&release.version, &release.platform_code)
        .parse().map_err(|_| Failure::new("manifest_url"))?;
    let mut builder = app.updater_builder().target(target).timeout(Duration::from_secs(20))
        .endpoints(vec![endpoint]).map_err(|_| Failure::new("manifest_url"))?;
    let proxy = crate::proxy::current();
    match proxy.mode.as_str() {
        "direct" => builder = builder.no_proxy(),
        "custom" => builder = builder.proxy(proxy.url.parse().map_err(|_| Failure::new("network"))?),
        _ => {}
    }
    let mut update = builder.build().map_err(|_| Failure::new("manifest_invalid"))?
        .check().await.map_err(|e| match e {
            tauri_plugin_updater::Error::Reqwest(e) => network_error(e),
            tauri_plugin_updater::Error::ReleaseNotFound => Failure::new("manifest_missing"),
            _ => Failure::new("manifest_invalid"),
        })?.ok_or_else(|| Failure::new("manifest_invalid"))?;
    if update.version != release.version || update.download_url.as_str() != artifact(&release.version, target)
        || update.signature.trim().is_empty() {
        return Err(Failure::new("manifest_invalid"));
    }
    update.timeout = Some(Duration::from_secs(15 * 60));
    Ok(update)
}

#[tauri::command]
pub fn app_update_status() -> View { snapshot() }

#[tauri::command]
pub fn app_update_settings_set(app: AppHandle, enabled: bool) -> Result<View> {
    let _operation = operations::begin().map_err(|_| Failure::new("update_busy"))?;
    settings::mutate(|s| { s.app_update.enabled = enabled; Ok(()) }).map_err(|_| Failure::new("settings"))?;
    Ok(change(&app, |_| {}))
}

#[tauri::command]
pub async fn app_update_check(app: AppHandle) -> Result<View> {
    let Ok(_action) = action().try_lock() else { return Ok(snapshot()); };
    change(&app, |r| { r.view.phase = "checking".into(); r.view.error = None; r.view.can_install = false; r.release = None; });
    let result = async {
        let platform = platform()?;
        // 只读取当前版本和最新索引；日志失败不阻断升级检查，也不枚举历史版本。
        let (release, current_release) = tokio::join!(
            fetch_index(platform, None),
            fetch_index(platform, Some(env!("CARGO_PKG_VERSION"))),
        );
        change(&app, |r| update_current_changelog(&mut r.view, release.as_ref().ok(), current_release));
        let release = release?;
        let current = protocol::version_code(env!("CARGO_PKG_VERSION")).ok_or_else(|| Failure::new("index_version"))?;
        let required = settings::read(|s| s.required_update.clone()).ok().flatten();
        if required.as_ref().is_some_and(|old| valid_required(old, platform, current) && old.version_code > release.version_code) {
            return Err(Failure::new("stale_index"));
        }
        settings::mutate(|s| {
            s.required_update = (release.force_update && release.version_code > current).then(|| release.clone());
            Ok(())
        }).map_err(|_| Failure::new("settings"))?;
        if release.version_code <= current {
            operations::require_update(false);
            return Ok(change(&app, |r| {
                r.view.phase = "upToDate".into(); r.view.latest_version = None;
                r.view.force_update = false; r.view.changelog.clear(); r.view.manual_reason = None;
                r.view.releases_url = protocol::RELEASES.into();
                r.view.checked_at_ms = Some(chrono::Utc::now().timestamp_millis());
            }));
        }
        operations::require_update(release.force_update);
        change(&app, |r| {
            r.view.latest_version = Some(release.version.clone());
            r.view.releases_url = protocol::release_page(Some(&release.version));
            r.view.changelog = release.changelog.clone().unwrap_or_default();
            r.view.force_update = release.force_update;
            r.release = Some(release.clone());
        });
        let manual_reason = match install_target() {
            Err(reason) => Some(reason.to_string()),
            Ok(_) if !release.has_manifest() => Some("该版本未提供自动更新包，请前往 GitHub Releases 手动安装。".into()),
            Ok(target) => { prepare(&app, &release, target).await?; None }
        };
        Ok(change(&app, |r| {
            r.view.phase = "available".into();
            r.view.can_install = manual_reason.is_none(); r.view.manual_reason = manual_reason;
            r.view.checked_at_ms = Some(chrono::Utc::now().timestamp_millis());
        }))
    }.await;
    result.map_err(|e| {
        change(&app, |r| { r.view.checked_at_ms = Some(chrono::Utc::now().timestamp_millis()); });
        fail(&app, e)
    })
}

#[tauri::command]
pub fn app_update_cancel() {
    let r = runtime().lock().unwrap_or_else(|e| e.into_inner());
    if matches!(r.view.phase.as_str(), "downloading" | "waiting") {
        if let Some(cancel) = &r.cancel { cancel.notify_one(); }
    }
}

#[tauri::command]
pub async fn app_update_install(app: AppHandle, version: String) -> Result<View> {
    let Ok(_action) = action().try_lock() else { return Ok(snapshot()); };
    let release = {
        let r = runtime().lock().unwrap_or_else(|e| e.into_inner());
        r.release.clone().filter(|v| v.version == version && r.view.can_install)
    }.ok_or_else(|| fail(&app, Failure::new("no_update")))?;
    let target = install_target().map_err(|_| fail(&app, Failure::new("manual_only")))?;
    let cancel = Arc::new(Notify::new());
    change(&app, |r| {
        r.view.phase = "downloading".into(); r.view.error = None;
        r.view.downloaded_bytes = 0; r.view.total_bytes = None; r.cancel = Some(cancel.clone());
    });
    let result = async {
        // 再次读取固定版本清单，使用最新代理配置；不会改成用户未确认的另一个版本。
        let work = async {
            let update = prepare(&app, &release, target).await?;
            let mut last = std::time::Instant::now() - Duration::from_secs(1);
            let mut downloaded = 0u64;
            let bytes = update.download(|chunk, total| {
                downloaded += chunk as u64;
                if last.elapsed() >= Duration::from_millis(150) || Some(downloaded) == total {
                    change(&app, |r| { r.view.downloaded_bytes = downloaded; r.view.total_bytes = total; });
                    last = std::time::Instant::now();
                }
            }, || {}).await.map_err(|e| match e {
                tauri_plugin_updater::Error::Reqwest(e) => network_error(e),
                tauri_plugin_updater::Error::Minisign(_)
                | tauri_plugin_updater::Error::Base64(_)
                | tauri_plugin_updater::Error::SignatureUtf8(_)
                | tauri_plugin_updater::Error::SignedVersionMismatch { .. }
                | tauri_plugin_updater::Error::MissingSignedVersion => Failure::new("signature"),
                _ => Failure::new("download"),
            })?;
            Ok::<_, Failure>((update, bytes))
        };
        let (update, bytes) = tokio::select! {
            biased;
            _ = cancel.notified() => return Err(Failure::new("cancelled")),
            result = work => result?,
        };
        change(&app, |r| { r.view.phase = "waiting".into(); r.view.downloaded_bytes = bytes.len() as u64; });
        let pause = tokio::select! {
            biased;
            _ = cancel.notified() => return Err(Failure::new("cancelled")),
            result = operations::pause(Duration::from_secs(30)) => result.map_err(|_| Failure::new("update_busy"))?,
        };
        change(&app, |r| { r.view.phase = "installing".into(); r.cancel = None; });
        // Pause 留在阻塞任务中：未来被丢弃也不会提前放行业务操作。
        let (installed, _pause) = tauri::async_runtime::spawn_blocking(move || {
            (update.install(bytes).map_err(install_error), pause)
        }).await.map_err(|_| Failure::new("install"))?;
        installed?;
        // Windows updater 成功启动安装器后退出；macOS 安装完成需显式重启。
        #[cfg(target_os = "macos")]
        app.restart();
        #[cfg(not(target_os = "macos"))]
        Ok(snapshot())
    }.await;
    result.map_err(|e| fail(&app, e))
}

pub fn open_browser(url: &str) -> std::result::Result<(), String> {
    let result = if cfg!(windows) {
        std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else { return Err("open_failed".into()); };
    result.map(|_| ()).map_err(|_| "open_failed".into())
}
#[tauri::command]
pub fn app_update_open_releases() -> Result<()> {
    open_browser(&snapshot().releases_url).map_err(|_| Failure::new("open_failed"))
}
#[tauri::command]
pub fn app_update_open_pricing() -> Result<()> {
    open_browser(protocol::PRICING_PAGE).map_err(|_| Failure::new("open_failed"))
}

fn valid_required(release: &Release, platform: &str, current: u32) -> bool {
    release.force_update && release.version_code > current && release.validate(platform).is_ok()
}
pub fn settings_changed(app: &AppHandle) { change(app, |_| {}); }

pub fn start(app: AppHandle) {
    if let (Ok(platform), Some(current), Some(release)) = (platform(), protocol::version_code(env!("CARGO_PKG_VERSION")),
        settings::read(|s| s.required_update.clone()).ok().flatten()) {
        if valid_required(&release, platform, current) {
            operations::require_update(true);
            change(&app, |r| {
                r.view.phase = "available".into();
                r.view.latest_version = Some(release.version.clone());
                r.view.releases_url = protocol::release_page(Some(&release.version));
                r.view.changelog = release.changelog.clone().unwrap_or_default();
                r.view.force_update = true;
                r.view.manual_reason = Some("此版本需要更新，请检查更新以重新验证安装包，或前往 GitHub Releases。".into());
                r.release = Some(release);
            });
        }
    }
    tauri::async_runtime::spawn(schedule(move || {
        let app = app.clone();
        async move { if automatic() { let _ = app_update_check(app).await; } }
    }));
}
async fn schedule<F, Fut>(mut check: F)
where F: FnMut() -> Fut, Fut: std::future::Future<Output = ()> {
    tokio::time::sleep(Duration::from_secs(protocol::START_DELAY_SECS)).await;
    loop {
        check().await;
        tokio::time::sleep(Duration::from_secs(protocol::CHECK_INTERVAL_SECS)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test(start_paused = true)]
    async fn scheduler_waits_thirty_seconds_then_twenty_four_hours() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let task = tokio::spawn(schedule(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            async {}
        }));
        tokio::task::yield_now().await;
        for (seconds, expected) in [(29, 0), (1, 1), (86_399, 1), (1, 2)] {
            tokio::time::advance(Duration::from_secs(seconds)).await;
            tokio::task::yield_now().await;
            assert_eq!(calls.load(Ordering::SeqCst), expected);
        }
        task.abort();
        let _ = task.await;
    }

    fn fixture() -> Release {
        serde_json::from_value(serde_json::json!({"projectCode":"my-ai-assistant",
            "platformCode":"windows-x64", "version":"9.0.0", "versionCode":protocol::version_code("9.0.0"),
            "forceUpdate":true, "changelog":"test", "downloadUrl":null, "extra":null})).unwrap()
    }
    #[test]
    fn required_update_cache_cannot_block_a_current_or_different_platform_build() {
        let r = fixture();
        assert!(valid_required(&r, "windows-x64", protocol::version_code("8.0.0").unwrap()));
        assert!(!valid_required(&r, "windows-x64", r.version_code));
        assert!(!valid_required(&r, "macos-arm64", 0));
    }

    #[test]
    fn current_notes_use_only_the_installed_version_and_survive_failures() {
        let release = fixture();
        let mut view = View { current_version: "9.0.0".into(), ..View::default() };
        update_current_changelog(&mut view, Some(&release), Err(Failure::new("not_published")));
        assert_eq!(view.current_changelog.as_deref(), Some("test"));
        assert!(view.current_changelog_error.is_none());

        let mut newer = release.clone();
        newer.version = "9.1.0".into();
        newer.changelog = Some("new version".into());
        update_current_changelog(&mut view, Some(&newer), Err(Failure::new("network")));
        assert_eq!(view.current_changelog.as_deref(), Some("test"));
        assert_eq!(view.current_changelog_error.as_ref().unwrap().code, "network");
        update_current_changelog(&mut view, Some(&newer), Ok(release));
        assert_eq!(view.current_changelog.as_deref(), Some("test"));
        assert!(view.current_changelog_error.is_none());

        let mut development = View { current_version: "9.2.0".into(), ..View::default() };
        update_current_changelog(&mut development, Some(&newer), Err(Failure::new("not_published")));
        assert!(development.current_changelog.is_none());
        update_current_changelog(&mut development, None, Ok(newer));
        assert!(development.current_changelog.is_none());
        assert_eq!(development.current_changelog_error.unwrap().code, "index_version");
    }

    fn server(status: u16, body: String, delay: Duration) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/latest.json", listener.local_addr().unwrap());
        let thread = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let _ = socket.read(&mut [0u8; 2048]);
            std::thread::sleep(delay);
            let reply = format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = socket.write_all(reply.as_bytes());
        });
        (url, thread)
    }
    #[tokio::test]
    async fn http_errors_invalid_json_identity_and_timeouts_are_distinct() {
        let good = serde_json::to_string(&fixture()).unwrap();
        for (status, body, delay, code) in [
            (404, "missing".into(), 0, "not_published"),
            (503, "unavailable".into(), 0, "http"),
            (200, "{".into(), 0, "invalid_json"),
            (200, good.replace("windows-x64", "macos-arm64"), 0, "index_identity"),
            (200, good.replace("9.0.0", "8.0.0"), 0, "index_version"),
            (200, good.clone(), 150, "timeout"),
        ] {
            let (url, thread) = server(status, body, Duration::from_millis(delay));
            let client = update_http::Client::builder().no_proxy().timeout(Duration::from_millis(if delay > 0 { 40 } else { 2000 })).build().unwrap();
            assert_eq!(fetch_index_with(client, &url, "windows-x64").await.unwrap_err().code, code);
            thread.join().unwrap();
        }
        let (url, thread) = server(200, good, Duration::ZERO);
        let client = update_http::Client::builder().no_proxy().build().unwrap();
        assert_eq!(fetch_index_with(client, &url, "windows-x64").await.unwrap().version, "9.0.0");
        thread.join().unwrap();
    }
    #[tokio::test]
    async fn failed_proxy_is_reported_as_a_network_failure() {
        let (address, thread) = server(407, "Proxy authentication required".into(), Duration::ZERO);
        let client = update_http::Client::builder().no_proxy()
            .proxy(update_http::Proxy::all(&address).unwrap()).timeout(Duration::from_secs(2)).build().unwrap();
        let error = fetch_index_with(client, "https://127.0.0.1:1/latest.json", "windows-x64").await.unwrap_err();
        thread.join().unwrap();
        assert_eq!(error.code, "network");
    }
    #[tokio::test]
    async fn concurrent_requests_share_the_single_active_action() {
        let owner = action().try_lock().unwrap();
        assert!(action().try_lock().is_err());
        drop(owner);
        assert!(action().try_lock().is_ok());
    }
    #[test]
    fn failures_never_claim_up_to_date_or_expose_remote_response() {
        for code in ["timeout", "network", "not_published", "invalid_json", "signature", "install"] {
            let e = Failure::new(code);
            assert!(!e.message.contains("最新版本"));
            assert_eq!(e.code, code);
            assert!(!e.message.contains("https://"));
        }
    }
    #[test]
    fn installer_mapping_preserves_distribution() {
        assert!(artifact("1.3.0", "windows-x86_64-nsis").ends_with("_x64-setup.exe"));
        assert!(artifact("1.3.0", "windows-x86_64-msi").ends_with("_x64_en-US.msi"));
        assert!(artifact("1.3.0", "darwin-aarch64").ends_with(".app.tar.gz"));
        assert!(Preferences::default().enabled);
    }
    #[test]
    fn failed_restore_explains_where_the_previous_app_was_kept() {
        let error = install_error(tauri_plugin_updater::Error::PreviousAppNotRestored(
            std::path::PathBuf::from("/tmp/previous.app")));
        assert_eq!(error.code, "install_restore");
        assert!(error.message.contains("/tmp/previous.app"));
        assert!(error.message.contains("移回原安装位置"));
    }
}

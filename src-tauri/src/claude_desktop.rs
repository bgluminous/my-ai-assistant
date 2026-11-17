//! Local-only Desktop login snapshots. OAuth cookies remain in their native OS
//! encryption. Only authentication-owned paths are restored; chats/MCP/UI settings
//! are not copied. A durable journal precedes every change to the live profile.
use crate::{accounts, claude_local, codex_auth_store::atomic_write, local_crypto, paths};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

const LOGIN_PATHS: &[&str] = &[
    "Local State",
    "Cookies",
    "Cookies-journal",
    "Cookies-wal",
    "Cookies-shm",
    "Network",
    "Local Storage",
    "IndexedDB",
    "Session Storage",
    "Service Worker",
    "WebStorage",
    "DIPS",
    "DIPS-wal",
    "SharedStorage",
    "SharedStorage-wal",
    "ant-did",
];
const MAX_BYTES: usize = 128 * 1024 * 1024;
const TIMEOUT_MS: i64 = 15 * 60_000;

#[derive(Clone, Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    owner: String,
    identity: Option<String>,
    auth: BTreeMap<String, Value>,
    files: BTreeMap<String, String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Journal {
    id: String,
    account: String,
    target: PathBuf,
    before: Snapshot,
    deadline: i64,
    binding: bool,
    was_running: bool,
}
fn pending() -> &'static Mutex<Option<(String, crate::operations::Operation<'static>)>> {
    static PENDING: OnceLock<Mutex<Option<(String, crate::operations::Operation<'static>)>>> =
        OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(None))
}
fn store_dir() -> Result<PathBuf, String> {
    paths::app_dir()
        .map(|p| p.join("claude-desktop"))
        .ok_or_else(|| "home_dir_unavailable".into())
}
fn journal_path() -> Result<PathBuf, String> {
    Ok(store_dir()?.join("recovery.json"))
}
fn binding_path(id: &str) -> Result<PathBuf, String> {
    Ok(store_dir()?
        .join("bindings")
        .join(format!("{:x}.json", Sha256::digest(id.as_bytes()))))
}
fn machine_id() -> Result<String, String> {
    #[cfg(test)]
    {
        return Ok("isolated-fixture-machine".into());
    }
    #[cfg(not(test))]
    {
        static ID: OnceLock<Result<String, String>> = OnceLock::new();
        ID.get_or_init(|| {
            let mut command = if cfg!(windows) {
                let mut c = std::process::Command::new("reg.exe");
                c.args([
                    "query",
                    r"HKLM\SOFTWARE\Microsoft\Cryptography",
                    "/v",
                    "MachineGuid",
                ]);
                c
            } else {
                let mut c = std::process::Command::new("ioreg");
                c.args(["-rd1", "-c", "IOPlatformExpertDevice"]);
                c
            };
            let output = crate::local_client::run_hidden(&mut command)
                .map_err(|_| "claude_desktop_machine_unavailable")?;
            if !output.status.success() {
                return Err("claude_desktop_machine_unavailable".into());
            }
            String::from_utf8_lossy(&output.stdout)
                .split(|c: char| c.is_whitespace() || c == '"')
                .find(|s| s.len() == 36 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
                .map(str::to_string)
                .ok_or_else(|| "claude_desktop_machine_unavailable".into())
        })
        .clone()
    }
}
fn owner(target: &Path) -> Result<String, String> {
    let target = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    let target = target.to_string_lossy().to_string();
    let target = if cfg!(windows) {
        target.trim_start_matches(r"\\?\").to_ascii_lowercase()
    } else {
        target
    };
    Ok(format!(
        "{:x}",
        Sha256::digest(
            format!(
                "{}|{}|{}|{}",
                std::env::consts::OS,
                machine_id()?,
                paths::home_dir().unwrap_or_default().display(),
                target
            )
            .as_bytes()
        )
    ))
}
fn auth_key(key: &str) -> bool {
    matches!(
        key,
        "oauth:tokenCache"
            | "oauth:tokenCacheV2"
            | "lastKnownAccountUuid"
            | "coworkTrustedDeviceToken"
    ) || key.starts_with("coworkTrustedDeviceToken:")
}
fn write_sealed<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let text = serde_json::to_string(value).map_err(|_| "claude_desktop_snapshot_invalid")?;
    atomic_write(path, local_crypto::seal(&text)?.as_bytes())
}
fn read_sealed<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let encrypted = fs::read_to_string(path).map_err(|_| "claude_desktop_snapshot_unreadable")?;
    let clear = local_crypto::open(&encrypted).ok_or("claude_desktop_snapshot_invalid")?;
    serde_json::from_str(&clear).map_err(|_| "claude_desktop_snapshot_invalid".into())
}
fn account_uuid(id: &str) -> Result<String, String> {
    let account = accounts::account_snapshot(id)?;
    if account.kind != "claude" {
        return Err("not_claude_account".into());
    }
    account
        .status
        .as_ref()
        .and_then(|v| v["accountUuid"].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "claude_identity_required".into())
}

/// Return only the active installation's data root. Multiple unmatched profiles
/// require an explicit path instead of choosing by modification time.
pub(crate) fn data_dir() -> Result<PathBuf, String> {
    let configured = claude_local::desktop_data_path();
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        if !path.is_absolute() || !path.is_dir() {
            return Err("claude_desktop_path_invalid".into());
        }
        return Ok(path);
    }
    if cfg!(windows) {
        if let Some(local) = paths::data_local_dir() {
            let families = claude_local::store_packages();
            let mut candidates = Vec::new();
            for package in families {
                let Some(family) = package["PackageFamilyName"].as_str() else {
                    continue;
                };
                if let Some(exe) = claude_local::resolve_exe_path().ok() {
                    if let Some(install) = package["InstallLocation"].as_str() {
                        if !crate::local_client::path_starts_with_ci(&exe, Path::new(install)) {
                            continue;
                        }
                    }
                }
                let modern = local.join("Claude-Data").join(family.to_ascii_lowercase());
                let legacy = local
                    .join("Packages")
                    .join(family)
                    .join("LocalCache/Roaming/Claude");
                candidates.push(if modern.exists() { modern } else { legacy });
            }
            candidates.sort();
            candidates.dedup();
            if candidates.len() > 1 {
                return Err("claude_desktop_path_ambiguous".into());
            }
            if let Some(path) = candidates.pop() {
                return Ok(path);
            }
        }
    }
    paths::config_dir()
        .map(|p| p.join("Claude"))
        .ok_or_else(|| "home_dir_unavailable".into())
}

fn collect(
    root: &Path,
    path: &Path,
    files: &mut BTreeMap<String, String>,
    total: &mut usize,
) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|_| "claude_desktop_read_failed")?;
    if meta.file_type().is_symlink() {
        return Err("claude_desktop_unsafe_path".into());
    }
    if meta.is_dir() {
        for entry in fs::read_dir(path).map_err(|_| "claude_desktop_read_failed")? {
            collect(
                root,
                &entry.map_err(|_| "claude_desktop_read_failed")?.path(),
                files,
                total,
            )?;
        }
    } else if meta.is_file() {
        *total = total
            .checked_add(meta.len() as usize)
            .ok_or("claude_desktop_snapshot_too_large")?;
        if *total > MAX_BYTES || files.len() >= 20_000 {
            return Err("claude_desktop_snapshot_too_large".into());
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "claude_desktop_unsafe_path")?
            .to_string_lossy()
            .replace('\\', "/");
        files.insert(
            relative,
            STANDARD.encode(fs::read(path).map_err(|_| "claude_desktop_read_failed")?),
        );
    } else {
        return Err("claude_desktop_unsafe_path".into());
    }
    Ok(())
}
fn config(root: &Path) -> Result<Value, String> {
    match fs::read(root.join("config.json")) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .ok()
            .filter(Value::is_object)
            .ok_or_else(|| "claude_desktop_config_invalid".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(_) => Err("claude_desktop_read_failed".into()),
    }
}
fn capture(root: &Path) -> Result<Snapshot, String> {
    let cfg = config(root)?;
    let mut files = BTreeMap::new();
    let mut total = 0;
    for item in LOGIN_PATHS {
        let path = root.join(item);
        if path.exists() {
            collect(root, &path, &mut files, &mut total)?;
        }
    }
    let auth = cfg
        .as_object()
        .ok_or("claude_desktop_config_invalid")?
        .iter()
        .filter(|(k, _)| auth_key(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Ok(Snapshot {
        version: 1,
        owner: owner(root)?,
        identity: cfg["lastKnownAccountUuid"].as_str().map(str::to_string),
        auth,
        files,
    })
}
fn valid_relative(name: &str) -> bool {
    let path = Path::new(name);
    !name.contains('\\')
        && !path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
        && name
            .split('/')
            .next()
            .is_some_and(|first| LOGIN_PATHS.contains(&first))
}
fn validate(snapshot: &Snapshot, target: &Path) -> Result<(), String> {
    if snapshot.version != 1 || snapshot.owner != owner(target)? {
        return Err("claude_desktop_snapshot_foreign".into());
    }
    let mut total = 0usize;
    for (name, value) in &snapshot.files {
        if !valid_relative(name) {
            return Err("claude_desktop_unsafe_path".into());
        }
        total = total
            .checked_add(
                STANDARD
                    .decode(value)
                    .map_err(|_| "claude_desktop_snapshot_invalid")?
                    .len(),
            )
            .ok_or("claude_desktop_snapshot_too_large")?;
        if total > MAX_BYTES {
            return Err("claude_desktop_snapshot_too_large".into());
        }
    }
    if snapshot.auth.keys().any(|k| !auth_key(k)) {
        return Err("claude_desktop_snapshot_invalid".into());
    }
    Ok(())
}
fn remove_owned(root: &Path, item: &str) -> Result<(), String> {
    let path = root.join(item);
    match fs::symlink_metadata(&path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err("claude_desktop_unsafe_path".into());
            }
            if meta.is_dir() {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            }
            .map_err(|_| "claude_desktop_write_failed".to_string())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("claude_desktop_write_failed".into()),
    }
}
fn apply(root: &Path, snapshot: &Snapshot) -> Result<(), String> {
    validate(snapshot, root)?;
    let mut cfg = config(root)?;
    let map = cfg.as_object_mut().ok_or("claude_desktop_config_invalid")?;
    map.retain(|k, _| !auth_key(k));
    map.extend(snapshot.auth.clone());
    // Validate every destination before removing anything. Profile symlinks/junctions
    // are rejected by capture; names in the persisted manifest are also checked.
    let _ = capture(root)?;
    for item in LOGIN_PATHS {
        remove_owned(root, item)?;
    }
    for (name, bytes) in &snapshot.files {
        atomic_write(
            &root.join(name),
            &STANDARD
                .decode(bytes)
                .map_err(|_| "claude_desktop_snapshot_invalid")?,
        )?;
    }
    atomic_write(
        &root.join("config.json"),
        &serde_json::to_vec_pretty(&cfg).map_err(|_| "claude_desktop_config_invalid")?,
    )?;
    // Electron can recover a stale login from this automatic config backup.
    remove_owned(root, "config.json.bak")?;
    Ok(())
}
fn has_session(root: &Path) -> bool {
    [root.join("Network/Cookies"), root.join("Cookies")].iter().any(|path| {
        let Ok(db) = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return false };
        let now = (chrono::Utc::now().timestamp() + 11_644_473_600) * 1_000_000;
        db.query_row("SELECT count(*) FROM cookies WHERE name IN ('sessionKey','sessionKeyV2','sessionKeyV3') AND (host_key='claude.ai' OR host_key='.claude.ai' OR host_key='claude.com' OR host_key='.claude.com') AND (length(value)>0 OR length(encrypted_value)>0) AND (expires_utc=0 OR expires_utc> ?1)", [now], |r|r.get::<_,i64>(0)).unwrap_or(0) > 0
    })
}
fn verify_login(root: &Path, uuid: &str) -> Result<(), String> {
    let cfg = config(root)?;
    if cfg["lastKnownAccountUuid"].as_str() != Some(uuid) {
        return Err("claude_desktop_account_mismatch".into());
    }
    let cache = ["oauth:tokenCacheV2", "oauth:tokenCache"].iter().any(|k| {
        cfg.get(k).is_some_and(|v| {
            v.as_str().is_some_and(|s| !s.is_empty())
                || v.as_object().is_some_and(|o| !o.is_empty())
        })
    });
    if !cache || !has_session(root) {
        return Err("claude_desktop_login_required".into());
    }
    Ok(())
}
fn checkpoint(root: &Path, snapshot: &Snapshot) -> Result<(), String> {
    let Some(uuid) = snapshot.identity.as_ref() else {
        return Ok(());
    };
    // A signed-out/stale profile must never replace a usable saved login.
    if verify_login(root, uuid).is_err() {
        return Ok(());
    }
    let dir = store_dir()?.join("bindings");
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).map_err(|_| "claude_desktop_read_failed")? {
        let path = entry.map_err(|_| "claude_desktop_read_failed")?.path();
        if let Ok(saved) = read_sealed::<Snapshot>(&path) {
            if saved.identity.as_ref() == Some(uuid) && saved.owner == snapshot.owner {
                write_sealed(&path, snapshot)?;
            }
        }
    }
    Ok(())
}
pub fn ensure_idle() -> Result<(), String> {
    if journal_path()?.exists() {
        Err("claude_desktop_binding_pending".into())
    } else {
        Ok(())
    }
}
pub fn binding_status(id: Option<&str>) -> Result<Value, String> {
    if journal_path()?.exists() {
        let journal: Journal = read_sealed(&journal_path()?)?;
        return Ok(
            json!({"state":"pending", "operationId":journal.id, "accountId":journal.account,
            "recoveryRequired":pending().lock().map_err(|_| "state_lock_poisoned")?.is_none()}),
        );
    }
    let target = data_dir()?;
    let Some(id) = id else {
        return Ok(json!({"state":"unbound"}));
    };
    let uuid = account_uuid(id)?;
    if !binding_path(id)?.exists() {
        return Ok(json!({"state":"unbound"}));
    }
    let snapshot: Snapshot = read_sealed(&binding_path(id)?)?;
    let valid =
        validate(&snapshot, &target).is_ok() && snapshot.identity.as_deref() == Some(uuid.as_str());
    Ok(json!({"state":if valid {"bound"} else {"unbound"}, "accountUuid":uuid}))
}
pub fn require_binding(id: &str) -> Result<(), String> {
    if binding_status(Some(id))?["state"] == "bound" {
        Ok(())
    } else {
        Err("claude_desktop_binding_required".into())
    }
}
fn load_journal(id: &str) -> Result<Journal, String> {
    let journal: Journal = read_sealed(&journal_path()?)?;
    if journal.id != id {
        return Err("claude_desktop_operation_stale".into());
    }
    validate(&journal.before, &journal.target)?;
    Ok(journal)
}
fn finish_journal() -> Result<(), String> {
    fs::remove_file(journal_path()?).map_err(|_| "claude_desktop_recovery_failed")?;
    *pending().lock().map_err(|_| "state_lock_poisoned")? = None;
    Ok(())
}
fn rollback(journal: &Journal) -> Result<(), String> {
    if !claude_local::close_desktop() {
        return Err("client_close_failed".into());
    }
    restore_profile(journal)?;
    if journal.was_running && !claude_local::launch_desktop() {
        return Err("client_launch_failed".into());
    }
    finish_journal()
}
fn restore_profile(journal: &Journal) -> Result<(), String> {
    apply(&journal.target, &journal.before).map_err(|_| "claude_desktop_recovery_failed".into())
}
fn new_journal(
    id: &str,
    root: PathBuf,
    before: Snapshot,
    binding: bool,
    was_running: bool,
) -> Result<Journal, String> {
    let mut nonce = [0u8; 16];
    getrandom::getrandom(&mut nonce).map_err(|_| "random_failed")?;
    let journal = Journal {
        id: nonce.iter().map(|b| format!("{b:02x}")).collect(),
        account: id.into(),
        target: root,
        before,
        deadline: chrono::Utc::now().timestamp_millis() + TIMEOUT_MS,
        binding,
        was_running,
    };
    write_sealed(&journal_path()?, &journal)?;
    Ok(journal)
}

struct ReopenOnFailure(bool);
impl Drop for ReopenOnFailure {
    fn drop(&mut self) {
        if self.0
            && journal_path().is_ok_and(|p| !p.exists())
            && !claude_local::is_desktop_running()
        {
            let _ = claude_local::launch_desktop();
        }
    }
}

#[tauri::command]
pub async fn claude_desktop_binding_start(
    id: String,
    close_desktop: bool,
) -> Result<Value, String> {
    let operation = crate::operations::begin()?;
    let _lease = crate::linked_clients::lease("claude")?;
    let _queue = accounts::refresh_queue("claude")?.lock().await;
    ensure_idle()?;
    let uuid = account_uuid(&id)?;
    let result = tauri::async_runtime::spawn_blocking(move || {
        claude_local::resolve_exe_path()?;
        let root = data_dir()?;
        let running = claude_local::is_desktop_running();
        if running && !close_desktop { return Err("client_close_required".into()); }
        if !claude_local::close_desktop() { return Err("client_close_failed".into()); }
        let _reopen=ReopenOnFailure(running);
        let before = capture(&root)?;
        if verify_login(&root, &uuid).is_ok() {
            write_sealed(&binding_path(&id)?, &before)?;
            if running && !claude_local::launch_desktop() { return Err("client_launch_failed".into()); }
            return Ok(json!({"state":"complete"}));
        }
        checkpoint(&root, &before)?;
        let journal = new_journal(&id, root.clone(), before.clone(), true, running)?;
        let empty = Snapshot {version:1,owner:before.owner.clone(),identity:None,auth:BTreeMap::new(),files:BTreeMap::new()};
        let prepared = apply(&root,&empty).and_then(|_| if claude_local::launch_desktop() {Ok(())} else {Err("client_launch_failed".into())});
        if let Err(error) = prepared { rollback(&journal)?; return Err(error); }
        Ok(json!({"state":"pending","id":journal.id,"instructions":"请在 Claude Desktop 登录此账户，完成后点击「完成绑定」。"}))
    }).await.map_err(|_| "claude_desktop_binding_failed")??;
    if let Some(id) = result["id"].as_str() {
        *pending().lock().map_err(|_| "state_lock_poisoned")? = Some((id.into(), operation));
        let id = id.to_string();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(TIMEOUT_MS as u64)).await;
            // A switch/status operation may briefly own the lease at the deadline.
            // Keep the durable recovery pending and retry instead of dropping timeout cleanup.
            loop {
                if claude_desktop_binding_cancel(id.clone()).await.is_ok() {
                    break;
                }
                if load_journal(&id).is_err() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
    }
    crate::audit::log(
        "claude_desktop_binding_start",
        "开始绑定 Claude Desktop 登录".into(),
        None,
    );
    Ok(result)
}

#[tauri::command]
pub async fn claude_desktop_binding_finish(id: String) -> Result<Value, String> {
    let _operation = crate::operations::begin()?;
    let _lease = crate::linked_clients::lease("claude")?;
    let _queue = accounts::refresh_queue("claude")?.lock().await;
    tauri::async_runtime::spawn_blocking(move || {
        let journal = load_journal(&id)?;
        if !journal.binding {
            return Err("claude_desktop_operation_stale".into());
        }
        if !claude_local::close_desktop() {
            return Err("client_close_failed".into());
        }
        let save = (|| {
            if chrono::Utc::now().timestamp_millis() >= journal.deadline {
                return Err("claude_desktop_binding_expired".into());
            }
            let uuid = account_uuid(&journal.account)?;
            verify_login(&journal.target, &uuid)?;
            write_sealed(&binding_path(&journal.account)?, &capture(&journal.target)?)
        })();
        rollback(&journal)?;
        save?;
        crate::audit::log(
            "claude_desktop_binding_saved",
            "已绑定 Claude Desktop 登录".into(),
            Some(json!({"id":journal.account})),
        );
        Ok(json!({"state":"complete"}))
    })
    .await
    .map_err(|_| "claude_desktop_binding_failed")?
}

#[tauri::command]
pub async fn claude_desktop_binding_cancel(id: String) -> Result<Value, String> {
    let _operation = crate::operations::begin()?;
    let _lease = crate::linked_clients::lease("claude")?;
    let _queue = accounts::refresh_queue("claude")?.lock().await;
    tauri::async_runtime::spawn_blocking(move || {
        if !journal_path()?.exists() {
            let mut active = pending().lock().map_err(|_| "state_lock_poisoned")?;
            if active
                .as_ref()
                .is_some_and(|(operation, _)| operation == &id)
            {
                *active = None;
            }
            return Ok(json!({"state":"cancelled"}));
        }
        rollback(&load_journal(&id)?)?;
        crate::audit::log(
            "claude_desktop_binding_cancelled",
            "已恢复绑定前的 Claude Desktop 登录".into(),
            None,
        );
        Ok(json!({"state":"cancelled"}))
    })
    .await
    .map_err(|_| "claude_desktop_recovery_failed")?
}

pub fn switch_bound(id: &str, was_running: bool) -> Result<(), String> {
    ensure_idle()?;
    require_binding(id)?;
    if claude_local::is_desktop_running() {
        return Err("claude_running".into());
    }
    let _reopen = ReopenOnFailure(was_running);
    let root = data_dir()?;
    let snapshot: Snapshot = read_sealed(&binding_path(id)?)?;
    validate(&snapshot, &root)?;
    let before = capture(&root)?;
    checkpoint(&root, &before)?;
    let journal = new_journal(id, root.clone(), before, false, was_running)?;
    let switched = (|| {
        apply(&root, &snapshot)?;
        verify_login(&root, &account_uuid(id)?)?;
        if !claude_local::launch_desktop() {
            return Err("client_launch_failed".into());
        }
        Ok(())
    })();
    if let Err(error) = switched {
        rollback(&journal)?;
        return Err(error);
    }
    finish_journal()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_does_not_restore_ui_settings_or_chats() {
        let root = tempfile::tempdir().unwrap();
        atomic_write(&root.path().join("config.json"), br#"{"theme":"dark","lastKnownAccountUuid":"one","oauth:tokenCacheV2":"encrypted-one"}"#).unwrap();
        atomic_write(&root.path().join("claude-code-sessions/keep"), b"chat").unwrap();
        let first = capture(root.path()).unwrap();
        atomic_write(&root.path().join("config.json"), br#"{"theme":"light","lastKnownAccountUuid":"two","oauth:tokenCacheV2":"encrypted-two"}"#).unwrap();
        apply(root.path(), &first).unwrap();
        let cfg = config(root.path()).unwrap();
        assert_eq!(cfg["theme"], "light");
        assert_eq!(cfg["lastKnownAccountUuid"], "one");
        assert_eq!(
            fs::read(root.path().join("claude-code-sessions/keep")).unwrap(),
            b"chat"
        );
    }
    #[test]
    fn foreign_or_traversing_snapshot_is_rejected_before_changes() {
        let root = tempfile::tempdir().unwrap();
        let mut snapshot = capture(root.path()).unwrap();
        snapshot
            .files
            .insert("Network/../../outside".into(), STANDARD.encode(b"bad"));
        assert!(apply(root.path(), &snapshot).is_err());
        snapshot.files.clear();
        snapshot.owner = "different-machine".into();
        assert!(apply(root.path(), &snapshot).is_err());
    }
    #[test]
    fn identity_without_native_session_is_not_a_login() {
        let root = tempfile::tempdir().unwrap();
        atomic_write(
            &root.path().join("config.json"),
            br#"{"lastKnownAccountUuid":"one","oauth:tokenCacheV2":"opaque"}"#,
        )
        .unwrap();
        assert_eq!(
            verify_login(root.path(), "two").unwrap_err(),
            "claude_desktop_account_mismatch"
        );
        assert_eq!(
            verify_login(root.path(), "one").unwrap_err(),
            "claude_desktop_login_required"
        );
    }
    #[test]
    fn persisted_transaction_restores_after_partial_write_and_process_restart() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("profile");
        atomic_write(
            &root.join("config.json"),
            br#"{"theme":"dark","lastKnownAccountUuid":"original"}"#,
        )
        .unwrap();
        atomic_write(&root.join("Network/Cookies"), b"original-encrypted-cookies").unwrap();
        let original = capture(&root).unwrap();
        let journal = Journal {
            id: "operation".into(),
            account: "fixture".into(),
            target: root.clone(),
            before: original.clone(),
            deadline: 0,
            binding: true,
            was_running: false,
        };
        let path = temp.path().join("recovery.json");
        write_sealed(&path, &journal).unwrap();
        let mut broken = original.clone();
        broken.files.clear();
        broken.files.insert(
            "Network/a".into(),
            STANDARD.encode(b"file-blocks-directory"),
        );
        broken
            .files
            .insert("Network/a/child".into(), STANDARD.encode(b"write-fails"));
        assert!(apply(&root, &broken).is_err());
        drop(journal);
        let recovered: Journal = read_sealed(&path).unwrap();
        restore_profile(&recovered).unwrap();
        assert_eq!(capture(&root).unwrap().files, original.files);
        assert_eq!(config(&root).unwrap()["lastKnownAccountUuid"], "original");
        assert!(
            path.exists(),
            "recovery remains durable until the full operation commits"
        );
    }
}

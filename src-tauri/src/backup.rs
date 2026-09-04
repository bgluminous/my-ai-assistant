//! 全量数据备份：settings.json 全部内容（设置 + 账号）+ 前端界面偏好，单 JSON 文件。
//!
//! 导出：可选密码加密——PBKDF2-SHA256（随机盐）派生 256 位密钥，AES-256-GCM
//! 加密 data 段整体；明文导出时 data 段直接内联。
//! 导入：合并语义——账号按身份去重后合并（同身份跳过），其余设置整体以备份为准；
//! 写盘后广播 accounts-changed，各窗口热更新，无需重启。
//! 流程拆两步：inspect 弹文件框并解析文件头（是否加密），apply 才真正解密合并，
//! 前端据此在两步之间向用户要密码。

use base64::Engine;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::AppHandle;

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce};
use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;

use crate::accounts::{self, Account};
use crate::audit;
use crate::settings::{self, Settings};

const FORMAT: &str = "my-ai-assistant-backup";
const VERSION: u32 = 1;
/// PBKDF2-HMAC-SHA256 迭代次数（本地文件加密，量级对齐 OWASP 建议）。
const KDF_ITERATIONS: u32 = 600_000;
const KDF_ALGO: &str = "pbkdf2-sha256";
const CIPHER_ALGO: &str = "aes-256-gcm";

// ---------------------------------------------------------------------------
// 文件结构
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KdfParams {
    algo: String,
    iterations: u32,
    /// base64 随机盐（16 字节）。
    salt: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CipherParams {
    algo: String,
    /// base64 随机 nonce（12 字节）。
    nonce: String,
    /// base64 密文（data 段 JSON 的 AES-256-GCM 加密结果，含认证标签）。
    data: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackupFile {
    format: String,
    version: u32,
    /// unix 秒。
    exported_at: i64,
    encrypted: bool,
    /// 明文导出时的数据段：{ settings, uiPrefs }。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kdf: Option<KdfParams>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cipher: Option<CipherParams>,
}

// ---------------------------------------------------------------------------
// 加解密
// ---------------------------------------------------------------------------

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn random_bytes<const N: usize>() -> Result<[u8; N], String> {
    let mut buf = [0u8; N];
    getrandom::getrandom(&mut buf).map_err(|_| "random_failed".to_string())?;
    Ok(buf)
}

fn derive_key(password: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, iterations, &mut key);
    key
}

/// 组装备份文件体；带密码时加密 data 段（密钥派生较慢，调用方放阻塞线程执行）。
fn build_backup_file(data: Value, password: Option<String>) -> Result<BackupFile, String> {
    let exported_at = Utc::now().timestamp();
    let Some(password) = password else {
        return Ok(BackupFile {
            format: FORMAT.into(),
            version: VERSION,
            exported_at,
            encrypted: false,
            data: Some(data),
            kdf: None,
            cipher: None,
        });
    };
    let salt: [u8; 16] = random_bytes()?;
    let nonce: [u8; 12] = random_bytes()?;
    let key = derive_key(&password, &salt, KDF_ITERATIONS);
    let plaintext = serde_json::to_vec(&data).map_err(|e| e.to_string())?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext.as_slice())
        .map_err(|_| "encrypt_failed".to_string())?;
    Ok(BackupFile {
        format: FORMAT.into(),
        version: VERSION,
        exported_at,
        encrypted: true,
        data: None,
        kdf: Some(KdfParams {
            algo: KDF_ALGO.into(),
            iterations: KDF_ITERATIONS,
            salt: b64().encode(salt),
        }),
        cipher: Some(CipherParams {
            algo: CIPHER_ALGO.into(),
            nonce: b64().encode(nonce),
            data: b64().encode(&ciphertext),
        }),
    })
}

/// 解密 data 段。密码错误（GCM 认证失败）报 wrong_password，参数缺失/损坏报 invalid_format。
fn decrypt_backup(kdf: &KdfParams, cipher: &CipherParams, password: &str) -> Result<Value, String> {
    if kdf.algo != KDF_ALGO || cipher.algo != CIPHER_ALGO || kdf.iterations == 0 {
        return Err("invalid_format".into());
    }
    let salt = b64().decode(&kdf.salt).map_err(|_| "invalid_format".to_string())?;
    let nonce = b64().decode(&cipher.nonce).map_err(|_| "invalid_format".to_string())?;
    let ciphertext = b64().decode(&cipher.data).map_err(|_| "invalid_format".to_string())?;
    if nonce.len() != 12 || salt.is_empty() {
        return Err("invalid_format".into());
    }
    let key = derive_key(password, &salt, kdf.iterations);
    let aead = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let plaintext = aead
        .decrypt(Nonce::from_slice(&nonce), ciphertext.as_slice())
        .map_err(|_| "wrong_password".to_string())?;
    serde_json::from_slice(&plaintext).map_err(|_| "invalid_format".to_string())
}

/// 解析并校验备份文件外层结构（容忍 BOM）。
fn parse_backup(text: &str) -> Result<BackupFile, String> {
    let parsed: BackupFile = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|_| "invalid_format".to_string())?;
    if parsed.format != FORMAT || parsed.version != VERSION {
        return Err("invalid_format".into());
    }
    if parsed.encrypted && (parsed.kdf.is_none() || parsed.cipher.is_none()) {
        return Err("invalid_format".into());
    }
    if !parsed.encrypted && parsed.data.is_none() {
        return Err("invalid_format".into());
    }
    Ok(parsed)
}

fn normalize_password(password: Option<String>) -> Option<String> {
    password
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
}

// ---------------------------------------------------------------------------
// 导出
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupExportResult {
    pub cancelled: bool,
    pub path: String,
    pub encrypted: bool,
}

/// 导出全部数据（设置 + 账号 + 界面偏好）为单个 JSON 文件；密码非空则加密。
/// ui_prefs 由前端收集（主题、数字单位等存于 localStorage，后端读不到）。
#[tauri::command]
pub async fn backup_export(
    app: AppHandle,
    password: Option<String>,
    ui_prefs: Value,
) -> Result<BackupExportResult, String> {
    settings::ensure_loaded()?;
    let password = normalize_password(password);
    let encrypted = password.is_some();
    let settings_value =
        settings::read(|s| serde_json::to_value(s.clone()).map_err(|e| e.to_string()))??;
    let account_count = settings_value
        .get("accounts")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let data = json!({ "settings": settings_value, "uiPrefs": ui_prefs });

    let file_name = format!(
        "my-ai-assistant-backup-{}.json",
        chrono::Local::now().format("%Y%m%d")
    );
    let app_for_dialog = app.clone();
    let picked = tokio::task::spawn_blocking(move || {
        accounts::pick_json_path(&app_for_dialog, "导出全部数据", Some(&file_name), true)
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(path) = picked else {
        return Ok(BackupExportResult {
            cancelled: true,
            path: String::new(),
            encrypted,
        });
    };
    let path = accounts::ensure_json_ext(path);

    let file = tokio::task::spawn_blocking(move || build_backup_file(data, password))
        .await
        .map_err(|e| e.to_string())??;
    let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| e.to_string())?;

    let shown = path.to_string_lossy().to_string();
    audit::log(
        "backup_export",
        format!(
            "导出全量备份（{} 个账号，{}）：{shown}",
            account_count,
            if encrypted { "已加密" } else { "明文" }
        ),
        Some(json!({ "path": shown, "accounts": account_count, "encrypted": encrypted })),
    );
    Ok(BackupExportResult {
        cancelled: false,
        path: shown,
        encrypted,
    })
}

// ---------------------------------------------------------------------------
// 导入（inspect 选文件 → apply 解密合并）
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInspectResult {
    pub cancelled: bool,
    pub path: String,
    pub encrypted: bool,
}

/// 弹文件框选择备份并校验文件头，返回是否加密；不改动任何数据。
#[tauri::command]
pub async fn backup_import_inspect(app: AppHandle) -> Result<BackupInspectResult, String> {
    settings::ensure_loaded()?;
    let app_for_dialog = app.clone();
    let picked = tokio::task::spawn_blocking(move || {
        accounts::pick_json_path(&app_for_dialog, "导入全部数据", None, false)
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(path) = picked else {
        return Ok(BackupInspectResult {
            cancelled: true,
            path: String::new(),
            encrypted: false,
        });
    };
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let parsed = parse_backup(&text)?;
    Ok(BackupInspectResult {
        cancelled: false,
        path: path.to_string_lossy().to_string(),
        encrypted: parsed.encrypted,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupApplyResult {
    /// 新增账号数。
    pub imported: usize,
    /// 新增账号 id（前端据此后台刷新验证）。
    pub imported_ids: Vec<String>,
    /// 同身份已存在而跳过的账号数。
    pub skipped_exists: usize,
    /// 凭据无法解析而跳过的账号数。
    pub skipped_invalid: usize,
    /// 备份携带的界面偏好（主题、数字单位），由前端应用到 localStorage。
    pub ui_prefs: Value,
}

/// 解析（必要时解密）备份并合并到本机：账号去重合并，其余设置整体覆盖。
/// 成功后广播 accounts-changed，各窗口热更新。
#[tauri::command]
pub async fn backup_import_apply(
    app: AppHandle,
    path: String,
    password: Option<String>,
) -> Result<BackupApplyResult, String> {
    settings::ensure_loaded()?;
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let parsed = parse_backup(&text)?;
    let data: Value = if parsed.encrypted {
        let password =
            normalize_password(password).ok_or_else(|| "password_required".to_string())?;
        let (kdf, cipher) = match (parsed.kdf, parsed.cipher) {
            (Some(k), Some(c)) => (k, c),
            _ => return Err("invalid_format".into()),
        };
        // 密钥派生 + 解密较慢，放阻塞线程
        tokio::task::spawn_blocking(move || decrypt_backup(&kdf, &cipher, &password))
            .await
            .map_err(|e| e.to_string())??
    } else {
        parsed.data.ok_or_else(|| "invalid_format".to_string())?
    };

    let settings_value = data
        .get("settings")
        .cloned()
        .ok_or_else(|| "invalid_format".to_string())?;
    let incoming: Settings =
        serde_json::from_value(settings_value).map_err(|_| "invalid_format".to_string())?;
    let incoming = settings::sanitize_loaded(incoming);
    let ui_prefs = data.get("uiPrefs").cloned().unwrap_or(Value::Null);

    let mut imported_ids: Vec<String> = Vec::new();
    let mut skipped_exists = 0usize;
    let mut skipped_invalid = 0usize;
    settings::mutate(|s| {
        // 账号合并：同身份（离线解析，解析不出回退 token 全等）跳过；
        // 逐个推入 s.accounts，天然覆盖备份文件内部的重复项。
        for acc in incoming.accounts {
            let Ok(kind) = accounts::sanitize_kind(&acc.kind) else {
                skipped_invalid += 1;
                continue;
            };
            let Ok(token) = accounts::sanitize_token(&kind, &acc.token) else {
                skipped_invalid += 1;
                continue;
            };
            if accounts::is_duplicate_account(&s.accounts, &kind, &token, None) {
                skipped_exists += 1;
                continue;
            }
            // 重新生成 id，避免与本机现有账号冲突；状态摘要与刷新时间随备份保留
            let refresh_token = if kind == "cursor" { None } else { acc.refresh_token };
            let entry = Account {
                id: accounts::new_id(),
                kind,
                note: acc.note,
                note_auto: acc.note_auto,
                token,
                refresh_token,
                last_refresh_at: acc.last_refresh_at,
                status: acc.status,
            };
            imported_ids.push(entry.id.clone());
            s.accounts.push(entry);
        }
        // 其余设置整体以备份为准（已在 sanitize_loaded 归一化）
        s.interval_minutes = incoming.interval_minutes;
        s.proxy = incoming.proxy;
        s.pricing = incoming.pricing;
        s.pricing_remote = incoming.pricing_remote;
        s.cursor_client = incoming.cursor_client;
        s.codex_client = incoming.codex_client;
        s.claude_client = incoming.claude_client;
        s.autostart_silent = incoming.autostart_silent;
        Ok(())
    })?;
    accounts::broadcast_changed(&app);

    audit::log(
        "backup_import",
        format!(
            "导入全量备份：新增 {} 个账号（{} 个已存在、{} 个无效已跳过），其余设置已覆盖",
            imported_ids.len(),
            skipped_exists,
            skipped_invalid
        ),
        Some(json!({
            "path": path,
            "imported": imported_ids.len(),
            "skippedExists": skipped_exists,
            "skippedInvalid": skipped_invalid,
        })),
    );
    Ok(BackupApplyResult {
        imported: imported_ids.len(),
        imported_ids,
        skipped_exists,
        skipped_invalid,
        ui_prefs,
    })
}

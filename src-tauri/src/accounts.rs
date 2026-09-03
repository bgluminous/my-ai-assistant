use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

use crate::{audit, claude, claude_local, claude_oauth, codex, codex_local, cursor, cursor_local, http, settings};

/// 账户 JSON 导入/导出文件标识。
const EXPORT_FORMAT: &str = "my-ai-assistant-accounts";
const EXPORT_VERSION: u32 = 1;

/// OpenAI OAuth token 端点（Codex access_token 续期）。
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// Codex CLI 使用的公开 OAuth client_id。
const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// access_token 距过期不足该秒数（含已过期）时提前续期。
const REFRESH_AHEAD_SECS: i64 = 1800;

// ---------------------------------------------------------------------------
// 数据结构与持久化
// ---------------------------------------------------------------------------

/// 单个托管账户。kind 取值："cursor" | "codex" | "claude"。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub note: String,
    /// 备注是否为自动生成（未手填时用邮箱/用户名兜底）。为 true 时刷新会用最新身份回填。
    #[serde(default)]
    pub note_auto: bool,
    /// Cursor user token / Codex access_token（JWT）/ Claude access_token（不透明值）。
    pub token: String,
    /// Codex / Claude：用于自动续期。
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// 上次刷新时间（unix 秒）。
    #[serde(default)]
    pub last_refresh_at: Option<i64>,
    /// 缓存的状态摘要（cursor_inspect_token / codex_usage 去掉 raw 后的结果）。
    #[serde(default)]
    pub status: Option<Value>,
}

/// 账户相关字段（写入统一 settings.json）。
/// interval_minutes 为定时刷新间隔（分钟，0 = 关闭），账户状态与用量统计共用。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AccountsFile {
    #[serde(default)]
    pub accounts: Vec<Account>,
    #[serde(default)]
    pub interval_minutes: u32,
}

/// 加写锁修改账户字段，随后立即写盘（同步 IO，锁内完成，保证内存与磁盘顺序一致）。
/// 本函数是同步的，调用方天然不会在持锁期间 await。
/// 写入成功后在锁外广播 accounts-changed 事件（携带最新视图），
/// 主窗口与托盘面板据此实时同步彼此触发的数据变更；广播失败不影响写入结果。
fn mutate<F>(app: &AppHandle, f: F) -> Result<AccountsFile, String>
where
    F: FnOnce(&mut AccountsFile) -> Result<(), String>,
{
    let data = settings::mutate(|s| {
        let mut data = s.accounts_file();
        f(&mut data)?;
        s.apply_accounts(data.clone());
        Ok(data)
    })?;
    let _ = app.emit("accounts-changed", view(&data));
    Ok(data)
}

fn current() -> AccountsFile {
    settings::read(|s| s.accounts_file()).unwrap_or_default()
}

/// 绕过本模块 mutate 直接改动账户数据的入口（如全量备份导入）在写盘后调用，
/// 向所有窗口广播最新账户视图；广播失败不影响数据。
pub(crate) fn broadcast_changed(app: &AppHandle) {
    let _ = app.emit("accounts-changed", view(&current()));
}

// ---------------------------------------------------------------------------
// 视图与入参
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountsView {
    pub accounts: Vec<Account>,
    pub interval_minutes: u32,
    pub path: String,
}

fn view(data: &AccountsFile) -> AccountsView {
    AccountsView {
        accounts: data.accounts.clone(),
        interval_minutes: data.interval_minutes,
        path: settings::path_display(),
    }
}

/// 新增账户的入参。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewAccount {
    pub kind: String,
    #[serde(default)]
    pub note: String,
    pub token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
}

pub(crate) fn new_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let millis = Utc::now().timestamp_millis();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("acc-{millis}-{seq}")
}

pub(crate) fn sanitize_kind(kind: &str) -> Result<String, String> {
    match kind.trim() {
        "cursor" => Ok("cursor".to_string()),
        "codex" => Ok("codex".to_string()),
        "claude" => Ok("claude".to_string()),
        _ => Err("invalid_kind".to_string()),
    }
}

/// 归一化并校验 token：cursor 走 normalize_cursor_token，codex 仅 trim；空值报错。
pub(crate) fn sanitize_token(kind: &str, token: &str) -> Result<String, String> {
    let token = if kind == "cursor" {
        http::normalize_cursor_token(token)
    } else {
        token.trim().to_string()
    };
    if token.is_empty() {
        return Err("empty_token".into());
    }
    Ok(token)
}

fn sanitize_refresh_token(rt: Option<String>) -> Option<String> {
    rt.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// 从账户凭据本地解析默认备注（无网络）：Codex 用 JWT 里的邮箱，Cursor 用 JWT sub 的 user_id。
/// Claude 的 access_token 是不透明值，本地解析不出身份（刷新成功后用接口返回的邮箱回填）。
/// 解析不到时回退到 token 的 `user_xxx` 前缀，最终仍可能为空。
fn default_note_from_token(kind: &str, token: &str) -> String {
    if kind == "claude" {
        return String::new();
    }
    if kind == "codex" {
        if let Some(email) = http::decode_jwt_payload(token)
            .and_then(|c| {
                c.get("https://api.openai.com/profile")
                    .and_then(|p| p.get("email"))
                    .and_then(|x| x.as_str())
                    .map(str::to_string)
            })
            .filter(|s| !s.is_empty())
        {
            return email;
        }
        return String::new();
    }
    // cursor：优先 JWT sub（auth0|user_01xxx -> user_01xxx）
    let jwt = http::jwt_part(token);
    if let Some(uid) = http::decode_jwt_payload(jwt)
        .and_then(|c| c.get("sub").and_then(|x| x.as_str()).map(str::to_string))
        .map(|sub| sub.rsplit('|').next().unwrap_or(&sub).to_string())
        .filter(|s| !s.is_empty())
    {
        return uid;
    }
    // 回退：token 的 `user_xxx::` 前缀部分
    if let Some(idx) = token.find("::") {
        let prefix = token[..idx].trim();
        if !prefix.is_empty() {
            return prefix.to_string();
        }
    }
    String::new()
}

/// 从已缓存 status 里取最优身份（邮箱），无则回退本地解析。用于自动备注回填。
fn best_identity(acc: &Account) -> String {
    let from_status = acc
        .status
        .as_ref()
        .and_then(|s| s.get("email"))
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    from_status.unwrap_or_else(|| default_note_from_token(&acc.kind, &acc.token))
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "codex" => "ChatGPT",
        "claude" => "Claude",
        _ => "Cursor",
    }
}

/// 审计日志里的账户显示名：优先备注，否则用打码后的 token 头部，绝不落全量 token。
fn display_name(kind: &str, note: &str, token: &str) -> String {
    let kind_label = kind_label(kind);
    let note = note.trim();
    if note.is_empty() {
        let head: String = token.trim().chars().take(8).collect();
        format!("{kind_label} {head}…")
    } else {
        format!("{kind_label}「{note}」")
    }
}

fn alive_text(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "有效",
        Some(false) => "失效",
        None => "未知",
    }
}

/// 离线解析账户身份标识（不联网），供查重比对：
/// - cursor：`user_xxx::<jwt>` 的前缀，否则 JWT sub 里 `provider|user_id` 的 user_id → "cursor:{user_id}"
/// - codex：JWT 的 chatgpt_account_id → "codex:{id}"，缺失时回退邮箱 → "codex:email:{email}"
/// - claude：access_token 为不透明值，无法本地解析 → None
/// 解析不出返回 None（调用方回退 token 全等判重）。
pub(crate) fn account_identity(kind: &str, token: &str) -> Option<String> {
    if kind == "claude" {
        return None;
    }
    if kind == "codex" {
        let claims = http::decode_jwt_payload(token)?;
        if let Some(id) = claims
            .get("https://api.openai.com/auth")
            .and_then(|a| a.get("chatgpt_account_id"))
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(format!("codex:{id}"));
        }
        if let Some(email) = claims
            .get("https://api.openai.com/profile")
            .and_then(|p| p.get("email"))
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
        {
            return Some(format!("codex:email:{email}"));
        }
        return None;
    }
    // cursor：优先 `user_xxx::` 前缀，其次 JWT sub（auth0|user_01xxx → user_01xxx）
    if let Some(idx) = token.find("::") {
        let prefix = token[..idx].trim();
        if !prefix.is_empty() {
            return Some(format!("cursor:{prefix}"));
        }
    }
    http::decode_jwt_payload(http::jwt_part(token))
        .and_then(|c| c.get("sub").and_then(|x| x.as_str()).map(str::to_string))
        .map(|sub| sub.rsplit('|').next().unwrap_or(&sub).to_string())
        .filter(|s| !s.is_empty())
        .map(|uid| format!("cursor:{uid}"))
}

/// 查重：与同 kind 现有账户逐个比对身份（不分大小写），新 token 身份解析不出时
/// 回退「同 kind 且 token 字符串全等」。编辑场景用 exclude_id 排除自身。
pub(crate) fn is_duplicate_account(
    accounts: &[Account],
    kind: &str,
    token: &str,
    exclude_id: Option<&str>,
) -> bool {
    let identity = account_identity(kind, token);
    accounts
        .iter()
        .filter(|a| a.kind == kind)
        .filter(|a| exclude_id.map_or(true, |id| a.id != id))
        .any(|a| match &identity {
            Some(idn) => account_identity(kind, &a.token)
                .map_or(false, |other| other.eq_ignore_ascii_case(idn)),
            None => a.token == token,
        })
}

// ---------------------------------------------------------------------------
// 增删改查命令
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn accounts_list(_app: AppHandle) -> Result<AccountsView, String> {
    // 启动时载入失败（如文件被短暂占用）会在这里自愈；仍失败则明确报错，
    // 前端展示错误横幅而不是被误导成「暂无账户」。
    settings::ensure_loaded()?;
    Ok(view(&current()))
}

#[tauri::command]
pub async fn accounts_add(app: AppHandle, account: NewAccount) -> Result<AccountsView, String> {
    let kind = sanitize_kind(&account.kind)?;
    let token = sanitize_token(&kind, &account.token)?;
    // 备注留空时用邮箱/用户名兜底，并标记为自动生成（刷新时可升级为真实邮箱）。
    let trimmed_note = account.note.trim().to_string();
    let note_auto = trimmed_note.is_empty();
    let note = if note_auto {
        // 本地兜底：Codex 从 JWT 拿邮箱、Cursor 拿 user_id。
        let mut n = default_note_from_token(&kind, &token);
        // Cursor 本地拿不到邮箱，添加时顺带调 auth/me 升级为真实邮箱（best-effort，与 Codex 体验对齐）。
        if kind == "cursor" {
            if let Some(email) = cursor::fetch_email(&token).await {
                n = email;
            }
        }
        n
    } else {
        trimmed_note
    };
    let entry = Account {
        id: new_id(),
        kind,
        note,
        note_auto,
        token,
        refresh_token: sanitize_refresh_token(account.refresh_token),
        last_refresh_at: None,
        status: None,
    };
    let name = display_name(&entry.kind, &entry.note, &entry.token);
    let entry_id = entry.id.clone();
    let data = mutate(&app, move |d| {
        // 同身份账户查重（离线解析，身份拿不到时回退 token 全等）
        if is_duplicate_account(&d.accounts, &entry.kind, &entry.token, None) {
            return Err("duplicate_account".into());
        }
        d.accounts.push(entry);
        Ok(())
    })?;
    audit::log(
        &app,
        "account_add",
        format!("添加账户：{name}"),
        Some(serde_json::json!({ "id": entry_id })),
    );
    Ok(view(&data))
}

#[tauri::command]
pub fn accounts_update(
    app: AppHandle,
    id: String,
    note: String,
    token: String,
    refresh_token: Option<String>,
) -> Result<AccountsView, String> {
    let mut name = String::new();
    let mut changed: Vec<&str> = Vec::new();
    let data = mutate(&app, |d| {
        let pos = d
            .accounts
            .iter()
            .position(|a| a.id == id)
            .ok_or_else(|| "not_found".to_string())?;
        let kind = d.accounts[pos].kind.clone();
        let token = sanitize_token(&kind, &token)?;
        let refresh_token = sanitize_refresh_token(refresh_token);
        // 改后的凭据与其它同类账户查重（排除自身）
        if is_duplicate_account(&d.accounts, &kind, &token, Some(id.as_str())) {
            return Err("duplicate_account".into());
        }
        let acc = &mut d.accounts[pos];
        // 备注留空 = 恢复自动（用邮箱/用户名兜底）；非空 = 用户手填，之后刷新不再覆盖
        let trimmed_note = note.trim().to_string();
        let new_note_auto = trimmed_note.is_empty();
        let new_note = if new_note_auto {
            default_note_from_token(&kind, &token)
        } else {
            trimmed_note
        };
        if new_note != acc.note {
            changed.push("备注");
        }
        if token != acc.token {
            changed.push("Token");
        }
        if refresh_token != acc.refresh_token {
            changed.push("Refresh Token");
        }
        // 凭据发生变化时旧的状态摘要随之失效
        if token != acc.token || refresh_token != acc.refresh_token {
            acc.status = None;
            acc.last_refresh_at = None;
        }
        acc.note = new_note;
        acc.note_auto = new_note_auto;
        acc.token = token;
        acc.refresh_token = refresh_token;
        name = display_name(&acc.kind, &acc.note, &acc.token);
        Ok(())
    })?;
    let what = if changed.is_empty() {
        "无字段变化".to_string()
    } else {
        format!("更新了{}", changed.join("、"))
    };
    audit::log(
        &app,
        "account_update",
        format!("编辑账户：{name}（{what}）"),
        Some(serde_json::json!({ "id": id })),
    );
    Ok(view(&data))
}

#[tauri::command]
pub fn accounts_delete(app: AppHandle, id: String) -> Result<AccountsView, String> {
    let mut removed: Option<Account> = None;
    let data = mutate(&app, |d| {
        if let Some(pos) = d.accounts.iter().position(|a| a.id == id) {
            removed = Some(d.accounts.remove(pos));
        }
        Ok(())
    })?;
    if let Some(acc) = &removed {
        // 全量用量存档随账户删除清理（与前端删账户时清 localStorage 缓存一致）
        crate::usage_archive::remove(&acc.id);
        audit::log(
            &app,
            "account_delete",
            format!("删除账户：{}", display_name(&acc.kind, &acc.note, &acc.token)),
            Some(serde_json::json!({ "id": acc.id })),
        );
    }
    Ok(view(&data))
}

/// 设置定时刷新间隔（分钟，0 = 关闭），账户状态刷新与用量统计自动更新共用。
#[tauri::command]
pub fn accounts_set_interval(app: AppHandle, interval_minutes: u32) -> Result<AccountsView, String> {
    let data = mutate(&app, |d| {
        d.interval_minutes = interval_minutes;
        Ok(())
    })?;
    let message = if interval_minutes > 0 {
        format!("定时刷新设为每 {interval_minutes} 分钟（账户状态 + 用量统计）")
    } else {
        "关闭定时刷新".to_string()
    };
    audit::log(&app, "interval_set", message, None);
    Ok(view(&data))
}

// ---------------------------------------------------------------------------
// 状态刷新（含 Codex token 自动续期）
// ---------------------------------------------------------------------------

/// 锁内取账户快照后立即放锁，避免持锁跨 await。
fn snapshot(id: &str) -> Result<Account, String> {
    settings::read(|s| {
        s.accounts
            .iter()
            .find(|a| a.id == id)
            .cloned()
    })?
    .ok_or_else(|| "not_found".to_string())
}

/// 跨模块读取账户快照的公开包装（复用内部 snapshot）。
pub fn account_snapshot(id: &str) -> Result<Account, String> {
    snapshot(id)
}

/// 续期成功后立即把新凭据写回内存并落盘。
/// OpenAI 的 refresh token 会轮换、旧值随即作废，若不马上持久化账户将永久失效。
pub(crate) fn persist_tokens(
    app: &AppHandle,
    id: &str,
    token: &str,
    refresh_token: &Option<String>,
) -> Result<(), String> {
    mutate(app, |d| {
        if let Some(acc) = d.accounts.iter_mut().find(|a| a.id == id) {
            acc.token = token.to_string();
            acc.refresh_token = refresh_token.clone();
        }
        Ok(())
    })?;
    Ok(())
}

/// 刷新结束：写入状态摘要与刷新时间并落盘，返回最新账户。
fn finish(app: &AppHandle, id: &str, status: Value) -> Result<Account, String> {
    let now = Utc::now().timestamp();
    let data = mutate(app, move |d| {
        let acc = d
            .accounts
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or_else(|| "not_found".to_string())?;
        acc.status = Some(status);
        acc.last_refresh_at = Some(now);
        Ok(())
    })?;
    data.accounts
        .into_iter()
        .find(|a| a.id == id)
        .ok_or_else(|| "not_found".to_string())
}

/// 刷新成功后，若备注为自动生成，用最新身份（优先 status.email）回填备注并落盘。
fn backfill_auto_note(app: &AppHandle, id: &str) -> Result<(), String> {
    let snap = match snapshot(id) {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };
    if !snap.note_auto {
        return Ok(());
    }
    let best = best_identity(&snap);
    if best.is_empty() || best == snap.note {
        return Ok(());
    }
    mutate(app, |d| {
        if let Some(acc) = d.accounts.iter_mut().find(|a| a.id == id) {
            acc.note = best.clone();
        }
        Ok(())
    })?;
    Ok(())
}

/// 把接口载荷转成 JSON 并去掉体积较大的 raw 字段，作为缓存的状态摘要。
fn to_status<T: Serialize>(payload: &T) -> Result<Value, String> {
    let mut v = serde_json::to_value(payload).map_err(|e| e.to_string())?;
    if let Some(obj) = v.as_object_mut() {
        obj.remove("raw");
    }
    Ok(v)
}

pub(crate) enum RefreshOutcome {
    /// 2xx 且拿到了新 access_token。
    Success {
        access_token: String,
        refresh_token: Option<String>,
        /// 带 openid scope 时返回；写本机 auth.json 必需（Codex 反序列化要求该字段）。
        id_token: Option<String>,
    },
    /// HTTP 4xx（如 invalid_grant）：refresh token 已失效，非致命。
    Denied { body: String },
}

/// 用 refresh_token 向 OpenAI OAuth 端点换取新 access_token。
/// 传输错误与其余异常返回 Err（调用方保留旧 status）；4xx 返回 Denied。
pub(crate) async fn request_codex_refresh(refresh_token: &str) -> Result<RefreshOutcome, String> {
    let body = json!({
        "client_id": OAUTH_CLIENT_ID,
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "scope": "openid profile email",
    });
    let resp = http::client()
        .post(OAUTH_TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if status.is_client_error() {
        return Ok(RefreshOutcome::Denied {
            body: http::truncate(&text, 200),
        });
    }
    if !status.is_success() {
        return Err(format!("refresh_http_{}", status.as_u16()));
    }
    let v: Value =
        serde_json::from_str(&text).map_err(|_| "refresh_invalid_response".to_string())?;
    let access_token = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "refresh_missing_access_token".to_string())?;
    let new_refresh = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    let id_token = v
        .get("id_token")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    Ok(RefreshOutcome::Success {
        access_token,
        refresh_token: new_refresh,
        id_token,
    })
}

/// Codex 账户刷新：必要时先续期 access_token，再查询用量；401/403 时补一次续期重试。
async fn refresh_codex_account(app: &AppHandle, id: &str, snap: Account) -> Result<Account, String> {
    let name = display_name(&snap.kind, &snap.note, &snap.token);
    let mut token = snap.token;
    let mut refresh_token = snap.refresh_token;
    let mut refreshed = false;

    // 1) access_token 临期（或已过期）且有 refresh_token 时先续期
    let exp = http::decode_jwt_payload(&token).and_then(|c| c.get("exp").and_then(|x| x.as_i64()));
    let near_expiry = exp
        .map(|e| e - Utc::now().timestamp() < REFRESH_AHEAD_SECS)
        .unwrap_or(false);
    if near_expiry {
        if let Some(rt) = refresh_token.clone() {
            match request_codex_refresh(&rt).await? {
                RefreshOutcome::Success {
                    access_token,
                    refresh_token: new_rt,
                    id_token,
                } => {
                    token = access_token;
                    if new_rt.is_some() {
                        refresh_token = new_rt;
                    }
                    persist_tokens(app, id, &token, &refresh_token)?;
                    refreshed = true;
                    audit::log(
                        app,
                        "codex_renewed",
                        format!("ChatGPT access_token 已自动续期：{name}"),
                        Some(json!({ "id": id })),
                    );
                    // 本机 auth.json 若是同一账号则顺带同步新凭据（失败在函数内部消化，不影响刷新）
                    codex_local::sync_auth_json(app, &token, &refresh_token, id_token.as_deref());
                }
                RefreshOutcome::Denied { body } => {
                    audit::log(
                        app,
                        "codex_renew_failed",
                        format!("ChatGPT 续期被拒绝：{name}（{body}）"),
                        Some(json!({ "id": id })),
                    );
                    return finish(app, id, json!({ "alive": false, "refreshError": body }));
                }
            }
        }
    }

    // 2) 查询用量；token 失效（401/403）且本次尚未续期过则补一次续期并重试一次
    let mut usage = codex::codex_usage(token.clone()).await?;
    if !usage.alive && (usage.status == 401 || usage.status == 403) && !refreshed {
        if let Some(rt) = refresh_token.clone() {
            match request_codex_refresh(&rt).await? {
                RefreshOutcome::Success {
                    access_token,
                    refresh_token: new_rt,
                    id_token,
                } => {
                    token = access_token;
                    if new_rt.is_some() {
                        refresh_token = new_rt;
                    }
                    persist_tokens(app, id, &token, &refresh_token)?;
                    audit::log(
                        app,
                        "codex_renewed",
                        format!("ChatGPT access_token 已自动续期：{name}"),
                        Some(json!({ "id": id })),
                    );
                    // 本机 auth.json 若是同一账号则顺带同步新凭据（失败在函数内部消化，不影响刷新）
                    codex_local::sync_auth_json(app, &token, &refresh_token, id_token.as_deref());
                    usage = codex::codex_usage(token.clone()).await?;
                }
                RefreshOutcome::Denied { body } => {
                    audit::log(
                        app,
                        "codex_renew_failed",
                        format!("ChatGPT 续期被拒绝：{name}（{body}）"),
                        Some(json!({ "id": id })),
                    );
                    return finish(app, id, json!({ "alive": false, "refreshError": body }));
                }
            }
        }
    }

    let status = to_status(&usage)?;
    finish(app, id, status)
}

/// Claude 续期成功的统一收尾：写回凭据、记审计。返回新的 token 过期时刻。
fn apply_claude_renewal(
    app: &AppHandle,
    id: &str,
    name: &str,
    token: &mut String,
    refresh_token: &mut Option<String>,
    access_token: String,
    new_rt: Option<String>,
    expires_at_ms: Option<i64>,
) -> Result<Option<i64>, String> {
    *token = access_token;
    if new_rt.is_some() {
        *refresh_token = new_rt;
    }
    persist_tokens(app, id, token, refresh_token)?;
    audit::log(
        app,
        "claude_renewed",
        format!("Claude access_token 已自动续期：{name}"),
        Some(json!({ "id": id })),
    );
    Ok(expires_at_ms)
}

/// Claude 账户刷新：access_token 非 JWT，过期时刻取上次续期时记录的
/// status.tokenExpiresAtMs；临期先续期，401/403 时补一次续期重试。
async fn refresh_claude_account(app: &AppHandle, id: &str, snap: Account) -> Result<Account, String> {
    let name = display_name(&snap.kind, &snap.note, &snap.token);
    let mut token = snap.token;
    let mut refresh_token = snap.refresh_token;
    let mut expires_at_ms = snap
        .status
        .as_ref()
        .and_then(|s| s.get("tokenExpiresAtMs"))
        .and_then(Value::as_i64);
    let mut refreshed = false;

    // 1) 已记录过期时刻且临期（或已过期）、有 refresh_token 时先续期
    let near_expiry = expires_at_ms
        .map(|ms| ms / 1000 - Utc::now().timestamp() < REFRESH_AHEAD_SECS)
        .unwrap_or(false);
    if near_expiry {
        if let Some(rt) = refresh_token.clone() {
            match claude_oauth::request_claude_refresh(&rt).await? {
                claude_oauth::ClaudeRefreshOutcome::Success {
                    access_token,
                    refresh_token: new_rt,
                    expires_at_ms: new_exp,
                    ..
                } => {
                    expires_at_ms = apply_claude_renewal(
                        app, id, &name, &mut token, &mut refresh_token, access_token, new_rt,
                        new_exp,
                    )?;
                    refreshed = true;
                }
                claude_oauth::ClaudeRefreshOutcome::Denied { body } => {
                    audit::log(
                        app,
                        "claude_renew_failed",
                        format!("Claude 续期被拒绝：{name}（{body}）"),
                        Some(json!({ "id": id })),
                    );
                    return finish(app, id, json!({ "alive": false, "refreshError": body }));
                }
            }
        }
    }

    // 2) 查询额度；token 失效（401/403）且本次尚未续期过则补一次续期并重试一次
    let mut usage = claude::claude_usage(token.clone()).await?;
    if !usage.alive && (usage.status == 401 || usage.status == 403) && !refreshed {
        if let Some(rt) = refresh_token.clone() {
            match claude_oauth::request_claude_refresh(&rt).await? {
                claude_oauth::ClaudeRefreshOutcome::Success {
                    access_token,
                    refresh_token: new_rt,
                    expires_at_ms: new_exp,
                    ..
                } => {
                    expires_at_ms = apply_claude_renewal(
                        app, id, &name, &mut token, &mut refresh_token, access_token, new_rt,
                        new_exp,
                    )?;
                    usage = claude::claude_usage(token.clone()).await?;
                }
                claude_oauth::ClaudeRefreshOutcome::Denied { body } => {
                    audit::log(
                        app,
                        "claude_renew_failed",
                        format!("Claude 续期被拒绝：{name}（{body}）"),
                        Some(json!({ "id": id })),
                    );
                    return finish(app, id, json!({ "alive": false, "refreshError": body }));
                }
            }
        }
    }

    let mut status = to_status(&usage)?;
    // token 过期时刻不来自接口，由续期流程维护，随状态一起缓存供下次判断
    if let (Some(obj), Some(ms)) = (status.as_object_mut(), expires_at_ms) {
        obj.insert("tokenExpiresAtMs".into(), json!(ms));
    }
    finish(app, id, status)
}

/// 全局凭据操作队列：所有账户刷新与本机 Codex 切换（见 codex_local::codex_switch_local，
/// 同样会轮换 refresh_token）在此排队，同一时刻只执行一个。
/// tokio Mutex 公平（FIFO），先到先执行；主窗口、托盘面板等所有入口共用一条队列，
/// 避免并发请求一次性打出大量请求触发服务端限流/风控，
/// 也防止两个操作并发消费同一 refresh_token（OpenAI 轮换后旧值随即作废）。
pub(crate) fn refresh_queue() -> &'static tokio::sync::Mutex<()> {
    static QUEUE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    QUEUE.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// 刷新单个账户的状态摘要；Codex 账户自动处理 access_token 续期。
/// 存活状态发生变化或刷新失败时写入审计日志。
/// 所有刷新全局排队执行（每次一个）；快照在拿到队列后再取，
/// 保证排队期间前序刷新轮换的凭据（Codex token）能被本次读到。
#[tauri::command]
pub async fn account_refresh(app: AppHandle, id: String) -> Result<Account, String> {
    let _queued = refresh_queue().lock().await;
    let snap = snapshot(&id)?;
    let kind = snap.kind.clone();
    let name = display_name(&snap.kind, &snap.note, &snap.token);
    let prev_alive = snap
        .status
        .as_ref()
        .and_then(|s| s.get("alive"))
        .and_then(Value::as_bool);

    let result: Result<Account, String> = match kind.as_str() {
        "cursor" => match cursor::cursor_inspect_token(snap.token).await {
            Ok(payload) => to_status(&payload).and_then(|status| finish(&app, &id, status)),
            Err(e) => Err(e),
        },
        "codex" => refresh_codex_account(&app, &id, snap).await,
        "claude" => refresh_claude_account(&app, &id, snap).await,
        _ => Err("invalid_kind".into()),
    };

    match &result {
        Ok(acc) => {
            let new_alive = acc
                .status
                .as_ref()
                .and_then(|s| s.get("alive"))
                .and_then(Value::as_bool);
            if new_alive != prev_alive {
                audit::log(
                    &app,
                    "account_state_changed",
                    format!(
                        "账户状态变化：{name} {} → {}",
                        alive_text(prev_alive),
                        alive_text(new_alive)
                    ),
                    Some(json!({ "id": acc.id, "from": prev_alive, "to": new_alive })),
                );
            }
        }
        Err(e) => {
            audit::log(
                &app,
                "account_refresh_failed",
                format!("刷新账户失败：{name}（{e}）"),
                Some(json!({ "id": id })),
            );
        }
    }
    // 刷新成功后，自动备注用最新身份回填，返回回填后的最新账户
    if result.is_ok() {
        backfill_auto_note(&app, &id)?;
        return snapshot(&id);
    }
    result
}

// ---------------------------------------------------------------------------
// 本机登录导入
// ---------------------------------------------------------------------------

/// 从本机客户端读到的一份登录凭据（cursor_local / codex_local 各自实现读取）。
pub struct LocalLogin {
    pub kind: String,
    pub token: String,
    pub refresh_token: Option<String>,
    /// 备注提示（邮箱等可读身份），拿不到为空串。
    pub note_hint: String,
}

/// 本机登录凭据的读取结果：区分「从未登录」与「有文件但解析不出」。
pub enum LocalLoginRead {
    /// 本机没有对应登录文件（从未登录），导入时静默跳过。
    Missing,
    /// 文件存在但关键字段缺失或无法解析，导入时记为 invalid。
    Invalid,
    Found(LocalLogin),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedItem {
    pub id: String,
    pub kind: String,
    pub label: String,
}

/// reason 取值："exists"（同身份已存在）| "invalid"（本机凭据无法解析）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedItem {
    pub kind: String,
    pub label: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportLocalResult {
    pub imported: Vec<ImportedItem>,
    pub skipped: Vec<SkippedItem>,
    pub view: AccountsView,
}

/// 导入结果里的可读标签：优先邮箱/身份，否则 token 前 8 字符打码（同 display_name 规则）。
fn import_label(hint: &str, token: &str) -> String {
    let hint = hint.trim();
    if hint.is_empty() {
        let head: String = token.trim().chars().take(8).collect();
        format!("{head}…")
    } else {
        hint.to_string()
    }
}

/// 读取本机已登录的指定类型凭据并导入为托管账户（Cursor / ChatGPT 互不影响）。
/// 本机从未登录既不算导入也不算跳过；同身份已存在记 exists，凭据无法解析记 invalid。
#[tauri::command]
pub fn accounts_import_local(app: AppHandle, kind: String) -> Result<ImportLocalResult, String> {
    settings::ensure_loaded()?;
    let kind = sanitize_kind(&kind)?;
    let mut imported: Vec<ImportedItem> = Vec::new();
    let mut skipped: Vec<SkippedItem> = Vec::new();
    let mut candidates: Vec<LocalLogin> = Vec::new();
    let read = match kind.as_str() {
        "codex" => codex_local::read_local_login(),
        "claude" => claude_local::read_local_login(),
        _ => cursor_local::read_local_login(),
    };
    match read {
        LocalLoginRead::Found(login) => candidates.push(login),
        LocalLoginRead::Invalid => skipped.push(SkippedItem {
            kind: kind.clone(),
            label: "本机登录".into(),
            reason: "invalid".into(),
        }),
        LocalLoginRead::Missing => {}
    }

    let mut audit_names: Vec<String> = Vec::new();
    let data = if candidates.is_empty() {
        current()
    } else {
        mutate(&app, |d| {
            for login in candidates {
                let Ok(token) = sanitize_token(&login.kind, &login.token) else {
                    skipped.push(SkippedItem {
                        kind: login.kind.clone(),
                        label: import_label(&login.note_hint, &login.token),
                        reason: "invalid".into(),
                    });
                    continue;
                };
                // 备注/标签：邮箱提示优先，否则本地解析身份兜底
                let note = {
                    let hint = login.note_hint.trim();
                    if hint.is_empty() {
                        default_note_from_token(&login.kind, &token)
                    } else {
                        hint.to_string()
                    }
                };
                let label = import_label(&note, &token);
                // 与现有账户查重；新增项随循环推入 d.accounts，天然覆盖本批次内部去重
                if is_duplicate_account(&d.accounts, &login.kind, &token, None) {
                    skipped.push(SkippedItem {
                        kind: login.kind.clone(),
                        label,
                        reason: "exists".into(),
                    });
                    continue;
                }
                let entry = Account {
                    id: new_id(),
                    kind: login.kind,
                    note,
                    note_auto: true,
                    token,
                    refresh_token: login.refresh_token,
                    last_refresh_at: None,
                    status: None,
                };
                audit_names.push(display_name(&entry.kind, &entry.note, &entry.token));
                imported.push(ImportedItem {
                    id: entry.id.clone(),
                    kind: entry.kind.clone(),
                    label,
                });
                d.accounts.push(entry);
            }
            Ok(())
        })?
    };

    if !audit_names.is_empty() {
        let ids: Vec<String> = imported.iter().map(|i| i.id.clone()).collect();
        audit::log(
            &app,
            "account_import",
            format!("从本机导入账户：{}", audit_names.join("、")),
            Some(json!({ "ids": ids })),
        );
    }
    Ok(ImportLocalResult {
        imported,
        skipped,
        view: view(&data),
    })
}

// ---------------------------------------------------------------------------
// JSON 文件导入 / 导出（按 kind 隔离）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountExportItem {
    #[serde(default)]
    note: String,
    #[serde(default)]
    note_auto: bool,
    token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountsExportFile {
    format: String,
    version: u32,
    kind: String,
    exported_at: i64,
    accounts: Vec<AccountExportItem>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub cancelled: bool,
    pub count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFileResult {
    pub cancelled: bool,
    pub imported: Vec<ImportedItem>,
    pub skipped: Vec<SkippedItem>,
    pub view: AccountsView,
}

pub(crate) fn pick_json_path(
    app: &AppHandle,
    title: &str,
    file_name: Option<&str>,
    save: bool,
) -> Option<PathBuf> {
    let mut builder = app
        .dialog()
        .file()
        .set_title(title)
        .add_filter("JSON", &["json"]);
    if let Some(name) = file_name {
        builder = builder.set_file_name(name);
    }
    if let Some(win) = app.get_webview_window("main") {
        builder = builder.set_parent(&win);
    }
    let picked = if save {
        builder.blocking_save_file()
    } else {
        builder.blocking_pick_file()
    };
    picked.and_then(|p| p.simplified().into_path().ok())
}

pub(crate) fn ensure_json_ext(mut path: PathBuf) -> PathBuf {
    let missing = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| !e.eq_ignore_ascii_case("json"))
        .unwrap_or(true);
    if missing {
        path.set_extension("json");
    }
    path
}

fn default_export_name(kind: &str) -> &'static str {
    match kind {
        "codex" => "chatgpt-accounts.json",
        "claude" => "claude-accounts.json",
        _ => "cursor-accounts.json",
    }
}

/// 导出指定类型账户为 JSON 文件。对话框取消返回 cancelled，不写盘。
#[tauri::command]
pub async fn accounts_export(app: AppHandle, kind: String) -> Result<ExportResult, String> {
    settings::ensure_loaded()?;
    let kind = sanitize_kind(&kind)?;
    let rows: Vec<AccountExportItem> = current()
        .accounts
        .iter()
        .filter(|a| a.kind == kind)
        .map(|a| AccountExportItem {
            note: a.note.clone(),
            note_auto: a.note_auto,
            token: a.token.clone(),
            // Cursor 没有 refresh_token 概念；Codex / Claude 随导出（用于自动续期）
            refresh_token: if kind == "cursor" {
                None
            } else {
                a.refresh_token.clone()
            },
        })
        .collect();
    if rows.is_empty() {
        return Err("no_accounts".into());
    }
    let count = rows.len();
    let label = kind_label(&kind);
    let title = format!("导出 {label} 账户");
    let file_name = default_export_name(&kind).to_string();
    let app_for_dialog = app.clone();
    let picked = tokio::task::spawn_blocking(move || {
        pick_json_path(&app_for_dialog, &title, Some(&file_name), true)
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(path) = picked else {
        return Ok(ExportResult {
            cancelled: true,
            count: 0,
        });
    };
    let path = ensure_json_ext(path);
    let payload = AccountsExportFile {
        format: EXPORT_FORMAT.into(),
        version: EXPORT_VERSION,
        kind: kind.clone(),
        exported_at: Utc::now().timestamp(),
        accounts: rows,
    };
    let text = serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    audit::log(
        &app,
        "account_export",
        format!("导出 {count} 个{label}账户"),
        Some(json!({ "kind": kind, "count": count })),
    );
    Ok(ExportResult {
        cancelled: false,
        count,
    })
}

/// 从 JSON 文件导入指定类型账户：合并到现有列表，同身份跳过，类型不符则报错。
#[tauri::command]
pub async fn accounts_import_file(app: AppHandle, kind: String) -> Result<ImportFileResult, String> {
    settings::ensure_loaded()?;
    let kind = sanitize_kind(&kind)?;
    let label = kind_label(&kind);
    let title = format!("导入 {label} 账户");
    let app_for_dialog = app.clone();
    let picked = tokio::task::spawn_blocking(move || {
        pick_json_path(&app_for_dialog, &title, None, false)
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(path) = picked else {
        return Ok(ImportFileResult {
            cancelled: true,
            imported: Vec::new(),
            skipped: Vec::new(),
            view: view(&current()),
        });
    };
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let parsed: AccountsExportFile =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|_| "invalid_format".to_string())?;
    if parsed.format != EXPORT_FORMAT || parsed.version != EXPORT_VERSION {
        return Err("invalid_format".into());
    }
    let file_kind = sanitize_kind(&parsed.kind).map_err(|_| "invalid_format".to_string())?;
    if file_kind != kind {
        return Err("kind_mismatch".into());
    }

    let mut imported: Vec<ImportedItem> = Vec::new();
    let mut skipped: Vec<SkippedItem> = Vec::new();
    let mut audit_names: Vec<String> = Vec::new();
    let data = if parsed.accounts.is_empty() {
        current()
    } else {
        mutate(&app, |d| {
            for item in parsed.accounts {
                let Ok(token) = sanitize_token(&kind, &item.token) else {
                    skipped.push(SkippedItem {
                        kind: kind.clone(),
                        label: import_label(&item.note, &item.token),
                        reason: "invalid".into(),
                    });
                    continue;
                };
                let trimmed_note = item.note.trim().to_string();
                let note_auto = item.note_auto || trimmed_note.is_empty();
                let note = if trimmed_note.is_empty() {
                    default_note_from_token(&kind, &token)
                } else {
                    trimmed_note
                };
                let label = import_label(&note, &token);
                if is_duplicate_account(&d.accounts, &kind, &token, None) {
                    skipped.push(SkippedItem {
                        kind: kind.clone(),
                        label,
                        reason: "exists".into(),
                    });
                    continue;
                }
                let refresh_token = if kind == "cursor" {
                    None
                } else {
                    sanitize_refresh_token(item.refresh_token)
                };
                let entry = Account {
                    id: new_id(),
                    kind: kind.clone(),
                    note,
                    note_auto,
                    token,
                    refresh_token,
                    last_refresh_at: None,
                    status: None,
                };
                audit_names.push(display_name(&entry.kind, &entry.note, &entry.token));
                imported.push(ImportedItem {
                    id: entry.id.clone(),
                    kind: entry.kind.clone(),
                    label,
                });
                d.accounts.push(entry);
            }
            Ok(())
        })?
    };

    if !audit_names.is_empty() {
        let ids: Vec<String> = imported.iter().map(|i| i.id.clone()).collect();
        audit::log(
            &app,
            "account_import_file",
            format!("从文件导入{label}账户：{}", audit_names.join("、")),
            Some(json!({ "kind": kind, "ids": ids })),
        );
    }
    Ok(ImportFileResult {
        cancelled: false,
        imported,
        skipped,
        view: view(&data),
    })
}

// ---------------------------------------------------------------------------
// Claude OAuth 授权添加（浏览器授权 → 粘贴授权码 → 直接落库）
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeOauthAddResult {
    pub id: String,
    pub label: String,
    pub view: AccountsView,
}

/// 用浏览器授权返回的授权码换取凭据组并添加为 Claude 账户。
/// 需先调用 claude_oauth_begin 打开授权页；备注用接口返回的邮箱兜底。
#[tauri::command]
pub async fn claude_oauth_finish(app: AppHandle, code: String) -> Result<ClaudeOauthAddResult, String> {
    settings::ensure_loaded()?;
    let result = claude_oauth::exchange_code(&code).await?;
    let note = result.email.clone().unwrap_or_default();
    let entry = Account {
        id: new_id(),
        kind: "claude".into(),
        note,
        note_auto: true,
        token: result.access_token,
        refresh_token: result.refresh_token,
        last_refresh_at: None,
        // 记录 token 过期时刻，刷新流程据此提前续期
        status: result
            .expires_at_ms
            .map(|ms| json!({ "tokenExpiresAtMs": ms })),
    };
    let name = display_name(&entry.kind, &entry.note, &entry.token);
    let label = import_label(&entry.note, &entry.token);
    let entry_id = entry.id.clone();
    let data = mutate(&app, move |d| {
        if is_duplicate_account(&d.accounts, &entry.kind, &entry.token, None) {
            return Err("duplicate_account".into());
        }
        d.accounts.push(entry);
        Ok(())
    })?;
    audit::log(
        &app,
        "account_add",
        format!("OAuth 授权添加账户：{name}"),
        Some(json!({ "id": entry_id })),
    );
    Ok(ClaudeOauthAddResult {
        id: entry_id,
        label,
        view: view(&data),
    })
}

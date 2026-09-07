use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

use crate::{
    audit, claude, claude_local, claude_oauth, codex, codex_local, cursor, cursor_local, http,
    local_crypto, settings,
};

/// 账户 JSON 导入/导出文件标识。
const EXPORT_FORMAT: &str = "my-ai-assistant-accounts";
const EXPORT_VERSION: u32 = 1;

/// OpenAI OAuth token 端点（Codex access_token 续期）。
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// Codex CLI 使用的公开 OAuth client_id。
const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// ChatGPT 续期策略与 Codex CLI 一致：access_token 距过期不足 5 分钟（含已过期）就续期；
/// 解析不出过期时刻时，距上次获取凭据超过 8 天就续期。每次续期都会轮换 refresh_token。
const CODEX_RENEW_AHEAD_SECS: i64 = 5 * 60;
const CODEX_RENEW_INTERVAL_DAYS: i64 = 8;
/// Claude access_token 距过期不足该秒数（含已过期）时提前续期。
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
    /// ChatGPT 账户有 codex_auth 副本时，本字段只在内存里由副本解出，不落盘。
    pub token: String,
    /// Codex / Claude：用于自动续期。ChatGPT 账户同 token，有副本时不落盘。
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// ChatGPT：本机 Codex 客户端 auth.json 的完整副本（id_token / access_token / refresh_token /
    /// account_id / last_refresh），经 local_crypto 加密。续期、切换、导入、添加都会写入；
    /// 也是「强制写入 auth.json」的数据源。发给前端的视图里去掉。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_auth: Option<String>,
    /// 上次刷新时间（unix 秒）。
    #[serde(default)]
    pub last_refresh_at: Option<i64>,
    /// 缓存的状态摘要（cursor_inspect_token / codex_usage 去掉 raw 后的结果）。
    #[serde(default)]
    pub status: Option<Value>,
}

// ---------------------------------------------------------------------------
// ChatGPT 凭据副本（加密的 auth.json）
// ---------------------------------------------------------------------------

fn json_str_at(v: &Value, pointer: &str) -> Option<String> {
    v.pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 解出 ChatGPT 账户保存的 auth.json 副本；无副本、解密或解析失败为 None。
pub(crate) fn codex_auth_json(acc: &Account) -> Option<Value> {
    let blob = acc.codex_auth.as_deref()?;
    serde_json::from_str(&local_crypto::open(blob)?).ok()
}

/// 副本里记录的上次获取凭据时刻（auth.json 的 last_refresh）。
pub(crate) fn codex_last_refresh(acc: &Account) -> Option<DateTime<Utc>> {
    let v = codex_auth_json(acc)?;
    let text = json_str_at(&v, "/last_refresh")?;
    DateTime::parse_from_rfc3339(&text)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// 用副本里的 access_token / refresh_token 覆盖内存字段（副本缺该字段时保留原值）。
fn hydrate_codex(acc: &mut Account) {
    if acc.kind != "codex" {
        return;
    }
    let Some(v) = codex_auth_json(acc) else {
        return;
    };
    if let Some(token) = json_str_at(&v, "/tokens/access_token") {
        acc.token = token;
    }
    if let Some(rt) = json_str_at(&v, "/tokens/refresh_token") {
        acc.refresh_token = Some(rt);
    }
}

/// 只有 access_token / refresh_token 时组一份骨架副本（无 id_token，首次续期或切换时补全）。
fn skeleton_codex_auth(token: &str, refresh_token: Option<&str>) -> Value {
    json!({
        "OPENAI_API_KEY": Value::Null,
        "tokens": {
            "access_token": token,
            "refresh_token": refresh_token,
            "account_id": codex_local::account_id_from_token(token),
        },
        "last_refresh": Value::Null,
    })
}

/// 把新凭据组写进副本：access_token 必填，refresh_token / id_token 有新值才替换，
/// account_id 由 access_token 解出，last_refresh 记当前时刻。
pub(crate) fn apply_codex_tokens(
    v: &mut Value,
    access_token: &str,
    refresh_token: Option<&str>,
    id_token: Option<&str>,
) {
    if !v.is_object() {
        *v = json!({});
    }
    let obj = v.as_object_mut().expect("object");
    obj.entry("OPENAI_API_KEY").or_insert(Value::Null);
    let tokens = obj.entry("tokens").or_insert_with(|| json!({}));
    if !tokens.is_object() {
        *tokens = json!({});
    }
    let t = tokens.as_object_mut().expect("object");
    t.insert("access_token".into(), json!(access_token));
    if let Some(rt) = refresh_token {
        t.insert("refresh_token".into(), json!(rt));
    }
    if let Some(idt) = id_token {
        t.insert("id_token".into(), json!(idt));
    }
    if let Some(account_id) = codex_local::account_id_from_token(access_token) {
        t.insert("account_id".into(), json!(account_id));
    }
    obj.insert("last_refresh".into(), json!(Utc::now().to_rfc3339()));
}

/// 加密写入 acc.codex_auth，并同步内存里的 access_token / refresh_token。
fn set_codex_auth(acc: &mut Account, v: &Value) -> Result<(), String> {
    let text = serde_json::to_string(v).map_err(|e| e.to_string())?;
    acc.codex_auth = Some(local_crypto::seal(&text)?);
    hydrate_codex(acc);
    Ok(())
}

/// 载入 / 导入后的归一：有副本的 ChatGPT 账户由副本解出 token；没有副本但有明文凭据的
/// （旧版本数据、文件导入）组骨架副本，明文随下次写盘不再保留。返回是否组了新副本。
pub(crate) fn hydrate_accounts(accounts: &mut [Account]) -> bool {
    let mut migrated = false;
    for acc in accounts.iter_mut().filter(|a| a.kind == "codex") {
        if acc.codex_auth.is_some() && codex_auth_json(acc).is_some() {
            hydrate_codex(acc);
        } else if !acc.token.trim().is_empty() {
            let v = skeleton_codex_auth(&acc.token, acc.refresh_token.as_deref());
            if set_codex_auth(acc, &v).is_ok() {
                migrated = true;
            }
        }
    }
    migrated
}

/// 落盘 / 备份导出形态：有副本的 ChatGPT 账户不再单独写出 token / refresh_token。
pub(crate) fn dehydrate_accounts(accounts: &mut [Account]) {
    for acc in accounts.iter_mut() {
        if acc.kind == "codex" && acc.codex_auth.is_some() {
            acc.token = String::new();
            acc.refresh_token = None;
        }
    }
}

/// 更新 ChatGPT 账户的副本：以现有副本为底（没有则由当前凭据组骨架），交给 f 修改后重新加密写回，
/// 并同步内存里的 access_token / refresh_token；写盘后广播 accounts-changed。
pub(crate) fn update_codex_auth(
    app: &AppHandle,
    id: &str,
    f: impl FnOnce(&mut Value),
) -> Result<(), String> {
    mutate(app, |d| {
        let acc = d
            .accounts
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or_else(|| "not_found".to_string())?;
        let mut v = codex_auth_json(acc)
            .unwrap_or_else(|| skeleton_codex_auth(&acc.token, acc.refresh_token.as_deref()));
        f(&mut v);
        set_codex_auth(acc, &v)
    })?;
    Ok(())
}

/// 用 refresh_token 换一组完整凭据并组成副本（添加 / 编辑 ChatGPT 账户时调用，拿到 id_token）。
/// 失败时返回可读原因（被拒的归类说明或网络错误），由调用方退回骨架副本并写进审计。
async fn exchange_codex_auth(refresh_token: &str) -> Result<Value, String> {
    match request_codex_refresh(refresh_token).await {
        Ok(RefreshOutcome::Success {
            access_token,
            refresh_token: new_rt,
            id_token,
        }) => {
            let mut v = json!({});
            apply_codex_tokens(
                &mut v,
                &access_token,
                Some(new_rt.as_deref().unwrap_or(refresh_token)),
                id_token.as_deref(),
            );
            Ok(v)
        }
        Ok(RefreshOutcome::Denied { reason, detail }) => {
            Err(format!("{}：{detail}", reason.label()))
        }
        Err(e) => Err(format!("网络或服务异常：{e}")),
    }
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

/// 发给前端的账户：与 Account 同一 JSON 形态，但去掉加密副本，附上副本记录的凭据换新时刻。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    #[serde(flatten)]
    pub account: Account,
    /// ChatGPT：凭据组（access / refresh token）最后一次换新的时刻（unix 毫秒），来自副本的 last_refresh；
    /// 无副本或副本没记时间为 None。
    pub codex_auth_refreshed_at: Option<i64>,
}

pub(crate) fn account_view(acc: &Account) -> AccountView {
    let codex_auth_refreshed_at = codex_last_refresh(acc).map(|t| t.timestamp_millis());
    let mut account = acc.clone();
    account.codex_auth = None;
    AccountView {
        account,
        codex_auth_refreshed_at,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountsView {
    pub accounts: Vec<AccountView>,
    pub interval_minutes: u32,
    pub path: String,
}

fn view(data: &AccountsFile) -> AccountsView {
    AccountsView {
        accounts: data.accounts.iter().map(account_view).collect(),
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

/// token 打码：只保留前 8 个字符加省略号，审计日志与导入结果标签共用。
fn masked_head(token: &str) -> String {
    let head: String = token.trim().chars().take(8).collect();
    format!("{head}…")
}

/// 审计日志里的账户显示名：优先备注，否则用打码后的 token 头部，绝不落全量 token。
pub(crate) fn display_name(kind: &str, note: &str, token: &str) -> String {
    let kind_label = kind_label(kind);
    let note = note.trim();
    if note.is_empty() {
        format!("{kind_label} {}", masked_head(token))
    } else {
        format!("{kind_label}「{note}」")
    }
}

/// 切号审计里的账户简称（不带类型前缀）：有备注用备注，否则用打码后的 token 头部。
pub(crate) fn short_display_name(acc: &Account) -> String {
    let note = acc.note.trim();
    if note.is_empty() {
        masked_head(&acc.token)
    } else {
        note.to_string()
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
///
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
        .filter(|a| exclude_id.is_none_or(|id| a.id != id))
        .any(|a| match &identity {
            Some(idn) => account_identity(kind, &a.token)
                .is_some_and(|other| other.eq_ignore_ascii_case(idn)),
            None => a.token == token,
        })
}

// ---------------------------------------------------------------------------
// 增删改查命令
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn accounts_list() -> Result<AccountsView, String> {
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
    let mut entry = Account {
        id: new_id(),
        kind,
        note,
        note_auto,
        token,
        refresh_token: sanitize_refresh_token(account.refresh_token),
        codex_auth: None,
        last_refresh_at: None,
        status: None,
    };
    // ChatGPT：手动粘贴的凭据没有 id_token，立刻用 refresh_token 换一组完整凭据存成副本
    //（会消耗并轮换 refresh_token）；换不到时退回骨架副本，首次续期或切换时再补全。
    let mut exchanged = false;
    let mut exchange_error = String::new();
    if entry.kind == "codex" {
        let full = match entry.refresh_token.as_deref() {
            Some(rt) => exchange_codex_auth(rt).await,
            None => Err("没有 Refresh Token".to_string()),
        };
        let v = match full {
            Ok(v) => {
                exchanged = true;
                v
            }
            Err(e) => {
                exchange_error = e;
                skeleton_codex_auth(&entry.token, entry.refresh_token.as_deref())
            }
        };
        set_codex_auth(&mut entry, &v)?;
    }
    let name = display_name(&entry.kind, &entry.note, &entry.token);
    let entry_id = entry.id.clone();
    let entry_kind = entry.kind.clone();
    let entry_token = entry.token.clone();
    let data = mutate(&app, move |d| {
        // 同身份账户查重（离线解析，身份拿不到时回退 token 全等）
        if is_duplicate_account(&d.accounts, &entry.kind, &entry.token, None) {
            return Err("duplicate_account".into());
        }
        d.accounts.push(entry);
        Ok(())
    })?;
    let suffix = match (entry_kind.as_str(), exchanged) {
        ("codex", true) => "（已换取完整凭据）".to_string(),
        ("codex", false) => format!("（未能换取完整凭据：{exchange_error}；首次续期或切换时补全）"),
        _ => String::new(),
    };
    audit::log(
        "account_add",
        format!("添加账户：{name}{suffix}"),
        Some(serde_json::json!({ "id": entry_id })),
    );
    adopt_deleted_usage(&app, &[(entry_id, entry_kind, entry_token, name)]).await;
    Ok(view(&data))
}

/// 新增 Cursor 账户后接管同身份已删除账户保留的用量事件库（accounts / backup 共用）。
/// 入参为 (id, kind, token, 审计显示名)；非 Cursor 账户跳过。发生接管时广播
/// usage-archive-changed：accounts-changed 在写盘时已经发出，用量页可能已按旧的
/// 已删除记录列表加载，需要它再读一次。返回是否发生了接管。
pub(crate) async fn adopt_deleted_usage(
    app: &AppHandle,
    added: &[(String, String, String, String)],
) -> bool {
    let mut adopted = false;
    for (id, kind, token, name) in added {
        if kind == "cursor" && crate::usage_archive::adopt_deleted(id, token, name).await {
            adopted = true;
        }
    }
    if adopted {
        notify_usage_archive_changed(app);
    }
    adopted
}

/// 已删除账户保留的统计数据集合有变化（接管 / 备份恢复）时通知各窗口。
pub(crate) fn notify_usage_archive_changed(app: &AppHandle) {
    let _ = app.emit("usage-archive-changed", ());
}

#[tauri::command]
pub async fn accounts_update(
    app: AppHandle,
    id: String,
    note: String,
    token: String,
    refresh_token: Option<String>,
) -> Result<AccountsView, String> {
    let before = snapshot(&id)?;
    let token = sanitize_token(&before.kind, &token)?;
    let refresh_token = sanitize_refresh_token(refresh_token);
    let credentials_changed = token != before.token || refresh_token != before.refresh_token;
    // ChatGPT 改了凭据：与添加时一样立刻换一组完整凭据存成副本（换不到退回骨架副本）
    let mut exchange_note = String::new();
    let new_codex_auth = if before.kind == "codex" && credentials_changed {
        let full = match refresh_token.as_deref() {
            Some(rt) => exchange_codex_auth(rt).await,
            None => Err("没有 Refresh Token".to_string()),
        };
        Some(match full {
            Ok(v) => {
                exchange_note = "，已换取完整凭据".to_string();
                v
            }
            Err(e) => {
                exchange_note = format!("，未能换取完整凭据：{e}");
                skeleton_codex_auth(&token, refresh_token.as_deref())
            }
        })
    } else {
        None
    };
    let mut name = String::new();
    let mut changed: Vec<&str> = Vec::new();
    let data = mutate(&app, |d| {
        let pos = d
            .accounts
            .iter()
            .position(|a| a.id == id)
            .ok_or_else(|| "not_found".to_string())?;
        let kind = d.accounts[pos].kind.clone();
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
        if credentials_changed {
            acc.status = None;
            acc.last_refresh_at = None;
        }
        acc.note = new_note;
        acc.note_auto = new_note_auto;
        acc.token = token.clone();
        acc.refresh_token = refresh_token.clone();
        if let Some(v) = &new_codex_auth {
            set_codex_auth(acc, v)?;
        }
        name = display_name(&acc.kind, &acc.note, &acc.token);
        Ok(())
    })?;
    let what = if changed.is_empty() {
        "无字段变化".to_string()
    } else {
        format!("更新了{}", changed.join("、"))
    };
    audit::log(
        "account_update",
        format!("编辑账户：{name}（{what}{exchange_note}）"),
        Some(serde_json::json!({ "id": id })),
    );
    Ok(view(&data))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteResult {
    pub view: AccountsView,
    /// 统计数据的处理结果："kept"（已保留）| "empty"（没有用量记录，无可保留）| "discarded"（未保留）。
    pub usage: String,
}

/// 删除账户。Cursor 账户可选保留统计数据（keep_usage）：先做一次最终同步，再把用量事件库
/// 标记为已删除留在本机；同步失败且本机没有该账户任何数据时报 no_usage_data:<原因> 且不删除，
/// 由前端询问用户是否仍然删除。不保留时事件库随账户一并清理。
#[tauri::command]
pub async fn accounts_delete(
    app: AppHandle,
    id: String,
    keep_usage: bool,
) -> Result<DeleteResult, String> {
    settings::ensure_loaded()?;
    let Ok(target) = snapshot(&id) else {
        return Ok(DeleteResult {
            view: view(&current()),
            usage: "discarded".into(),
        });
    };
    let mut usage = "discarded";
    if keep_usage && target.kind == "cursor" {
        usage = match crate::usage_archive::retain_for_deleted(&target).await? {
            crate::usage_archive::Retained::Kept => "kept",
            crate::usage_archive::Retained::Empty => "empty",
        };
    }
    let data = mutate(&app, |d| {
        d.accounts.retain(|a| a.id != id);
        Ok(())
    })?;
    if usage != "kept" {
        // 用量事件库随账户删除清理（与前端删账户时清 localStorage 缓存一致）
        crate::usage_archive::discard(&id).await;
    }
    let suffix = match usage {
        "kept" => "（保留统计数据）",
        "empty" => "（没有用量记录，无统计数据可保留）",
        _ => "",
    };
    audit::log(
        "account_delete",
        format!(
            "删除账户：{}{suffix}",
            display_name(&target.kind, &target.note, &target.token)
        ),
        Some(serde_json::json!({ "id": id, "usage": usage })),
    );
    Ok(DeleteResult {
        view: view(&data),
        usage: usage.into(),
    })
}

/// 设置定时刷新间隔（分钟，0 = 关闭），账户状态刷新与用量统计自动更新共用。
#[tauri::command]
pub fn accounts_set_interval(app: AppHandle, interval_minutes: u32) -> Result<AccountsView, String> {
    let data = mutate(&app, |d| {
        d.interval_minutes = interval_minutes;
        Ok(())
    })?;
    let message = if interval_minutes == 0 {
        "关闭定时刷新".to_string()
    } else if interval_minutes >= 60 && interval_minutes.is_multiple_of(60) {
        format!("定时刷新设为每 {} 小时（账户状态 + 用量统计）", interval_minutes / 60)
    } else {
        format!("定时刷新设为每 {interval_minutes} 分钟（账户状态 + 用量统计）")
    };
    audit::log("interval_set", message, None);
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

/// 换票被拒的原因，按服务端错误码归类（与 Codex CLI 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshDenial {
    /// `refresh_token_expired`：refresh_token 已过期。
    Expired,
    /// `refresh_token_reused`：这个 refresh_token 已被使用过——被别处（如本机 Codex 客户端）
    /// 轮换后又拿旧值来换，触发了重放检测。
    Reused,
    /// `refresh_token_invalidated`：refresh_token 已被吊销（登出 / 撤销授权）。
    Revoked,
    /// 401 或 400 `invalid_grant` 但没有细分子码：不可用，原因未知。
    Other,
}

impl RefreshDenial {
    /// 机器可读短码：写进状态摘要与切换错误码后缀。
    pub(crate) fn code(self) -> &'static str {
        match self {
            RefreshDenial::Expired => "expired",
            RefreshDenial::Reused => "reused",
            RefreshDenial::Revoked => "revoked",
            RefreshDenial::Other => "invalid",
        }
    }

    /// 界面与审计用的说明。
    pub(crate) fn label(self) -> &'static str {
        match self {
            RefreshDenial::Expired => "Refresh Token 已过期",
            RefreshDenial::Reused => "Refresh Token 已被使用过（已被别处轮换）",
            RefreshDenial::Revoked => "Refresh Token 已被吊销",
            RefreshDenial::Other => "Refresh Token 已失效",
        }
    }
}

pub(crate) enum RefreshOutcome {
    /// 2xx 且拿到了新 access_token。
    Success {
        access_token: String,
        refresh_token: Option<String>,
        /// 带 openid scope 时返回；写本机 auth.json 必需（Codex 反序列化要求该字段）。
        id_token: Option<String>,
    },
    /// 永久失败：refresh token 不可用。detail 为服务端给出的说明（error_description 等）。
    Denied { reason: RefreshDenial, detail: String },
}

/// 从 OAuth 错误响应体里取错误码：`error` 为字符串直接用；为对象取其 `code`；否则取顶层 `code`。
fn refresh_error_code(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body.trim()).ok()?;
    let map = v.as_object()?;
    match map.get("error") {
        Some(Value::String(code)) => return Some(code.clone()),
        Some(Value::Object(obj)) => {
            if let Some(code) = obj.get("code").and_then(Value::as_str) {
                return Some(code.to_string());
            }
        }
        _ => {}
    }
    map.get("code").and_then(Value::as_str).map(str::to_string)
}

/// 错误响应体里的可读说明：`error_description` / `error.message` / `message`，都没有就截断原文。
fn refresh_error_detail(body: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(body.trim()).ok();
    let from_json = parsed.as_ref().and_then(|v| {
        ["/error_description", "/error/message", "/message"]
            .iter()
            .find_map(|p| v.pointer(p).and_then(Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    });
    from_json.unwrap_or_else(|| http::truncate(body.trim(), 200))
}

/// 用 refresh_token 向 OpenAI OAuth 端点换取新 access_token。
/// 与 Codex CLI 同一判定：401、带 expired / reused / invalidated 子码、或 400 `invalid_grant`
/// 视为永久失败返回 Denied；网络错误、5xx、429 等其它失败视为临时失败返回 Err，
/// 调用方保留旧状态、下次再试，不把账户标为失效。
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
    if !status.is_success() {
        let code = refresh_error_code(&text).map(|c| c.to_ascii_lowercase());
        let reason = match code.as_deref() {
            Some("refresh_token_expired") => Some(RefreshDenial::Expired),
            Some("refresh_token_reused") => Some(RefreshDenial::Reused),
            Some("refresh_token_invalidated") => Some(RefreshDenial::Revoked),
            _ => None,
        };
        let invalid_grant = status.as_u16() == 400 && code.as_deref() == Some("invalid_grant");
        let permanent = status.as_u16() == 401 || reason.is_some() || invalid_grant;
        if permanent {
            return Ok(RefreshOutcome::Denied {
                reason: reason.unwrap_or(RefreshDenial::Other),
                detail: refresh_error_detail(&text),
            });
        }
        return Err(format!(
            "refresh_http_{}: {}",
            status.as_u16(),
            refresh_error_detail(&text)
        ));
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

fn jwt_exp(token: &str) -> Option<i64> {
    http::decode_jwt_payload(token).and_then(|c| c.get("exp").and_then(|x| x.as_i64()))
}

/// 与 Codex CLI 同一策略：access_token 距过期不足 5 分钟（含已过期）就该续期；
/// 解析不出过期时刻时，距上次获取凭据超过 8 天就该续期。
pub(crate) fn codex_token_needs_renewal(token: &str, last_refresh: Option<DateTime<Utc>>) -> bool {
    match jwt_exp(token) {
        Some(exp) => exp <= Utc::now().timestamp() + CODEX_RENEW_AHEAD_SECS,
        None => last_refresh
            .is_some_and(|t| t < Utc::now() - chrono::Duration::days(CODEX_RENEW_INTERVAL_DAYS)),
    }
}

/// 定时同步入口：本机 auth.json 有变化时，把与之同账号的 ChatGPT 账户更新为本机更新的凭据
///（判定与取回逻辑同 adopt_newer_local_codex）。返回是否有账户被更新。
pub(crate) fn sync_codex_from_local(app: &AppHandle) -> Result<bool, String> {
    let mut changed = false;
    for acc in current().accounts.into_iter().filter(|a| a.kind == "codex") {
        let name = display_name(&acc.kind, &acc.note, &acc.token);
        let mut token = acc.token.clone();
        let mut refresh_token = acc.refresh_token.clone();
        if adopt_newer_local_codex(app, &acc.id, &name, &mut token, &mut refresh_token)? {
            changed = true;
        }
    }
    Ok(changed)
}

/// 本机 Codex 客户端会自行续期并轮换 refresh_token，账户里保存的那组随即作废。
/// 刷新 / 切换前先看本机 auth.json：同一账号且其 access_token 过期时刻更晚（客户端在程序之后
/// 续期过），就改用本机这组凭据并写回账户。返回是否发生了同步。
pub(crate) fn adopt_newer_local_codex(
    app: &AppHandle,
    id: &str,
    name: &str,
    token: &mut String,
    refresh_token: &mut Option<String>,
) -> Result<bool, String> {
    let Some(local) = codex_local::local_login_of_same_account(token) else {
        return Ok(false);
    };
    if local.token == *token {
        return Ok(false);
    }
    match (jwt_exp(&local.token), jwt_exp(token)) {
        (Some(local_exp), Some(own_exp)) if local_exp > own_exp => {}
        _ => return Ok(false),
    }
    *token = local.token.clone();
    if local.refresh_token.is_some() {
        *refresh_token = local.refresh_token.clone();
    }
    // 本机文件就是完整的 auth.json，整份存为副本；读不到原文时只更新 token 字段
    let (new_token, new_rt) = (token.clone(), refresh_token.clone());
    update_codex_auth(app, id, move |v| match local.raw {
        Some(raw) => *v = raw,
        None => apply_codex_tokens(v, &new_token, new_rt.as_deref(), None),
    })?;
    audit::log(
        "codex_adopt_local",
        format!("ChatGPT 凭据已从本机登录同步：{name}（客户端已自行续期）"),
        Some(json!({ "id": id })),
    );
    Ok(true)
}

enum CodexRenewal {
    Renewed,
    NoRefreshToken,
    /// refresh token 最终被拒绝（含本机凭据重试）：归类原因与服务端说明。
    Denied(RefreshDenial, String),
}

/// 续期被拒后写入状态摘要的字段：alive=false，附归类原因（短码 + 说明）与服务端细节。
fn denied_status(reason: RefreshDenial, detail: &str) -> Value {
    json!({
        "alive": false,
        "refreshError": format!("{}（{detail}）", reason.label()),
        "refreshErrorCode": reason.code(),
    })
}

/// 用账户的 refresh_token 续期并换上新凭据、写回账户、回同步本机 auth.json。
/// 被拒绝多半是本机客户端已自行续期并轮换了 refresh_token：本机同一账号持有不同的
/// refresh_token 时改用它再试一次，再失败才判定失效。
async fn renew_codex(
    app: &AppHandle,
    id: &str,
    name: &str,
    token: &mut String,
    refresh_token: &mut Option<String>,
) -> Result<CodexRenewal, String> {
    let Some(own_rt) = refresh_token.clone() else {
        return Ok(CodexRenewal::NoRefreshToken);
    };
    let mut used_rt = own_rt.clone();
    let mut outcome = request_codex_refresh(&own_rt).await?;
    if let RefreshOutcome::Denied { reason, detail } = &outcome {
        let local_rt = codex_local::local_login_of_same_account(token)
            .and_then(|l| l.refresh_token)
            .filter(|l| *l != own_rt);
        if let Some(local_rt) = local_rt {
            audit::log(
                "codex_renew_failed",
                format!(
                    "ChatGPT 续期被拒绝：{name}（{}：{detail}），改用本机登录的凭据重试",
                    reason.label()
                ),
                Some(json!({ "id": id, "reason": reason.code() })),
            );
            used_rt = local_rt.clone();
            outcome = request_codex_refresh(&local_rt).await?;
        }
    }
    match outcome {
        RefreshOutcome::Success {
            access_token,
            refresh_token: new_rt,
            id_token,
        } => {
            *token = access_token;
            // 轮换出的新值优先；接口未返回新值时保留本次实际使用的那个（可能是本机的）
            *refresh_token = new_rt.or(Some(used_rt));
            // 新凭据组整体写进副本（含 id_token），旧 refresh_token 已作废，必须先落盘
            let (new_token, new_rt, new_idt) = (token.clone(), refresh_token.clone(), id_token.clone());
            update_codex_auth(app, id, move |v| {
                apply_codex_tokens(v, &new_token, new_rt.as_deref(), new_idt.as_deref())
            })?;
            audit::log(
                "codex_renewed",
                format!("ChatGPT access_token 已自动续期：{name}"),
                Some(json!({ "id": id })),
            );
            // 本机 auth.json 若是同一账号则顺带同步新凭据（失败在函数内部消化，不影响刷新）
            codex_local::sync_auth_json(token, refresh_token, id_token.as_deref());
            Ok(CodexRenewal::Renewed)
        }
        RefreshOutcome::Denied { reason, detail } => {
            audit::log(
                "codex_renew_failed",
                format!("ChatGPT 续期被拒绝：{name}（{}：{detail}）", reason.label()),
                Some(json!({ "id": id, "reason": reason.code() })),
            );
            Ok(CodexRenewal::Denied(reason, detail))
        }
    }
}

/// Codex 账户刷新：先同步本机客户端更新过的凭据，必要时续期 access_token，再查询用量；
/// 401/403 时补一次续期重试。
async fn refresh_codex_account(app: &AppHandle, id: &str, snap: Account) -> Result<Account, String> {
    let name = display_name(&snap.kind, &snap.note, &snap.token);
    let mut token = snap.token;
    let mut refresh_token = snap.refresh_token;
    let mut refreshed = false;

    // 0) 本机客户端若在程序之后自行续期过，先换上它的那组凭据
    adopt_newer_local_codex(app, id, &name, &mut token, &mut refresh_token)?;

    // 1) 与 Codex CLI 同一策略判断是否续期（refresh_token 随之轮换）
    let last_refresh = snapshot(id).ok().and_then(|a| codex_last_refresh(&a));
    if codex_token_needs_renewal(&token, last_refresh) {
        match renew_codex(app, id, &name, &mut token, &mut refresh_token).await? {
            CodexRenewal::Renewed => refreshed = true,
            CodexRenewal::NoRefreshToken => {}
            CodexRenewal::Denied(reason, detail) => {
                return finish(app, id, denied_status(reason, &detail));
            }
        }
    }

    // 2) 查询用量；token 失效（401/403）且本次尚未续期过则补一次续期并重试一次
    let mut usage = codex::codex_usage(token.clone()).await?;
    if !usage.alive && (usage.status == 401 || usage.status == 403) && !refreshed {
        match renew_codex(app, id, &name, &mut token, &mut refresh_token).await? {
            CodexRenewal::Renewed => usage = codex::codex_usage(token.clone()).await?,
            CodexRenewal::NoRefreshToken => {}
            CodexRenewal::Denied(reason, detail) => {
                return finish(app, id, denied_status(reason, &detail));
            }
        }
    }

    let status = to_status(&usage)?;
    finish(app, id, status)
}

/// Claude 续期成功的统一收尾：换上新凭据（refresh_token 有新值才替换）、写回账户、记审计。
fn apply_claude_renewal(
    app: &AppHandle,
    id: &str,
    name: &str,
    token: &mut String,
    refresh_token: &mut Option<String>,
    access_token: String,
    new_rt: Option<String>,
) -> Result<(), String> {
    *token = access_token;
    if new_rt.is_some() {
        *refresh_token = new_rt;
    }
    persist_tokens(app, id, token, refresh_token)?;
    audit::log(
        "claude_renewed",
        format!("Claude access_token 已自动续期：{name}"),
        Some(json!({ "id": id })),
    );
    Ok(())
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
                    apply_claude_renewal(app, id, &name, &mut token, &mut refresh_token, access_token, new_rt)?;
                    expires_at_ms = new_exp;
                    refreshed = true;
                }
                claude_oauth::ClaudeRefreshOutcome::Denied { body } => {
                    audit::log(
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
                    apply_claude_renewal(app, id, &name, &mut token, &mut refresh_token, access_token, new_rt)?;
                    expires_at_ms = new_exp;
                    usage = claude::claude_usage(token.clone()).await?;
                }
                claude_oauth::ClaudeRefreshOutcome::Denied { body } => {
                    audit::log(
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
/// 开始（含排队等待）与结束时广播 account-refreshing，主窗口与托盘据此同步行内「刷新中」状态，
/// 不论刷新由哪个窗口发起。
#[tauri::command]
pub async fn account_refresh(app: AppHandle, id: String) -> Result<AccountView, String> {
    let _ = app.emit("account-refreshing", json!({ "id": id, "active": true }));
    let result = refresh_account_inner(&app, &id).await;
    let _ = app.emit("account-refreshing", json!({ "id": id, "active": false }));
    result.map(|acc| account_view(&acc))
}

async fn refresh_account_inner(app: &AppHandle, id: &str) -> Result<Account, String> {
    let _queued = refresh_queue().lock().await;
    let snap = snapshot(id)?;
    let kind = snap.kind.clone();
    let name = display_name(&snap.kind, &snap.note, &snap.token);
    let prev_alive = snap
        .status
        .as_ref()
        .and_then(|s| s.get("alive"))
        .and_then(Value::as_bool);

    let result: Result<Account, String> = match kind.as_str() {
        "cursor" => match cursor::cursor_inspect_token(snap.token).await {
            Ok(payload) => to_status(&payload).and_then(|status| finish(app, id, status)),
            Err(e) => Err(e),
        },
        "codex" => refresh_codex_account(app, id, snap).await,
        "claude" => refresh_claude_account(app, id, snap).await,
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
                "account_refresh_failed",
                format!("刷新账户失败：{name}（{e}）"),
                Some(json!({ "id": id })),
            );
        }
    }
    // 刷新成功后，自动备注用最新身份回填，返回回填后的最新账户
    if result.is_ok() {
        backfill_auto_note(app, id)?;
        return snapshot(id);
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
    /// ChatGPT：本机 auth.json 的完整内容，导入 / 同步时整份存为账户副本；其它类型为 None。
    pub raw: Option<Value>,
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
        masked_head(token)
    } else {
        hint.to_string()
    }
}

/// 读取本机已登录的指定类型凭据并导入为托管账户（Cursor / ChatGPT 互不影响）。
/// 本机从未登录既不算导入也不算跳过；同身份已存在记 exists，凭据无法解析记 invalid。
#[tauri::command]
pub async fn accounts_import_local(app: AppHandle, kind: String) -> Result<ImportLocalResult, String> {
    settings::ensure_loaded()?;
    let kind = sanitize_kind(&kind)?;
    let mut imported: Vec<ImportedItem> = Vec::new();
    let mut skipped: Vec<SkippedItem> = Vec::new();
    let mut added: Vec<(String, String, String, String)> = Vec::new();
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
                let mut entry = Account {
                    id: new_id(),
                    kind: login.kind,
                    note,
                    note_auto: true,
                    token,
                    refresh_token: login.refresh_token,
                    codex_auth: None,
                    last_refresh_at: None,
                    status: None,
                };
                // ChatGPT：本机 auth.json 本身就是完整凭据组，整份存为副本
                if entry.kind == "codex" {
                    let v = login
                        .raw
                        .unwrap_or_else(|| skeleton_codex_auth(&entry.token, entry.refresh_token.as_deref()));
                    set_codex_auth(&mut entry, &v)?;
                }
                let name = display_name(&entry.kind, &entry.note, &entry.token);
                audit_names.push(name.clone());
                added.push((entry.id.clone(), entry.kind.clone(), entry.token.clone(), name));
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
            "account_import",
            format!("从本机导入账户：{}", audit_names.join("、")),
            Some(json!({ "ids": ids })),
        );
    }
    adopt_deleted_usage(&app, &added).await;
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
    /// ChatGPT：加密的完整 auth.json 副本，随导出携带，导入时优先用它恢复（含 id_token）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    codex_auth: Option<String>,
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
            codex_auth: a.codex_auth.clone(),
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
    let mut added: Vec<(String, String, String, String)> = Vec::new();
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
                let mut entry = Account {
                    id: new_id(),
                    kind: kind.clone(),
                    note,
                    note_auto,
                    token,
                    refresh_token,
                    // 文件里带的副本能解开就用它（含 id_token），否则由明文凭据组骨架副本
                    codex_auth: if kind == "codex" { item.codex_auth } else { None },
                    last_refresh_at: None,
                    status: None,
                };
                if entry.kind == "codex" {
                    hydrate_accounts(std::slice::from_mut(&mut entry));
                }
                let name = display_name(&entry.kind, &entry.note, &entry.token);
                audit_names.push(name.clone());
                added.push((entry.id.clone(), entry.kind.clone(), entry.token.clone(), name));
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
            "account_import_file",
            format!("从文件导入{label}账户：{}", audit_names.join("、")),
            Some(json!({ "kind": kind, "ids": ids })),
        );
    }
    adopt_deleted_usage(&app, &added).await;
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
        codex_auth: None,
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

#[cfg(test)]
mod refresh_error_tests {
    use super::*;

    #[test]
    fn error_code_extraction_matches_codex_cli() {
        // RFC 6749 形态：error 为字符串，说明在 error_description
        let rfc = r#"{"error":"invalid_grant","error_description":"refresh token expired"}"#;
        assert_eq!(refresh_error_code(rfc).as_deref(), Some("invalid_grant"));
        assert_eq!(refresh_error_detail(rfc), "refresh token expired");
        // 旧形态：error 为对象，子码在 error.code
        let legacy = r#"{"error":{"code":"refresh_token_reused","message":"already used"}}"#;
        assert_eq!(refresh_error_code(legacy).as_deref(), Some("refresh_token_reused"));
        assert_eq!(refresh_error_detail(legacy), "already used");
        // 顶层 code
        assert_eq!(
            refresh_error_code(r#"{"code":"refresh_token_expired"}"#).as_deref(),
            Some("refresh_token_expired")
        );
        // 非 JSON：没有错误码，说明取截断原文
        assert!(refresh_error_code("Bad Gateway").is_none());
        assert_eq!(refresh_error_detail("  Bad Gateway  "), "Bad Gateway");
    }
}

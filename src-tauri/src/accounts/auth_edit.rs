//! 主窗口按需添加或编辑单个托管账户的 Auth。明文只存在于此次请求，错误只报告位置或字段。
use super::{codex_auth_json, json_str_at, skeleton_codex_auth, Account};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, WebviewWindow};

pub(super) fn revision(account: &Account) -> String {
    let mut hash = Sha256::new();
    if let Some(blob) = &account.codex_auth {
        hash.update(blob.as_bytes());
    } else {
        hash.update(account.token.as_bytes());
        hash.update([0]);
        hash.update(account.refresh_token.as_deref().unwrap_or("").as_bytes());
    }
    format!("{:x}", hash.finalize())
}

pub(super) fn check_revision(account: &Account, expected: &str) -> Result<(), String> {
    if account.kind != "codex" {
        return Err("not_codex_account".into());
    }
    if revision(account) != expected {
        return Err("codex_auth_conflict".into());
    }
    Ok(())
}

#[derive(Serialize)]
pub(crate) struct AuthSnapshot {
    text: String,
    revision: String,
}

pub(super) fn saved_value(account: &Account) -> Result<Value, String> {
    if account.kind != "codex" {
        return Err("not_codex_account".into());
    }
    if account.codex_auth.is_some() {
        return codex_auth_json(account).ok_or_else(|| "codex_auth_unreadable".into());
    }
    if account.token.trim().is_empty() {
        return Err("codex_auth_missing".into());
    }
    Ok(skeleton_codex_auth(
        &account.token,
        account.refresh_token.as_deref(),
    ))
}

fn main_only(window: &WebviewWindow) -> Result<(), String> {
    if window.label() == crate::main_window::LABEL {
        Ok(())
    } else {
        Err("main_window_required".into())
    }
}

#[tauri::command]
pub(crate) fn codex_auth_read(window: WebviewWindow, id: String) -> Result<AuthSnapshot, String> {
    main_only(&window)?;
    let _operation = crate::operations::begin()?;
    crate::settings::ensure_loaded()?;
    let account = super::account_snapshot(&id)?;
    let value = saved_value(&account)?;
    Ok(AuthSnapshot {
        text: format!("{value:#}"),
        revision: revision(&account),
    })
}

#[derive(Serialize)]
pub(crate) struct AuthFile {
    name: String,
    text: String,
}

/// 只读取用户选中的文件，不修改本机登录，也不创建账户。取消选择保持现有草稿。
#[tauri::command]
pub(crate) async fn codex_auth_pick_file(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<Option<AuthFile>, String> {
    main_only(&window)?;
    let _operation = crate::operations::begin()?;
    tokio::task::spawn_blocking(move || -> Result<Option<AuthFile>, String> {
        let Some(path) = super::pick_json_path(&app, "选择 auth.json", None, false) else {
            return Ok(None);
        };
        let text = std::fs::read_to_string(&path).map_err(|_| "codex_auth_file_read".to_string())?;
        credentials(&parse(&text)?)?;
        Ok(Some(AuthFile {
            name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
            text,
        }))
    })
    .await
    .map_err(|_| "codex_auth_file_read".to_string())?
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TokenFields {
    access_token: String,
    refresh_token: Option<String>,
}

pub(super) fn parse(text: &str) -> Result<Value, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let value: Value = serde_json::from_str(text).map_err(|e| {
        let line = e.line().max(1);
        // serde_json 以 UTF-8 字节计列，界面以字符显示，避免中文前缀使位置偏移。
        let byte = if e.is_eof() {
            e.column()
        } else {
            e.column().saturating_sub(1)
        };
        let column = text
            .split('\n')
            .nth(line - 1)
            .unwrap_or_default()
            .char_indices()
            .take_while(|(offset, _)| *offset < byte)
            .count()
            + 1;
        format!("codex_auth_json:{line}:{column}")
    })?;
    if !value.is_object() {
        return Err("codex_auth_object".into());
    }
    Ok(value)
}

pub(super) fn credentials(auth: &Value) -> Result<(String, Option<String>), String> {
    let access = json_str_at(auth, "/tokens/access_token").ok_or("codex_auth_access_token")?;
    if auth
        .pointer("/tokens/refresh_token")
        .is_some_and(|v| !v.is_null() && !v.is_string())
    {
        return Err("codex_auth_refresh_token".into());
    }
    Ok((access, json_str_at(auth, "/tokens/refresh_token")))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuthProjection {
    text: String,
    access_token: String,
    refresh_token: Option<String>,
}

/// 在 Rust 中投影两个简易字段，保留 JSON 扩展字段（包括 JavaScript 无法精确表示的整数）。
fn project(text: &str, patch: Option<TokenFields>) -> Result<AuthProjection, String> {
    let mut value = parse(text)?;
    if let Some(fields) = patch {
        let access = fields.access_token.trim();
        if access.is_empty() {
            return Err("codex_auth_access_token".into());
        }
        let refresh = fields
            .refresh_token
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let (old_access, old_refresh) = credentials(&value)?;
        let tokens = value["tokens"]
            .as_object_mut()
            .ok_or("codex_auth_access_token")?;
        if old_access != access {
            tokens.insert("access_token".into(), access.into());
        }
        if old_refresh.as_deref() != refresh {
            if let Some(rt) = refresh {
                tokens.insert("refresh_token".into(), rt.into());
            } else {
                tokens.remove("refresh_token");
            }
        }
    }
    let (access_token, refresh_token) = credentials(&value)?;
    Ok(AuthProjection {
        text: format!("{value:#}"),
        access_token,
        refresh_token,
    })
}

#[tauri::command]
pub(crate) fn codex_auth_project(
    window: WebviewWindow,
    text: String,
    patch: Option<TokenFields>,
) -> Result<AuthProjection, String> {
    main_only(&window)?;
    project(&text, patch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_keeps_large_extension_values_and_reports_safe_json_positions() {
        let source = r#"{"extra":9007199254740993,"long":123456789012345678901234567890,"ratio":0.123456789012345678901,"tokens":{"access_token":"fixture","refresh_token":"old","nested":[1,2]}}"#;
        let result = project(
            source,
            Some(TokenFields {
                access_token: "new".into(),
                refresh_token: None,
            }),
        )
        .unwrap();
        let value = parse(&result.text).unwrap();
        assert_eq!(value["extra"].as_u64(), Some(9_007_199_254_740_993));
        assert_eq!(value["long"].to_string(), "123456789012345678901234567890");
        assert_eq!(value["ratio"].to_string(), "0.123456789012345678901");
        assert_eq!(parse("{\"中文\": }").unwrap_err(), "codex_auth_json:1:8");
        assert_eq!(value["tokens"]["nested"], serde_json::json!([1, 2]));
        assert_eq!(parse(&format!("\u{feff}{source}")).unwrap(), parse(source).unwrap());
        assert_eq!(result.access_token, "new");
        assert!(value["tokens"].get("refresh_token").is_none());
        assert!(parse("{\n  \"fixture\": }")
            .unwrap_err()
            .starts_with("codex_auth_json:2:"));
        assert_eq!(
            credentials(&serde_json::json!({"tokens":{"access_token":false}})).unwrap_err(),
            "codex_auth_access_token"
        );
    }
}

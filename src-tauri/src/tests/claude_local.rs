//! claude_local 模块的单元测试（由 claude_local.rs 以 `#[path]` 挂载为 `crate::claude_local::tests`）。

use super::*;

#[test]
fn strip_login_keeps_other_entries() {
    // 只有账号登录：去掉后为空，整份凭据可删
    let only = r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r"}}"#;
    assert_eq!(strip_claude_login(only), (true, None));

    // 还有其它条目（如 MCP 服务器的 OAuth 凭据）：去掉登录段后保留其余内容
    let mixed = r#"{"claudeAiOauth":{"accessToken":"a"},"mcpOAuth":{"server":{"accessToken":"m"}}}"#;
    let (had, rest) = strip_claude_login(mixed);
    assert!(had);
    let rest: Value = serde_json::from_str(&rest.expect("其余条目应保留")).unwrap();
    assert_eq!(rest, json!({ "mcpOAuth": { "server": { "accessToken": "m" } } }));

    // 没有登录段：原本就未登录，其余内容照样返回
    let (had, rest) = strip_claude_login(r#"{"mcpOAuth":{}}"#);
    assert!(!had);
    assert!(rest.is_some());
    assert_eq!(strip_claude_login("{}"), (false, None));

    // 解析不了：视为有登录残留、无可保留内容
    assert_eq!(strip_claude_login("not json"), (true, None));
}

#[test]
fn credential_switch_preserves_other_auth_entries_and_rejects_invalid_files() {
    let oauth=serde_json::from_value::<Map<String,Value>>(json!({"accessToken":"new"})).unwrap();
    let merged=merge_oauth(Some(r#"{"claudeAiOauth":{"accessToken":"old"},"mcp":{"key":"keep"}}"#),&oauth).unwrap();
    let value:Value=serde_json::from_str(&merged).unwrap();
    assert_eq!(value["mcp"]["key"],"keep");
    assert_eq!(value["claudeAiOauth"]["accessToken"],"new");
    assert!(merge_oauth(Some("not-json"),&oauth).is_err());
}

//! codex_local 模块的单元测试（由 codex_local.rs 以 `#[path]` 挂载为 `crate::codex_local::tests`）。

use super::*;

#[test]
fn logout_keeps_only_api_key() {
    // 有账号登录也有 API Key：退出后只剩 API Key
    let both = json!({
        "OPENAI_API_KEY": "sk-test",
        "tokens": { "id_token": "i", "access_token": "a", "refresh_token": "r", "account_id": "acc" },
        "last_refresh": "2026-09-22T00:00:00Z",
    });
    assert!(has_chatgpt_login(&both));
    assert_eq!(logged_out_auth_value(&both), Some(json!({ "OPENAI_API_KEY": "sk-test" })));

    // 只有账号登录、API Key 为 null / 空白：整个文件应删除
    let null_key = json!({ "OPENAI_API_KEY": null, "tokens": { "access_token": "a" } });
    assert!(has_chatgpt_login(&null_key));
    assert_eq!(logged_out_auth_value(&null_key), None);
    let blank_key = json!({ "OPENAI_API_KEY": "  ", "tokens": { "access_token": "a" } });
    assert_eq!(logged_out_auth_value(&blank_key), None);

    // 只有 API Key、没有账号登录：不算登录
    let key_only = json!({ "OPENAI_API_KEY": "sk-test" });
    assert!(!has_chatgpt_login(&key_only));
    // tokens 存在但 access_token 空白同样不算登录
    assert!(!has_chatgpt_login(&json!({ "tokens": { "access_token": " " } })));
}

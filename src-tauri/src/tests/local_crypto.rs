//! local_crypto 模块的单元测试（由 local_crypto.rs 以 `#[path]` 挂载为 `crate::local_crypto::tests`）。

use super::*;

#[test]
fn round_trip_and_tamper_detection() {
    let sealed = seal(r#"{"tokens":{"access_token":"a","refresh_token":"b"}}"#).unwrap();
    assert!(sealed.starts_with(PREFIX));
    assert_eq!(
        open(&sealed).as_deref(),
        Some(r#"{"tokens":{"access_token":"a","refresh_token":"b"}}"#)
    );
    // 同一明文两次加密 nonce 不同，密文不同
    assert_ne!(seal("x").unwrap(), seal("x").unwrap());
    // 篡改 / 非本程序格式 / 明文都解不开
    let mut tampered = sealed.clone();
    tampered.pop();
    tampered.push(if sealed.ends_with('A') { 'B' } else { 'A' });
    assert!(open(&tampered).is_none());
    assert!(open("plain text").is_none());
    assert!(open("mv1:").is_none());
}

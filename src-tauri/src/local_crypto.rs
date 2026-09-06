//! 本机存储用的简单对称加密：AES-256-GCM + 程序内置固定密钥。
//!
//! 目的只是让 settings.json 里的 ChatGPT 凭据副本不能被直接读出，不是防御能反编译或调试本程序的
//! 攻击者（密钥就在程序里）。密文自带随机 nonce，格式 `mv1:` + base64(nonce || ciphertext)，
//! 不依赖机器或用户身份，随全量备份迁移到别的机器后仍可解开。

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce};
use base64::Engine;
use sha2::{Digest, Sha256};

const PREFIX: &str = "mv1:";
const NONCE_LEN: usize = 12;
const KEY_SEED: &str = "my-ai-assistant::local-credential-store::v1";

fn cipher() -> Aes256Gcm {
    let key = Sha256::digest(KEY_SEED.as_bytes());
    Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key))
}

/// 加密明文，返回可直接落盘的字符串。
pub fn seal(plaintext: &str) -> Result<String, String> {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce).map_err(|_| "random_failed".to_string())?;
    let ciphertext = cipher()
        .encrypt(Nonce::from_slice(&nonce), plaintext.as_bytes())
        .map_err(|_| "encrypt_failed".to_string())?;
    let mut bytes = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    bytes.extend_from_slice(&nonce);
    bytes.extend_from_slice(&ciphertext);
    Ok(format!(
        "{PREFIX}{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// 解密 [`seal`] 的输出；格式不对、被改动或不是本程序加密的一律返回 None。
pub fn open(blob: &str) -> Option<String> {
    let encoded = blob.trim().strip_prefix(PREFIX)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    if bytes.len() <= NONCE_LEN {
        return None;
    }
    let (nonce, ciphertext) = bytes.split_at(NONCE_LEN);
    let plaintext = cipher().decrypt(Nonce::from_slice(nonce), ciphertext).ok()?;
    String::from_utf8(plaintext).ok()
}

#[cfg(test)]
mod tests {
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
}

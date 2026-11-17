//! Codex's configured auth store. Never infer absence from a failed keychain read.
//! Format reference: openai/codex, codex-rs/login/src/auth/storage.rs.
use age::secrecy::SecretString;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const AUTH_SECRET: &str = "global/CODEX_AUTH";
const LIMIT: u64 = 8 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    File,
    Keyring,
    Auto,
}
struct Store {
    root: PathBuf,
    mode: Mode,
    secrets: bool,
}

pub fn home() -> Result<PathBuf, String> {
    crate::paths::env_nonempty("CODEX_HOME")
        .or_else(|| crate::paths::home_dir().map(|p| p.join(".codex")))
        .ok_or_else(|| "codex_home_unavailable".into())
}
fn parse_policy(text: &str) -> Result<(Mode, Option<bool>), String> {
    let config: toml::Value = toml::from_str(text).map_err(|_| "codex_config_invalid")?;
    let mode_value = match config.get("cli_auth_credentials_store") {
        None => "file",
        Some(value) => value.as_str().ok_or("codex_config_invalid")?,
    };
    let mode = match mode_value {
        "file" => Mode::File,
        "keyring" => Mode::Keyring,
        "auto" => Mode::Auto,
        _ => return Err("codex_auth_store_unsupported".into()),
    };
    let profile = config
        .get("profile")
        .map(|v| v.as_str().ok_or("codex_config_invalid"))
        .transpose()?
        .and_then(|name| config.get("profiles").and_then(|v| v.get(name)));
    let secrets = profile
        .and_then(|v| v.get("features"))
        .and_then(|v| v.get("secret_auth_storage"))
        .or_else(|| {
            config
                .get("features")
                .and_then(|v| v.get("secret_auth_storage"))
        })
        .map(|v| {
            v.as_bool()
                .ok_or_else(|| "codex_config_invalid".to_string())
        })
        .transpose()?;
    Ok((mode, secrets))
}

fn store() -> Result<Store, String> {
    let root = home()?;
    let text = match fs::read_to_string(root.join("config.toml")) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return Err("codex_config_unreadable".into()),
    };
    let (mode, configured_secrets) = parse_policy(&text)?;
    // Match Codex's platform default, including a new profile without a secrets
    // file yet. File existence cannot select which backend a client will read.
    let secrets = configured_secrets.unwrap_or(cfg!(windows));
    Ok(Store {
        root,
        mode,
        secrets,
    })
}
pub fn check() -> Result<(), String> {
    store()?.read().map(|_| ())
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("credential_path_invalid")?;
    fs::create_dir_all(parent).map_err(|_| "credential_write_failed")?;
    let mut temp =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| "credential_write_failed")?;
    temp.write_all(bytes)
        .map_err(|_| "credential_write_failed")?;
    temp.as_file()
        .sync_all()
        .map_err(|_| "credential_write_failed")?;
    temp.persist(path).map_err(|_| "credential_write_failed")?;
    Ok(())
}
fn json_value(text: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(text).map_err(|_| "codex_auth_invalid")?;
    if !value.is_object() {
        return Err("codex_auth_invalid".into());
    }
    Ok(value)
}
fn file_read(path: &Path) -> Result<Option<Value>, String> {
    match fs::read_to_string(path) {
        Ok(v) => json_value(&v).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("codex_auth_read_failed".into()),
    }
}
fn entry(root: &Path, service: &str, prefix: &str) -> Result<keyring::Entry, String> {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let hash = format!(
        "{:x}",
        Sha256::digest(canonical.to_string_lossy().as_bytes())
    );
    keyring::Entry::new(service, &format!("{prefix}|{}", &hash[..16]))
        .map_err(|_| "codex_keyring_unavailable".into())
}
fn password(key: &keyring::Entry) -> Result<Option<String>, String> {
    match key.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err("codex_keyring_unavailable".into()),
    }
}
fn delete_key(key: &keyring::Entry) -> Result<(), String> {
    match key.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err("codex_keyring_write_failed".into()),
    }
}
fn decrypt(bytes: &[u8], key: String) -> Result<Value, String> {
    let identity = age::scrypt::Identity::new(SecretString::from(key));
    let decryptor = age::Decryptor::new(bytes).map_err(|_| "codex_auth_secrets_invalid")?;
    let reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|_| "codex_auth_secrets_invalid")?;
    let mut clear = Vec::new();
    reader
        .take(LIMIT + 1)
        .read_to_end(&mut clear)
        .map_err(|_| "codex_auth_secrets_invalid")?;
    if clear.len() as u64 > LIMIT {
        return Err("codex_auth_secrets_invalid".into());
    }
    let doc: Value = serde_json::from_slice(&clear).map_err(|_| "codex_auth_secrets_invalid")?;
    if doc["version"] != 1 || !doc["secrets"].is_object() {
        return Err("codex_auth_secrets_invalid".into());
    }
    Ok(doc)
}
fn encrypt(doc: &Value, key: String) -> Result<Vec<u8>, String> {
    let recipient = age::scrypt::Recipient::new(SecretString::from(key));
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
            .map_err(|_| "codex_auth_secrets_write_failed")?;
    let mut bytes = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut bytes)
        .map_err(|_| "codex_auth_secrets_write_failed")?;
    writer
        .write_all(&serde_json::to_vec(doc).map_err(|_| "codex_auth_invalid")?)
        .map_err(|_| "codex_auth_secrets_write_failed")?;
    writer
        .finish()
        .map_err(|_| "codex_auth_secrets_write_failed")?;
    Ok(bytes)
}
impl Store {
    fn direct(&self) -> Result<keyring::Entry, String> {
        entry(&self.root, "Codex Auth", "cli")
    }
    fn secrets_path(&self) -> PathBuf {
        self.root.join("secrets/codex_auth.age")
    }
    fn keyring_read(&self) -> Result<Option<Value>, String> {
        if self.secrets {
            if !self.secrets_path().exists() {
                return Ok(None);
            }
            let key = password(&entry(&self.root, "codex", "secrets")?)?
                .ok_or("codex_auth_key_missing")?;
            let doc = decrypt(
                &fs::read(self.secrets_path()).map_err(|_| "codex_auth_read_failed")?,
                key,
            )?;
            if let Some(text) = doc["secrets"][AUTH_SECRET].as_str() {
                return json_value(text).map(Some);
            }
            return Ok(None);
        }
        password(&self.direct()?)?
            .map(|s| json_value(&s))
            .transpose()
    }
    fn read(&self) -> Result<Option<Value>, String> {
        match self.mode {
            Mode::File => file_read(&self.root.join("auth.json")),
            Mode::Keyring => self.keyring_read(),
            Mode::Auto => match self.keyring_read() {
                Ok(Some(v)) => Ok(Some(v)),
                Ok(None) => file_read(&self.root.join("auth.json")),
                Err(e) if e == "codex_keyring_unavailable" => {
                    file_read(&self.root.join("auth.json"))
                }
                Err(e) => Err(e),
            },
        }
    }
    fn keyring_write(&self, value: Option<&Value>) -> Result<(), String> {
        let serialized = value
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| "codex_auth_invalid")?;
        if !self.secrets {
            return match serialized {
                Some(v) => self
                    .direct()?
                    .set_password(&v)
                    .map_err(|_| "codex_keyring_write_failed".into()),
                None => delete_key(&self.direct()?),
            };
        }
        let file = self.secrets_path();
        let key_entry = entry(&self.root, "codex", "secrets")?;
        let key = match password(&key_entry)? {
            Some(v) => v,
            None if file.exists() => return Err("codex_auth_key_missing".into()),
            None if value.is_none() => return delete_key(&self.direct()?),
            None => {
                use base64::Engine;
                let mut bytes = [0u8; 32];
                getrandom::getrandom(&mut bytes).map_err(|_| "random_failed")?;
                let v = base64::engine::general_purpose::STANDARD.encode(bytes);
                key_entry
                    .set_password(&v)
                    .map_err(|_| "codex_keyring_write_failed")?;
                v
            }
        };
        let mut doc = if file.exists() {
            decrypt(
                &fs::read(&file).map_err(|_| "codex_auth_read_failed")?,
                key.clone(),
            )?
        } else {
            json!({"version":1,"secrets":{}})
        };
        if let Some(text) = serialized {
            doc["secrets"][AUTH_SECRET] = json!(text);
        } else {
            doc["secrets"]
                .as_object_mut()
                .ok_or("codex_auth_invalid")?
                .remove(AUTH_SECRET);
        }
        atomic_write(&file, &encrypt(&doc, key)?)?;
        // A stale legacy entry must not become authoritative after logout/migration.
        delete_key(&self.direct()?)
    }
    fn file_write(&self, value: Option<&Value>) -> Result<(), String> {
        let path = self.root.join("auth.json");
        if path.exists() {
            fs::copy(&path, path.with_extension("json.bak"))
                .map_err(|_| "codex_auth_backup_failed")?;
        }
        match value {
            Some(v) => atomic_write(
                &path,
                &serde_json::to_vec_pretty(v).map_err(|_| "codex_auth_invalid")?,
            ),
            None => match fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(_) => Err("codex_auth_write_failed".into()),
            },
        }
    }
    fn write(&self, value: Option<&Value>) -> Result<(), String> {
        fs::create_dir_all(&self.root).map_err(|_| "codex_auth_write_failed")?;
        // Cooperate with running Codex sessions. Only the short local commit
        // holds auth.lock; credential exchanges never hold this filesystem lock.
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join("auth.lock"))
            .map_err(|_| "codex_auth_write_failed")?;
        fs2::FileExt::lock_exclusive(&lock).map_err(|_| "codex_auth_write_failed")?;
        if self.mode == Mode::File {
            return self.file_write(value);
        }
        let previous = self.keyring_read();
        // Auto may use the file backend only when the OS store is unavailable.
        if self.mode == Mode::Auto
            && previous
                .as_ref()
                .is_err_and(|e| e == "codex_keyring_unavailable")
        {
            return self.file_write(value);
        }
        let previous = previous?;
        if let Err(error) = self.keyring_write(value) {
            self.keyring_write(previous.as_ref())
                .map_err(|_| "codex_auth_rollback_failed")?;
            return Err(error);
        }
        if let Err(error) = self.file_write(None) {
            self.keyring_write(previous.as_ref())
                .map_err(|_| "codex_auth_rollback_failed")?;
            return Err(error);
        }
        Ok(())
    }
}
pub fn read() -> Result<Option<Value>, String> {
    store()?.read()
}
pub fn write(value: &Value) -> Result<(), String> {
    store()?.write(Some(value))
}
pub fn clear() -> Result<(), String> {
    store()?.write(None)
}
pub fn revision() -> Result<Option<String>, String> {
    read()?
        .map(|v| {
            serde_json::to_vec(&v)
                .map(|b| format!("{:x}", Sha256::digest(b)))
                .map_err(|_| "codex_auth_invalid".into())
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_does_not_downgrade_unknown_or_ephemeral_stores() {
        assert_eq!(parse_policy("").unwrap().0, Mode::File);
        assert_eq!(
            parse_policy("cli_auth_credentials_store = 'auto'")
                .unwrap()
                .0,
            Mode::Auto
        );
        assert!(parse_policy("cli_auth_credentials_store = 'ephemeral'").is_err());
        assert!(parse_policy("[invalid").is_err());
        assert!(parse_policy("cli_auth_credentials_store = 123").is_err());
        assert_eq!(
            parse_policy("cli_auth_credentials_store = 'keyring'")
                .unwrap()
                .1,
            None
        );
        assert_eq!(
            parse_policy("[features]\nsecret_auth_storage = true")
                .unwrap()
                .1,
            Some(true)
        );
        assert_eq!(parse_policy("profile='work'\n[features]\nsecret_auth_storage=false\n[profiles.work.features]\nsecret_auth_storage=true").unwrap().1,Some(true));
    }
    #[test]
    fn file_write_preserves_existing_data_on_invalid_parent() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("auth.json");
        atomic_write(&path, b"original").unwrap();
        assert!(atomic_write(&path.join("child"), b"new").is_err());
        assert_eq!(fs::read(path).unwrap(), b"original");
    }
    #[test]
    fn secrets_round_trip_preserves_other_entries() {
        let doc = json!({"version":1,"secrets":{"global/CODEX_AUTH":"{}","other":"keep"}});
        let encrypted = encrypt(&doc, "synthetic-test-key".into()).unwrap();
        assert_eq!(
            decrypt(&encrypted, "synthetic-test-key".into()).unwrap(),
            doc
        );
        assert!(decrypt(&encrypted, "wrong-key".into()).is_err());
    }
}

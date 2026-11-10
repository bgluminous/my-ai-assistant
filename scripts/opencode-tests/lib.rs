//! Includes production modules unchanged; substitutes only desktop IPC/settings.
//! No production user directory or network credentials are loaded by the fixtures.
#![allow(dead_code, unused_imports)]
extern crate self as tauri;
pub use command_stub::command;
#[derive(Clone)]
pub struct AppHandle;
pub trait Emitter {
    fn emit<S: serde::Serialize + Clone>(&self, _: &str, _: S) -> Result<(), String> {
        Ok(())
    }
}
impl Emitter for AppHandle {}
pub mod async_runtime {
    pub fn spawn_blocking<F: FnOnce() -> R + Send + 'static, R: Send + 'static>(
        f: F,
    ) -> tokio::task::JoinHandle<R> {
        tokio::task::spawn_blocking(f)
    }
}
mod audit {
    pub fn log(_: &str, _: String, _: Option<serde_json::Value>) {}
}
mod paths {
    pub fn home_dir() -> Option<std::path::PathBuf> {
        None
    }
    pub fn env_nonempty(_: &str) -> Option<std::path::PathBuf> {
        None
    }
    pub fn app_dir() -> Option<std::path::PathBuf> {
        None
    }
}
mod settings {
    use std::sync::{OnceLock, RwLock};
    #[derive(Clone, Default)]
    pub struct Settings {
        pub opencode_client: crate::opencode_local::OpenCodeClientConfig,
        pub pricing: crate::pricing::PricingTable,
        pub pricing_remote: crate::pricing::PricingRemote,
        pub proxy: crate::proxy::ProxyConfig,
        pub accounts: Vec<crate::accounts::Account>,
    }
    fn cell() -> &'static RwLock<Settings> {
        static S: OnceLock<RwLock<Settings>> = OnceLock::new();
        S.get_or_init(|| RwLock::new(Settings::default()))
    }
    pub fn ensure_loaded() -> Result<(), String> {
        Ok(())
    }
    pub fn path_display() -> String {
        "isolated-memory".into()
    }
    pub fn read<T>(f: impl FnOnce(&Settings) -> T) -> Result<T, String> {
        Ok(f(&cell().read().unwrap()))
    }
    pub fn mutate<T>(f: impl FnOnce(&mut Settings) -> Result<T, String>) -> Result<T, String> {
        f(&mut cell().write().unwrap())
    }
}
mod accounts {
    use serde::Serialize;
    #[derive(Clone, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Account {
        pub id: String,
        pub kind: String,
        pub token: String,
        pub note: String,
        pub opencode_auth: Option<String>,
        pub status: Option<serde_json::Value>,
    }
    #[derive(Serialize)]
    pub struct AccountView {
        pub account: Account,
    }
    pub fn new_id() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        format!("fixture-{}", N.fetch_add(1, Ordering::Relaxed))
    }
    pub fn account_snapshot(id: &str) -> Result<Account, String> {
        crate::settings::read(|s| s.accounts.iter().find(|a| a.id == id).cloned())?
            .ok_or("not_found".into())
    }
    pub fn save_opencode(
        _: &crate::AppHandle,
        auth: Option<&crate::opencode::Credential>,
        workspace: &str,
        _: &str,
        _: &str,
        note: Option<&str>,
    ) -> Result<AccountView, String> {
        let account = Account {
            id: new_id(),
            kind: "opencode".into(),
            token: workspace.into(),
            note: note.unwrap_or_default().into(),
            opencode_auth: auth.map(|a| a.seal()).transpose()?,
            status: None,
        };
        crate::settings::mutate(|s| {
            s.accounts.push(account.clone());
            Ok(())
        })?;
        Ok(AccountView { account })
    }
    pub fn replace_opencode_auth(
        _: &crate::AppHandle,
        id: &str,
        expected: Option<&str>,
        auth: &crate::opencode::Credential,
    ) -> Result<(), String> {
        crate::settings::mutate(|s| {
            let a = s
                .accounts
                .iter_mut()
                .find(|a| a.id == id)
                .ok_or("not_found")?;
            if a.opencode_auth.as_deref() == expected {
                a.opencode_auth = Some(auth.seal()?);
            }
            Ok(())
        })
    }
    pub fn finish(
        _: &crate::AppHandle,
        id: &str,
        status: serde_json::Value,
    ) -> Result<Account, String> {
        crate::settings::mutate(|s| {
            let a = s
                .accounts
                .iter_mut()
                .find(|a| a.id == id)
                .ok_or("not_found")?;
            a.status = Some(status);
            Ok(a.clone())
        })
    }
}
#[path = "../../src-tauri/src/http.rs"]
mod http;
#[path = "../../src-tauri/src/local_crypto.rs"]
mod local_crypto;
#[path = "../../src-tauri/src/model_match.rs"]
mod model_match;
#[path = "../../src-tauri/src/opencode.rs"]
mod opencode;
#[path = "../../src-tauri/src/opencode_local.rs"]
mod opencode_local;
#[path = "../../src-tauri/src/opencode_store.rs"]
mod opencode_store;
#[path = "../../src-tauri/src/operations.rs"]
mod operations;
#[path = "../../src-tauri/src/pricing.rs"]
mod pricing;
#[path = "../../src-tauri/src/proxy.rs"]
mod proxy;

#[cfg(test)]
mod flows;

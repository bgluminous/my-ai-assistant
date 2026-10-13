//! Focused tests include production sources unchanged. Only desktop IPC, audit output
//! and user settings persistence are replaced. No GUI or production data is opened.
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
    use std::{future::Future, sync::OnceLock};
    fn runtime() -> &'static tokio::runtime::Runtime {
        static R: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        R.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
        })
    }
    pub fn block_on<F: Future>(future: F) -> F::Output {
        runtime().block_on(future)
    }
    pub fn spawn<F: Future + Send + 'static>(future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F::Output: Send + 'static,
    {
        runtime().spawn(future)
    }
    pub fn spawn_blocking<F: FnOnce() -> R + Send + 'static, R: Send + 'static>(
        f: F,
    ) -> tokio::task::JoinHandle<R> {
        runtime().spawn_blocking(f)
    }
}
mod audit {
    pub fn log(_: &str, _: String, _: Option<serde_json::Value>) {}
}
mod settings {
    use std::sync::{OnceLock, RwLock};
    #[derive(Clone, Default, serde::Deserialize)]
    #[serde(default, rename_all = "camelCase")]
    pub struct Settings {
        pub codex_usage_sync: crate::codex_sync::SyncConfig,
        pub interval_minutes: u32,
        pub pricing: crate::pricing::PricingTable,
        pub pricing_remote: crate::pricing::PricingRemote,
        pub proxy: crate::proxy::ProxyConfig,
    }
    fn cell() -> &'static RwLock<Settings> {
        static S: OnceLock<RwLock<Settings>> = OnceLock::new();
        S.get_or_init(|| RwLock::new(Settings::default()))
    }
    pub fn ensure_loaded() -> Result<(), String> {
        Ok(())
    }
    pub fn path_display() -> String {
        "headless-test-memory".into()
    }
    pub fn read<T>(f: impl FnOnce(&Settings) -> T) -> Result<T, String> {
        Ok(f(&cell().read().unwrap()))
    }
    pub fn mutate<T>(f: impl FnOnce(&mut Settings) -> Result<T, String>) -> Result<T, String> {
        f(&mut cell().write().unwrap())
    }
}
#[path = "../../src-tauri/src/codex.rs"]
mod codex;
#[path = "../../src-tauri/src/codex_store.rs"]
mod codex_store;
#[path = "../../src-tauri/src/codex_sync.rs"]
mod codex_sync;
#[path = "../../src-tauri/src/http.rs"]
mod http;
#[path = "../../src-tauri/src/local_crypto.rs"]
mod local_crypto;
#[path = "../../src-tauri/src/model_match.rs"]
mod model_match;
#[path = "../../src-tauri/src/operations.rs"]
mod operations;
#[path = "../../src-tauri/src/paths.rs"]
mod paths;
#[path = "../../src-tauri/src/pricing.rs"]
mod pricing;
#[path = "../../src-tauri/src/proxy.rs"]
mod proxy;
#[path = "../../src-tauri/src/session_scan.rs"]
mod session_scan;

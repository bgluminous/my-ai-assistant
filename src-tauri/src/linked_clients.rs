//! One native operation per provider, across both windows. Network refreshes use the
//! existing provider queue; interactive binding releases that queue between steps.
use crate::{
    accounts, claude_desktop, claude_local, codex_local, local_client as common, opencode_local,
};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    sync::{Mutex, OnceLock},
};
use tauri::AppHandle;

fn active() -> &'static Mutex<HashSet<String>> {
    static ACTIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashSet::new()))
}
pub struct Lease(String);
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut active) = active().lock() {
            active.remove(&self.0);
        }
    }
}
pub fn lease(kind: &str) -> Result<Lease, String> {
    if !active()
        .lock()
        .map_err(|_| "state_lock_poisoned")?
        .insert(kind.into())
    {
        return Err("client_operation_busy".into());
    }
    Ok(Lease(kind.into()))
}

#[tauri::command]
pub async fn local_clients_status(kind: String, id: Option<String>) -> Result<Value, String> {
    let _operation = crate::operations::begin()?;
    crate::settings::ensure_loaded()?;
    tauri::async_runtime::spawn_blocking(move || status(&kind, id.as_deref()))
        .await
        .map_err(|_| "client_status_failed")?
}
pub fn status(kind: &str, id: Option<&str>) -> Result<Value, String> {
    match kind {
        "codex" => codex_local::linked_status(),
        "claude" => {
            let mut status = claude_local::linked_status();
            if status["desktop"]["installed"] == true {
                match claude_desktop::binding_status(id) {
                    Ok(binding) => {
                        status["desktop"]["binding"] = binding;
                    }
                    Err(error) => {
                        status["desktop"]["switchable"] = json!(false);
                        status["desktop"]["error"] = json!(error);
                    }
                }
            }
            Ok(status)
        }
        "opencode" => opencode_local::linked_status(),
        _ => Err("unsupported_client".into()),
    }
}

#[tauri::command]
pub async fn local_clients_switch(
    app: AppHandle,
    kind: String,
    id: String,
    close_desktop: bool,
    force: Option<bool>,
    target: Option<common::SwitchTarget>,
) -> Result<Value, String> {
    let _operation = crate::operations::begin()?;
    let _lease = lease(&kind)?;
    let _queue = accounts::refresh_queue(&kind)?.lock().await;
    let account = accounts::account_snapshot(&id)?;
    if account.kind != kind {
        return Err("account_kind_mismatch".into());
    }
    if kind == "opencode" {
        return opencode_local::switch_linked(id, close_desktop, target).await;
    }
    let st = status(&kind, Some(&id))?;
    let (cli,desktop)=common::selected_clients(target,st["cli"]["installed"]==true,st["desktop"]["installed"]==true)?;
    let running = desktop && st["desktop"]["running"] == true;
    for (endpoint, selected) in [("cli",cli),("desktop",desktop)] {
        if selected { if let Some(error)=st[endpoint]["error"].as_str() { return Err(error.into()); } }
    }
    if kind == "claude" && desktop {
        claude_desktop::ensure_idle()?;
        claude_desktop::require_binding(&id)?;
    }
    if running && !close_desktop {
        return Err("client_close_required".into());
    }
    if running {
        let k = kind.clone();
        let closed = tauri::async_runtime::spawn_blocking(move || match k.as_str() {
            "codex" => codex_local::close_desktop(),
            "claude" => claude_local::close_desktop(),
            _ => false,
        })
        .await
        .map_err(|_| "client_close_failed")?;
        if !closed {
            return Err("client_close_failed".into());
        }
    }
    let mut clients = Vec::new();
    let mut exchanged = None;
    if kind == "codex" {
        // Both clients use this same native store. A failed Desktop launch rolls
        // the shared target back; never claim the CLI still has the new login.
        let previous = match crate::codex_auth_store::read() {
            Ok(value) => value,
            Err(error) => {
                if running {
                    let _ = codex_local::launch_desktop();
                }
                return Err(error);
            }
        };
        let written = if force.unwrap_or(false) {
            codex_local::force_credentials(id.clone(), desktop).await
        } else {
            codex_local::switch_credentials(app.clone(), id.clone(), desktop).await
        };
        if let Ok(result) = &written {
            exchanged = result.exchanged;
        }
        let mut result = written.map(|_| ());
        if desktop {
            let launch = if result.is_ok() || running {
                codex_local::launch_desktop()
            } else {
                false
            };
            if result.is_ok() && !launch {
                let restored = match previous {
                    Some(value) => crate::codex_auth_store::write(&value),
                    None => crate::codex_auth_store::clear(),
                };
                result = Err(if restored.is_ok() {
                    "client_launch_failed_rolled_back"
                } else {
                    "codex_auth_rollback_failed"
                }
                .into());
                if restored.is_ok() && running {
                    let _ = codex_local::launch_desktop();
                }
            }
        }
        if cli {
            clients.push(common::client_result("cli", result.clone()));
        }
        if desktop {
            clients.push(common::client_result("desktop", result));
        }
    } else if kind == "claude" {
        if cli {
            let result = claude_local::switch_credentials(app.clone(), id.clone())
                .await
                .map(|_| ());
            clients.push(common::client_result("cli", result));
        }
        if desktop {
            let target = id.clone();
            let result = tauri::async_runtime::spawn_blocking(move || {
                claude_desktop::switch_bound(&target, running)
            })
            .await
            .map_err(|_| "claude_desktop_switch_failed".to_string())
            .and_then(|v| v);
            clients.push(common::client_result("desktop", result));
        }
    } else {
        return Err("unsupported_client".into());
    }
    crate::audit::log(
        "client_switch_result",
        "本机客户端切换结果".into(),
        Some(json!({"id":id,"kind":kind,"target":target,"clients":clients})),
    );
    Ok(common::switch_result(clients, exchanged))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operation_lease_is_per_provider_and_released_after_failure() {
        let one = lease("fixture-one").unwrap();
        assert!(lease("fixture-one").is_err());
        let two = lease("fixture-two").unwrap();
        drop(one);
        assert!(lease("fixture-one").is_ok());
        drop(two);
    }
    #[test]
    fn partial_results_are_never_reported_as_success() {
        let result = common::switch_result(
            vec![
                common::client_result("cli", Ok(())),
                common::client_result("desktop", Err("client_launch_failed".into())),
            ],
            None,
        );
        assert_eq!(result["switched"], false);
        assert_eq!(result["clients"][0]["status"], "success");
        assert_eq!(result["clients"][1]["status"], "failed");
    }
}

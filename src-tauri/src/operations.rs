//! 安装更新时关闭业务入口，并等已经开始的操作（含异步写盘）结束。
use std::sync::{atomic::{AtomicBool, Ordering}, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Gate { active: usize, paused: bool }
static GATE: Coordinator = Coordinator::new();
static REQUIRED: AtomicBool = AtomicBool::new(false);

struct Coordinator { state: Mutex<Gate> }
pub struct Operation<'a>(&'a Coordinator);
impl Drop for Operation<'_> {
    fn drop(&mut self) {
        let mut gate = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        gate.active -= 1;
    }
}

pub struct Pause<'a>(&'a Coordinator);
impl Drop for Pause<'_> {
    fn drop(&mut self) { self.0.state.lock().unwrap_or_else(|e| e.into_inner()).paused = false; }
}
impl Coordinator {
    const fn new() -> Self { Self { state: Mutex::new(Gate { active: 0, paused: false }) } }
    fn begin(&self) -> Result<Operation<'_>, String> {
        let mut gate = self.state.lock().map_err(|_| "operation_lock_failed")?;
        if gate.paused { return Err("update_in_progress".into()); }
        gate.active += 1;
        Ok(Operation(self))
    }
    async fn pause(&self, timeout: Duration) -> Result<Pause<'_>, String> {
        {
            let mut gate = self.state.lock().map_err(|_| "operation_lock_failed")?;
            if gate.paused { return Err("update_in_progress".into()); }
            gate.paused = true;
        }
        let pause = Pause(self);
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.state.lock().map_err(|_| "operation_lock_failed")?.active == 0 { return Ok(pause); }
            if tokio::time::Instant::now() >= deadline { return Err("update_busy".into()); }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}
pub fn begin() -> Result<Operation<'static>, String> { GATE.begin() }
pub async fn pause(timeout: Duration) -> Result<Pause<'static>, String> { GATE.pause(timeout).await }
pub fn require_update(required: bool) { REQUIRED.store(required, Ordering::SeqCst); }
pub fn required() -> bool { REQUIRED.load(Ordering::SeqCst) }
pub fn paused() -> bool { GATE.state.lock().map(|g| g.paused).unwrap_or(true) }

/// 更新、代理与备份导出是恢复入口；不开放通用的 updater 插件 IPC 权限。
pub fn rejection(command: &str) -> Option<&'static str> {
    rejection_for(command, paused(), required())
}
fn rejection_for(command: &str, paused: bool, required: bool) -> Option<&'static str> {
    if command.starts_with("app_update_") || matches!(command, "app_info" | "launch_info" | "tray_open_main") {
        return None;
    }
    if paused { return Some("update_in_progress"); }
    if required && !matches!(command, "proxy_get" | "proxy_set" | "proxy_test" | "backup_export" | "audit_list") {
        return Some("update_required");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn pause_waits_for_work_and_reopens_after_timeout_or_drop() {
        let gate = Coordinator::new();
        let work = gate.begin().unwrap();
        assert!(gate.pause(Duration::from_millis(1)).await.is_err());
        assert!(!gate.state.lock().unwrap().paused);
        drop(work);
        let p = gate.pause(Duration::from_secs(1)).await.unwrap();
        assert!(gate.begin().is_err());
        drop(p);
        assert!(gate.begin().is_ok());
        assert_eq!(rejection_for("accounts_add", false, true), Some("update_required"));
        assert_eq!(rejection_for("proxy_set", false, true), None);
        assert_eq!(rejection_for("app_update_check", true, true), None);
    }
    #[tokio::test]
    async fn cancelled_wait_drops_pause_and_never_overlaps_a_write() {
        let gate = Coordinator::new();
        let work = gate.begin().unwrap();
        tokio::select! {
            _ = gate.pause(Duration::from_secs(2)) => panic!("must wait for write"),
            _ = tokio::time::sleep(Duration::from_millis(5)) => {},
        }
        assert!(gate.begin().is_ok());
        drop(work);
        let pause = gate.pause(Duration::from_secs(1)).await.unwrap();
        assert!(gate.begin().is_err());
        drop(pause);
        assert!(gate.begin().is_ok());
    }
}

//! 开机自启动与启动上下文。
//!
//! 自启动开关注册在系统里（Windows 注册表 Run 项 / macOS LaunchAgent /
//! Linux autostart desktop 文件），由 tauri-plugin-autostart 托管，注册时携带
//! `--autostart` 参数；「静默启动」偏好存 settings.json（autostartSilent）。
//! 进程以 `--autostart` 拉起且勾选了静默启动时，主窗口保持隐藏（仅托盘运行）：
//! 前端首帧通过 launch_info 查询后决定是否 show()，lib.rs 的超时兜底同样跳过。

use serde::Serialize;
use std::sync::OnceLock;
use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;

use crate::{audit, settings};

/// 本次进程是否按「静默启动」运行（启动时判定一次，之后只读）。
static SILENT_LAUNCH: OnceLock<bool> = OnceLock::new();

/// 启动时由 lib.rs 在设置载入后调用一次；重复调用忽略。
pub fn set_silent_launch(value: bool) {
    let _ = SILENT_LAUNCH.set(value);
}

pub fn is_silent_launch() -> bool {
    SILENT_LAUNCH.get().copied().unwrap_or(false)
}

/// 命令行带 --autostart 即视为开机自启动拉起（注册自启动项时写入的参数）。
pub fn launched_by_autostart() -> bool {
    std::env::args().any(|a| a == "--autostart")
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutostartView {
    pub enabled: bool,
    pub silent: bool,
}

fn current_view(app: &AppHandle) -> Result<AutostartView, String> {
    let enabled = app.autolaunch().is_enabled().map_err(|e| e.to_string())?;
    let silent = settings::read(|s| s.autostart_silent)?;
    Ok(AutostartView { enabled, silent })
}

#[tauri::command]
pub fn autostart_get(app: AppHandle) -> Result<AutostartView, String> {
    settings::ensure_loaded()?;
    current_view(&app)
}

/// 设置开机自启动与静默启动。开关只在状态变化时写系统（disable 不存在的注册项会报错）。
#[tauri::command]
pub fn autostart_set(app: AppHandle, enabled: bool, silent: bool) -> Result<AutostartView, String> {
    settings::ensure_loaded()?;
    let manager = app.autolaunch();
    let currently = manager.is_enabled().map_err(|e| e.to_string())?;
    if enabled && !currently {
        manager.enable().map_err(|e| e.to_string())?;
    } else if !enabled && currently {
        manager.disable().map_err(|e| e.to_string())?;
    }
    let silent_changed = settings::mutate(|s| {
        let changed = s.autostart_silent != silent;
        s.autostart_silent = silent;
        Ok(changed)
    })?;
    if enabled != currently || silent_changed {
        let message = if enabled {
            if silent {
                "开启开机自启动（静默启动，仅托盘运行）".to_string()
            } else {
                "开启开机自启动".to_string()
            }
        } else {
            "关闭开机自启动".to_string()
        };
        audit::log(&app, "autostart_set", message, None);
    }
    current_view(&app)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchInfo {
    /// 本次启动是否应保持主窗口隐藏（开机自启动 + 静默启动）。
    pub silent_start: bool,
}

/// 前端首帧查询：静默启动时跳过主窗口 show()。
#[tauri::command]
pub fn launch_info() -> LaunchInfo {
    LaunchInfo {
        silent_start: is_silent_launch(),
    }
}

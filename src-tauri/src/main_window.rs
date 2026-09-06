//! 主窗口的唤出与位置复位。
//!
//! 主窗口点关闭只是隐藏到托盘，也可能被最小化到任务栏。再次打开时要回到本次启动
//! 首次显示的位置：首次以可见状态获得焦点时记录外框位置，之后每次唤出（托盘双击 /
//! 托盘菜单「显示主窗口」/ 面板「打开主窗口」/ 重复启动）或从任务栏还原都复位到该位置。
//! 只复位位置不改尺寸；用户拖动后只要不隐藏 / 最小化，位置不受影响。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow, Window, WindowEvent};

pub const LABEL: &str = "main";

/// 首次显示时的外框位置（物理像素），本次进程只记录一次。
static INITIAL_POS: OnceLock<PhysicalPosition<i32>> = OnceLock::new();
/// 是否处于最小化：Resized 事件里由 true 变 false 即「从任务栏还原」的瞬间。
static MINIMIZED: AtomicBool = AtomicBool::new(false);

/// 唤出主窗口：解除最小化、复位到首次显示的位置、显示并聚焦。
/// 从隐藏状态（关闭到托盘）重新显示时广播 main-window-shown，前端据此把统计页时间范围回到今天。
pub fn show(app: &AppHandle) {
    let Some(win) = app.get_webview_window(LABEL) else {
        return;
    };
    let was_hidden = !win.is_visible().unwrap_or(true);
    let _ = win.unminimize();
    restore_position(&win);
    let _ = win.show();
    let _ = win.set_focus();
    if was_hidden {
        let _ = app.emit("main-window-shown", ());
    }
}

/// 主窗口事件钩子（lib.rs 的 on_window_event 转发）。
pub fn on_event(window: &Window, event: &WindowEvent) {
    match event {
        WindowEvent::Focused(true) => remember_initial(window),
        WindowEvent::Resized(_) => {
            if window.is_minimized().unwrap_or(false) {
                MINIMIZED.store(true, Ordering::Relaxed);
            } else if MINIMIZED.swap(false, Ordering::Relaxed) {
                if let Some(win) = window.get_webview_window(LABEL) {
                    restore_position(&win);
                }
            }
        }
        _ => {}
    }
}

/// 首次以可见、非最小化状态获得焦点时记录位置。
/// 静默启动时窗口一直隐藏，直到用户首次从托盘唤出才记录。
fn remember_initial(window: &Window) {
    if INITIAL_POS.get().is_some() {
        return;
    }
    if !window.is_visible().unwrap_or(false) || window.is_minimized().unwrap_or(false) {
        return;
    }
    if let Ok(pos) = window.outer_position() {
        let _ = INITIAL_POS.set(pos);
    }
}

/// 复位到首次显示的位置。最大化时跳过（移动会解除最大化）；
/// 记录的位置已不在任何显示器上（如外接屏拔掉）也跳过，避免把窗口挪到屏幕外。
fn restore_position(win: &WebviewWindow) {
    let Some(pos) = INITIAL_POS.get() else {
        return;
    };
    if win.is_maximized().unwrap_or(false) {
        return;
    }
    let on_screen = win
        .available_monitors()
        .map(|monitors| {
            monitors.iter().any(|m| {
                let (p, s) = (m.position(), m.size());
                (p.x..p.x + s.width as i32).contains(&pos.x)
                    && (p.y..p.y + s.height as i32).contains(&pos.y)
            })
        })
        .unwrap_or(true);
    if on_screen {
        let _ = win.set_position(*pos);
    }
}

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

use crate::main_window;

/// 单击延迟：等过这段时间仍无第二次点击才弹出面板，避免双击第一下先闪小窗。
const SINGLE_CLICK_DELAY_MS: u64 = 320;
/// 两次单击间隔小于此值视为双击（macOS 不发 DoubleClick，靠两次 Click 配对）。
const DOUBLE_CLICK_MS: i64 = 320;
/// Windows 在 DoubleClick 之后还会再发一次 Click Up，这段时间内忽略残余点击。
const DOUBLE_CLICK_LEFTOVER_MS: i64 = 400;

/// 托盘面板最近一次因失焦隐藏的毫秒时间戳。
/// 面板打开时点击托盘图标：焦点丢失会先隐藏面板，紧随其后的 Click 事件若不加
/// 判断会立刻重新打开；把 300ms 内的点击视为「想关闭」，忽略即可实现开关切换。
static PANEL_HIDDEN_AT: AtomicI64 = AtomicI64::new(0);

/// 单击/双击序号：定时器到期时若序号已变则放弃，避免双击后还弹出面板。
static CLICK_SEQ: AtomicI64 = AtomicI64::new(0);
/// 最近一次左键单击（Up）的毫秒时间戳，用于把两次 Click 配对成双击。
static LAST_CLICK_AT: AtomicI64 = AtomicI64::new(0);
/// 最近一次判定为双击的毫秒时间戳。
static LAST_DOUBLE_AT: AtomicI64 = AtomicI64::new(0);

pub fn note_panel_hidden() {
    PANEL_HIDDEN_AT.store(now_ms(), Ordering::Relaxed);
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn bump_seq() -> i64 {
    CLICK_SEQ.fetch_add(1, Ordering::Relaxed) + 1
}

/// 切换托盘账户面板：在托盘点击位置上方弹出，再次点击（或失焦）关闭。
fn toggle_panel(app: &AppHandle, x: f64, y: f64) {
    let Some(win) = app.get_webview_window("tray") else {
        return;
    };
    if win.is_visible().unwrap_or(false) {
        let _ = win.hide();
        return;
    }
    if now_ms() - PANEL_HIDDEN_AT.load(Ordering::Relaxed) < 300 {
        return;
    }
    let size = win
        .outer_size()
        .unwrap_or_else(|_| tauri::PhysicalSize::new(350, 520));
    let px = (x - f64::from(size.width)).max(0.0);
    let py = (y - f64::from(size.height) - 12.0).max(0.0);
    let _ = win.set_position(tauri::PhysicalPosition::new(px as i32, py as i32));
    let _ = win.show();
    let _ = win.set_focus();
}

fn hide_panel_and_show_main(app: &AppHandle) {
    if let Some(panel) = app.get_webview_window("tray") {
        let _ = panel.hide();
    }
    main_window::show(app);
}

/// 双击托盘：作废待处理单击，隐藏面板并唤起主窗口。
fn open_main_from_tray(app: &AppHandle) {
    LAST_DOUBLE_AT.store(now_ms(), Ordering::Relaxed);
    LAST_CLICK_AT.store(0, Ordering::Relaxed);
    let _ = bump_seq();
    hide_panel_and_show_main(app);
}

/// 左键抬起：短间隔内的第二次点击视为双击；否则延迟后再切换面板。
fn on_left_click(app: AppHandle, x: f64, y: f64) {
    let now = now_ms();
    if now - LAST_DOUBLE_AT.load(Ordering::Relaxed) < DOUBLE_CLICK_LEFTOVER_MS {
        return;
    }
    let last_click = LAST_CLICK_AT.load(Ordering::Relaxed);
    if last_click != 0 && now - last_click < DOUBLE_CLICK_MS {
        open_main_from_tray(&app);
        return;
    }
    LAST_CLICK_AT.store(now, Ordering::Relaxed);
    let seq = bump_seq();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(SINGLE_CLICK_DELAY_MS)).await;
        if CLICK_SEQ.load(Ordering::Relaxed) != seq {
            return;
        }
        toggle_panel(&app, x, y);
    });
}

/// 托盘面板「打开主窗口」：隐藏面板并唤起主窗口。
#[tauri::command]
pub fn tray_open_main(app: AppHandle) {
    hide_panel_and_show_main(&app);
}

/// 创建系统托盘：左键单击弹出账户面板，双击打开主窗口；右键菜单提供「显示主窗口 / 退出」。
/// 窗口关闭按钮只是隐藏到托盘（见 lib.rs），从托盘「退出」才真正结束进程。
pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("AI 助手 · 用量与费用")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => main_window::show(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                position,
                ..
            } => on_left_click(tray.app_handle().clone(), position.x, position.y),
            TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => open_main_from_tray(tray.app_handle()),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

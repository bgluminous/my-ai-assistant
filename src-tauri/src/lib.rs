mod accounts;
mod audit;
mod codex;
mod codex_local;
mod cursor;
mod cursor_local;
mod http;
mod model_match;
mod paths;
mod pricing;
mod process;
mod proxy;
mod settings;
mod tray;

use serde::Serialize;
use tauri::Manager;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    version: String,
    authors: String,
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        authors: env!("CARGO_PKG_AUTHORS").into(),
    }
}

pub fn run() {
    tauri::Builder::default()
        // 单实例：重复启动不再新开进程（多实例会并发读写 settings.json，
        // 撞上写入瞬间的实例会以空数据运行），改为唤出并聚焦已有主窗口。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.unminimize();
                let _ = win.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            settings::load(app.handle());
            tray::setup(app.handle())?;
            // 最小窗口尺寸运行时兜底（与 tauri.conf.json 的 minWidth/minHeight 一致），
            // 防止个别环境下窗口配置未生效导致界面被压得过小。
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_min_size(Some(tauri::LogicalSize::new(1180.0, 640.0)));
                // 主窗口以隐藏状态创建（消除启动白闪），正常由前端首帧渲染后调用 show()；
                // 若前端初始化异常没能显示，这里兜底拉起，避免窗口永远不可见。
                let win = win.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    if !win.is_visible().unwrap_or(true) {
                        let _ = win.show();
                    }
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            match event {
                // 关闭按钮 = 缩到托盘继续后台运行（定时刷新不中断），托盘菜单「退出」才结束进程
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    let _ = window.hide();
                    api.prevent_close();
                }
                // 托盘账户面板失焦自动隐藏
                tauri::WindowEvent::Focused(false) if window.label() == "tray" => {
                    let _ = window.hide();
                    tray::note_panel_hidden();
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            cursor::cursor_aggregate,
            cursor_local::cursor_switch_local,
            cursor_local::cursor_client_get,
            cursor_local::cursor_client_set,
            cursor_local::cursor_client_scan_step,
            cursor_local::cursor_client_scan_cancel,
            cursor_local::cursor_client_status,
            cursor_local::cursor_client_close,
            cursor_local::cursor_client_launch,
            codex::codex_scan_sessions,
            codex_local::codex_switch_local,
            codex_local::codex_client_get,
            codex_local::codex_client_set,
            codex_local::codex_client_detect,
            codex_local::codex_client_status,
            codex_local::codex_client_close,
            codex_local::codex_client_launch,
            pricing::pricing_get,
            pricing::pricing_save,
            pricing::pricing_reset,
            pricing::pricing_update_check,
            pricing::pricing_update_apply,
            proxy::proxy_get,
            proxy::proxy_set,
            proxy::proxy_test,
            accounts::accounts_list,
            accounts::accounts_add,
            accounts::accounts_update,
            accounts::accounts_delete,
            accounts::accounts_import_local,
            accounts::accounts_export,
            accounts::accounts_import_file,
            accounts::accounts_set_interval,
            accounts::account_refresh,
            audit::audit_list,
            audit::audit_clear,
            tray::tray_open_main,
            app_info,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

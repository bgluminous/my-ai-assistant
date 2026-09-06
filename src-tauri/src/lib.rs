mod accounts;
mod audit;
mod backup;
mod claude;
mod claude_local;
mod claude_oauth;
mod codex;
mod codex_local;
mod cursor;
mod cursor_local;
mod http;
mod launch;
mod local_client;
mod main_window;
mod model_match;
mod paths;
mod pricing;
mod process;
mod proxy;
mod session_scan;
mod settings;
mod tray;
mod usage_archive;

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
    // 是否由开机自启动拉起（注册自启动项时写入 --autostart 参数）
    let autostart_launch = launch::launched_by_autostart();
    tauri::Builder::default()
        // 单实例：重复启动不再新开进程（多实例会并发读写 settings.json，
        // 撞上写入瞬间的实例会以空数据运行），改为唤出并聚焦已有主窗口。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            main_window::show(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
        .setup(move |app| {
            // 【过渡期临时代码，到期删除】旧用户目录（xilore/ 无点号）迁到 .xilore/，
            // 必须先于 settings::load 与任何 audit::log（它们会创建新目录，导致迁移被跳过）
            let migration = paths::migrate_legacy_app_dir();
            settings::load();
            match migration {
                Ok(Some(m)) => {
                    let leftover = m
                        .leftover_error
                        .as_deref()
                        .map(|e| format!("；旧目录删除失败，请手动清理：{e}"))
                        .unwrap_or_default();
                    audit::log(
                        "data_dir_migrated",
                        format!(
                            "用户数据目录已从 {} 迁移到 {}（{}）{leftover}",
                            m.from.display(),
                            m.to.display(),
                            if m.method == "rename" { "整体移动" } else { "复制后删除旧目录" }
                        ),
                        Some(serde_json::json!({
                            "from": m.from.to_string_lossy(),
                            "to": m.to.to_string_lossy(),
                            "method": m.method,
                            "leftoverError": m.leftover_error,
                        })),
                    );
                }
                Ok(None) => {}
                Err(e) => audit::log(
                    "data_dir_migrate_failed",
                    format!("旧用户数据目录迁移失败（{e}），本次以新目录空数据运行；旧数据仍在原目录，可手动移动到新目录后重启"),
                    None,
                ),
            }
            // 静默启动 = 开机自启动拉起 + 设置勾选静默；需在设置载入后判定
            let silent = autostart_launch && settings::read(|s| s.autostart_silent).unwrap_or(false);
            launch::set_silent_launch(silent);
            tray::setup(app.handle())?;
            // 最小窗口尺寸运行时兜底（与 tauri.conf.json 的 minWidth/minHeight 一致），
            // 防止个别环境下窗口配置未生效导致界面被压得过小。
            if let Some(win) = app.get_webview_window(main_window::LABEL) {
                let _ = win.set_min_size(Some(tauri::LogicalSize::new(1180.0, 640.0)));
                // 主窗口以隐藏状态创建（消除启动白闪），正常由前端首帧渲染后调用 show()；
                // 若前端初始化异常没能显示，这里兜底拉起，避免窗口永远不可见。
                // 静默启动（开机自启动 + 仅托盘）时不兜底，保持隐藏。
                if !silent {
                    let win = win.clone();
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        if !win.is_visible().unwrap_or(true) {
                            let _ = win.show();
                        }
                    });
                }
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
                // 主窗口：记录首次显示位置，从任务栏还原时复位
                _ if window.label() == main_window::LABEL => main_window::on_event(window, event),
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            usage_archive::cursor_usage_fetch,
            usage_archive::cursor_usage_slice,
            usage_archive::cursor_usage_deleted_list,
            usage_archive::cursor_usage_deleted_remove,
            usage_archive::cursor_usage_events,
            usage_archive::cursor_usage_clear,
            usage_archive::usage_events_export,
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
            claude::claude_scan_sessions,
            claude_local::claude_switch_local,
            claude_local::claude_client_get,
            claude_local::claude_client_set,
            claude_local::claude_client_detect,
            claude_local::claude_client_status,
            claude_local::claude_client_close,
            claude_local::claude_client_launch,
            claude_oauth::claude_oauth_begin,
            accounts::claude_oauth_finish,
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
            usage_archive::usage_snapshot_save,
            tray::tray_open_main,
            launch::autostart_get,
            launch::autostart_set,
            launch::launch_info,
            backup::backup_export,
            backup::backup_import_inspect,
            backup::backup_import_apply,
            app_info,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

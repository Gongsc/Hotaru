mod commands;
mod models;
mod monitor;
mod settings;
mod state;
mod tray;
mod window_geometry;
mod windows;

use tauri::{Manager, WindowEvent};

use state::AppState;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            windows::open_settings(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let handle = app.handle().clone();
            app.manage(state::init(&handle));
            app.manage(window_geometry::init(&handle));
            let theme = handle.state::<AppState>().settings.read().theme;
            windows::sync_theme(&handle, theme);
            tray::create(&handle)?;
            monitor::spawn(handle.clone());
            windows::spawn_panel_watchdog_loop(&handle);
            monitor::spawn_ping_loop(handle.clone());
            // 面板必须在启动时创建:实测经 IPC/命令线程延后创建的 External
            // webview 在本机 WebView2 上会静默失败(白屏且导航不启动)。
            // 静默启动只是不显示它,窗口照旧建好。
            let (unconfigured, silent) = {
                let state = handle.state::<AppState>();
                let s = state.settings.read();
                (s.backend_url.trim().is_empty(), s.silent_start)
            };
            if unconfigured {
                windows::open_settings(&handle);
            } else if silent {
                windows::preload_panel_hidden(&handle);
            } else {
                windows::open_panel(&handle);
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
                window_geometry::remember(window);
            }
            WindowEvent::ThemeChanged(_) => {
                tray::refresh(window.app_handle());
            }
            WindowEvent::CloseRequested { api, .. } => {
                window_geometry::remember(window);
                if window.label() == "main" {
                    // Closing the panel hides it; the tray keeps running.
                    // Bump the panel epoch so pending open-watchdogs don't
                    // resurrect a window the user just closed.
                    window
                        .app_handle()
                        .state::<AppState>()
                        .panel_epoch
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    api.prevent_close();
                    let _ = window.hide();
                } else if window.label() == "chart" {
                    // Preserve the popover webview and its cached node state.
                    api.prevent_close();
                    crate::windows::close_chart(window.app_handle());
                }
            }
            WindowEvent::Focused(false) => {
                // The chart popover dismisses itself on focus loss — unless
                // the user pinned it open. The hide is delayed so a brief
                // focus flicker (the shell re-asserting after a tray click)
                // does not instantly close a freshly opened popover.
                if window.label() == "chart" {
                    let handle = window.app_handle();
                    let pinned = handle
                        .state::<AppState>()
                        .chart_pinned
                        .load(std::sync::atomic::Ordering::Relaxed);
                    if pinned || handle.state::<AppState>().settings.read().chart_keep_open {
                        return;
                    }
                    let win = window.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(250));
                        let state = win.app_handle().state::<AppState>();
                        if !state.settings.read().chart_keep_open
                            && !state
                                .chart_pinned
                                .load(std::sync::atomic::Ordering::Relaxed)
                            && win.is_visible().unwrap_or(false)
                            && !win.is_focused().unwrap_or(true)
                        {
                            *win.app_handle().state::<AppState>().chart_hidden_at.lock() =
                                Some(std::time::Instant::now());
                            crate::windows::close_chart(win.app_handle());
                        }
                    });
                }
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::get_app_info,
            commands::check_for_updates,
            commands::open_github_page,
            commands::save_settings,
            commands::get_snapshot,
            commands::resync_nodes,
            commands::get_net_history,
            commands::test_connection,
            commands::list_nodes,
            commands::get_autostart,
            commands::set_autostart,
            commands::open_panel_cmd,
            commands::open_settings_cmd,
            commands::get_chart_pinned,
            commands::set_chart_pinned,
            commands::hide_chart,
            commands::resize_chart,
            commands::get_ping_records,
        ])
        .build(tauri::generate_context!())
        .expect("error while building hotaru")
        .run(|_app, event| {
            if let tauri::RunEvent::ExitRequested { code, api, .. } = event {
                // A tray app must survive closing its last window, including
                // the gap while refresh destroys and rebuilds the panel.
                // Explicit exit/restart requests carry a code and still work.
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}

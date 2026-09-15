#[cfg(target_os = "macos")]
use tauri::window::{Effect, EffectState, EffectsBuilder};
use tauri::{
    webview::PageLoadEvent, AppHandle, Manager, PhysicalPosition, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

use crate::models::{normalize_base, ThemeMode};
use crate::state::AppState;

/// Injected into every navigation of the remote panel. Keeping the chrome in
/// the initialization script lets the panel stay on its configured external
/// origin while still giving the frameless window native-feeling controls.
const PANEL_CHROME_SCRIPT: &str = r#"
(() => {
  const TOOLBAR_ID = 'hotaru-panel-toolbar';
  if (document.getElementById(TOOLBAR_ID)) return;

  const install = () => {
    if (!document.body || document.getElementById(TOOLBAR_ID)) return;

    const originalPaddingTop = getComputedStyle(document.body).paddingTop;
    document.documentElement.style.setProperty('--hotaru-page-padding-top', originalPaddingTop);

    const css = `
      :root {
        --hotaru-titlebar-height: 42px;
        --hotaru-titlebar-bg: rgba(248, 250, 252, .94);
        --hotaru-titlebar-border: rgba(15, 23, 42, .12);
        --hotaru-titlebar-ink: #334155;
        --hotaru-titlebar-muted: #64748b;
        --hotaru-titlebar-hover: rgba(15, 23, 42, .08);
        --hotaru-titlebar-pressed: rgba(15, 23, 42, .13);
        --hotaru-titlebar-focus: #0ea5e9;
      }
      @media (prefers-color-scheme: dark) {
        :root {
          --hotaru-titlebar-bg: rgba(15, 23, 42, .94);
          --hotaru-titlebar-border: rgba(226, 232, 240, .14);
          --hotaru-titlebar-ink: #e2e8f0;
          --hotaru-titlebar-muted: #94a3b8;
          --hotaru-titlebar-hover: rgba(226, 232, 240, .10);
          --hotaru-titlebar-pressed: rgba(226, 232, 240, .16);
          --hotaru-titlebar-focus: #38bdf8;
        }
      }
      body {
        box-sizing: border-box !important;
        padding-top: calc(var(--hotaru-page-padding-top, 0px) + var(--hotaru-titlebar-height)) !important;
      }
      #${TOOLBAR_ID} {
        position: fixed;
        z-index: 2147483647;
        inset: 0 0 auto 0;
        box-sizing: border-box;
        display: flex;
        align-items: center;
        gap: 8px;
        height: var(--hotaru-titlebar-height);
        padding: 0 8px;
        color: var(--hotaru-titlebar-ink);
        background: var(--hotaru-titlebar-bg);
        border-bottom: 1px solid var(--hotaru-titlebar-border);
        box-shadow: 0 1px 4px rgba(15, 23, 42, .06);
        backdrop-filter: blur(20px) saturate(1.35);
        -webkit-backdrop-filter: blur(20px) saturate(1.35);
        font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
        user-select: none;
        -webkit-user-select: none;
      }
      #${TOOLBAR_ID} * { box-sizing: border-box; }
      #${TOOLBAR_ID} .hotaru-nav,
      #${TOOLBAR_ID} .hotaru-window-controls {
        display: flex;
        align-items: center;
        gap: 3px;
        flex: none;
      }
      #${TOOLBAR_ID} .hotaru-drag-region {
        align-self: stretch;
        flex: 1;
        min-width: 24px;
      }
      #${TOOLBAR_ID} button {
        display: inline-grid;
        place-items: center;
        width: 30px;
        height: 30px;
        margin: 0;
        padding: 0;
        color: var(--hotaru-titlebar-muted);
        background: transparent;
        border: 0;
        border-radius: 7px;
        box-shadow: none;
        cursor: default;
        appearance: none;
        -webkit-appearance: none;
      }
      #${TOOLBAR_ID} button:hover {
        color: var(--hotaru-titlebar-ink);
        background: var(--hotaru-titlebar-hover);
      }
      #${TOOLBAR_ID} button:active { background: var(--hotaru-titlebar-pressed); }
      #${TOOLBAR_ID} button:focus-visible {
        outline: 2px solid var(--hotaru-titlebar-focus);
        outline-offset: -2px;
      }
      #${TOOLBAR_ID} svg {
        width: 16px;
        height: 16px;
        pointer-events: none;
      }
      #${TOOLBAR_ID} .hotaru-window-controls { gap: 1px; }
      #${TOOLBAR_ID} .hotaru-close:hover {
        color: #fff;
        background: #e5484d;
      }
      #${TOOLBAR_ID}.hotaru-macos .hotaru-window-controls { order: 1; gap: 7px; padding: 0 4px; }
      #${TOOLBAR_ID}.hotaru-macos .hotaru-nav { order: 2; }
      #${TOOLBAR_ID}.hotaru-macos .hotaru-drag-region { order: 3; }
      #${TOOLBAR_ID}.hotaru-macos .hotaru-window-controls button {
        width: 12px;
        height: 12px;
        border-radius: 50%;
      }
      #${TOOLBAR_ID}.hotaru-macos .hotaru-window-controls svg { display: none; }
      /* The group's DOM order is the Windows one (minimise, maximise, close).
         macOS reads close → minimise → zoom from the left, so reorder there. */
      #${TOOLBAR_ID}.hotaru-macos .hotaru-close { order: 1; background: #ff5f57; }
      #${TOOLBAR_ID}.hotaru-macos .hotaru-minimize { order: 2; background: #febc2e; }
      #${TOOLBAR_ID}.hotaru-macos .hotaru-maximize { order: 3; background: #28c840; }
      #${TOOLBAR_ID}:not(.hotaru-macos) .hotaru-nav { order: 1; }
      #${TOOLBAR_ID}:not(.hotaru-macos) .hotaru-drag-region { order: 2; }
      #${TOOLBAR_ID}:not(.hotaru-macos) .hotaru-window-controls { order: 3; }
      @media (prefers-reduced-motion: no-preference) {
        #${TOOLBAR_ID} button { transition: color 120ms ease, background-color 120ms ease; }
      }
    `;

    try {
      const sheet = new CSSStyleSheet();
      sheet.replaceSync(css);
      document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
    } catch (_) {
      const style = document.createElement('style');
      style.textContent = css;
      (document.head || document.documentElement).appendChild(style);
    }

    const icon = (path) => `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="${path}"/></svg>`;
    const button = (className, label, path) => `<button type="button" class="${className}" aria-label="${label}" title="${label}">${icon(path)}</button>`;
    const toolbar = document.createElement('header');
    toolbar.id = TOOLBAR_ID;
    toolbar.className = '__HOTARU_PLATFORM__';
    toolbar.setAttribute('role', 'toolbar');
    toolbar.setAttribute('aria-label', '面板导航');
    toolbar.innerHTML = `
      <nav class="hotaru-nav" aria-label="页面导航">
        ${button('hotaru-back', '后退', 'M15 18l-6-6 6-6')}
        ${button('hotaru-forward', '前进', 'M9 18l6-6-6-6')}
        ${button('hotaru-reload', '刷新', 'M20 11a8.1 8.1 0 10-2.2 5.5M20 4v7h-7')}
      </nav>
      <div class="hotaru-drag-region" data-tauri-drag-region="deep" aria-hidden="true"></div>
      <div class="hotaru-window-controls" aria-label="窗口控制">
        ${button('hotaru-minimize', '最小化', 'M5 12h14')}
        ${button('hotaru-maximize', '最大化或还原', 'M6 6h12v12H6z')}
        ${button('hotaru-close', '关闭', 'M6 6l12 12M18 6L6 18')}
      </div>`;

    const invokeWindow = (command) => {
      const invoke = window.__TAURI_INTERNALS__?.invoke;
      if (invoke) void invoke(`plugin:window|${command}`, { label: 'main' });
    };
    toolbar.querySelector('.hotaru-back').addEventListener('click', () => history.back());
    toolbar.querySelector('.hotaru-forward').addEventListener('click', () => history.forward());
    toolbar.querySelector('.hotaru-reload').addEventListener('click', () => location.reload());
    toolbar.querySelector('.hotaru-minimize').addEventListener('click', () => invokeWindow('minimize'));
    toolbar.querySelector('.hotaru-maximize').addEventListener('click', () => invokeWindow('toggle_maximize'));
    toolbar.querySelector('.hotaru-close').addEventListener('click', () => invokeWindow('close'));
    document.body.prepend(toolbar);
  };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', install, { once: true });
  } else {
    install();
  }
})();
"#;

/// Fill in the platform class the injected chrome keys its window-control
/// layout off. Resolved from the build target rather than sniffed in the page:
/// `navigator.platform` is deprecated and a miss would silently give macOS the
/// Windows control order.
fn panel_chrome_script() -> String {
    PANEL_CHROME_SCRIPT.replace(
        "__HOTARU_PLATFORM__",
        if cfg!(target_os = "macos") {
            "hotaru-macos"
        } else {
            ""
        },
    )
}

/// Logical width of the chart popover, and the height bounds for its initial
/// size. Once the page reports its content height the screen's free space is
/// the real ceiling — [`CHART_MAX_H`] then only covers an unknown monitor.
const CHART_W: f64 = 320.0;
const CHART_MIN_H: f64 = 300.0;
const CHART_MAX_H: f64 = 900.0;
/// Vertical room left free so the popover never touches the screen edge.
const CHART_SCREEN_MARGIN: f64 = 12.0;

/// Logical size of the chart popover. Height depends on the node count so
/// the builder's initial size already matches the content (the page still
/// fine-tunes it from JS).
fn chart_logical_size(node_count: usize) -> (f64, f64) {
    let base = 224.0 + node_count as f64 * 32.0 + 46.0;
    (CHART_W, base.clamp(CHART_MIN_H, CHART_MAX_H))
}
/// Ignore tray clicks that arrive right after the popover auto-hid on blur,
/// so the same click does not instantly reopen it (toggle semantics).
const CHART_REOPEN_GUARD: std::time::Duration = std::time::Duration::from_millis(700);
/// Local settings pages normally load immediately. A longer delay on WebView2
/// indicates a stuck renderer; retry without leaving a cached white window.
const SETTINGS_LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6);
const SETTINGS_MAX_LOAD_RETRIES: u8 = 2;

fn should_retry_settings_load(retry: u8) -> bool {
    retry < SETTINGS_MAX_LOAD_RETRIES
}

fn native_theme(theme: ThemeMode) -> Option<tauri::Theme> {
    match theme {
        ThemeMode::System => None,
        ThemeMode::Light => Some(tauri::Theme::Light),
        ThemeMode::Dark => Some(tauri::Theme::Dark),
    }
}

/// Keep WebView media queries, native controls and macOS vibrancy on the
/// same appearance. On macOS this is app-wide; Windows applies it per window.
pub fn sync_theme(app: &AppHandle, theme: ThemeMode) {
    for window in app.webview_windows().values() {
        let _ = window.set_theme(native_theme(theme));
    }
}

/// Recreate the panel webview from scratch. Recover even from a crashed
/// renderer, where an in-page `location.reload()` would never run. The new
/// window is created on the main thread after a short delay so the old
/// WebView2 instance is fully torn down first.
pub fn recreate_panel(app: &AppHandle) {
    let base = app.state::<AppState>().loaded_panel_url.lock().clone();
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.destroy();
    }
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        let inner = handle.clone();
        let _ = handle.run_on_main_thread(move || {
            if let Some(base) = base {
                let _ = create_panel(&inner, &base, true);
            } else {
                open_panel(&inner);
            }
        });
    });
}

/// If the panel's external dashboard did not finish loading within this
/// window of time, reload it once (white-screen self-healing).
const PANEL_LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// A navigation that still has not finished after this long looks wedged.
const PANEL_STALL_MS: u64 = 30_000;
/// Minimum gap between two automatic heal attempts.
const PANEL_HEAL_COOLDOWN_MS: u64 = 60_000;

/// Whether the watchdog armed for one navigation should reload the panel.
/// `loaded_now == loaded_before` means that navigation never reported
/// `Finished`. A minimized or hidden panel is exempt: WebView2 throttles
/// background windows, so an unfinished load there is expected rather than
/// broken, and the user is not looking at a white page anyway.
fn panel_load_timed_out(
    visible: bool,
    minimized: bool,
    loaded_before: u64,
    loaded_now: u64,
) -> bool {
    visible && !minimized && loaded_now <= loaded_before
}

/// Whether the periodic watchdog should rebuild a wedged panel. Same exemption
/// as above, and healing a background window would additionally yank it back in
/// front of the user.
fn panel_needs_healing(
    visible: bool,
    minimized: bool,
    started_ms: u64,
    loaded_ms: u64,
    now_ms: u64,
    last_heal_ms: u64,
) -> bool {
    visible
        && !minimized
        && started_ms > loaded_ms
        && now_ms.saturating_sub(started_ms) > PANEL_STALL_MS
        && now_ms.saturating_sub(last_heal_ms) > PANEL_HEAL_COOLDOWN_MS
}

fn now_ms_u64() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Mark the panel webview as loaded (called from on_page_load).
pub fn mark_panel_loaded(app: &AppHandle) {
    let ms = now_ms_u64();
    let st = app.state::<AppState>();
    st.panel_load_ms
        .store(ms, std::sync::atomic::Ordering::Relaxed);
    st.panel_recreate_streak
        .store(0, std::sync::atomic::Ordering::Relaxed);
}

/// Spawn a watchdog for one pending navigation: if the page hasn't shown
/// itself within 6s (load-finished triggers the show), display it anyway so the
/// window can't stay invisible; if that navigation never finished, reload at
/// 20s. Only arm this where a navigation is actually pending — on a panel that
/// is merely being re-shown there is nothing to wait for, and the 20s reload
/// would fire on every open. `expect_visible` is false for a silent start,
/// where an invisible panel is the point rather than a fault to heal.
pub fn spawn_panel_watchdog(app: &AppHandle, expect_visible: bool) {
    let handle = app.clone();
    let st = handle.state::<AppState>();
    let before = st.panel_load_ms.load(std::sync::atomic::Ordering::Relaxed);
    // 面板世代计数:用户在此看门狗存活期间关闭/重新打开面板,即视为过期
    let epoch = st.panel_epoch.load(std::sync::atomic::Ordering::Relaxed);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(6));
        let Some(window) = handle.get_webview_window("main") else {
            return;
        };
        let st = handle.state::<AppState>();
        if st.panel_epoch.load(std::sync::atomic::Ordering::Relaxed) != epoch {
            return; // 用户已关闭/重新打开面板,该看门狗作废
        }
        // A minimized window is invisible on macOS but deliberately so — never
        // un-minimize it here, that is the user's state to keep.
        if expect_visible
            && !window.is_visible().unwrap_or(true)
            && !window.is_minimized().unwrap_or(false)
        {
            let _ = window.show();
            let _ = window.set_focus();
        }
        std::thread::sleep(PANEL_LOAD_TIMEOUT - std::time::Duration::from_secs(6));
        let Some(window) = handle.get_webview_window("main") else {
            return;
        };
        let st = handle.state::<AppState>();
        if st.panel_epoch.load(std::sync::atomic::Ordering::Relaxed) != epoch {
            return;
        }
        if panel_load_timed_out(
            window.is_visible().unwrap_or(false),
            window.is_minimized().unwrap_or(false),
            before,
            st.panel_load_ms.load(std::sync::atomic::Ordering::Relaxed),
        ) {
            let _ = window.eval("location.reload()");
        }
    });
}

/// Periodic panel health check: while the panel is on screen, a navigation
/// that started but never finished within 30s triggers one rebuild (with a
/// per-attempt cooldown). Runs forever from a background thread.
pub fn panel_watchdog_tick(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let st = app.state::<AppState>();
    let now = now_ms_u64();
    if !panel_needs_healing(
        window.is_visible().unwrap_or(false),
        window.is_minimized().unwrap_or(false),
        st.panel_load_started_ms
            .load(std::sync::atomic::Ordering::Relaxed),
        st.panel_load_ms.load(std::sync::atomic::Ordering::Relaxed),
        now,
        st.panel_reload_ms
            .load(std::sync::atomic::Ordering::Relaxed),
    ) {
        return;
    }
    // 熔断:连续重建 3 次仍未加载成功就停止,避免无限循环
    let streak = st
        .panel_recreate_streak
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if streak >= 3 {
        return;
    }
    st.panel_reload_ms
        .store(now, std::sync::atomic::Ordering::Relaxed);
    // 页面可能已挂死,eval 的 reload 未必能执行——直接重建面板 webview。
    recreate_panel(app);
}

/// Start the periodic watchdog thread (once, from setup).
pub fn spawn_panel_watchdog_loop(app: &AppHandle) {
    let handle = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(15));
        panel_watchdog_tick(&handle);
    });
}

/// Open (or focus) the main window showing the Komari dashboard at the
/// configured backend URL. Falls back to settings when unconfigured.
///
/// Always dispatches to the main thread: WebView2 controllers must be created
/// on the UI thread. Note that `run_on_main_thread` only *queues* the work
/// when the caller is off the main thread — callers reached from a webview must
/// therefore be `#[tauri::command(async)]`, or the window ends up being built
/// inside a WebView2 callback and hangs the process (see `commands.rs`).
pub fn open_panel(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        // 新的打开动作 = 新的面板世代,使旧的打开看门狗全部作废
        handle
            .state::<AppState>()
            .panel_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        open_panel_on_main(&handle);
    });
}

fn open_panel_on_main(app: &AppHandle) {
    let raw = app.state::<AppState>().settings.read().backend_url.clone();
    let Ok(base) = normalize_base(&raw) else {
        open_settings(app);
        return;
    };
    match app.get_webview_window("main") {
        Some(window) => {
            let mut navigated = false;
            {
                let st = app.state::<AppState>();
                let loaded = st.loaded_panel_url.lock().clone();
                if loaded.as_deref() != Some(base.as_str()) {
                    *st.loaded_panel_url.lock() = Some(base.clone());
                    // Navigate the webview without tearing down the window.
                    let js =
                        serde_json::to_string(&base).unwrap_or_else(|_| "\"about:blank\"".into());
                    let _ = window.eval(format!("window.location.replace({js})"));
                    navigated = true;
                }
            }
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
            // Only a fresh navigation has something for the watchdog to wait
            // on. Arming it when merely re-showing an already-loaded panel made
            // the page reload itself 20s after every open.
            if navigated {
                spawn_panel_watchdog(app, true);
            }
        }
        None => {
            let _ = create_panel(app, &base, true);
        }
    }
}

/// If the main window is open and the backend URL changed, point it at the
/// new URL in the background (no focus steal).
pub fn sync_panel_url(app: &AppHandle, backend_url: &str) {
    let Ok(base) = normalize_base(backend_url) else {
        return;
    };
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let st = app.state::<AppState>();
    let loaded = st.loaded_panel_url.lock().clone();
    if loaded.as_deref() != Some(base.as_str()) {
        *st.loaded_panel_url.lock() = Some(base.clone());
        let js = serde_json::to_string(&base).unwrap_or_else(|_| "\"about:blank\"".into());
        let _ = window.eval(format!("window.location.replace({js})"));
    }
}

/// Create the panel webview at launch without showing it (silent start). It is
/// still built up front rather than on first use, so opening it later is just a
/// `show` — see `create_panel` for why lazy creation is avoided.
pub fn preload_panel_hidden(app: &AppHandle) {
    let raw = app.state::<AppState>().settings.read().backend_url.clone();
    let Ok(base) = normalize_base(&raw) else {
        return;
    };
    let _ = create_panel(app, &base, false);
}

/// Build the panel window. `visible` is false for a silent start: the webview
/// is still created up front (a lazily created External webview has been
/// unreliable here) but never shown, so the app comes up in the tray only.
fn create_panel(app: &AppHandle, base: &str, visible: bool) -> tauri::Result<()> {
    let url: url::Url = base.parse().map_err(|_| tauri::Error::WindowNotFound)?;
    let theme = app.state::<AppState>().settings.read().theme;
    let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
        .title("Hotaru Panel")
        .theme(native_theme(theme))
        .decorations(false)
        .visible(false)
        .initialization_script(panel_chrome_script())
        .on_page_load(|window, payload| match payload.event() {
            PageLoadEvent::Started => {
                window
                    .app_handle()
                    .state::<AppState>()
                    .panel_load_started_ms
                    .store(now_ms_u64(), std::sync::atomic::Ordering::Relaxed);
            }
            PageLoadEvent::Finished => mark_panel_loaded(window.app_handle()),
        })
        .inner_size(1200.0, 800.0)
        .min_inner_size(780.0, 560.0)
        .build()?;
    crate::window_geometry::restore(&window);
    *app.state::<AppState>().loaded_panel_url.lock() = Some(base.to_string());
    if visible {
        let _ = window.show();
        let _ = window.set_focus();
    }
    spawn_panel_watchdog(app, visible);
    Ok(())
}

/// Open (or toggle) the chart popover anchored to the tray icon. The icon
/// rect comes from the tray click event, in physical pixels. A pinned
/// popover keeps its dragged position instead of re-anchoring. The window is
/// hidden rather than reloaded so its node data and expansion state survive
/// repeated opens.
pub fn open_chart(app: &AppHandle, icon_rect: (f64, f64, f64, f64)) {
    let (ix, iy, iw, ih) = icon_rect;
    let center_x = ix + iw / 2.0;

    let st = app.state::<AppState>();

    // The popover hides itself on blur; when the tray click caused that blur
    // (last hide < CHART_REOPEN_GUARD ago) this very click is the toggle-close.
    let just_auto_hidden = st
        .chart_hidden_at
        .lock()
        .is_some_and(|t| t.elapsed() < CHART_REOPEN_GUARD);
    if just_auto_hidden {
        return;
    }

    // Only the rows the popover will actually draw, so the builder's initial
    // height already accounts for nodes the user hid.
    let hidden = st.settings.read().hidden_nodes.clone();
    let node_count = st
        .snapshot
        .read()
        .nodes
        .iter()
        .filter(|n| !hidden.contains(&n.uuid))
        .count();
    let (cw, ch) = chart_logical_size(node_count);

    let window = match app.get_webview_window("chart") {
        Some(window) => {
            if window.is_visible().unwrap_or(false) {
                // Toggle-close without destroying the webview; reopening can
                // reuse the current node data and UI state.
                crate::window_geometry::remember(&window.as_ref().window());
                let _ = window.hide();
                crate::tray::set_popover_active(app, false);
                return;
            }
            window
        }
        None => {
            let theme = app.state::<AppState>().settings.read().theme;
            let builder =
                WebviewWindowBuilder::new(app, "chart", WebviewUrl::App("chart.html".into()))
                    .title("Hotaru")
                    .theme(native_theme(theme))
                    .inner_size(cw, ch)
                    .decorations(false);
            // WKWebView otherwise paints an opaque white surface behind the
            // HTML panel, which leaks through its rounded corners on macOS.
            #[cfg(target_os = "macos")]
            let builder = builder
                .transparent(true)
                .effects(
                    EffectsBuilder::new()
                        .effect(Effect::Popover)
                        // Focus briefly flickers while the tray click is
                        // handed to the webview. Keep the material active so
                        // AppKit does not drop and restore the blur.
                        .state(EffectState::Active)
                        .radius(13.0)
                        .build(),
                )
                .initialization_script(
                    "const setNativeVibrancy=()=>document.documentElement?.classList.add('native-vibrancy');if(document.documentElement){setNativeVibrancy()}else{document.addEventListener('DOMContentLoaded',setNativeVibrancy,{once:true})}",
                );
            #[cfg(target_os = "windows")]
            let builder = builder.initialization_script(
                "const setWindowsOpaque=()=>document.documentElement?.classList.add('windows-opaque');if(document.documentElement){setWindowsOpaque()}else{document.addEventListener('DOMContentLoaded',setWindowsOpaque,{once:true})}",
            );
            let Ok(window) = builder
                .always_on_top(st.chart_pinned.load(std::sync::atomic::Ordering::Relaxed))
                .skip_taskbar(true)
                .resizable(true)
                .min_inner_size(CHART_W, CHART_MIN_H)
                .shadow(false)
                .visible(false)
                .build()
            else {
                return;
            };
            round_window_corners(&window);
            position_chart(app, &window, center_x, iy, iy + ih, ch);
            crate::window_geometry::restore(&window);
            if let (Ok(size), Ok(scale)) = (window.inner_size(), window.scale_factor()) {
                let _ =
                    window.set_size_constraints(chart_size_constraints(size.height as f64 / scale));
            }
            window
        }
    };

    let _ = window.show();
    let _ = window.set_focus();
    crate::tray::set_popover_active(app, true);
}

/// Give the popover rounded corners on Windows, matching the macOS popover.
///
/// The window cannot simply use a rounded CSS panel there: WebView2 paints an
/// opaque surface, so the corners outside the radius would show the square
/// window behind them (which is why the page keeps `border-radius: 0` under
/// `.windows-opaque`). Asking DWM to round the frame instead makes the OS clip
/// and anti-alias the window itself, at the system corner radius. Left as-is on
/// Windows 10, where the attribute is simply not supported.
#[cfg(target_os = "windows")]
fn round_window_corners(window: &WebviewWindow) {
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUND: i32 = 2;
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attr: u32, value: *const i32, size: u32) -> i32;
    }
    let Ok(hwnd) = window.hwnd() else { return };
    let pref = DWMWCP_ROUND;
    // SAFETY: the handle is alive for the duration of the call and DWM copies
    // the value out of `pref`. An unsupported attribute only returns an error.
    unsafe {
        DwmSetWindowAttribute(
            hwnd.0 as isize,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref,
            std::mem::size_of::<i32>() as u32,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn round_window_corners(_window: &WebviewWindow) {}

/// Close the chart popover from the Rust side (blur timeout, close request,
/// or the page's own dismiss paths by way of `hide_chart`). Hiding keeps the
/// page and its cached node state alive.
///
/// Every dismissal goes through here so the menu bar item stops looking
/// selected at the same moment the popover leaves the screen.
pub fn close_chart(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("chart") {
        crate::window_geometry::remember(&window.as_ref().window());
        let _ = window.hide();
    }
    crate::tray::set_popover_active(app, false);
}

fn position_chart(
    app: &AppHandle,
    window: &WebviewWindow,
    center_x: f64,
    icon_top: f64,
    icon_bottom: f64,
    logical_h: f64,
) {
    let monitor = monitor_containing(app, center_x, icon_top);
    let Some(monitor) = monitor else { return };
    let scale = monitor.scale_factor();
    let (w, h) = (320.0 * scale, logical_h * scale);
    let gap = 6.0 * scale;

    let mp = monitor.position();
    let ms = monitor.size();
    let (mx, my, mw, mh) = (mp.x as f64, mp.y as f64, ms.width as f64, ms.height as f64);

    let x = (center_x - w / 2.0).clamp(mx, (mx + mw - w).max(mx));
    // Icons near the top of the screen (macOS menu bar) pop below; icons near
    // the bottom (Windows taskbar) pop above.
    let below = icon_bottom < my + mh / 2.0;
    app.state::<AppState>()
        .chart_below
        .store(below, std::sync::atomic::Ordering::Relaxed);
    let y = if below {
        icon_bottom + gap
    } else {
        icon_top - gap - h
    };
    let y = y.clamp(my, (my + mh - h).max(my));

    let _ = window.set_position(PhysicalPosition::new(x as i32, y as i32));
}

/// Clamp a requested logical popover height to what the screen actually leaves
/// free on the anchored side, minus [`CHART_SCREEN_MARGIN`]. Anything past that
/// is reached by scrolling the node list instead of by a taller window.
/// [`CHART_MAX_H`] only stands in when the monitor could not be determined.
fn clamp_chart_h(logical_h: f64, available_logical_h: Option<f64>) -> f64 {
    let max_h = match available_logical_h {
        Some(avail) => avail - CHART_SCREEN_MARGIN,
        None => CHART_MAX_H,
    };
    logical_h.clamp(CHART_MIN_H, CHART_MIN_H.max(max_h))
}

/// Physical pixels the popover can grow into: from the edge pinned to the tray
/// icon to the far edge of the screen's work area. `work` is that area's
/// `(top, height)`.
fn available_room(below: bool, cur_top: f64, cur_h: f64, work: (f64, f64)) -> f64 {
    let (wy, wh) = work;
    if below {
        // Top edge is pinned under the tray icon; grow down to the work area's
        // bottom.
        (wy + wh - cur_top).max(0.0)
    } else {
        // Bottom edge is pinned above the tray icon; grow up to its top.
        (cur_top + cur_h - wy).max(0.0)
    }
}

/// New top edge (physical px) for a popover resized from `cur_h` to
/// `target_h`. A `below` popover hangs off the menu bar and keeps its top
/// edge; the others sit above the taskbar and keep their bottom edge, so they
/// grow upwards. `monitor` is the containing monitor's `(top, height)`.
fn anchored_y(
    below: bool,
    cur_top: f64,
    cur_h: f64,
    target_h: f64,
    monitor: Option<(f64, f64)>,
) -> f64 {
    let y = if below {
        cur_top
    } else {
        cur_top + cur_h - target_h
    };
    match monitor {
        Some((my, mh)) => y.clamp(my, (my + mh - target_h).max(my)),
        None => y,
    }
}

/// Lock only the vertical axis. Update this lock whenever content height changes.
fn chart_size_constraints(height: f64) -> tauri::WindowSizeConstraints {
    tauri::WindowSizeConstraints {
        min_width: Some(tauri::LogicalUnit(CHART_W).into()),
        max_width: None,
        min_height: Some(tauri::LogicalUnit(height).into()),
        max_height: Some(tauri::LogicalUnit(height).into()),
    }
}

/// Resize the popover to `logical_h`, growing away from the edge anchored to
/// the tray icon: hanging under the menu bar it keeps its top edge and extends
/// downwards, sitting above the taskbar it keeps its bottom edge and extends
/// upwards. Returns the logical height actually applied after clamping to the
/// popover's monitor. A plain `set_size` always grows downwards, which pushes
/// an expanded node card off the bottom of the screen.
pub fn resize_chart(app: &AppHandle, logical_h: f64) -> f64 {
    let Some(window) = app.get_webview_window("chart") else {
        return logical_h;
    };
    let Ok(pos) = window.outer_position() else {
        return logical_h;
    };
    let Ok(size) = window.inner_size() else {
        return logical_h;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let cur_h = size.height as f64;
    let below = app
        .state::<AppState>()
        .chart_below
        .load(std::sync::atomic::Ordering::Relaxed);

    // Probe the monitor at the anchored edge: that edge sits next to the tray
    // icon, so it is on-screen even when the opposite edge already overflowed.
    let anchor_y = if below {
        pos.y as f64
    } else {
        pos.y as f64 + cur_h - 1.0
    };
    let center_x = pos.x as f64 + size.width as f64 / 2.0;
    let monitor = monitor_containing(app, center_x, anchor_y);
    // The work area excludes the taskbar / Dock, so the popover grows into the
    // space the shell actually leaves free.
    let work = monitor.as_ref().map(|m| {
        let area = m.work_area();
        (area.position.y as f64, area.size.height as f64)
    });
    let available = work.map(|w| available_room(below, pos.y as f64, cur_h, w) / scale);

    let applied = clamp_chart_h(logical_h, available);
    let target_h = (applied * scale).round().max(1.0);
    let y = anchored_y(below, pos.y as f64, cur_h, target_h, work);

    let new_pos = PhysicalPosition::new(pos.x, y as i32);
    // Content owns height; keep the width chosen by the user, also across DPI changes.
    let new_size = tauri::LogicalSize::new(size.width as f64 / scale, applied);
    // Move before growing, shrink before moving: either order leaves the
    // window briefly overhanging the anchored screen edge otherwise.
    if target_h > cur_h {
        let _ = window.set_position(new_pos);
        let _ = window.set_size_constraints(chart_size_constraints(applied));
        let _ = window.set_size(new_size);
    } else {
        let _ = window.set_size_constraints(chart_size_constraints(applied));
        let _ = window.set_size(new_size);
        let _ = window.set_position(new_pos);
    }
    applied
}

fn monitor_containing(app: &AppHandle, x: f64, y: f64) -> Option<tauri::Monitor> {
    let monitors = app.available_monitors().ok()?;
    monitors
        .into_iter()
        .find(|m| {
            let p = m.position();
            let s = m.size();
            let (mx, my) = (p.x as f64, p.y as f64);
            x >= mx && x <= mx + s.width as f64 && y >= my && y <= my + s.height as f64
        })
        .or_else(|| app.primary_monitor().ok().flatten())
}

/// Open (or focus) the settings window. Dispatches to the main thread (see
/// open_panel for why).
pub fn open_settings(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || open_settings_on_main(&handle));
}

fn open_settings_on_main(app: &AppHandle) {
    match app.get_webview_window("settings") {
        Some(window) => {
            if window.is_visible().unwrap_or(false) {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            } else {
                // Invisible means the page has not reached load-finished yet.
                // Treat another explicit open as a manual recovery attempt.
                let _ = window.destroy();
                let _ = create_settings_window(app, 0);
            }
        }
        None => {
            let _ = create_settings_window(app, 0);
        }
    }
}

fn create_settings_window(app: &AppHandle, retry: u8) -> tauri::Result<()> {
    let theme = app.state::<AppState>().settings.read().theme;
    let loaded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let loaded_on_page = loaded.clone();
    let window = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("index.html".into()))
        .title("Hotaru 设置")
        .theme(native_theme(theme))
        .inner_size(540.0, 760.0)
        .min_inner_size(480.0, 600.0)
        // Avoid exposing WebView2's blank backing surface while the local page
        // is still initializing. The load callback reveals the real content.
        .visible(false)
        .on_page_load(move |window, payload| {
            if matches!(payload.event(), PageLoadEvent::Finished) {
                loaded_on_page.store(true, std::sync::atomic::Ordering::Release);
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        })
        .build()?;
    crate::window_geometry::restore(&window);

    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(SETTINGS_LOAD_TIMEOUT);
        if loaded.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let inner = handle.clone();
        let _ = handle.run_on_main_thread(move || {
            if loaded.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            let Some(window) = inner.get_webview_window("settings") else {
                return;
            };
            if should_retry_settings_load(retry) {
                let _ = window.destroy();
                let _ = create_settings_window(&inner, retry + 1);
            } else {
                // Discard the permanently blank instance. A later explicit
                // open starts a fresh retry sequence instead of reusing it.
                let _ = window.destroy();
            }
        });
    });

    Ok(())
}

#[cfg(test)]
mod panel_chrome_tests {
    use super::*;

    const CHART_HTML: &str = include_str!("../../ui/chart.html");
    /// 1080p monitor at the virtual-desktop origin: `(top, height)`.
    const SCREEN: Option<(f64, f64)> = Some((0.0, 1080.0));

    #[test]
    fn chart_allows_width_changes_but_locks_content_height() {
        for height in [300.0, 540.0, 988.0] {
            let constraints = chart_size_constraints(height);
            assert_eq!(
                constraints.min_height,
                Some(tauri::LogicalUnit(height).into())
            );
            assert_eq!(constraints.max_height, constraints.min_height);
            assert_eq!(
                constraints.min_width,
                Some(tauri::LogicalUnit(CHART_W).into())
            );
            assert_eq!(constraints.max_width, None);
        }
        assert_eq!(clamp_chart_h(540.0, Some(1000.0)), 540.0);
        assert_eq!(clamp_chart_h(1400.0, Some(1000.0)), 988.0);
    }

    #[test]
    fn grows_downwards_when_anchored_below_the_icon() {
        assert_eq!(anchored_y(true, 30.0, 300.0, 600.0, SCREEN), 30.0);
        assert_eq!(anchored_y(true, 30.0, 600.0, 300.0, SCREEN), 30.0);
    }

    #[test]
    fn grows_upwards_when_anchored_above_the_icon() {
        assert_eq!(anchored_y(false, 726.0, 300.0, 600.0, SCREEN), 426.0);
        assert_eq!(anchored_y(false, 426.0, 600.0, 300.0, SCREEN), 726.0);
    }

    #[test]
    fn keeps_the_popover_inside_its_monitor() {
        assert_eq!(anchored_y(false, 1000.0, 26.0, 900.0, SCREEN), 126.0);
        assert_eq!(anchored_y(false, 300.0, 26.0, 900.0, SCREEN), 0.0);
        assert_eq!(anchored_y(true, 900.0, 100.0, 600.0, SCREEN), 480.0);
        assert_eq!(
            anchored_y(false, 100.0, 100.0, 900.0, Some((0.0, 800.0))),
            0.0
        );
    }

    #[test]
    fn secondary_monitor_bounds_use_its_own_origin() {
        let above = Some((-1200.0, 1200.0));
        assert_eq!(anchored_y(false, -300.0, 100.0, 900.0, above), -1100.0);
        assert_eq!(anchored_y(false, -1150.0, 50.0, 900.0, above), -1200.0);
    }

    #[test]
    fn height_is_capped_by_the_free_space_not_the_whole_screen() {
        // Plenty of room: the request goes through untouched.
        assert_eq!(clamp_chart_h(500.0, Some(1000.0)), 500.0);
        assert_eq!(clamp_chart_h(200.0, Some(1000.0)), CHART_MIN_H);
        // Tall screens may show a tall popover; the old flat 900px ceiling hid
        // nodes that had room to be drawn.
        assert_eq!(clamp_chart_h(1600.0, Some(2000.0)), 1600.0);
        // Past the free space the list scrolls instead of the window growing.
        assert_eq!(
            clamp_chart_h(900.0, Some(700.0)),
            700.0 - CHART_SCREEN_MARGIN
        );
        // Screens too short even for the minimum height still get the minimum.
        assert_eq!(clamp_chart_h(700.0, Some(200.0)), CHART_MIN_H);
        // Unknown monitor falls back to the fixed ceiling.
        assert_eq!(clamp_chart_h(2000.0, None), CHART_MAX_H);
    }

    #[test]
    fn free_space_is_measured_from_the_anchored_edge() {
        // 1080-tall screen, 48px taskbar at the bottom.
        let work = (0.0, 1032.0);
        // Bottom edge pinned at 1026 (300 tall, top at 726): it may grow all the
        // way up to the top of the work area.
        assert_eq!(available_room(false, 726.0, 300.0, work), 1026.0);
        // Top edge pinned just under a 25px menu bar: down to the work bottom.
        assert_eq!(available_room(true, 31.0, 300.0, (25.0, 1007.0)), 1001.0);
        // A popover already overhanging reports no room, never a negative one.
        assert_eq!(available_room(true, 2000.0, 300.0, work), 0.0);
        assert_eq!(available_room(false, -900.0, 300.0, work), 0.0);
    }

    #[test]
    fn initial_size_tracks_the_node_count() {
        assert_eq!(chart_logical_size(0), (CHART_W, CHART_MIN_H));
        assert_eq!(chart_logical_size(4), (CHART_W, 398.0));
        assert_eq!(chart_logical_size(100), (CHART_W, CHART_MAX_H));
    }

    #[test]
    fn panel_chrome_contains_navigation_and_window_actions() {
        for action in [
            "history.back()",
            "history.forward()",
            "location.reload()",
            "invokeWindow('minimize')",
            "invokeWindow('toggle_maximize')",
            "invokeWindow('close')",
        ] {
            assert!(PANEL_CHROME_SCRIPT.contains(action), "missing {action}");
        }
    }

    #[test]
    fn panel_chrome_uses_system_theme_and_offsets_page_content() {
        assert!(PANEL_CHROME_SCRIPT.contains("prefers-color-scheme: dark"));
        assert!(PANEL_CHROME_SCRIPT.contains("padding-top: calc("));
        assert!(PANEL_CHROME_SCRIPT.contains("data-tauri-drag-region"));
    }

    #[test]
    fn windows_chart_uses_an_opaque_theme_surface() {
        assert!(CHART_HTML.contains(":root.windows-opaque"));
        assert!(CHART_HTML.contains("html.windows-opaque body"));
        assert!(CHART_HTML.contains("border-radius: 0"));
    }

    #[test]
    fn settings_load_retries_are_bounded() {
        assert!(should_retry_settings_load(0));
        assert!(should_retry_settings_load(1));
        assert!(!should_retry_settings_load(2));
    }

    #[test]
    fn reload_only_when_the_watched_navigation_never_finished() {
        // Armed at T=1000, nothing finished since: the page is stuck.
        assert!(panel_load_timed_out(true, false, 1000, 1000));
        // A load finished after the watchdog was armed.
        assert!(!panel_load_timed_out(true, false, 1000, 1200));
        // Hidden or minimized panels are left alone.
        assert!(!panel_load_timed_out(false, false, 1000, 1000));
        assert!(!panel_load_timed_out(true, true, 1000, 1000));
    }

    #[test]
    fn healing_needs_a_stalled_navigation_on_a_foreground_panel() {
        // Epoch-milliseconds scale, so "never healed" (0) clears the cooldown.
        const START: u64 = 1_700_000_000_000;
        const LOADED: u64 = START - 4_000;
        let now = START + PANEL_STALL_MS + 1;
        assert!(panel_needs_healing(true, false, START, LOADED, now, 0));
        // The navigation did finish.
        assert!(!panel_needs_healing(true, false, START, START + 1, now, 0));
        // Not stalled long enough yet.
        assert!(!panel_needs_healing(
            true,
            false,
            START,
            LOADED,
            START + PANEL_STALL_MS,
            0
        ));
        // Still inside the cooldown after a previous attempt.
        assert!(!panel_needs_healing(
            true,
            false,
            START,
            LOADED,
            now,
            now - 1
        ));
        assert!(panel_needs_healing(
            true,
            false,
            START,
            LOADED,
            now,
            now - PANEL_HEAL_COOLDOWN_MS - 1
        ));
        // Never rebuild a panel the user cannot see: a minimized WebView2 is
        // throttled, so an unfinished load there is not a wedged page — and the
        // rebuilt window would pop back in front of the user.
        assert!(!panel_needs_healing(true, true, START, LOADED, now, 0));
        assert!(!panel_needs_healing(false, false, START, LOADED, now, 0));
    }

    #[test]
    fn a_fresh_process_has_no_navigation_to_heal() {
        // All timestamps zero (nothing ever loaded): must not look stalled.
        assert!(!panel_needs_healing(true, false, 0, 0, 10_000_000, 0));
    }

    #[test]
    fn macos_window_controls_follow_the_traffic_light_order() {
        // Left to right: close, minimise, zoom — the group's DOM order is the
        // Windows one, so macOS has to reorder all three.
        for (class, order) in [
            (".hotaru-close", 1),
            (".hotaru-minimize", 2),
            (".hotaru-maximize", 3),
        ] {
            let rule = format!(".hotaru-macos {class} {{ order: {order};");
            assert!(PANEL_CHROME_SCRIPT.contains(&rule), "missing {rule}");
        }
        // Windows keeps the controls on the right, macOS moves them left.
        assert!(PANEL_CHROME_SCRIPT.contains(".hotaru-macos .hotaru-window-controls { order: 1;"));
        assert!(PANEL_CHROME_SCRIPT
            .contains(":not(.hotaru-macos) .hotaru-window-controls { order: 3; }"));
    }

    #[test]
    fn platform_class_comes_from_the_build_target() {
        let script = panel_chrome_script();
        assert!(!script.contains("__HOTARU_PLATFORM__"));
        assert_eq!(
            script.contains("toolbar.className = 'hotaru-macos';"),
            cfg!(target_os = "macos")
        );
    }
}

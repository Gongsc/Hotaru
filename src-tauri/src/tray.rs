use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::{const_mutex, Mutex};
use tauri::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_autostart::ManagerExt as _;
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform,
};

use crate::models::{
    aggregate, fmt_rate, icon_state, scoped_nodes, Aggregate, IconState, MonitorSnapshot, Severity,
};
use crate::state::AppState;

pub const TRAY_ID: &str = "hotaru-main-tray";

// ---------------------------------------------------------------------------
// Creation
// ---------------------------------------------------------------------------

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let cache = MenuCache::build(app)?;
    #[cfg(not(target_os = "macos"))]
    let menu = cache.menu.clone();
    *MENU_CACHE.lock() = Some(cache);
    let builder = TrayIconBuilder::with_id(TRAY_ID)
        .icon(tauri::image::Image::new_owned(
            draw_icon(
                &IconState {
                    severity: Severity::Down,
                    gauge: None,
                    badge: false,
                },
                icon_foreground(app),
            ),
            ICON_SIZE,
            ICON_SIZE,
        ))
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip("Hotaru · 正在连接后端…")
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(|tray, event| match event {
            // Left click opens the chart popover anchored to the icon; right
            // click keeps the native menu, double click opens the panel.
            // Windows delivers a Click event for press AND release — act on
            // release only, or one physical click toggles the popover twice.
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } => {
                let (px, py) = match rect.position {
                    tauri::Position::Physical(p) => (p.x as f64, p.y as f64),
                    tauri::Position::Logical(p) => (p.x, p.y),
                };
                let (sw, sh) = match rect.size {
                    tauri::Size::Physical(s) => (s.width as f64, s.height as f64),
                    tauri::Size::Logical(s) => (s.width, s.height),
                };
                crate::windows::open_chart(tray.app_handle(), (px, py, sw, sh));
            }
            #[cfg(target_os = "macos")]
            TrayIconEvent::Click {
                button: MouseButton::Right,
                button_state: MouseButtonState::Up,
                ..
            } => show_context_menu(tray.app_handle()),
            TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => {
                crate::windows::open_panel(tray.app_handle());
            }
            _ => {}
        });
    // AppKit can consume left clicks when a menu is attached to NSStatusItem.
    // On macOS, present the menu explicitly only for a right click.
    #[cfg(not(target_os = "macos"))]
    let builder = builder.menu(&menu);
    builder.build(app)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn show_context_menu(app: &AppHandle) {
    // Release the cache lock before entering AppKit's nested menu event loop:
    // menu actions and monitoring updates may need to acquire it again.
    let menu = MENU_CACHE.lock().as_ref().map(|cache| cache.menu.clone());
    if let Some(menu) = menu {
        // A user may have closed settings before configuring a backend.
        // Keep a hidden window available as the context menu host.
        let window = app.webview_windows().into_values().next().or_else(|| {
            tauri::WebviewWindowBuilder::new(
                app,
                "tray-menu-host",
                tauri::WebviewUrl::External("about:blank".parse().expect("valid blank URL")),
            )
                .visible(false)
                .build()
                .ok()
        });
        if let Some(window) = window {
            // With no position, AppKit uses the mouse's screen coordinates,
            // including when the backing window is hidden.
            let _ = window.popup_menu(&menu);
        }
    }
}

// ---------------------------------------------------------------------------
// Periodic refresh (runs on the main thread)
// ---------------------------------------------------------------------------

pub fn apply(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    let st = app.state::<AppState>();
    let settings = st.settings.read().clone();
    let snap = st.snapshot.read().clone();
    let scope = scoped_nodes(&settings, &snap.nodes);
    let agg = aggregate(&scope);
    let state = icon_state(&settings, &snap);

    let _ = tray.set_icon_with_as_template(Some(tauri::image::Image::new_owned(
        draw_icon(&state, icon_foreground(app)),
        ICON_SIZE,
        ICON_SIZE,
    )), cfg!(target_os = "macos"));
    #[cfg(target_os = "macos")]
    if settings.show_menu_bar_text {
        set_menu_bar_rates(&agg);
    } else {
        // tray-icon 0.24.x ignores `None` on macOS instead of clearing the
        // existing NSStatusBarButton title. An explicit empty string both
        // clears the stale text and makes AppKit recalculate the item width.
        let _ = tray.set_title(Some(String::new()));
    }
    // Keep this after the title: `set_tooltip` is the only call of tray-icon's
    // that also re-fits its click-target subview to the button, and the title
    // is what changes the button's width.
    let _ = tray.set_tooltip(Some(tooltip_text(&snap, &agg)));
    // Redrawing the icon clears the button's highlight, so put it back while
    // the popover is open. Only while it is: re-asserting `false` here would
    // fight the highlight AppKit draws for the right-click menu. `apply` is
    // already on the main thread, so this can go straight through.
    if POPOVER_ACTIVE.load(Ordering::Relaxed) {
        highlight_status_item(true);
    }

    sync_menu(app);
}

// ---------------------------------------------------------------------------
// Click feedback
// ---------------------------------------------------------------------------

/// Whether the popover is on screen, mirrored onto the menu bar item.
static POPOVER_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Draw the menu bar item selected, or not, to match the popover.
///
/// A status item highlights itself only for the menu AppKit shows on its
/// behalf. The popover is a window of our own, so clicking the icon otherwise
/// leaves it looking untouched for as long as the popover is up. Setting the
/// button's highlight ourselves borrows the same selected background a native
/// menu bar item draws, and costs nothing on Windows, where the tray already
/// has its own pressed state.
pub fn set_popover_active(app: &AppHandle, active: bool) {
    POPOVER_ACTIVE.store(active, Ordering::Relaxed);
    // Reached from the blur watchdog thread as well as the tray click, and
    // AppKit only takes this from the main thread.
    let _ = app.run_on_main_thread(move || highlight_status_item(active));
}

/// Highlight the one `NSStatusBarButton` in this process.
///
/// Tauri keeps the `NSStatusItem` private, so the button is found the long way
/// round: AppKit parks it in a window of its own in the menu bar, and every
/// other window of ours holds a webview instead. Only this process's windows
/// are searched, and only this app puts a status item in them. Does nothing
/// off the main thread, or before the tray exists.
#[cfg(target_os = "macos")]
fn highlight_status_item(active: bool) {
    if let Some(button) = status_button() {
        button.setHighlighted(active);
    }
}

/// This process's status item button, or `None` off the main thread or before
/// the tray exists.
#[cfg(target_os = "macos")]
fn status_button() -> Option<objc2::rc::Retained<objc2_app_kit::NSStatusBarButton>> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let mtm = MainThreadMarker::new()?;
    let windows = NSApplication::sharedApplication(mtm).windows();
    for window in &windows {
        let Some(view) = window.contentView() else {
            continue;
        };
        if let Some(button) = find_status_button(&view) {
            return Some(button);
        }
    }
    None
}

/// The button sits one level inside the status bar window's content view, but
/// look a little deeper anyway rather than depend on that layout.
#[cfg(target_os = "macos")]
fn find_status_button(
    view: &objc2_app_kit::NSView,
) -> Option<objc2::rc::Retained<objc2_app_kit::NSStatusBarButton>> {
    use objc2::rc::Retained;
    use objc2_app_kit::NSStatusBarButton;

    if let Some(button) = view.downcast_ref::<NSStatusBarButton>() {
        return Some(Retained::from(button));
    }
    for sub in &view.subviews() {
        if let Some(button) = find_status_button(&sub) {
            return Some(button);
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn highlight_status_item(_active: bool) {}

// ---------------------------------------------------------------------------
// Menu
// ---------------------------------------------------------------------------

/// Cached tray menu, built once and only patched in place afterwards (the
/// autostart check state). Swapping the menu on every tick makes Windows
/// recycle the internal item ids and clicks can hit the wrong item.
struct MenuCache {
    menu: Menu<Wry>,
    autostart: CheckMenuItem<Wry>,
}

static MENU_CACHE: Mutex<Option<MenuCache>> = const_mutex(None);

impl MenuCache {
    fn build(app: &AppHandle) -> tauri::Result<Self> {
        let open_panel = MenuItem::with_id(app, "open-panel", "打开面板", true, None::<&str>)?;
        let reload_panel = MenuItem::with_id(app, "reload-panel", "刷新面板", true, None::<&str>)?;
        let open_settings = MenuItem::with_id(app, "open-settings", "设置…", true, None::<&str>)?;

        let autostart = CheckMenuItem::with_id(
            app,
            "autostart",
            "开机自启",
            true,
            app.autolaunch().is_enabled().unwrap_or(false),
            None::<&str>,
        )?;
        let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;

        let sep_bottom = PredefinedMenuItem::separator(app)?;

        let menu = Menu::new(app)?;
        menu.append(&open_panel)?;
        menu.append(&reload_panel)?;
        menu.append(&open_settings)?;
        menu.append(&autostart)?;
        menu.append(&sep_bottom)?;
        menu.append(&quit)?;

        Ok(Self { menu, autostart })
    }
}

fn sync_menu(app: &AppHandle) {
    let mut cache = MENU_CACHE.lock();
    if let Some(c) = cache.as_mut() {
        let _ = c.autostart.set_checked(app.autolaunch().is_enabled().unwrap_or(false));
    }
}

fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    let id = event.id().0.as_str().to_string();
    match id.as_str() {
        "quit" => app.exit(0),
        "open-panel" => crate::windows::open_panel(app),
        "reload-panel" => crate::windows::recreate_panel(app),
        "open-settings" => crate::windows::open_settings(app),
        "autostart" => {
            let launcher = app.autolaunch();
            let result = if launcher.is_enabled().unwrap_or(false) {
                launcher.disable()
            } else {
                launcher.enable()
            };
            if let Err(e) = result {
                log::error!("切换开机自启失败: {e}");
            }
            refresh(app);
        }
        _ => {}
    }
}

pub fn refresh(app: &AppHandle) {
    let a = app.clone();
    let _ = app.run_on_main_thread(move || apply(&a));
}

fn tooltip_text(snap: &MonitorSnapshot, agg: &Aggregate) -> String {
    if !snap.backend_ok {
        let err = snap.error.as_deref().unwrap_or("后端不可达");
        return format!("Hotaru · 后端不可达\n{}", truncate(err, 90));
    }
    format!(
        "Hotaru · 在线 {}/{}\nCPU {:.0}% · 内存 {:.0}%\n↑{} ↓{}",
        agg.online,
        agg.total,
        agg.cpu,
        agg.mem_pct,
        fmt_rate(agg.net_up),
        fmt_rate(agg.net_down)
    )
}

// ---------------------------------------------------------------------------
// macOS menu bar text
// ---------------------------------------------------------------------------

/// Point size of the menu bar rates. Two lines have to sit inside the height
/// of the menu bar, so this runs smaller than ordinary menu bar text.
#[cfg(target_os = "macos")]
const MENU_BAR_FONT_SIZE: f64 = 9.0;
/// Leading between the two lines, pinned rather than left to the font: the
/// system font's own line height would push the pair out of the menu bar.
#[cfg(target_os = "macos")]
const MENU_BAR_LINE_HEIGHT: f64 = 10.0;
/// Nudge down from where the button would otherwise centre the pair. Pinning
/// the line height above crops the first line's ascent, which lifts both lines
/// off centre; this drops them back so the block of figures shares a centre
/// line with the icon beside it, measured ink to ink. Half a device pixel is
/// as close as it gets — the layout quantises — and the leftover half is spent
/// upwards, which is the side that reads as centred.
#[cfg(target_os = "macos")]
const MENU_BAR_BASELINE_OFFSET: f64 = -4.25;
/// Where the rates' right edge lands, measured from the start of the line.
/// Fixed, so the item keeps one width while the figures change under it —
/// wide enough for the longest rate the ladder below can produce.
#[cfg(target_os = "macos")]
const MENU_BAR_TAB_STOP: f64 = 56.0;

/// The menu bar's own rate format: a space before the unit, and only as many
/// digits as the rung needs. [`fmt_rate`]'s decimals throughout are for the
/// tooltip and the popover, which have room for them.
///
/// B/s and KB/s are whole numbers — a single byte or kilobyte either way is
/// noise. A whole MB/s is a coarse step, though: 1.5 and 2.4 would both read
/// as 2, so that rung and the one above it keep a decimal, up to the point
/// where three digits already say everything — see [`with_decimal`].
#[cfg(any(target_os = "macos", test))]
fn fmt_rate_menu_bar(bps: f64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    const GB: f64 = 1024.0 * MB;
    if bps < 0.0 {
        "0 B/s".into()
    } else if bps < KB {
        format!("{bps:.0} B/s")
    } else if bps < MB {
        format!("{:.0} KB/s", bps / KB)
    } else if bps < GB {
        with_decimal(bps / MB, "MB/s")
    } else {
        with_decimal(bps / GB, "GB/s")
    }
}

/// A decimal below 100, none above it. Past 100 the tenths digit is worth a
/// fraction of a percent, and holding on to it would cost every reading a
/// permanently wider item: `1023.9 MB/s` runs some 15pt past the tab stop the
/// rest of the ladder fits inside, and the arrow column would have nowhere to
/// go.
#[cfg(any(target_os = "macos", test))]
fn with_decimal(value: f64, unit: &str) -> String {
    if value < 100.0 {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.0} {unit}")
    }
}

/// Up over down, each on its own line. The tab is what keeps the two columns:
/// a right-aligned tab stop leaves the arrows in a column on the left and
/// lines the figures up on the right.
#[cfg(any(target_os = "macos", test))]
fn menu_bar_text(agg: &Aggregate) -> String {
    format!(
        "↑\t{}\n↓\t{}",
        fmt_rate_menu_bar(agg.net_up),
        fmt_rate_menu_bar(agg.net_down)
    )
}

/// Put the two-line rates on the status item.
///
/// A status item's plain string title is one line in the menu bar font, so the
/// stacked pair has to go on as an attributed string instead — which also
/// means setting it straight on the button, since that is not something Tauri
/// or tray-icon pass through. No colour is set: left alone, the button draws
/// the title in whatever the menu bar currently calls for, including inverting
/// it while the item is highlighted.
#[cfg(target_os = "macos")]
fn set_menu_bar_rates(agg: &Aggregate) {
    use objc2::AnyThread;
    use objc2_app_kit::{
        NSBaselineOffsetAttributeName, NSFont, NSFontAttributeName, NSFontWeightRegular,
        NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSTextAlignment, NSTextTab,
    };
    use objc2_foundation::{
        NSArray, NSDictionary, NSMutableAttributedString, NSNumber, NSRange, NSString,
    };

    let Some(button) = status_button() else {
        return;
    };

    let style = NSMutableParagraphStyle::new();
    style.setAlignment(NSTextAlignment::Left);
    style.setMinimumLineHeight(MENU_BAR_LINE_HEIGHT);
    style.setMaximumLineHeight(MENU_BAR_LINE_HEIGHT);
    let tab = unsafe {
        NSTextTab::initWithTextAlignment_location_options(
            NSTextTab::alloc(),
            NSTextAlignment::Right,
            MENU_BAR_TAB_STOP,
            &NSDictionary::new(),
        )
    };
    style.setTabStops(Some(&NSArray::from_retained_slice(&[tab])));

    // Monospaced digits: proportional ones would shuffle the columns sideways
    // every time a figure changed.
    let font = NSFont::monospacedDigitSystemFontOfSize_weight(MENU_BAR_FONT_SIZE, unsafe {
        NSFontWeightRegular
    });

    let text = NSString::from_str(&menu_bar_text(agg));
    let title = NSMutableAttributedString::from_nsstring(&text);
    let all = NSRange::new(0, text.length());
    unsafe {
        title.addAttribute_value_range(NSFontAttributeName, &font, all);
        title.addAttribute_value_range(NSParagraphStyleAttributeName, &style, all);
        title.addAttribute_value_range(
            NSBaselineOffsetAttributeName,
            &NSNumber::new_f64(MENU_BAR_BASELINE_OFFSET),
            all,
        );
    }
    button.setAttributedTitle(&title);
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_bar_rates_keep_a_decimal_only_from_mb_up() {
        const KB: f64 = 1024.0;
        const MB: f64 = 1024.0 * KB;
        const GB: f64 = 1024.0 * MB;
        assert_eq!(fmt_rate_menu_bar(0.0), "0 B/s");
        assert_eq!(fmt_rate_menu_bar(-1.0), "0 B/s");
        assert_eq!(fmt_rate_menu_bar(940.0), "940 B/s");
        // 16 KB/s and change still reads as 16, not 16.3
        assert_eq!(fmt_rate_menu_bar(16.3 * KB), "16 KB/s");
        assert_eq!(fmt_rate_menu_bar(KB), "1 KB/s");
        // from MB up a whole unit is too coarse to round to
        assert_eq!(fmt_rate_menu_bar(MB), "1.0 MB/s");
        assert_eq!(fmt_rate_menu_bar(2.75 * MB), "2.8 MB/s");
        assert_eq!(fmt_rate_menu_bar(4.0 * GB), "4.0 GB/s");
        // ...but three digits is the whole budget: past 100 the tenths digit
        // buys nothing and would push the text into the arrow column.
        assert_eq!(fmt_rate_menu_bar(99.94 * MB), "99.9 MB/s");
        assert_eq!(fmt_rate_menu_bar(125.4 * MB), "125 MB/s");
        // the widest the ladder can produce, which the tab stop has to clear
        assert_eq!(fmt_rate_menu_bar(1023.9 * MB), "1024 MB/s");
    }

    #[test]
    fn menu_bar_text_stacks_the_two_rates_around_a_tab() {
        let agg = Aggregate {
            net_up: 16.0 * 1024.0,
            net_down: 5.0 * 1024.0,
            ..Aggregate::default()
        };
        // The tab is the column split; the newline is the second line. Both
        // only mean anything against the paragraph style set alongside them.
        assert_eq!(menu_bar_text(&agg), "↑\t16 KB/s\n↓\t5 KB/s");
    }

    #[test]
    fn tray_icon_is_monochrome_and_states_have_distinct_shapes() {
        let color = Color::from_rgba8(0, 0, 0, 255);
        let normal = draw_icon(
            &IconState { severity: Severity::Ok, gauge: Some(42.0), badge: false },
            color,
        );
        let warning = draw_icon(
            &IconState { severity: Severity::Warn, gauge: Some(88.0), badge: false },
            color,
        );
        let offline = draw_icon(
            &IconState { severity: Severity::Down, gauge: None, badge: false },
            color,
        );

        for pixel in normal.chunks_exact(4).filter(|p| p[3] > 0) {
            assert_eq!(&pixel[..3], &[0, 0, 0]);
        }
        assert_ne!(normal, warning);
        assert_ne!(normal, offline);
        assert_ne!(warning, offline);
    }
}

// ---------------------------------------------------------------------------
// Icon drawing (tiny-skia, 32x32 RGBA)
// ---------------------------------------------------------------------------

const ICON_SIZE: u32 = 32;

fn icon_foreground(app: &AppHandle) -> Color {
    #[cfg(target_os = "windows")]
    {
        let theme = app
            .webview_windows()
            .values()
            .find_map(|window| window.theme().ok())
            .unwrap_or_else(|| match app.state::<AppState>().settings.read().theme {
                crate::models::ThemeMode::Dark => tauri::Theme::Dark,
                _ => tauri::Theme::Light,
            });
        return match theme {
            tauri::Theme::Dark => Color::from_rgba8(0xF5, 0xF5, 0xF7, 0xFF),
            _ => Color::from_rgba8(0x1D, 0x1D, 0x1F, 0xFF),
        };
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        Color::from_rgba8(0, 0, 0, 0xFF)
    }
}

fn draw_icon(state: &IconState, color: Color) -> Vec<u8> {
    let Some(mut pm) = Pixmap::new(ICON_SIZE, ICON_SIZE) else {
        return vec![0; (ICON_SIZE * ICON_SIZE * 4) as usize];
    };
    draw_status_symbol(&mut pm, color);

    let offline = state.severity == Severity::Down
        || (state.severity == Severity::Err && state.gauge.is_none());
    if offline {
        draw_slash(&mut pm, color);
    } else if state.badge || matches!(state.severity, Severity::Warn | Severity::Err) {
        let mut pb = PathBuilder::new();
        let radius = if state.severity == Severity::Err { 3.2 } else { 2.6 };
        pb.push_circle(26.0, 6.0, radius);
        if let Some(path) = pb.finish() {
            fill(&mut pm, &path, color);
        }
    }

    let mut rgba = Vec::with_capacity((ICON_SIZE * ICON_SIZE * 4) as usize);
    for p in pm.pixels() {
        let c = p.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    rgba
}

fn draw_status_symbol(pm: &mut Pixmap, color: Color) {
    let mut ring = PathBuilder::new();
    ring.push_circle(16.0, 16.0, 11.1);
    if let Some(path) = ring.finish() {
        stroke(pm, &path, color, 2.9);
    }

    let mut pulse = PathBuilder::new();
    pulse.move_to(7.55, 16.0);
    pulse.line_to(11.73, 16.0);
    pulse.line_to(13.95, 11.2);
    pulse.line_to(17.78, 20.71);
    pulse.line_to(20.53, 14.22);
    pulse.line_to(24.44, 14.22);
    if let Some(path) = pulse.finish() {
        stroke(pm, &path, color, 2.9);
    }
}

fn fill(pm: &mut Pixmap, path: &tiny_skia::Path, color: Color) {
    let paint = Paint {
        shader: tiny_skia::Shader::SolidColor(color),
        anti_alias: true,
        ..Default::default()
    };
    pm.fill_path(path, &paint, FillRule::Winding, Transform::identity(), None);
}

fn stroke(pm: &mut Pixmap, path: &tiny_skia::Path, color: Color, width: f32) {
    let paint = Paint {
        shader: tiny_skia::Shader::SolidColor(color),
        anti_alias: true,
        ..Default::default()
    };
    let stroke = Stroke {
        width,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    };
    pm.stroke_path(path, &paint, &stroke, Transform::identity(), None);
}

fn draw_slash(pm: &mut Pixmap, color: Color) {
    let mut pb = PathBuilder::new();
    pb.move_to(7.0, 7.0);
    pb.line_to(25.0, 25.0);
    if let Some(path) = pb.finish() {
        stroke(pm, &path, color, 3.5);
    }
}

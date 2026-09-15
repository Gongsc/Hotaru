//! Use NSStatusBarButton's public target/action API. A mouse-handling subview
//! is not a reliable event target on newer AppKit versions.
use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSEventMask, NSEventModifierFlags, NSEventType, NSStatusBarButton,
};
use objc2_foundation::NSObject;
use tauri::AppHandle;

thread_local! {
    // NSControl does not retain its target.
    static TARGET: RefCell<Option<Retained<StatusButtonTarget>>> = const { RefCell::new(None) };
}

#[derive(Debug, PartialEq)]
enum ClickAction {
    ToggleChart,
    OpenPanel,
    ContextMenu,
    Ignore,
}

fn click_action(kind: NSEventType, control: bool, count: isize) -> ClickAction {
    match kind {
        NSEventType::RightMouseUp => ClickAction::ContextMenu,
        NSEventType::LeftMouseUp if control => ClickAction::ContextMenu,
        NSEventType::LeftMouseUp if count == 2 => ClickAction::OpenPanel,
        NSEventType::LeftMouseUp if count <= 1 => ClickAction::ToggleChart,
        _ => ClickAction::Ignore,
    }
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppHandle]
    struct StatusButtonTarget;

    impl StatusButtonTarget {
        #[unsafe(method(hotaruStatusClick:))]
        fn click(&self, _sender: &NSStatusBarButton) {
            let Some(event) = NSApplication::sharedApplication(self.mtm()).currentEvent() else {
                return;
            };
            let app = self.ivars();
            match click_action(
                event.r#type(),
                event.modifierFlags().contains(NSEventModifierFlags::Control),
                event.clickCount(),
            ) {
                ClickAction::ContextMenu => {
                    crate::windows::close_chart(app);
                    super::show_context_menu(app);
                }
                ClickAction::OpenPanel => {
                    crate::windows::close_chart(app);
                    crate::windows::open_panel(app);
                }
                ClickAction::ToggleChart => {
                    if let Some(tray) = app.tray_by_id(super::TRAY_ID) {
                        if let Ok(Some(rect)) = tray.rect() {
                            super::toggle_chart(app, rect);
                        }
                    }
                }
                ClickAction::Ignore => {}
            }
        }
    }
);

pub(super) fn install(app: &AppHandle) -> tauri::Result<()> {
    let button = super::status_button()
        .ok_or_else(|| std::io::Error::other("Hotaru status bar button is unavailable"))?;
    let target = StatusButtonTarget::alloc(button.mtm()).set_ivars(app.clone());
    // SAFETY: NSObject's init has no extra requirements; the retained target
    // lives on the main thread for as long as the status button can call it.
    let target: Retained<StatusButtonTarget> = unsafe { msg_send![super(target), init] };
    unsafe {
        button.setTarget(Some(&target));
        button.setAction(Some(sel!(hotaruStatusClick:)));
    }
    button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);

    // Disable only tray-icon's event overlay. Leave AppKit's own subviews
    // intact and let the actual button handle clicks across its full width.
    for view in &button.subviews() {
        if view.class().name().to_bytes() == b"TaoTrayTarget" {
            view.setHidden(true);
        }
    }
    TARGET.with(|slot| *slot.borrow_mut() = Some(target));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_release_toggles_and_presses_are_ignored() {
        assert_eq!(
            click_action(NSEventType::LeftMouseDown, false, 1),
            ClickAction::Ignore
        );
        assert_eq!(
            click_action(NSEventType::LeftMouseUp, false, 1),
            ClickAction::ToggleChart
        );
    }

    #[test]
    fn right_click_and_control_click_open_only_the_menu() {
        assert_eq!(
            click_action(NSEventType::RightMouseUp, false, 1),
            ClickAction::ContextMenu
        );
        assert_eq!(
            click_action(NSEventType::LeftMouseUp, true, 2),
            ClickAction::ContextMenu
        );
        assert_eq!(
            click_action(NSEventType::RightMouseDown, false, 1),
            ClickAction::Ignore
        );
    }

    #[test]
    fn double_click_opens_panel_without_another_toggle() {
        assert_eq!(
            click_action(NSEventType::LeftMouseUp, false, 2),
            ClickAction::OpenPanel
        );
        assert_eq!(
            click_action(NSEventType::LeftMouseUp, false, 3),
            ClickAction::Ignore
        );
    }
}

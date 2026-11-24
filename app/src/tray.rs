//! "Minimize to the tray" (Windows): the window hides and an icon appears next to the clock.
//! Clicking the icon, its "Show Corvox" menu item or the global shortcut brings Corvox back.
//!
//! The icon's events arrive on the UI thread's message loop even while the window is hidden
//! (and egui isn't drawing), so they show the window directly instead of waiting for a frame.

use crate::engine::{send, Event, Tx};

/// Whether this system has a tray to minimize to.
pub fn supported() -> bool {
    cfg!(windows)
}

#[cfg(windows)]
pub struct Tray {
    _icon: tray_icon::TrayIcon,
}

#[cfg(windows)]
impl Tray {
    /// Puts the Corvox icon in the tray. `hwnd` is the window it brings back.
    pub fn show(hwnd: isize, tx: Tx) -> Option<Self> {
        use tray_icon::menu::{Menu, MenuEvent, MenuItem};
        use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

        let open = MenuItem::new(crate::i18n::t("Show Corvox"), true, None);
        let quit = MenuItem::new(crate::i18n::t("Quit"), true, None);
        let menu = Menu::with_items(&[&open, &quit]).ok()?;
        let (open_id, quit_id) = (open.id().clone(), quit.id().clone());
        let (rgba, size) = crate::icons::app_icon_rgba(32);
        let icon = tray_icon::Icon::from_rgba(rgba, size, size).ok()?;
        let tray = TrayIconBuilder::new()
            .with_icon(icon)
            .with_tooltip("Corvox")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .ok()?;
        let tx1 = tx.clone();
        TrayIconEvent::set_event_handler(Some(move |e| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                restore(hwnd, &tx1);
            }
        }));
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            if e.id == open_id {
                restore(hwnd, &tx);
            } else if e.id == quit_id {
                crate::platform::close_raw(hwnd);
            }
        }));
        Some(Self { _icon: tray })
    }
}

#[cfg(windows)]
impl Drop for Tray {
    fn drop(&mut self) {
        tray_icon::TrayIconEvent::set_event_handler(None::<fn(tray_icon::TrayIconEvent)>);
        tray_icon::menu::MenuEvent::set_event_handler(None::<fn(tray_icon::menu::MenuEvent)>);
    }
}

#[cfg(not(windows))]
pub struct Tray;

#[cfg(not(windows))]
impl Tray {
    pub fn show(_hwnd: isize, _tx: Tx) -> Option<Self> {
        None
    }
}

/// Shows the hidden window and tells the app (which then removes the tray icon).
pub fn restore(hwnd: isize, tx: &Tx) {
    crate::platform::show_raw(hwnd);
    send(tx, Event::Restored);
}

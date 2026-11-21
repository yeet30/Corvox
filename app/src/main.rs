#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod audio;
mod catalog;
mod config;
mod engine;
mod i18n;
mod icons;
mod platform;
mod ptt;
mod update;
mod vmic;

use eframe::egui;

fn main() -> eframe::Result {
    update::init();
    if let Some(pid) = update::wait_pid_arg() {
        platform::wait_for_exit(pid, 10_000); // restarted after an update: let the old one close
    }
    update::cleanup();
    let cfg = config::Config::load();
    let size = if cfg.collapsed { app::COLLAPSED } else { app::EXPANDED };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(app::APP_NAME)
        .with_inner_size(size)
        .with_min_inner_size(app::COLLAPSED)
        .with_decorations(false)
        .with_transparent(!platform::native_corners())
        .with_resizable(false);
    if cfg.always_on_top {
        viewport = viewport.with_always_on_top();
    }
    if std::env::var_os("CORVOX_OFFSCREEN").is_some() {
        // Testing: open off-screen without taking focus or a taskbar button.
        viewport = viewport.with_position([-4000.0, -4000.0]).with_active(false).with_taskbar(false);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(app::APP_NAME, options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, cfg)))))
}

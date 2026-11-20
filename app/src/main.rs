#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod audio;
mod catalog;
mod config;
mod engine;
mod icons;
mod platform;
mod ptt;

use eframe::egui;

fn main() -> eframe::Result {
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
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(app::APP_NAME, options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, cfg)))))
}

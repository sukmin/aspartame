//! Aspartame - a lightweight image viewer for Windows 11.
//!
//! Usage: `aspartame.exe [IMAGE_FILE]`

// Release builds must not open a console window; debug builds keep it for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod archive;
mod cr3;
mod gallery;
mod image_loader;
mod input;
mod navigator;
mod thumbnails;
mod viewport;

use std::path::PathBuf;

const APP_NAME: &str = "Aspartame";

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let initial_path = std::env::args_os().nth(1).map(PathBuf::from);

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(APP_NAME)
        .with_app_id("aspartame")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([400.0, 300.0])
        .with_drag_and_drop(true);
    if let Some(icon) = load_window_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |cc| Ok(Box::new(app::ViewerApp::new(cc, initial_path)))),
    )
}

/// The icon shown in the title bar and the task bar (embedded at compile time).
fn load_window_icon() -> Option<egui::IconData> {
    let bytes = include_bytes!("../assets/icon.png");
    let image = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    Some(egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}

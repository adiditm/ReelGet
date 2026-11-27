#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use reelget_gui::{app_icon, ReelGetApp};

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let mut viewport = egui::ViewportBuilder::default()
        .with_app_id("com.reelget.app")
        .with_inner_size([960.0, 740.0])
        .with_min_inner_size([640.0, 480.0]);
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }

    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "ReelGet — Video & Reel Downloader",
        native_options,
        Box::new(|cc| Ok(Box::new(ReelGetApp::new(cc)))),
    )
}

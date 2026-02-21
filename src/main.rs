mod app;
mod browser;
mod config;
mod editor;
mod metadata;
mod processing;
mod state;
mod thumbnail;
mod viewer;

use app::ImageManagerApp;
use config::AppConfig;

fn main() -> eframe::Result {
    let config = AppConfig::load();

    let width = config.window_width.unwrap_or(1200.0);
    let height = config.window_height.unwrap_or(800.0);

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Image Manager")
            .with_inner_size([width, height]),
        ..Default::default()
    };

    eframe::run_native(
        "image-manager",
        native_options,
        Box::new(|cc| Ok(Box::new(ImageManagerApp::new(cc, config)))),
    )
}

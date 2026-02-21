mod app;
mod browser;
mod editor;
mod metadata;
mod processing;
mod state;
mod thumbnail;
mod viewer;

use app::ImageManagerApp;

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Image Manager")
            .with_inner_size([1200.0, 800.0]),
        ..Default::default()
    };

    eframe::run_native(
        "image-manager",
        native_options,
        Box::new(|cc| Ok(Box::new(ImageManagerApp::new(cc)))),
    )
}

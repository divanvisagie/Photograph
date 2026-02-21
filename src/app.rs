use std::path::PathBuf;

use crate::{browser::Browser, viewer::Viewer};

pub struct ImageManagerApp {
    browser: Browser,
    viewer: Viewer,
    prev_selected: Option<PathBuf>,
}

impl ImageManagerApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            browser: Browser::new(),
            viewer: Viewer::new(),
            prev_selected: None,
        }
    }
}

impl eframe::App for ImageManagerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Nav bar (top panel — must be before central panel)
        self.browser.show_nav(ctx);

        // Viewer right panel — must be before central panel
        if self.browser.selected.is_some() {
            self.viewer.show_panel(ctx);
        }

        // Thumbnail grid (central panel)
        self.browser.show_grid(ctx);

        // Trigger full-image load when selection changes
        let sel = self.browser.selected.clone();
        if sel != self.prev_selected {
            if let Some(path) = sel.clone() {
                self.viewer.set_image(path, ctx);
            }
            self.prev_selected = sel;
        }
    }
}

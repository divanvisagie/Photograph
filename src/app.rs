use std::path::PathBuf;

use crate::{browser::Browser, config::AppConfig, viewer::Viewer};

struct ViewerWindow {
    viewer: Viewer,
    open: bool,
}

pub struct ImageManagerApp {
    browser: Browser,
    viewers: Vec<ViewerWindow>,
    active_viewer: Option<usize>,
    next_id: usize,
    prev_selected: Option<PathBuf>,
    show_browser: bool,
    show_tools_window: bool,
    config: AppConfig,
}

impl ImageManagerApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, config: AppConfig) -> Self {
        let browser = Browser::new(config.browse_path.clone());
        Self {
            browser,
            viewers: Vec::new(),
            active_viewer: None,
            next_id: 0,
            prev_selected: None,
            show_browser: true,
            show_tools_window: true,
            config,
        }
    }
}

impl eframe::App for ImageManagerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Track window size for saving on exit
        if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
            self.config.window_width = Some(rect.width());
            self.config.window_height = Some(rect.height());
        }

        // Poll background work before rendering windows
        self.browser.poll(ctx);
        for vw in &mut self.viewers {
            vw.viewer.drain(ctx);
        }

        // When a thumbnail is clicked, open or activate a viewer for that path
        let sel = self.browser.selected.clone();
        if sel != self.prev_selected {
            if let Some(ref path) = sel {
                // Find existing viewer for this path, or create a new one
                let existing = self.viewers.iter_mut().find(|vw| {
                    vw.viewer.path() == Some(path)
                });
                if let Some(vw) = existing {
                    vw.open = true;
                    self.active_viewer = Some(vw.viewer.id());
                } else {
                    let id = self.next_id;
                    self.next_id += 1;
                    let mut viewer = Viewer::new(id);
                    viewer.set_image(path.clone(), ctx);
                    self.viewers.push(ViewerWindow { viewer, open: true });
                    self.active_viewer = Some(id);
                }
            }
            self.prev_selected = sel;
        }

        // Remove closed viewers; clear active if it was closed
        self.viewers.retain(|vw| vw.open);
        if let Some(active_id) = self.active_viewer {
            if !self.viewers.iter().any(|vw| vw.viewer.id() == active_id) {
                // Active was closed — fall back to last viewer if any
                self.active_viewer = self.viewers.last().map(|vw| vw.viewer.id());
            }
        }

        // Empty central panel as background (required by egui)
        egui::CentralPanel::default().show(ctx, |_ui| {});

        // Images window — path bar + thumbnail grid
        egui::Window::new("Images")
            .open(&mut self.show_browser)
            .default_size([600.0, 700.0])
            .default_pos([10.0, 50.0])
            .show(ctx, |ui| {
                self.browser.show_contents(ui, ctx);
            });

        // Render each viewer window
        let mut newly_active: Option<usize> = None;
        for vw in &mut self.viewers {
            let title = vw.viewer.filename();
            let window_id = format!("viewer_{}", vw.viewer.id());
            let resp = egui::Window::new(&title)
                .id(egui::Id::new(&window_id))
                .open(&mut vw.open)
                .default_size([800.0, 600.0])
                .default_pos([620.0, 50.0])
                .show(ctx, |ui| {
                    vw.viewer.show_image(ui);
                });

            // Detect clicks inside this viewer window to make it active
            if let Some(inner) = resp {
                let clicked_inside = inner.response.hovered()
                    && ctx.input(|i| i.pointer.any_pressed());
                if clicked_inside {
                    newly_active = Some(vw.viewer.id());
                }
            }
        }
        if let Some(id) = newly_active {
            self.active_viewer = Some(id);
        }

        // Tool window — controls + EXIF for the active viewer
        if self.active_viewer.is_some() {
            let active_id = self.active_viewer.unwrap();
            if let Some(vw) = self.viewers.iter_mut().find(|vw| vw.viewer.id() == active_id) {
                let label = format!("Tools — {}", vw.viewer.filename());
                egui::Window::new(&label)
                    .id(egui::Id::new("tools_window"))
                    .open(&mut self.show_tools_window)
                    .default_size([300.0, 400.0])
                    .default_pos([620.0, 660.0])
                    .show(ctx, |ui| {
                        vw.viewer.show_controls(ui);
                    });
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.config.browse_path = Some(self.browser.current_dir.clone());
        self.config.save();
    }
}

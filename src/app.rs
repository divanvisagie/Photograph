use std::path::{Path, PathBuf};
use std::sync::mpsc;

use image::DynamicImage;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{
    CompressionType as PngCompressionType, FilterType as PngFilterType, PngEncoder,
};
use image::codecs::webp::WebPEncoder;
use image::imageops::FilterType;

use crate::{browser::Browser, config::AppConfig, state::EditState, viewer::Viewer};

struct ViewerWindow {
    viewer: Viewer,
    open: bool,
    spawn_pos: egui::Pos2,
}

#[derive(Clone)]
struct RenderTask {
    source_path: PathBuf,
    edit_state: EditState,
}

#[derive(Clone, Copy)]
struct RenderOptions {
    format: RenderFormat,
    jpg_quality: u8,
    png_compression: u8,
    resize_enabled: bool,
    resize_long_edge: u32,
}

enum RenderEvent {
    Progress {
        done: usize,
        total: usize,
        ok: usize,
        failed: usize,
        current: String,
    },
    Finished {
        ok: usize,
        failed: usize,
        total: usize,
        output_dir: PathBuf,
        first_error: Option<String>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RenderFormat {
    Jpg,
    Png,
    Webp,
}

impl RenderFormat {
    const ALL: [RenderFormat; 3] = [RenderFormat::Jpg, RenderFormat::Png, RenderFormat::Webp];

    fn label(self) -> &'static str {
        match self {
            RenderFormat::Jpg => "JPG",
            RenderFormat::Png => "PNG",
            RenderFormat::Webp => "WebP",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            RenderFormat::Jpg => "jpg",
            RenderFormat::Png => "png",
            RenderFormat::Webp => "webp",
        }
    }
}

pub struct PhotographApp {
    browser: Browser,
    viewers: Vec<ViewerWindow>,
    active_viewer: Option<usize>,
    next_id: usize,
    prev_selected: Option<PathBuf>,
    show_browser: bool,
    show_render_window: bool,
    render_output_path: String,
    render_format: RenderFormat,
    render_jpg_quality: u8,
    render_png_compression: u8,
    render_resize_enabled: bool,
    render_resize_long_edge: u32,
    render_status: String,
    render_in_progress: bool,
    render_total: usize,
    render_done: usize,
    render_ok: usize,
    render_failed: usize,
    render_current: String,
    render_rx: Option<mpsc::Receiver<RenderEvent>>,
    config: AppConfig,
}

impl PhotographApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, config: AppConfig) -> Self {
        let browser = Browser::new(config.browse_path.clone());
        let output_dir = default_render_dir();
        Self {
            browser,
            viewers: Vec::new(),
            active_viewer: None,
            next_id: 0,
            prev_selected: None,
            show_browser: true,
            show_render_window: false,
            render_output_path: output_dir.display().to_string(),
            render_format: RenderFormat::Jpg,
            render_jpg_quality: 90,
            render_png_compression: 6,
            render_resize_enabled: false,
            render_resize_long_edge: 3000,
            render_status: String::new(),
            render_in_progress: false,
            render_total: 0,
            render_done: 0,
            render_ok: 0,
            render_failed: 0,
            render_current: String::new(),
            render_rx: None,
            config,
        }
    }

    fn build_render_tasks(&self) -> Vec<RenderTask> {
        self.viewers
            .iter()
            .filter_map(|vw| {
                vw.viewer.path().map(|path| RenderTask {
                    source_path: path.clone(),
                    edit_state: vw.viewer.edit_state.clone(),
                })
            })
            .collect()
    }

    fn start_render_job(&mut self, ctx: &egui::Context) {
        let output_dir = expand_home_prefix(&self.render_output_path);
        if output_dir.as_os_str().is_empty() {
            self.render_status = "Output path is empty".to_string();
            return;
        }
        if self.render_resize_enabled && self.render_resize_long_edge == 0 {
            self.render_status = "Resize long edge must be greater than 0".to_string();
            return;
        }
        let tasks = self.build_render_tasks();
        if tasks.is_empty() {
            self.render_status = "No open images to render".to_string();
            return;
        }

        let total = tasks.len();
        let options = RenderOptions {
            format: self.render_format,
            jpg_quality: self.render_jpg_quality.clamp(1, 100),
            png_compression: self.render_png_compression.min(9),
            resize_enabled: self.render_resize_enabled,
            resize_long_edge: self.render_resize_long_edge.max(1),
        };
        let output_dir_for_thread = output_dir.clone();
        let (tx, rx) = mpsc::channel();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let mut ok = 0usize;
            let mut failed = 0usize;
            let mut first_error: Option<String> = None;

            for (idx, task) in tasks.into_iter().enumerate() {
                let filename = task
                    .source_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();

                if let Err(err) = render_single_image(
                    &task.source_path,
                    &task.edit_state,
                    &output_dir_for_thread,
                    options,
                ) {
                    failed += 1;
                    if first_error.is_none() {
                        first_error = Some(format!("{}: {}", filename, err));
                    }
                } else {
                    ok += 1;
                }

                let done = idx + 1;
                let _ = tx.send(RenderEvent::Progress {
                    done,
                    total,
                    ok,
                    failed,
                    current: filename,
                });
                ctx2.request_repaint();
            }

            let _ = tx.send(RenderEvent::Finished {
                ok,
                failed,
                total,
                output_dir: output_dir_for_thread,
                first_error,
            });
            ctx2.request_repaint();
        });

        self.render_in_progress = true;
        self.render_total = total;
        self.render_done = 0;
        self.render_ok = 0;
        self.render_failed = 0;
        self.render_current = String::new();
        self.render_status = "Render started...".to_string();
        self.render_rx = Some(rx);
    }

    fn poll_render_events(&mut self) {
        let Some(rx) = self.render_rx.take() else {
            return;
        };

        let mut keep_receiver = true;
        while let Ok(event) = rx.try_recv() {
            match event {
                RenderEvent::Progress {
                    done,
                    total,
                    ok,
                    failed,
                    current,
                } => {
                    self.render_done = done;
                    self.render_total = total;
                    self.render_ok = ok;
                    self.render_failed = failed;
                    self.render_current = current;
                }
                RenderEvent::Finished {
                    ok,
                    failed,
                    total,
                    output_dir,
                    first_error,
                } => {
                    self.render_in_progress = false;
                    self.render_done = total;
                    self.render_total = total;
                    self.render_ok = ok;
                    self.render_failed = failed;
                    self.render_status = if failed == 0 {
                        format!("Rendered {} image(s) to {}", ok, output_dir.display())
                    } else {
                        format!(
                            "Rendered {} image(s), {} failed. First error: {}",
                            ok,
                            failed,
                            first_error.unwrap_or_else(|| "unknown error".to_string())
                        )
                    };
                    keep_receiver = false;
                }
            }
        }

        if keep_receiver {
            self.render_rx = Some(rx);
        }
    }
}

fn next_viewer_spawn_pos(index: usize, viewport_rect: Option<egui::Rect>) -> egui::Pos2 {
    const BASE_X: f32 = 620.0;
    const BASE_Y: f32 = 64.0;
    const STEP: f32 = 40.0;

    let raw_x = BASE_X + STEP * index as f32;
    let raw_y = BASE_Y + STEP * index as f32;

    if let Some(rect) = viewport_rect {
        let max_x = (rect.right() - 220.0).max(rect.left() + 8.0);
        let max_y = (rect.bottom() - 160.0).max(rect.top() + 8.0);
        egui::pos2(raw_x.min(max_x), raw_y.min(max_y))
    } else {
        egui::pos2(raw_x, raw_y)
    }
}

fn default_render_dir() -> PathBuf {
    dirs::picture_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Rendered")
}

fn expand_home_prefix(raw: &str) -> PathBuf {
    if raw == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

fn render_single_image(
    source_path: &Path,
    state: &EditState,
    output_dir: &Path,
    options: RenderOptions,
) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(output_dir)?;
    let input = crate::thumbnail::open_image(source_path)?;
    let processed = crate::processing::transform::apply(&input, state);
    let rendered = apply_export_resize(processed, options);
    let output_path = build_output_path(source_path, output_dir, options.format);
    write_rendered_image(&rendered, &output_path, options)?;
    Ok(output_path)
}

fn build_output_path(source_path: &Path, output_dir: &Path, format: RenderFormat) -> PathBuf {
    let stem = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image");
    let base = output_dir.join(format!("{}.{}", stem, format.extension()));
    if !base.exists() {
        return base;
    }
    for n in 2..10000 {
        let candidate = output_dir.join(format!("{}-{}.{}", stem, n, format.extension()));
        if !candidate.exists() {
            return candidate;
        }
    }
    output_dir.join(format!("{}-final.{}", stem, format.extension()))
}

fn apply_export_resize(img: DynamicImage, options: RenderOptions) -> DynamicImage {
    if !options.resize_enabled {
        return img;
    }
    let Some((new_w, new_h)) =
        resized_dimensions(img.width(), img.height(), options.resize_long_edge)
    else {
        return img;
    };
    img.resize_exact(new_w, new_h, FilterType::Lanczos3)
}

fn resized_dimensions(width: u32, height: u32, max_long_edge: u32) -> Option<(u32, u32)> {
    if width == 0 || height == 0 || max_long_edge == 0 {
        return None;
    }
    let long = width.max(height);
    if long <= max_long_edge {
        return None;
    }
    let scale = max_long_edge as f32 / long as f32;
    let new_w = ((width as f32 * scale).round() as u32).max(1);
    let new_h = ((height as f32 * scale).round() as u32).max(1);
    Some((new_w, new_h))
}

fn write_rendered_image(
    rendered: &DynamicImage,
    output_path: &Path,
    options: RenderOptions,
) -> anyhow::Result<()> {
    let file = std::fs::File::create(output_path)?;
    let writer = std::io::BufWriter::new(file);
    match options.format {
        RenderFormat::Jpg => {
            let encoder = JpegEncoder::new_with_quality(writer, options.jpg_quality.clamp(1, 100));
            rendered.write_with_encoder(encoder)?;
        }
        RenderFormat::Png => {
            let compression = PngCompressionType::Level(options.png_compression.min(9));
            let encoder =
                PngEncoder::new_with_quality(writer, compression, PngFilterType::Adaptive);
            rendered.write_with_encoder(encoder)?;
        }
        RenderFormat::Webp => {
            let encoder = WebPEncoder::new_lossless(writer);
            rendered.write_with_encoder(encoder)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::resized_dimensions;

    #[test]
    fn resized_dimensions_skips_when_already_within_limit() {
        assert_eq!(resized_dimensions(1600, 900, 2000), None);
    }

    #[test]
    fn resized_dimensions_scales_landscape_preserving_aspect() {
        assert_eq!(resized_dimensions(6000, 4000, 3000), Some((3000, 2000)));
    }

    #[test]
    fn resized_dimensions_scales_portrait_preserving_aspect() {
        assert_eq!(resized_dimensions(3000, 6000, 2400), Some((1200, 2400)));
    }
}

impl eframe::App for PhotographApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let viewport_rect = ctx.input(|i| i.viewport().inner_rect);

        // Track window size for saving on exit
        if let Some(rect) = viewport_rect {
            self.config.window_width = Some(rect.width());
            self.config.window_height = Some(rect.height());
        }

        // Poll background work before rendering windows
        self.browser.poll(ctx);
        for vw in &mut self.viewers {
            vw.viewer.drain(ctx);
        }
        self.poll_render_events();

        // When a thumbnail is clicked, open or activate a viewer for that path
        let sel = self.browser.selected.clone();
        if sel != self.prev_selected {
            if let Some(ref path) = sel {
                // Find existing viewer for this path, or create a new one
                let existing = self
                    .viewers
                    .iter_mut()
                    .find(|vw| vw.viewer.path() == Some(path));
                if let Some(vw) = existing {
                    vw.open = true;
                    self.active_viewer = Some(vw.viewer.id());
                } else {
                    let id = self.next_id;
                    self.next_id += 1;
                    let mut viewer = Viewer::new(id);
                    viewer.set_image(path.clone(), ctx);
                    let spawn_pos = next_viewer_spawn_pos(id, viewport_rect);
                    self.viewers.push(ViewerWindow {
                        viewer,
                        open: true,
                        spawn_pos,
                    });
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

        // Top menu bar
        egui::TopBottomPanel::top("main_menu").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Render").clicked() {
                    self.show_render_window = true;
                }
            });
        });
        let content_rect = ctx.available_rect();

        // Empty central panel as background (required by egui)
        egui::CentralPanel::default().show(ctx, |_ui| {});

        // Render window
        if self.show_render_window {
            let mut show_render_window = self.show_render_window;
            egui::Window::new("Render")
                .open(&mut show_render_window)
                .default_size([560.0, 360.0])
                .default_pos([40.0, 70.0])
                .show(ctx, |ui| {
                    ui.label("Output Directory");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.render_output_path)
                            .desired_width(ui.available_width())
                            .font(egui::TextStyle::Monospace),
                    );

                    ui.add_space(8.0);
                    egui::ComboBox::from_label("Format")
                        .selected_text(self.render_format.label())
                        .show_ui(ui, |ui| {
                            for fmt in RenderFormat::ALL {
                                ui.selectable_value(&mut self.render_format, fmt, fmt.label());
                            }
                        });

                    ui.add_space(8.0);
                    match self.render_format {
                        RenderFormat::Jpg => {
                            ui.horizontal(|ui| {
                                ui.label("JPEG Quality");
                                ui.add(
                                    egui::Slider::new(&mut self.render_jpg_quality, 1_u8..=100_u8)
                                        .clamping(egui::SliderClamping::Always),
                                );
                            });
                        }
                        RenderFormat::Png => {
                            ui.horizontal(|ui| {
                                ui.label("PNG Compression");
                                ui.add(
                                    egui::Slider::new(
                                        &mut self.render_png_compression,
                                        0_u8..=9_u8,
                                    )
                                    .clamping(egui::SliderClamping::Always),
                                );
                            });
                        }
                        RenderFormat::Webp => {
                            ui.label(
                                egui::RichText::new("WebP export is currently lossless").weak(),
                            );
                        }
                    }

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.render_resize_enabled, "Resize on export");
                        if self.render_resize_enabled {
                            ui.label("Long edge");
                            ui.add(
                                egui::DragValue::new(&mut self.render_resize_long_edge)
                                    .speed(10)
                                    .range(128_u32..=10000_u32),
                            );
                            ui.label("px");
                        }
                    });

                    ui.add_space(8.0);
                    let label = if self.render_in_progress {
                        "Rendering...".to_string()
                    } else {
                        format!("Render {} Open Image(s)", self.viewers.len())
                    };
                    if ui
                        .add_enabled(
                            !self.viewers.is_empty() && !self.render_in_progress,
                            egui::Button::new(label),
                        )
                        .clicked()
                    {
                        self.start_render_job(ctx);
                    }

                    ui.add_space(8.0);
                    if self.render_in_progress && self.render_total > 0 {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Rendering...");
                        });
                        let progress = self.render_done as f32 / self.render_total as f32;
                        ui.add(
                            egui::ProgressBar::new(progress)
                                .desired_width(ui.available_width())
                                .show_percentage()
                                .text(format!("{}/{}", self.render_done, self.render_total)),
                        );
                        ui.label(format!(
                            "Current: {}",
                            if self.render_current.is_empty() {
                                "starting..."
                            } else {
                                &self.render_current
                            }
                        ));
                        ui.label(format!(
                            "Done: {}  Succeeded: {}  Failed: {}",
                            self.render_done, self.render_ok, self.render_failed
                        ));
                    }

                    ui.add_space(8.0);
                    if !self.render_status.is_empty() {
                        ui.separator();
                        ui.label(&self.render_status);
                    }
                });
            self.show_render_window = show_render_window;
        }

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
                .default_pos(vw.spawn_pos)
                .show(ctx, |ui| {
                    vw.viewer.show_image(ui);
                });

            // Detect clicks inside this viewer window to make it active
            if let Some(inner) = resp {
                let clicked_inside =
                    inner.response.hovered() && ctx.input(|i| i.pointer.any_pressed());
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
            if let Some(vw) = self
                .viewers
                .iter_mut()
                .find(|vw| vw.viewer.id() == active_id)
            {
                let label = format!("Tools — {}", vw.viewer.filename());
                let mut window = egui::Window::new(&label)
                    .id(egui::Id::new("tools_window"))
                    .order(egui::Order::Foreground)
                    .resizable(false)
                    .movable(false);

                if let Some(rect) = viewport_rect {
                    const TOOLS_WIDTH: f32 = 320.0;
                    let x = rect.right() - TOOLS_WIDTH;
                    let y = content_rect.top();
                    let height = (content_rect.height() - 50.0).max(1.0);
                    window = window
                        .fixed_pos(egui::pos2(x, y))
                        .fixed_size(egui::vec2(TOOLS_WIDTH, height));
                } else {
                    window = window.fixed_pos([620.0, 64.0]).fixed_size([320.0, 700.0]);
                }

                window.show(ctx, |ui| {
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

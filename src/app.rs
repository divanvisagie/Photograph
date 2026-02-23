use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};

use image::DynamicImage;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{
    CompressionType as PngCompressionType, FilterType as PngFilterType, PngEncoder,
};
use image::codecs::webp::WebPEncoder;
use image::imageops::FilterType;
use rayon::prelude::*;

use crate::{
    browser::Browser,
    config::AppConfig,
    state::EditState,
    viewer::{PreviewBackend, Viewer},
};

const BROWSER_WIDTH: f32 = 550.0;
const TOOLS_WIDTH: f32 = 320.0;

struct ViewerWindow {
    viewer: Viewer,
    open: bool,
    spawn_pos: egui::Pos2,
    placed: bool,
}

#[derive(Clone)]
struct RenderTask {
    source_path: PathBuf,
    edit_state: EditState,
}

#[derive(Clone)]
struct RenderJob {
    source_path: PathBuf,
    edit_state: EditState,
    output_path: PathBuf,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum RenderSpeedProfile {
    Quality,
    Balanced,
    Speed,
}

impl RenderSpeedProfile {
    const ALL: [RenderSpeedProfile; 3] = [
        RenderSpeedProfile::Quality,
        RenderSpeedProfile::Balanced,
        RenderSpeedProfile::Speed,
    ];

    fn label(self) -> &'static str {
        match self {
            RenderSpeedProfile::Quality => "Quality",
            RenderSpeedProfile::Balanced => "Balanced",
            RenderSpeedProfile::Speed => "Speed",
        }
    }
}

/// Top-level `eframe` application state for the Photograph UI.
pub struct PhotographApp {
    browser: Browser,
    preview_backend: PreviewBackend,
    preview_status_label: String,
    preview_status_details: Option<String>,
    preview_status_vendor: Option<GpuVendor>,
    viewers: Vec<ViewerWindow>,
    active_viewer: Option<usize>,
    next_id: usize,
    prev_selected: Option<PathBuf>,
    show_render_window: bool,
    render_output_path: String,
    render_format: RenderFormat,
    render_speed_profile: RenderSpeedProfile,
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
    tools_window_was_visible: bool,
    config: AppConfig,
}

impl PhotographApp {
    /// Builds the app from persisted config and the selected preview backend.
    pub fn new(
        _cc: &eframe::CreationContext<'_>,
        config: AppConfig,
        preview_backend: PreviewBackend,
    ) -> Self {
        let browser = Browser::new(config.browse_path.clone());
        let output_dir = default_render_dir();
        let (preview_status_label, preview_status_details, preview_status_vendor) =
            preview_status_summary(preview_backend);
        Self {
            browser,
            preview_backend,
            preview_status_label,
            preview_status_details,
            preview_status_vendor,
            viewers: Vec::new(),
            active_viewer: None,
            next_id: 0,
            prev_selected: None,
            show_render_window: false,
            render_output_path: output_dir.display().to_string(),
            render_format: RenderFormat::Jpg,
            render_speed_profile: RenderSpeedProfile::Balanced,
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
            tools_window_was_visible: false,
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

    fn apply_render_speed_profile(&mut self) {
        let (jpg_quality, png_compression) = render_profile_defaults(self.render_speed_profile);
        self.render_jpg_quality = jpg_quality;
        self.render_png_compression = png_compression;
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
        if let Err(err) = std::fs::create_dir_all(&output_dir) {
            self.render_status = format!("Failed to create output directory: {}", err);
            return;
        }

        let options = RenderOptions {
            format: self.render_format,
            jpg_quality: self.render_jpg_quality.clamp(1, 100),
            png_compression: self.render_png_compression.min(9),
            resize_enabled: self.render_resize_enabled,
            resize_long_edge: self.render_resize_long_edge.max(1),
        };
        let jobs = build_render_jobs(tasks, &output_dir, options.format);
        let total = jobs.len();
        let output_dir_for_thread = output_dir.clone();
        let (tx, rx) = mpsc::channel();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let done = Arc::new(AtomicUsize::new(0));
            let ok = Arc::new(AtomicUsize::new(0));
            let failed = Arc::new(AtomicUsize::new(0));
            let first_error = Arc::new(Mutex::new(None::<String>));

            jobs.into_par_iter().for_each(|job| {
                let filename = job
                    .source_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();

                if let Err(err) = render_single_image(
                    &job.source_path,
                    &job.edit_state,
                    &job.output_path,
                    options,
                ) {
                    failed.fetch_add(1, Ordering::Relaxed);
                    if let Ok(mut first) = first_error.lock() {
                        if first.is_none() {
                            *first = Some(format!("{}: {}", filename, err));
                        }
                    }
                } else {
                    ok.fetch_add(1, Ordering::Relaxed);
                }

                let done_now = done.fetch_add(1, Ordering::Relaxed) + 1;
                let ok_now = ok.load(Ordering::Relaxed);
                let failed_now = failed.load(Ordering::Relaxed);
                let _ = tx.send(RenderEvent::Progress {
                    done: done_now,
                    total,
                    ok: ok_now,
                    failed: failed_now,
                    current: filename,
                });
                ctx2.request_repaint();
            });

            let ok_final = ok.load(Ordering::Relaxed);
            let failed_final = failed.load(Ordering::Relaxed);
            let first_error = first_error.lock().ok().and_then(|v| v.clone());

            let _ = tx.send(RenderEvent::Finished {
                ok: ok_final,
                failed: failed_final,
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
    const STEP: f32 = 40.0;

    // Place viewer just past the browser's default width
    let base_x = BROWSER_WIDTH + 24.0;
    let base_y = 32.0;

    let raw_x = base_x + STEP * index as f32;
    let raw_y = base_y + STEP * index as f32;

    let pos = egui::pos2(raw_x, raw_y);
    tracing::debug!(
        index,
        base_x,
        base_y,
        x = pos.x,
        y = pos.y,
        ?viewport_rect,
        "viewer spawn position"
    );
    pos
}

fn viewer_default_size(content_rect: egui::Rect) -> egui::Vec2 {
    // Fill the space between browser and tools
    let width = (content_rect.width() - BROWSER_WIDTH - TOOLS_WIDTH - 80.0).max(400.0);
    let height = (content_rect.height() - 16.0).max(300.0);
    egui::vec2(width, height)
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
    output_path: &Path,
    options: RenderOptions,
) -> anyhow::Result<()> {
    let input = crate::thumbnail::open_image(source_path)?;
    let processed = crate::processing::transform::apply(&input, state);
    let rendered = apply_export_resize(processed, options);
    write_rendered_image(&rendered, output_path, options)?;
    Ok(())
}

fn build_render_jobs(
    tasks: Vec<RenderTask>,
    output_dir: &Path,
    format: RenderFormat,
) -> Vec<RenderJob> {
    let mut reserved = HashSet::new();
    tasks
        .into_iter()
        .map(|task| {
            let output_path =
                build_output_path(&task.source_path, output_dir, format, &mut reserved);
            RenderJob {
                source_path: task.source_path,
                edit_state: task.edit_state,
                output_path,
            }
        })
        .collect()
}

fn build_output_path(
    source_path: &Path,
    output_dir: &Path,
    format: RenderFormat,
    reserved: &mut HashSet<PathBuf>,
) -> PathBuf {
    let stem = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image");
    let base = output_dir.join(format!("{}.{}", stem, format.extension()));
    if output_path_available(&base, reserved) {
        reserved.insert(base.clone());
        return base;
    }
    for n in 2..10000 {
        let candidate = output_dir.join(format!("{}-{}.{}", stem, n, format.extension()));
        if output_path_available(&candidate, reserved) {
            reserved.insert(candidate.clone());
            return candidate;
        }
    }
    let fallback = output_dir.join(format!("{}-final.{}", stem, format.extension()));
    reserved.insert(fallback.clone());
    fallback
}

fn output_path_available(path: &Path, reserved: &HashSet<PathBuf>) -> bool {
    !reserved.contains(path) && !path.exists()
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

fn render_profile_defaults(profile: RenderSpeedProfile) -> (u8, u8) {
    match profile {
        RenderSpeedProfile::Quality => (95, 9),
        RenderSpeedProfile::Balanced => (90, 6),
        RenderSpeedProfile::Speed => (82, 1),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        RenderFormat, RenderSpeedProfile, build_output_path, render_profile_defaults,
        resized_dimensions,
    };

    fn unique_test_dir(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "photograph-{}-{}-{}",
            name,
            std::process::id(),
            nanos
        ))
    }

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

    #[test]
    fn build_output_path_disambiguates_duplicate_stems() {
        let output_dir = unique_test_dir("render-path-dupes");
        let mut reserved = HashSet::new();
        let source = std::path::Path::new("/photos/IMG_0001.RAF");

        let first = build_output_path(source, &output_dir, RenderFormat::Jpg, &mut reserved);
        let second = build_output_path(source, &output_dir, RenderFormat::Jpg, &mut reserved);

        assert_eq!(first, output_dir.join("IMG_0001.jpg"));
        assert_eq!(second, output_dir.join("IMG_0001-2.jpg"));
    }

    #[test]
    fn build_output_path_skips_existing_files() {
        let output_dir = unique_test_dir("render-path-existing");
        std::fs::create_dir_all(&output_dir).unwrap();
        std::fs::write(output_dir.join("IMG_0001.jpg"), b"x").unwrap();

        let source = std::path::Path::new("/photos/IMG_0001.RAF");
        let mut reserved = HashSet::new();
        let next = build_output_path(source, &output_dir, RenderFormat::Jpg, &mut reserved);

        assert_eq!(next, output_dir.join("IMG_0001-2.jpg"));

        let _ = std::fs::remove_dir_all(&output_dir);
    }

    #[test]
    fn render_profile_quality_is_high_quality_defaults() {
        assert_eq!(
            render_profile_defaults(RenderSpeedProfile::Quality),
            (95, 9)
        );
    }

    #[test]
    fn render_profile_balanced_matches_current_defaults() {
        assert_eq!(
            render_profile_defaults(RenderSpeedProfile::Balanced),
            (90, 6)
        );
    }

    #[test]
    fn render_profile_speed_prioritizes_throughput() {
        assert_eq!(render_profile_defaults(RenderSpeedProfile::Speed), (82, 1));
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
                    let mut viewer = Viewer::new(id, self.preview_backend);
                    viewer.set_image(path.clone(), ctx);
                    let open_count = self.viewers.iter().filter(|v| v.open).count();
                    let spawn_pos = next_viewer_spawn_pos(open_count, viewport_rect);
                    self.viewers.push(ViewerWindow {
                        viewer,
                        open: true,
                        spawn_pos,
                        placed: false,
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
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(vendor) = self.preview_status_vendor {
                        let dot = egui::RichText::new("●")
                            .color(vendor.badge_fill())
                            .size(14.0);
                        ui.label(dot)
                            .on_hover_text(format!("{} GPU", vendor.badge_text()));
                    }
                    let response = ui.label(
                        egui::RichText::new(&self.preview_status_label)
                            .weak()
                            .monospace(),
                    );
                    if let Some(details) = &self.preview_status_details {
                        response.on_hover_text(details);
                    }
                });
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
                    ui.horizontal(|ui| {
                        ui.label("Speed profile");
                        egui::ComboBox::from_id_salt("render_speed_profile")
                            .selected_text(self.render_speed_profile.label())
                            .show_ui(ui, |ui| {
                                for profile in RenderSpeedProfile::ALL {
                                    ui.selectable_value(
                                        &mut self.render_speed_profile,
                                        profile,
                                        profile.label(),
                                    );
                                }
                            });
                        if ui.button("Apply preset").clicked() {
                            self.apply_render_speed_profile();
                        }
                    });
                    ui.label(
                        egui::RichText::new(
                            "Presets tune JPEG quality and PNG compression for throughput.",
                        )
                        .weak(),
                    );

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
                                egui::RichText::new(
                                    "WebP export is currently lossless (image crate limitation)",
                                )
                                .weak(),
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

        // Debug: collect window rects for the debug overlay
        #[cfg(debug_assertions)]
        let mut debug_windows: Vec<(&str, egui::Rect)> = Vec::new();

        // Browser window — path bar + thumbnail grid (left side, no close button)
        {
            let height = (content_rect.height() - 50.0).max(1.0);
            let window = egui::Window::new("Browser")
                .id(egui::Id::new("browser_window"))
                .resizable(true)
                .collapsible(true)
                .anchor(egui::Align2::LEFT_TOP, egui::vec2(0.0, content_rect.top()))
                .default_size(egui::vec2(BROWSER_WIDTH, height));

            let resp = window.show(ctx, |ui| {
                self.browser.show_contents(ui, ctx);
            });
            #[cfg(debug_assertions)]
            if let Some(inner) = resp {
                debug_windows.push(("Browser", inner.response.rect));
            }
        }

        // Render each viewer window
        let viewer_size = viewer_default_size(content_rect);
        let mut newly_active: Option<usize> = None;
        for vw in &mut self.viewers {
            let title = vw.viewer.filename();
            let window_id = format!("viewer_{}", vw.viewer.id());
            let id = egui::Id::new(&window_id);

            let mut window = egui::Window::new(&title)
                .id(id)
                .open(&mut vw.open)
                .default_size(viewer_size)
                .default_pos(vw.spawn_pos);

            // Force position on first frame so egui doesn't ignore the offset
            if !vw.placed {
                window = window.current_pos(vw.spawn_pos);
                vw.placed = true;
            }

            let resp = window.show(ctx, |ui| {
                vw.viewer.show_image(ui);
            });

            // Detect clicks inside this viewer window to make it active
            if let Some(inner) = resp {
                let clicked_inside =
                    inner.response.hovered() && ctx.input(|i| i.pointer.any_pressed());
                if clicked_inside {
                    newly_active = Some(vw.viewer.id());
                }
                #[cfg(debug_assertions)]
                debug_windows.push(("Viewer", inner.response.rect));
            }
        }
        if let Some(id) = newly_active {
            self.active_viewer = Some(id);
        }

        // Tool window — controls + EXIF for the active viewer
        let tools_visible = self.active_viewer.is_some();
        let tools_just_opened = tools_visible && !self.tools_window_was_visible;
        if tools_visible {
            let active_id = self.active_viewer.unwrap();
            if let Some(vw) = self
                .viewers
                .iter_mut()
                .find(|vw| vw.viewer.id() == active_id)
            {
                let label = format!("Tools — {}", vw.viewer.filename());
                let mut window = egui::Window::new(&label)
                    .id(egui::Id::new("tools_window"))
                    //.order(egui::Order::Foreground)
                    .resizable(false)
                    .movable(true);

                let height = (content_rect.height() - 50.0).max(1.0);
                // Anchor to top-right so the window sits flush against
                // the right edge, accounting for all frame/chrome automatically.
                window = window
                    .anchor(egui::Align2::RIGHT_TOP, egui::vec2(0.0, content_rect.top()))
                    .fixed_size(egui::vec2(TOOLS_WIDTH, height))
                    .movable(false);

                let resp = window.show(ctx, |ui| {
                    vw.viewer.show_controls(ui);
                });
                #[cfg(debug_assertions)]
                if let Some(inner) = resp {
                    debug_windows.push(("Tools", inner.response.rect));
                }
            }
        }
        self.tools_window_was_visible = tools_visible;

        #[cfg(debug_assertions)]
        {
            egui::Window::new("Debug")
                .id(egui::Id::new("debug_window"))
                .default_pos([10.0, 400.0])
                .default_size([300.0, 200.0])
                .show(ctx, |ui| {
                    if let Some(rect) = viewport_rect {
                        ui.label(format!(
                            "Viewport: {:.0}x{:.0}",
                            rect.width(),
                            rect.height()
                        ));
                    }
                    ui.label(format!(
                        "Content rect: {:.0},{:.0} -> {:.0}x{:.0}",
                        content_rect.left(),
                        content_rect.top(),
                        content_rect.width(),
                        content_rect.height()
                    ));
                    ui.label(format!(
                        "Viewer default size: {:.0}x{:.0}",
                        viewer_size.x, viewer_size.y
                    ));
                    ui.separator();
                    for (name, rect) in &debug_windows {
                        ui.label(format!(
                            "{}: {:.0},{:.0}  {:.0}x{:.0}",
                            name,
                            rect.left(),
                            rect.top(),
                            rect.width(),
                            rect.height()
                        ));
                    }
                });
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        for vw in &self.viewers {
            vw.viewer.save_edits();
        }
        self.config.browse_path = Some(self.browser.current_dir.clone());
        self.config.save();
    }
}

fn preview_status_summary(backend: PreviewBackend) -> (String, Option<String>, Option<GpuVendor>) {
    let status = crate::processing::gpu_spike::runtime_status();
    let adapter_desc = match (
        status.adapter_name.as_deref(),
        status.adapter_backend.as_deref(),
    ) {
        (Some(name), Some(api)) => format!("{} ({})", name, api),
        (Some(name), None) => name.to_string(),
        _ => "n/a".to_string(),
    };
    let vendor = detect_gpu_vendor(&status);
    let driver = status
        .adapter_driver
        .as_deref()
        .unwrap_or("unknown")
        .to_string();
    let (label, details) = match backend {
        PreviewBackend::Cpu => (
            "GPU accel: off (cpu mode)".to_string(),
            Some("Preview backend forced to CPU; GPU acceleration is disabled.".to_string()),
        ),
        PreviewBackend::Auto => {
            if status.available {
                (
                    format!("GPU accel: on [{}]", adapter_desc),
                    Some(format!("auto mode active; driver: {}", driver)),
                )
            } else {
                (
                    "GPU accel: off (auto fallback)".to_string(),
                    Some(
                        "auto mode selected, but no usable GPU backend was initialized."
                            .to_string(),
                    ),
                )
            }
        }
        PreviewBackend::GpuSpike => {
            if status.available {
                (
                    format!("GPU accel: on [{}]", adapter_desc),
                    Some(format!("gpu_spike mode active; driver: {}", driver)),
                )
            } else {
                (
                    "GPU accel: off (gpu fallback)".to_string(),
                    Some(
                        "gpu_spike requested, but GPU init failed; running CPU fallback."
                            .to_string(),
                    ),
                )
            }
        }
    };

    (label, details, vendor)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GpuVendor {
    Nvidia,
    Amd,
    Apple,
    Intel,
}

impl GpuVendor {
    fn badge_text(self) -> &'static str {
        match self {
            GpuVendor::Nvidia => "NVIDIA",
            GpuVendor::Amd => "AMD",
            GpuVendor::Apple => "APPLE",
            GpuVendor::Intel => "INTEL",
        }
    }

    fn badge_fill(self) -> egui::Color32 {
        match self {
            GpuVendor::Nvidia => egui::Color32::from_rgb(118, 185, 0),
            GpuVendor::Amd => egui::Color32::from_rgb(237, 28, 36),
            GpuVendor::Apple => egui::Color32::from_rgb(120, 120, 120),
            GpuVendor::Intel => egui::Color32::from_rgb(0, 113, 197),
        }
    }
}

fn detect_gpu_vendor(status: &crate::processing::gpu_spike::RuntimeStatus) -> Option<GpuVendor> {
    let vendor_id = status.adapter_vendor_id.unwrap_or_default();
    if vendor_id == 0x10DE {
        return Some(GpuVendor::Nvidia);
    }
    if vendor_id == 0x1002 || vendor_id == 0x1022 {
        return Some(GpuVendor::Amd);
    }
    if vendor_id == 0x8086 {
        return Some(GpuVendor::Intel);
    }
    if vendor_id == 0x106B {
        return Some(GpuVendor::Apple);
    }

    let mut haystack = String::new();
    if let Some(name) = &status.adapter_name {
        haystack.push_str(&name.to_ascii_lowercase());
    }
    if let Some(driver) = &status.adapter_driver {
        if !haystack.is_empty() {
            haystack.push(' ');
        }
        haystack.push_str(&driver.to_ascii_lowercase());
    }

    if haystack.contains("nvidia") {
        return Some(GpuVendor::Nvidia);
    }
    if haystack.contains("amd") || haystack.contains("radeon") {
        return Some(GpuVendor::Amd);
    }
    if haystack.contains("intel") || haystack.contains("iris") || haystack.contains("arc") {
        return Some(GpuVendor::Intel);
    }
    if haystack.contains("apple")
        || haystack.contains("m1")
        || haystack.contains("m2")
        || haystack.contains("m3")
        || haystack.contains("m4")
    {
        return Some(GpuVendor::Apple);
    }

    None
}

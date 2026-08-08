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

const SIDEBAR_WIDTH: f32 = 220.0;
const TOOLS_WIDTH: f32 = 340.0;
const FILMSTRIP_HEIGHT: f32 = 100.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Library,
    Detail,
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
    preview_status_label: String,
    preview_status_details: Option<String>,
    preview_status_vendor: Option<GpuVendor>,
    viewer: Viewer,
    view_mode: ViewMode,
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
    config: AppConfig,
}

/// Ubuntu/Yaru-flavoured surface colors, distinct per theme; the accent and
/// semantic colors are shared between them.
struct SurfacePalette {
    window_bg: egui::Color32,
    extreme_bg: egui::Color32,
    faint_bg: egui::Color32,
    button_normal: egui::Color32,
    button_hover: egui::Color32,
    button_active: egui::Color32,
    button_disabled: egui::Color32,
    primary_text: egui::Color32,
    disabled_text: egui::Color32,
    border: egui::Color32,
}

fn surface_palette(theme: egui::Theme) -> SurfacePalette {
    match theme {
        egui::Theme::Dark => SurfacePalette {
            window_bg: egui::Color32::from_rgb(0x30, 0x30, 0x30),
            extreme_bg: egui::Color32::from_rgb(0x24, 0x24, 0x24),
            faint_bg: egui::Color32::from_rgb(0x3A, 0x3A, 0x3A),
            button_normal: egui::Color32::from_rgb(0x3D, 0x3D, 0x3D),
            button_hover: egui::Color32::from_rgb(0x47, 0x47, 0x47),
            button_active: egui::Color32::from_rgb(0x2A, 0x2A, 0x2A),
            button_disabled: egui::Color32::from_rgb(0x30, 0x30, 0x30),
            primary_text: egui::Color32::from_rgb(0xEE, 0xEE, 0xEE),
            disabled_text: egui::Color32::from_rgb(0x94, 0x94, 0x94),
            border: egui::Color32::from_rgb(0x47, 0x47, 0x47),
        },
        egui::Theme::Light => SurfacePalette {
            window_bg: egui::Color32::from_rgb(0xF6, 0xF5, 0xF4),
            extreme_bg: egui::Color32::from_rgb(0xFF, 0xFF, 0xFF),
            faint_bg: egui::Color32::from_rgb(0xE8, 0xE7, 0xE6),
            button_normal: egui::Color32::from_rgb(0xFF, 0xFF, 0xFF),
            button_hover: egui::Color32::from_rgb(0xEC, 0xEC, 0xEA),
            button_active: egui::Color32::from_rgb(0xDE, 0xDC, 0xDA),
            button_disabled: egui::Color32::from_rgb(0xF0, 0xEF, 0xED),
            primary_text: egui::Color32::from_rgb(0x26, 0x23, 0x2A),
            disabled_text: egui::Color32::from_rgb(0x8B, 0x8A, 0x8D),
            border: egui::Color32::from_rgb(0xD5, 0xD3, 0xD1),
        },
    }
}

/// Builds the `Visuals` for one theme in the Yaru (stock Ubuntu GTK theme) style.
fn photograph_visuals(theme: egui::Theme) -> egui::Visuals {
    let mut visuals = theme.default_visuals();
    let p = surface_palette(theme);

    // Ubuntu orange — shared accent across both themes.
    let accent = egui::Color32::from_rgb(0xE9, 0x54, 0x20);
    let focus_ring = egui::Color32::from_rgba_unmultiplied(0xE9, 0x54, 0x20, 0xB3);
    let error = egui::Color32::from_rgb(0xC0, 0x1C, 0x28);
    let warn = egui::Color32::from_rgb(0xE6, 0x6A, 0x00);

    // GTK/Adwaita-style rounding: 6px on widgets, 8px on windows/menus.
    let rounding = egui::CornerRadius::same(6);
    visuals.window_corner_radius = egui::CornerRadius::same(8);
    visuals.menu_corner_radius = egui::CornerRadius::same(8);

    // Panel & window fills
    visuals.window_fill = p.window_bg;
    visuals.panel_fill = p.window_bg;
    visuals.extreme_bg_color = p.extreme_bg;
    visuals.faint_bg_color = p.faint_bg;

    // Text
    visuals.override_text_color = Some(p.primary_text);

    // Selection
    visuals.selection.bg_fill = accent;
    visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);

    // Hyperlinks & semantic colors
    visuals.hyperlink_color = accent;
    visuals.error_fg_color = error;
    visuals.warn_fg_color = warn;

    // Window stroke — very subtle
    visuals.window_stroke = egui::Stroke::new(1.0, p.border);

    // Noninteractive (labels, separators, disabled)
    visuals.widgets.noninteractive.bg_fill = p.button_disabled;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, p.border);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, p.disabled_text);
    visuals.widgets.noninteractive.corner_radius = rounding;

    // Widget styles — inactive (enabled but not hovered)
    visuals.widgets.inactive.bg_fill = p.button_normal;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, p.border);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, p.primary_text);
    visuals.widgets.inactive.corner_radius = rounding;

    // Widget styles — hovered
    visuals.widgets.hovered.bg_fill = p.button_hover;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, p.border);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.5, focus_ring);
    visuals.widgets.hovered.corner_radius = rounding;

    // Widget styles — active (pressed)
    visuals.widgets.active.bg_fill = p.button_active;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, accent);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.5, p.primary_text);
    visuals.widgets.active.corner_radius = rounding;

    // Widget styles — open (e.g. combo box expanded)
    visuals.widgets.open.bg_fill = p.button_hover;
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, p.border);
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, p.primary_text);
    visuals.widgets.open.corner_radius = rounding;

    visuals
}

/// Wires up dark and light `Visuals`, GTK-style spacing, and a theme
/// preference that follows the OS light/dark setting (egui's default).
fn configure_visuals(ctx: &egui::Context) {
    ctx.set_visuals_of(egui::Theme::Dark, photograph_visuals(egui::Theme::Dark));
    ctx.set_visuals_of(egui::Theme::Light, photograph_visuals(egui::Theme::Light));
    ctx.set_theme(egui::ThemePreference::System);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.window_margin = egui::Margin::same(12);
        style.spacing.menu_margin = egui::Margin::same(8);
        style.spacing.indent = 20.0;
        style.spacing.interact_size.y = 22.0;
    });
}

fn configure_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    fonts.font_data.insert(
        "Ubuntu".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/Ubuntu-R.ttf"
        ))),
    );
    fonts.font_data.insert(
        "UbuntuMono".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/UbuntuMono-R.ttf"
        ))),
    );

    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "Ubuntu".to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "UbuntuMono".to_owned());

    ctx.set_fonts(fonts);
}

impl PhotographApp {
    /// Builds the app from persisted config and the selected preview backend.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        config: AppConfig,
        preview_backend: PreviewBackend,
    ) -> Self {
        configure_fonts(&cc.egui_ctx);
        configure_visuals(&cc.egui_ctx);
        let browser = Browser::new(config.browse_path.clone());
        let output_dir = default_render_dir();
        let (preview_status_label, preview_status_details, preview_status_vendor) =
            preview_status_summary(preview_backend);
        Self {
            browser,
            preview_status_label,
            preview_status_details,
            preview_status_vendor,
            viewer: Viewer::new(0, preview_backend),
            view_mode: ViewMode::Library,
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
            config,
        }
    }

    /// Number of photos a render job would currently target, without
    /// touching disk (cheap enough to call every frame for the button label).
    fn render_target_count(&self) -> usize {
        let marked = self.browser.marked_count();
        if marked > 0 {
            marked
        } else if self.browser.selected.is_some() {
            1
        } else {
            0
        }
    }

    /// Builds render tasks from marked photos, falling back to the currently
    /// selected photo if nothing is marked. Edit state for the active photo
    /// comes from the live `Viewer`; other marked photos load their sidecar.
    fn build_render_tasks(&self) -> Vec<RenderTask> {
        let mut paths = self.browser.marked_paths();
        if paths.is_empty() {
            if let Some(path) = &self.browser.selected {
                paths.push(path.clone());
            }
        }

        paths
            .into_iter()
            .map(|source_path| {
                let edit_state = if self.viewer.path() == Some(&source_path) {
                    self.viewer.edit_state.clone()
                } else {
                    EditState::load(&source_path).unwrap_or_default()
                };
                RenderTask {
                    source_path,
                    edit_state,
                }
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
            self.render_status = "No images selected to render".to_string();
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

    /// Steps the active photo to the previous/next image (by `delta`) in the
    /// current folder, wrapping around, for filmstrip-style keyboard nav.
    fn step_active_photo(&mut self, delta: i32, ctx: &egui::Context) {
        let Some(current) = self.viewer.path().cloned() else {
            return;
        };
        let images = &self.browser.images;
        let Some(idx) = images.iter().position(|(p, _)| *p == current) else {
            return;
        };
        let len = images.len() as i32;
        if len == 0 {
            return;
        }
        let new_idx = (idx as i32 + delta).rem_euclid(len) as usize;
        let new_path = images[new_idx].0.clone();
        self.viewer.set_image(new_path.clone(), ctx);
        self.browser.selected = Some(new_path.clone());
        self.prev_selected = Some(new_path);
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
    output_path: &Path,
    options: RenderOptions,
) -> anyhow::Result<()> {
    let input = crate::thumbnail::open_image(source_path)?;
    let processed = match crate::processing::gpu_pipeline::try_apply(&input, state) {
        Some(img) => img,
        None if crate::processing::gpu_pipeline::allow_debug_cpu_fallback() => {
            crate::processing::transform::apply(&input, state)
        }
        None => {
            anyhow::bail!(
                "gpu pipeline render failed while CPU fallback is disabled (set {}=1 for debug fallback)",
                crate::processing::gpu_pipeline::DEBUG_ALLOW_CPU_FALLBACK_ENV
            );
        }
    };
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
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        let viewport_rect = ctx.input(|i| i.viewport().inner_rect);

        // Track window size for saving on exit
        if let Some(rect) = viewport_rect {
            self.config.window_width = Some(rect.width());
            self.config.window_height = Some(rect.height());
        }

        // Poll background work before rendering panels
        self.browser.poll(ctx);
        self.viewer.drain(ctx);
        self.poll_render_events();

        // When a thumbnail is clicked (grid or filmstrip), load it into the
        // single viewer and switch to the Detail view.
        let sel = self.browser.selected.clone();
        if sel != self.prev_selected {
            if let Some(path) = sel.clone() {
                self.viewer.set_image(path, ctx);
                self.view_mode = ViewMode::Detail;
            }
            self.prev_selected = sel;
        }

        // Keyboard navigation while in Detail mode (skip while a text field
        // like the sidebar path bar has focus).
        if self.view_mode == ViewMode::Detail && ctx.memory(|m| m.focused().is_none()) {
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.view_mode = ViewMode::Library;
            }
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                self.step_active_photo(1, ctx);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
                self.step_active_photo(-1, ctx);
            }
        }

        // Top menu bar
        egui::Panel::top("main_menu")
            .frame(
                egui::Frame::side_top_panel(ui.style())
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Render").clicked() {
                        self.show_render_window = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(vendor) = self.preview_status_vendor {
                            let (rect, response) = ui
                                .allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                            ui.painter()
                                .circle_filled(rect.center(), 5.0, vendor.badge_fill());
                            response.on_hover_text(format!("{} GPU", vendor.badge_text()));
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
        // Left sidebar — locations + current folder's subfolders
        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(SIDEBAR_WIDTH)
            .min_size(160.0)
            .frame(
                egui::Frame::side_top_panel(ui.style())
                    .inner_margin(egui::Margin::symmetric(10, 10)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.browser.show_sidebar(ui);
                    });
            });

        // Tools panel + filmstrip only apply while viewing/editing a photo
        if self.view_mode == ViewMode::Detail {
            egui::Panel::right("tools")
                .resizable(false)
                .exact_size(TOOLS_WIDTH)
                .frame(
                    egui::Frame::side_top_panel(ui.style())
                        .inner_margin(egui::Margin::symmetric(10, 10)),
                )
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            self.viewer.show_controls(ui);
                        });
                });

            egui::Panel::bottom("filmstrip")
                .resizable(false)
                .exact_size(FILMSTRIP_HEIGHT)
                .frame(
                    egui::Frame::side_top_panel(ui.style())
                        .inner_margin(egui::Margin::symmetric(10, 8)),
                )
                .show(ui, |ui| {
                    let active_path = self.viewer.path().map(|p| p.as_path());
                    if let Some(clicked) = self.browser.show_filmstrip(ui, active_path) {
                        self.viewer.set_image(clicked.clone(), ctx);
                        self.browser.selected = Some(clicked.clone());
                        self.prev_selected = Some(clicked);
                    }
                });
        }

        // Central panel — Library grid or Detail image view
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin::same(12)))
            .show(ui, |ui| match self.view_mode {
                ViewMode::Library => {
                    self.browser.show_contents(ui, ctx);
                }
                ViewMode::Detail => {
                    ui.horizontal(|ui| {
                        if ui.button("\u{2039} Back to Library").clicked() {
                            self.view_mode = ViewMode::Library;
                        }
                        ui.separator();
                        ui.label(self.viewer.filename());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if let Some(path) = self.viewer.path().cloned() {
                                let marked = self.browser.is_marked(&path);
                                let star = if marked {
                                    "\u{2605} Marked"
                                } else {
                                    "\u{2606} Mark"
                                };
                                if ui.selectable_label(marked, star).clicked() {
                                    self.browser.toggle_mark(path);
                                }
                            }
                        });
                    });
                    ui.separator();
                    self.viewer.show_image(ui);
                }
            });

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
                    let render_count = self.render_target_count();
                    let label = if self.render_in_progress {
                        "Rendering...".to_string()
                    } else if self.browser.marked_count() > 0 {
                        format!("Render {} Marked Image(s)", render_count)
                    } else {
                        format!("Render {} Image(s)", render_count)
                    };
                    if ui
                        .add_enabled(
                            render_count > 0 && !self.render_in_progress,
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

        #[cfg(debug_assertions)]
        {
            egui::Window::new("Debug")
                .id(egui::Id::new("debug_window"))
                .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -10.0])
                .default_size([300.0, 150.0])
                .show(ctx, |ui| {
                    if let Some(rect) = viewport_rect {
                        ui.label(format!(
                            "Viewport: {:.0}x{:.0}",
                            rect.width(),
                            rect.height()
                        ));
                    }
                    ui.label(match self.view_mode {
                        ViewMode::Library => "Mode: Library".to_string(),
                        ViewMode::Detail => format!(
                            "Mode: Detail ({})",
                            self.viewer
                                .path()
                                .map(|p| p.display().to_string())
                                .unwrap_or_default()
                        ),
                    });
                    ui.label(format!("Marked: {}", self.browser.marked_count()));
                });
        }
    }

    fn on_exit(&mut self) {
        self.viewer.save_edits();
        self.config.browse_path = Some(self.browser.current_dir.clone());
        self.config.save();
    }
}

fn preview_status_summary(backend: PreviewBackend) -> (String, Option<String>, Option<GpuVendor>) {
    let status = crate::processing::gpu_pipeline::runtime_status();
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
            "GPU accel: off (debug cpu mode)".to_string(),
            Some("CPU preview backend is active in debug fallback mode.".to_string()),
        ),
        PreviewBackend::Auto => {
            if status.available {
                (
                    format!("GPU accel: on [{}]", adapter_desc),
                    Some(format!("auto mode active; driver: {}", driver)),
                )
            } else {
                (
                    "GPU accel: off (debug cpu fallback)".to_string(),
                    Some(
                        "auto mode selected and GPU is unavailable; debug CPU fallback is active."
                            .to_string(),
                    ),
                )
            }
        }
        PreviewBackend::GpuPipeline => {
            if status.available {
                (
                    format!("GPU accel: on [{}]", adapter_desc),
                    Some(format!("gpu_pipeline mode active; driver: {}", driver)),
                )
            } else {
                (
                    "GPU accel: off (gpu unavailable)".to_string(),
                    Some(
                        "gpu_pipeline requested, but no compatible GPU was initialized."
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

fn detect_gpu_vendor(status: &crate::processing::gpu_pipeline::RuntimeStatus) -> Option<GpuVendor> {
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

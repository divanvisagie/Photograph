use std::{
    collections::hash_map::DefaultHasher,
    collections::{HashMap, VecDeque},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

use image::DynamicImage;

use crate::state::{EditState, GradFilter, Rect};

/// Downscale loaded images to this longest-edge size for the preview.
const PREVIEW_MAX: u32 = 1920;
/// During active slider drags, process a lower-resolution preview for responsiveness.
const INTERACTIVE_PREVIEW_MAX: u32 = 960;
const DEBOUNCE: Duration = Duration::from_millis(300);
const INTERACTIVE_REFRESH: Duration = Duration::from_millis(90);
const PREVIEW_CACHE_CAPACITY: usize = 24;

/// Size in screen pixels for crop corner drag handles.
const HANDLE_SIZE: f32 = 8.0;

enum BgResult {
    Loaded {
        path: PathBuf,
        img: DynamicImage,
    },
    LoadFailed(PathBuf),
    Processed {
        generation: u64,
        cache_key: PreviewCacheKey,
        data: Vec<u8>,
        width: usize,
        height: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewBackend {
    Cpu,
    Auto,
    GpuSpike,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ProcessQuality {
    Interactive,
    Final,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct PreviewCacheKey {
    source_signature: u64,
    edit_signature: u64,
    input_width: u32,
    input_height: u32,
    quality: ProcessQuality,
}

#[derive(Clone)]
struct PreviewCacheEntry {
    data: Vec<u8>,
    width: usize,
    height: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum CropAspect {
    Free,
    Square,
    Photo4x3,
    Wide16x9,
    Original,
}

impl CropAspect {
    fn ratio(self) -> Option<f32> {
        match self {
            CropAspect::Free => None,
            CropAspect::Square => Some(1.0),
            CropAspect::Photo4x3 => Some(4.0 / 3.0),
            CropAspect::Wide16x9 => Some(16.0 / 9.0),
            CropAspect::Original => None, // caller uses image aspect
        }
    }

    fn label(self) -> &'static str {
        match self {
            CropAspect::Free => "Free",
            CropAspect::Square => "1:1",
            CropAspect::Photo4x3 => "4:3",
            CropAspect::Wide16x9 => "16:9",
            CropAspect::Original => "Original",
        }
    }

    const ALL: [CropAspect; 5] = [
        CropAspect::Free,
        CropAspect::Square,
        CropAspect::Photo4x3,
        CropAspect::Wide16x9,
        CropAspect::Original,
    ];
}

/// Which part of the crop rect is being dragged.
#[derive(Clone, Copy, PartialEq)]
enum DragTarget {
    Corner(u8),
    Interior,
}

/// Image viewer/editor window state, including async preview processing.
pub struct Viewer {
    id: usize,
    preview_backend: PreviewBackend,
    current_path: Option<PathBuf>,
    preview: Option<DynamicImage>,
    pub edit_state: EditState,
    needs_process: bool,
    needs_final_process: bool,
    last_slider_change: Option<Instant>,
    last_interactive_process: Option<Instant>,
    texture: Option<egui::TextureHandle>,
    original_texture: Option<egui::TextureHandle>,
    split_view: bool,
    crop_mode: bool,
    crop_aspect: CropAspect,
    /// Visual-only crop selection — not applied to processing until user confirms.
    pending_crop: Option<Rect>,
    /// Active drag operation on the pending crop rect.
    crop_drag: Option<DragTarget>,
    /// Normalized drag start position (for interior moves).
    crop_drag_start_pos: Option<egui::Pos2>,
    /// Pending crop rect snapshot at drag start (for interior moves).
    crop_drag_start_rect: Option<Rect>,
    /// Normalized position where the initial drag began (for creating new rects).
    crop_create_origin: Option<egui::Pos2>,
    zoom: f32,
    pan_offset: egui::Vec2,
    loading: bool,
    /// Background reload for higher-res preview (doesn't show spinner).
    reloading_preview: bool,
    processing: bool,
    requested_generation: u64,
    in_flight_generation: Option<u64>,
    pub metadata: Option<crate::metadata::ImageMetadata>,
    source_signature: u64,
    preview_max: u32,
    last_zoom_change: Option<Instant>,
    preview_cache: HashMap<PreviewCacheKey, PreviewCacheEntry>,
    preview_cache_lru: VecDeque<PreviewCacheKey>,
    tx: mpsc::SyncSender<BgResult>,
    rx: mpsc::Receiver<BgResult>,
}

impl Viewer {
    /// Creates a viewer instance with a stable window ID and preview backend.
    pub fn new(id: usize, preview_backend: PreviewBackend) -> Self {
        let (tx, rx) = mpsc::sync_channel(8);
        Self {
            id,
            preview_backend,
            current_path: None,
            preview: None,
            edit_state: EditState::default(),
            needs_process: false,
            needs_final_process: false,
            last_slider_change: None,
            last_interactive_process: None,
            texture: None,
            original_texture: None,
            split_view: false,
            crop_mode: false,
            crop_aspect: CropAspect::Free,
            pending_crop: None,
            crop_drag: None,
            crop_drag_start_pos: None,
            crop_drag_start_rect: None,
            crop_create_origin: None,
            zoom: 1.0,
            pan_offset: egui::Vec2::ZERO,
            loading: false,
            reloading_preview: false,
            processing: false,
            requested_generation: 0,
            in_flight_generation: None,
            metadata: None,
            source_signature: 0,
            preview_max: PREVIEW_MAX,
            last_zoom_change: None,
            preview_cache: HashMap::new(),
            preview_cache_lru: VecDeque::new(),
            tx,
            rx,
        }
    }

    /// Returns the viewer's stable window identifier.
    pub fn id(&self) -> usize {
        self.id
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// Returns the currently loaded image path, if any.
    pub fn path(&self) -> Option<&PathBuf> {
        self.current_path.as_ref()
    }

    /// Returns the current image filename for window labels/UI.
    pub fn filename(&self) -> String {
        self.current_path
            .as_ref()
            .and_then(|p| p.file_name())
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }

    /// Loads a new image path and resets viewer state for background preview loading.
    pub fn set_image(&mut self, path: PathBuf, ctx: &egui::Context) {
        if self.current_path.as_ref() == Some(&path) {
            return;
        }
        self.current_path = Some(path.clone());
        self.source_signature = source_signature(&path);
        self.preview = None;
        self.texture = None;
        self.original_texture = None;
        self.edit_state = EditState::default();
        self.needs_process = false;
        self.needs_final_process = false;
        self.last_slider_change = None;
        self.last_interactive_process = None;
        self.loading = true;
        self.reloading_preview = false;
        self.processing = false;
        // Invalidate any in-flight processing result from the previous image.
        self.requested_generation = self.requested_generation.wrapping_add(1);
        self.in_flight_generation = None;
        self.crop_mode = false;
        self.pending_crop = None;
        self.crop_drag = None;
        self.crop_create_origin = None;
        self.zoom = 1.0;
        self.pan_offset = egui::Vec2::ZERO;
        self.preview_max = PREVIEW_MAX;
        self.last_zoom_change = None;
        self.metadata = crate::metadata::read(&path).ok();

        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        let cap = self.preview_max;
        std::thread::spawn(move || {
            match crate::thumbnail::open_image_for_preview(&path) {
                Ok(img) => {
                    let preview = if img.width() > cap || img.height() > cap {
                        img.thumbnail(cap, cap)
                    } else {
                        img
                    };
                    let _ = tx.send(BgResult::Loaded { path, img: preview });
                }
                Err(_) => {
                    let _ = tx.send(BgResult::LoadFailed(path));
                }
            }
            ctx2.request_repaint();
        });
    }

    fn trigger_process(&mut self, ctx: &egui::Context, quality: ProcessQuality) {
        let Some(preview) = self.preview.clone() else {
            return;
        };
        if self.processing {
            return;
        }
        self.processing = true;
        self.needs_process = false;
        match quality {
            ProcessQuality::Interactive => {
                self.needs_final_process = true;
                self.last_interactive_process = Some(Instant::now());
            }
            ProcessQuality::Final => {
                self.needs_final_process = false;
                self.last_slider_change = None;
                self.last_interactive_process = None;
            }
        }
        self.requested_generation = self.requested_generation.wrapping_add(1);
        let generation = self.requested_generation;
        self.in_flight_generation = Some(generation);
        let cache_key = self.build_preview_cache_key(&preview, quality);

        if let Some(img) = self.cached_preview_image(&cache_key) {
            self.texture = Some(ctx.load_texture(
                format!("viewer_tex_{}", self.id),
                img,
                egui::TextureOptions::LINEAR,
            ));
            self.processing = false;
            self.in_flight_generation = None;
            return;
        }

        let img = preview;
        let state = self.edit_state.clone();
        let preview_backend = self.preview_backend;
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let source = match quality {
                ProcessQuality::Interactive => downscale_for_interactive(img),
                ProcessQuality::Final => img,
            };
            let result = process_preview_with_backend(&source, &state, preview_backend);
            let rgba = result.to_rgba8();
            let w = rgba.width() as usize;
            let h = rgba.height() as usize;
            let _ = tx.send(BgResult::Processed {
                generation,
                cache_key,
                data: rgba.into_raw(),
                width: w,
                height: h,
            });
            ctx2.request_repaint();
        });
    }

    fn mark_inflight_stale_if_needed(&mut self) {
        bump_requested_generation_for_pending_changes(
            self.processing,
            self.needs_process,
            self.in_flight_generation,
            &mut self.requested_generation,
        );
    }

    fn build_preview_cache_key(
        &self,
        preview: &DynamicImage,
        quality: ProcessQuality,
    ) -> PreviewCacheKey {
        PreviewCacheKey {
            source_signature: self.source_signature,
            edit_signature: edit_state_signature(&self.edit_state),
            input_width: preview.width(),
            input_height: preview.height(),
            quality,
        }
    }

    fn cached_preview_image(&mut self, key: &PreviewCacheKey) -> Option<egui::ColorImage> {
        let img = self.preview_cache.get(key).map(|entry| {
            egui::ColorImage::from_rgba_unmultiplied([entry.width, entry.height], &entry.data)
        })?;
        self.touch_preview_cache_key(key);
        Some(img)
    }

    fn store_preview_cache(
        &mut self,
        key: PreviewCacheKey,
        data: Vec<u8>,
        width: usize,
        height: usize,
    ) {
        if !self.preview_cache.contains_key(&key)
            && self.preview_cache.len() >= PREVIEW_CACHE_CAPACITY
        {
            if let Some(oldest) = self.preview_cache_lru.pop_front() {
                self.preview_cache.remove(&oldest);
            }
        }
        self.preview_cache.insert(
            key.clone(),
            PreviewCacheEntry {
                data,
                width,
                height,
            },
        );
        self.touch_preview_cache_key(&key);
    }

    fn touch_preview_cache_key(&mut self, key: &PreviewCacheKey) {
        if let Some(idx) = self.preview_cache_lru.iter().position(|k| k == key) {
            let _ = self.preview_cache_lru.remove(idx);
        }
        self.preview_cache_lru.push_back(key.clone());
    }

    /// Drains background load/process results and updates viewer textures/state.
    pub fn drain(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                BgResult::Loaded { path, img } => {
                    if self.current_path.as_ref() == Some(&path) {
                        self.preview = Some(img);
                        self.loading = false;
                        self.reloading_preview = false;
                        self.needs_process = true;
                        self.needs_final_process = false;
                        self.last_interactive_process = None;
                    }
                }
                BgResult::LoadFailed(path) => {
                    if self.current_path.as_ref() == Some(&path) {
                        self.loading = false;
                        self.reloading_preview = false;
                    }
                }
                BgResult::Processed {
                    generation,
                    cache_key,
                    data,
                    width: w,
                    height: h,
                } => {
                    self.processing = false;
                    self.in_flight_generation = None;
                    self.store_preview_cache(cache_key, data.clone(), w, h);
                    if generation != self.requested_generation {
                        continue;
                    }
                    let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &data);
                    self.texture = Some(ctx.load_texture(
                        format!("viewer_tex_{}", self.id),
                        img,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
        }
    }

    fn ensure_original_texture(&mut self, ctx: &egui::Context) {
        if self.original_texture.is_some() {
            return;
        }
        if let Some(ref preview) = self.preview {
            let rgba = preview.to_rgba8();
            let w = rgba.width() as usize;
            let h = rgba.height() as usize;
            let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba.into_raw());
            self.original_texture = Some(ctx.load_texture(
                format!("viewer_orig_{}", self.id),
                img,
                egui::TextureOptions::LINEAR,
            ));
        }
    }

    fn effective_crop_ratio(&self) -> Option<f32> {
        match self.crop_aspect {
            CropAspect::Original => {
                if let Some(ref preview) = self.preview {
                    Some(preview.width() as f32 / preview.height() as f32)
                } else {
                    None
                }
            }
            other => other.ratio(),
        }
    }

    /// Renders the image viewport and kicks off preview processing when needed.
    pub fn show_image(&mut self, ui: &mut egui::Ui) {
        // If edits arrive while processing is active, bump the requested generation
        // so the in-flight result is ignored on arrival.
        self.mark_inflight_stale_if_needed();

        // Kick off processing when ready. During active slider drags, run low-res
        // interactive passes; after debounce settle, run a final full-quality pass.
        let work_pending = self.needs_process || self.needs_final_process;
        if work_pending && !self.processing && self.preview.is_some() {
            let since_change = self.last_slider_change.map(|t| t.elapsed());
            let debounce_done = since_change.map(|d| d >= DEBOUNCE).unwrap_or(true);

            if debounce_done {
                self.trigger_process(ui.ctx(), ProcessQuality::Final);
            } else if self.needs_process {
                let interactive_ready = self
                    .last_interactive_process
                    .map(|t| t.elapsed() >= INTERACTIVE_REFRESH)
                    .unwrap_or(true);
                if interactive_ready {
                    self.trigger_process(ui.ctx(), ProcessQuality::Interactive);
                } else {
                    ui.ctx().request_repaint_after(INTERACTIVE_REFRESH);
                }
                if let Some(elapsed) = since_change {
                    let until_final = DEBOUNCE.saturating_sub(elapsed);
                    ui.ctx()
                        .request_repaint_after(until_final.min(INTERACTIVE_REFRESH));
                }
            } else if let Some(elapsed) = since_change {
                ui.ctx()
                    .request_repaint_after(DEBOUNCE.saturating_sub(elapsed));
            }
        }

        // Adaptive preview reload: when zoomed in, load a higher-resolution preview
        if let Some(zoom_changed_at) = self.last_zoom_change {
            let elapsed = zoom_changed_at.elapsed();
            if elapsed >= DEBOUNCE {
                // Compute needed resolution based on zoom level
                let needed_max = ((PREVIEW_MAX as f32 * self.zoom).ceil() as u32).max(PREVIEW_MAX);
                // Cap at original image dimensions
                let orig_max = self
                    .metadata
                    .as_ref()
                    .and_then(|m| match (m.width, m.height) {
                        (Some(w), Some(h)) => Some(w.max(h)),
                        _ => None,
                    })
                    .unwrap_or(u32::MAX);
                let needed_max = needed_max.min(orig_max);

                if needed_max > self.preview_max && !self.loading && !self.reloading_preview {
                    self.preview_max = needed_max;
                    self.reloading_preview = true;
                    self.last_zoom_change = None;
                    // Invalidate in-flight processing so stale results are discarded
                    self.requested_generation = self.requested_generation.wrapping_add(1);
                    self.in_flight_generation = None;

                    let tx = self.tx.clone();
                    let ctx2 = ui.ctx().clone();
                    let path = self.current_path.clone().unwrap();
                    let cap = self.preview_max;
                    std::thread::spawn(move || {
                        match crate::thumbnail::open_image_for_preview(&path) {
                            Ok(img) => {
                                let preview = if img.width() > cap || img.height() > cap {
                                    img.thumbnail(cap, cap)
                                } else {
                                    img
                                };
                                let _ = tx.send(BgResult::Loaded { path, img: preview });
                            }
                            Err(_) => {
                                let _ = tx.send(BgResult::LoadFailed(path));
                            }
                        }
                        ctx2.request_repaint();
                    });
                } else {
                    self.last_zoom_change = None;
                }
            } else {
                // Still waiting for debounce — schedule a repaint
                ui.ctx()
                    .request_repaint_after(DEBOUNCE.saturating_sub(elapsed));
            }
        }

        // Toolbar row
        ui.horizontal(|ui| {
            if ui.selectable_label(self.split_view, "Split view").clicked() {
                self.split_view = !self.split_view;
            }
            if ui.selectable_label(self.crop_mode, "Crop").clicked() {
                self.crop_mode = !self.crop_mode;
                if self.crop_mode {
                    // Enter crop mode: start with full image or existing applied crop
                    self.pending_crop = Some(self.edit_state.crop.clone().unwrap_or(Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 1.0,
                        height: 1.0,
                    }));
                } else {
                    // Exiting crop mode discards unapplied selection
                    self.pending_crop = None;
                    self.crop_drag = None;
                    self.crop_create_origin = None;
                }
            }

            if self.processing || self.reloading_preview {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spinner();
                });
            }
        });

        // Crop mode toolbar: aspect ratio + apply/cancel/reset
        if self.crop_mode {
            ui.horizontal(|ui| {
                ui.label("Aspect:");
                for aspect in CropAspect::ALL {
                    if ui
                        .selectable_label(self.crop_aspect == aspect, aspect.label())
                        .clicked()
                    {
                        self.crop_aspect = aspect;
                        let ratio = self.effective_crop_ratio();
                        if let Some(ref mut crop) = self.pending_crop {
                            constrain_aspect(crop, ratio);
                        }
                    }
                }
            });
            ui.horizontal(|ui| {
                let has_pending = self.pending_crop.is_some();
                let has_applied = self.edit_state.crop.is_some();

                if ui
                    .add_enabled(has_pending, egui::Button::new("Apply"))
                    .clicked()
                {
                    self.edit_state.crop = self.pending_crop.take();
                    self.crop_mode = false;
                    self.crop_drag = None;
                    self.needs_process = true;
                    self.last_slider_change = None;
                }
                if ui
                    .add_enabled(has_pending, egui::Button::new("Cancel"))
                    .clicked()
                {
                    self.pending_crop = None;
                    self.crop_drag = None;
                    self.crop_create_origin = None;
                }
                if ui
                    .add_enabled(has_applied, egui::Button::new("Reset"))
                    .clicked()
                {
                    self.edit_state.crop = None;
                    self.pending_crop = None;
                    self.crop_drag = None;
                    self.crop_create_origin = None;
                    self.needs_process = true;
                    self.last_slider_change = None;
                }
            });
        }
        ui.separator();

        // Build original texture lazily when split view is on
        if self.split_view {
            self.ensure_original_texture(ui.ctx());
        }

        let loading = self.loading;
        let texture = self.texture.clone();
        let original_texture = self.original_texture.clone();
        let split = self.split_view;

        let avail_w = ui.available_width();
        let img_max_h = ui.available_height().max(180.0);

        if loading || (self.processing && texture.is_none()) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Loading...");
            });
        } else if let Some(ref tex) = texture {
            if split {
                let half_w = (avail_w - ui.spacing().item_spacing.x) / 2.0;
                ui.horizontal(|ui| {
                    if let Some(ref orig) = original_texture {
                        draw_fitted_image(ui, orig, half_w, img_max_h, 1.0, egui::Vec2::ZERO);
                    } else {
                        ui.allocate_ui(egui::vec2(half_w, img_max_h), |ui| {
                            ui.centered_and_justified(|ui| {
                                ui.label("No original");
                            });
                        });
                    }
                    draw_fitted_image(ui, tex, half_w, img_max_h, 1.0, egui::Vec2::ZERO);
                });
            } else {
                if self.crop_mode {
                    // Disable zoom/pan while in crop mode
                    let img_rect =
                        draw_fitted_image(ui, tex, avail_w, img_max_h, 1.0, egui::Vec2::ZERO);
                    self.handle_crop_interaction(ui, img_rect);
                } else {
                    let zoom_before = self.zoom;
                    let img_rect =
                        draw_fitted_image(ui, tex, avail_w, img_max_h, self.zoom, self.pan_offset);

                    // Compute fit_size for clamping
                    let tex_size = tex.size_vec2();
                    let fit_scale = (avail_w / tex_size.x).min(img_max_h / tex_size.y);
                    let fit_size = tex_size * fit_scale;
                    let viewport_rect =
                        egui::Rect::from_center_size(img_rect.center() - self.pan_offset, fit_size);

                    // Single interaction widget for zoom/pan — only the hovered
                    // viewer responds to scroll, so stacked windows don't conflict.
                    let sense = if self.zoom > 1.0 {
                        egui::Sense::click_and_drag()
                    } else {
                        egui::Sense::click()
                    };
                    let resp = ui.interact(viewport_rect, ui.id().with("zoom_pan"), sense);

                    // Scroll-to-zoom and pinch-to-zoom (only when hovered)
                    if resp.hovered() {
                        // Mouse wheel zoom
                        let scroll_delta = ui.input(|i| i.raw_scroll_delta.y);
                        // Trackpad pinch zoom (egui reports as a multiplier, e.g. 1.02)
                        let pinch_delta = ui.input(|i| i.zoom_delta());

                        let old_zoom = self.zoom;
                        let new_zoom = if scroll_delta != 0.0 {
                            (self.zoom * (1.1_f32).powf(scroll_delta / 50.0)).clamp(1.0, 10.0)
                        } else if (pinch_delta - 1.0).abs() > 0.001 {
                            (self.zoom * pinch_delta).clamp(1.0, 10.0)
                        } else {
                            old_zoom
                        };

                        if new_zoom != old_zoom {
                            // Zoom toward cursor
                            if let Some(cursor_pos) = ui.input(|i| i.pointer.hover_pos()) {
                                let img_center = viewport_rect.center();
                                let rel =
                                    cursor_pos.to_vec2() - img_center.to_vec2() - self.pan_offset;
                                self.pan_offset += rel * (1.0 - new_zoom / old_zoom);
                            }
                            self.zoom = new_zoom;
                        }
                    }

                    // Drag-to-pan when zoomed in
                    if self.zoom > 1.0 {
                        if resp.dragged() {
                            self.pan_offset += resp.drag_delta();
                        }
                        // Double-click to reset zoom
                        if resp.double_clicked() {
                            self.zoom = 1.0;
                            self.pan_offset = egui::Vec2::ZERO;
                        }
                    }

                    // Clamp pan so image doesn't leave viewport excessively
                    let zoomed_size = fit_size * self.zoom;
                    let max_pan_x = ((zoomed_size.x - fit_size.x) / 2.0).max(0.0);
                    let max_pan_y = ((zoomed_size.y - fit_size.y) / 2.0).max(0.0);
                    self.pan_offset.x = self.pan_offset.x.clamp(-max_pan_x, max_pan_x);
                    self.pan_offset.y = self.pan_offset.y.clamp(-max_pan_y, max_pan_y);

                    // Track zoom changes for adaptive preview reload
                    if (self.zoom - zoom_before).abs() > 0.001 {
                        self.last_zoom_change = Some(Instant::now());
                    }
                }
            }
        } else {
            ui.allocate_ui(egui::vec2(avail_w, 40.0), |ui| {
                ui.label("Could not open image");
            });
        }

        // Bottom status bar: resolution + zoom control
        let zoom_before_bar = self.zoom;
        ui.horizontal(|ui| {
            // Preview resolution (from texture) / Original resolution (from EXIF)
            if let Some(ref tex) = texture {
                let size = tex.size();
                let preview_res = format!("{}x{}", size[0], size[1]);
                let orig_res = self
                    .metadata
                    .as_ref()
                    .and_then(|m| match (m.width, m.height) {
                        (Some(w), Some(h)) => Some(format!("{}x{}", w, h)),
                        _ => None,
                    });
                if let Some(orig) = orig_res {
                    ui.weak(format!("{} / {}", preview_res, orig));
                } else {
                    ui.weak(preview_res);
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !self.crop_mode {
                    // Zoom presets menu
                    let zoom_label = if (self.zoom - 1.0).abs() < 0.01 {
                        "Fit".to_string()
                    } else {
                        format!("{:.0}%", self.zoom * 100.0)
                    };
                    egui::ComboBox::from_id_salt(ui.id().with("zoom_combo"))
                        .selected_text(&zoom_label)
                        .width(60.0)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label((self.zoom - 1.0).abs() < 0.01, "Fit")
                                .clicked()
                            {
                                self.zoom = 1.0;
                                self.pan_offset = egui::Vec2::ZERO;
                            }
                            for &pct in &[200, 300, 500, 1000] {
                                let z = pct as f32 / 100.0;
                                let label = format!("{}%", pct);
                                if ui
                                    .selectable_label((self.zoom - z).abs() < 0.01, label)
                                    .clicked()
                                {
                                    self.pan_offset = egui::Vec2::ZERO;
                                    self.zoom = z;
                                }
                            }
                        });

                    // +/- buttons
                    if ui.small_button("+").clicked() {
                        self.zoom = (self.zoom * 1.25).clamp(1.0, 10.0);
                    }
                    if ui.small_button("\u{2212}").clicked() {
                        let new_zoom = (self.zoom / 1.25).clamp(1.0, 10.0);
                        if new_zoom < 1.01 {
                            self.zoom = 1.0;
                            self.pan_offset = egui::Vec2::ZERO;
                        } else {
                            self.zoom = new_zoom;
                        }
                    }
                }
            });
        });
        // Track zoom changes from status bar controls (+/-, presets)
        if (self.zoom - zoom_before_bar).abs() > 0.001 {
            self.last_zoom_change = Some(Instant::now());
        }
    }

    /// Handle crop drag interaction on the pending crop and draw the overlay.
    fn handle_crop_interaction(&mut self, ui: &mut egui::Ui, img_rect: egui::Rect) {
        // Single interaction widget — only the hovered viewer responds,
        // so stacked windows don't conflict.
        let crop_resp = ui.interact(
            img_rect,
            ui.id().with("crop_interact"),
            egui::Sense::click_and_drag(),
        );

        let pointer = ui.input(|i| i.pointer.clone());
        let aspect_ratio = self.effective_crop_ratio();
        let is_this_viewer = crop_resp.hovered() || crop_resp.dragged() || self.crop_drag.is_some();

        // Determine which crop rect to show and interact with.
        // If there's a pending crop, that takes priority for interaction.
        // If there's only an applied crop, show it with handles so the user
        // can grab it directly (auto-promotes to pending on click).
        let visible_crop: Option<Rect> = self
            .pending_crop
            .clone()
            .or_else(|| self.edit_state.crop.clone());
        let has_pending = self.pending_crop.is_some();

        if let Some(crop) = visible_crop {
            let crop_screen = norm_to_screen(&crop, img_rect);
            // Always draw interactive overlay (handles + thirds) for visible crop
            draw_crop_overlay(ui, img_rect, crop_screen, true);

            // Handle drag initiation — only for the interacted viewer
            if is_this_viewer {
                if let Some(pos) = pointer.interact_pos() {
                    if pointer.any_pressed() && self.crop_drag.is_none() && img_rect.contains(pos) {
                        let corners = corner_rects(crop_screen);
                        let mut target = None;
                        for (i, cr) in corners.iter().enumerate() {
                            if cr.contains(pos) {
                                target = Some(DragTarget::Corner(i as u8));
                                break;
                            }
                        }
                        if target.is_none() && crop_screen.contains(pos) {
                            target = Some(DragTarget::Interior);
                        }

                        if let Some(t) = target {
                            // Auto-promote applied crop to pending on grab
                            if !has_pending {
                                self.pending_crop = self.edit_state.crop.clone();
                            }
                            self.crop_drag = Some(t);
                            if matches!(t, DragTarget::Interior) {
                                self.crop_drag_start_pos = Some(screen_to_norm_pos(pos, img_rect));
                                self.crop_drag_start_rect = self.pending_crop.clone();
                            }
                        }
                    }
                }
            }

            // Handle ongoing drag
            if let Some(drag) = self.crop_drag {
                if pointer.any_down() {
                    if let Some(pos) = pointer.interact_pos() {
                        let norm = screen_to_norm_pos(pos, img_rect);
                        let nx = norm.x.clamp(0.0, 1.0);
                        let ny = norm.y.clamp(0.0, 1.0);

                        match drag {
                            DragTarget::Corner(idx) => {
                                if let Some(ref mut pc) = self.pending_crop {
                                    resize_from_corner(pc, idx, nx, ny, aspect_ratio);
                                }
                            }
                            DragTarget::Interior => {
                                if let (Some(start), Some(start_rect)) =
                                    (self.crop_drag_start_pos, &self.crop_drag_start_rect)
                                {
                                    let dx = nx - start.x;
                                    let dy = ny - start.y;
                                    if let Some(ref mut pc) = self.pending_crop {
                                        pc.x =
                                            (start_rect.x + dx).clamp(0.0, 1.0 - start_rect.width);
                                        pc.y =
                                            (start_rect.y + dy).clamp(0.0, 1.0 - start_rect.height);
                                    }
                                }
                            }
                        }
                    }
                } else {
                    self.crop_drag = None;
                    self.crop_drag_start_pos = None;
                    self.crop_drag_start_rect = None;
                }
            }
        } else if is_this_viewer {
            // No crop at all — drag to create a new pending one (only for this viewer)
            if crop_resp.drag_started() {
                if let Some(origin) = pointer.interact_pos() {
                    let n = screen_to_norm_pos(origin, img_rect);
                    self.crop_create_origin =
                        Some(egui::pos2(n.x.clamp(0.0, 1.0), n.y.clamp(0.0, 1.0)));
                    self.pending_crop = Some(Rect {
                        x: n.x.clamp(0.0, 1.0),
                        y: n.y.clamp(0.0, 1.0),
                        width: 0.0,
                        height: 0.0,
                    });
                }
            }
            if crop_resp.dragged() {
                if let (Some(pos), Some(origin)) = (pointer.interact_pos(), self.crop_create_origin)
                {
                    if let Some(ref mut crop) = self.pending_crop {
                        let n = screen_to_norm_pos(pos, img_rect);
                        let nx = n.x.clamp(0.0, 1.0);
                        let ny = n.y.clamp(0.0, 1.0);
                        let new_x = origin.x.min(nx);
                        let new_y = origin.y.min(ny);
                        crop.x = new_x;
                        crop.y = new_y;
                        crop.width = (origin.x.max(nx) - new_x).min(1.0 - new_x);
                        crop.height = (origin.y.max(ny) - new_y).min(1.0 - new_y);
                        if let Some(ratio) = aspect_ratio {
                            constrain_aspect(crop, Some(ratio));
                        }
                    }
                }
            }
            if crop_resp.drag_stopped() {
                self.crop_create_origin = None;
                if let Some(ref crop) = self.pending_crop {
                    if crop.width < 0.01 || crop.height < 0.01 {
                        self.pending_crop = None;
                    }
                }
            }
        }
    }

    /// Renders the editing controls panel for the current image.
    pub fn show_controls(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("controls_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.show_crop_section(ui);

                ui.separator();

                show_transform_section(
                    ui,
                    &mut self.edit_state,
                    &mut self.needs_process,
                    &mut self.last_slider_change,
                );

                ui.separator();

                show_color_section(
                    ui,
                    &mut self.edit_state,
                    &mut self.needs_process,
                    &mut self.last_slider_change,
                );

                ui.separator();

                if let Some(ref meta) = self.metadata {
                    show_exif(ui, meta);
                } else {
                    ui.label(egui::RichText::new("No EXIF data").weak());
                }
            });
    }

    fn show_crop_section(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Crop").strong());
        ui.add_space(4.0);

        // Aspect ratio selector
        ui.horizontal(|ui| {
            ui.label("Aspect");
            for aspect in CropAspect::ALL {
                if ui
                    .selectable_label(self.crop_aspect == aspect, aspect.label())
                    .clicked()
                {
                    self.crop_aspect = aspect;
                    let ratio = self.effective_crop_ratio();
                    if let Some(ref mut crop) = self.pending_crop {
                        constrain_aspect(crop, ratio);
                    }
                }
            }
        });

        // Show sliders for the pending crop if it exists
        if let Some(ref mut crop) = self.pending_crop {
            ui.horizontal(|ui| {
                ui.label("X");
                ui.add(
                    egui::Slider::new(&mut crop.x, 0.0_f32..=1.0_f32)
                        .fixed_decimals(3)
                        .clamping(egui::SliderClamping::Always),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Y");
                ui.add(
                    egui::Slider::new(&mut crop.y, 0.0_f32..=1.0_f32)
                        .fixed_decimals(3)
                        .clamping(egui::SliderClamping::Always),
                );
            });
            ui.horizontal(|ui| {
                ui.label("W");
                ui.add(
                    egui::Slider::new(&mut crop.width, 0.01_f32..=1.0_f32)
                        .fixed_decimals(3)
                        .clamping(egui::SliderClamping::Always),
                );
            });
            ui.horizontal(|ui| {
                ui.label("H");
                ui.add(
                    egui::Slider::new(&mut crop.height, 0.01_f32..=1.0_f32)
                        .fixed_decimals(3)
                        .clamping(egui::SliderClamping::Always),
                );
            });
            // Clamp position so rect stays in bounds
            crop.x = crop.x.min(1.0 - crop.width);
            crop.y = crop.y.min(1.0 - crop.height);

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    self.edit_state.crop = self.pending_crop.take();
                    self.crop_mode = false;
                    self.crop_drag = None;
                    self.needs_process = true;
                    self.last_slider_change = None;
                }
                if ui.button("Cancel").clicked() {
                    self.pending_crop = None;
                    self.crop_mode = false;
                    self.crop_drag = None;
                    self.crop_create_origin = None;
                }
            });
        } else if self.edit_state.crop.is_some() {
            // Show read-only info about the applied crop
            if let Some(ref crop) = self.edit_state.crop {
                ui.label(format!(
                    "Applied: {:.1}% x {:.1}% at ({:.1}%, {:.1}%)",
                    crop.width * 100.0,
                    crop.height * 100.0,
                    crop.x * 100.0,
                    crop.y * 100.0,
                ));
            }
            ui.horizontal(|ui| {
                if ui.button("Edit").clicked() {
                    self.pending_crop = self.edit_state.crop.clone();
                    self.crop_mode = true;
                }
                if ui.button("Reset").clicked() {
                    self.edit_state.crop = None;
                    self.needs_process = true;
                    self.last_slider_change = None;
                }
            });
        }
    }
}

fn bump_requested_generation_for_pending_changes(
    processing: bool,
    needs_process: bool,
    in_flight_generation: Option<u64>,
    requested_generation: &mut u64,
) {
    if processing && needs_process && in_flight_generation == Some(*requested_generation) {
        *requested_generation = requested_generation.wrapping_add(1);
    }
}

fn process_preview_with_backend(
    source: &DynamicImage,
    state: &EditState,
    preview_backend: PreviewBackend,
) -> DynamicImage {
    process_preview_with_backend_and_gpu_hook(
        source,
        state,
        preview_backend,
        crate::processing::gpu_spike::try_apply,
    )
}

fn process_preview_with_backend_and_gpu_hook<F>(
    source: &DynamicImage,
    state: &EditState,
    preview_backend: PreviewBackend,
    gpu_apply: F,
) -> DynamicImage
where
    F: Fn(&DynamicImage, &EditState) -> Option<DynamicImage>,
{
    match preview_backend {
        PreviewBackend::Cpu => crate::processing::transform::apply(source, state),
        PreviewBackend::Auto | PreviewBackend::GpuSpike => gpu_apply(source, state)
            .unwrap_or_else(|| crate::processing::transform::apply(source, state)),
    }
}

fn downscale_for_interactive(img: DynamicImage) -> DynamicImage {
    if img.width() > INTERACTIVE_PREVIEW_MAX || img.height() > INTERACTIVE_PREVIEW_MAX {
        img.thumbnail(INTERACTIVE_PREVIEW_MAX, INTERACTIVE_PREVIEW_MAX)
    } else {
        img
    }
}

fn source_signature(path: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.to_string_lossy().hash(&mut hasher);
    if let Ok(meta) = std::fs::metadata(path) {
        meta.len().hash(&mut hasher);
        let modified_nanos = meta
            .modified()
            .ok()
            .and_then(|ts| ts.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        modified_nanos.hash(&mut hasher);
    }
    hasher.finish()
}

fn edit_state_signature(state: &EditState) -> u64 {
    match serde_json::to_vec(state) {
        Ok(bytes) => {
            let mut hasher = DefaultHasher::new();
            bytes.hash(&mut hasher);
            hasher.finish()
        }
        Err(_) => 0,
    }
}

// ---------------------------------------------------------------------------
// Coordinate conversion helpers
// ---------------------------------------------------------------------------

fn norm_to_screen(crop: &Rect, img_rect: egui::Rect) -> egui::Rect {
    let min = egui::pos2(
        img_rect.min.x + crop.x * img_rect.width(),
        img_rect.min.y + crop.y * img_rect.height(),
    );
    let max = egui::pos2(
        min.x + crop.width * img_rect.width(),
        min.y + crop.height * img_rect.height(),
    );
    egui::Rect::from_min_max(min, max)
}

fn screen_to_norm_pos(pos: egui::Pos2, img_rect: egui::Rect) -> egui::Pos2 {
    egui::pos2(
        (pos.x - img_rect.min.x) / img_rect.width(),
        (pos.y - img_rect.min.y) / img_rect.height(),
    )
}

fn corner_rects(crop_screen: egui::Rect) -> [egui::Rect; 4] {
    let hs = HANDLE_SIZE;
    let corners = [
        crop_screen.left_top(),
        crop_screen.right_top(),
        crop_screen.right_bottom(),
        crop_screen.left_bottom(),
    ];
    corners.map(|c| egui::Rect::from_center_size(c, egui::vec2(hs * 3.0, hs * 3.0)))
}

// ---------------------------------------------------------------------------
// Crop geometry
// ---------------------------------------------------------------------------

fn resize_from_corner(crop: &mut Rect, corner: u8, nx: f32, ny: f32, aspect: Option<f32>) {
    let (x1, y1, x2, y2) = (crop.x, crop.y, crop.x + crop.width, crop.y + crop.height);
    let (mut new_x1, mut new_y1, mut new_x2, mut new_y2) = match corner {
        0 => (nx, ny, x2, y2), // TL
        1 => (x1, ny, nx, y2), // TR
        2 => (x1, y1, nx, ny), // BR
        3 => (nx, y1, x2, ny), // BL
        _ => return,
    };

    if (new_x2 - new_x1).abs() < 0.01 || (new_y2 - new_y1).abs() < 0.01 {
        return;
    }

    if new_x1 > new_x2 {
        std::mem::swap(&mut new_x1, &mut new_x2);
    }
    if new_y1 > new_y2 {
        std::mem::swap(&mut new_y1, &mut new_y2);
    }

    new_x1 = new_x1.clamp(0.0, 1.0);
    new_y1 = new_y1.clamp(0.0, 1.0);
    new_x2 = new_x2.clamp(0.0, 1.0);
    new_y2 = new_y2.clamp(0.0, 1.0);

    crop.x = new_x1;
    crop.y = new_y1;
    crop.width = new_x2 - new_x1;
    crop.height = new_y2 - new_y1;

    if let Some(ratio) = aspect {
        constrain_aspect(crop, Some(ratio));
    }
}

fn constrain_aspect(crop: &mut Rect, ratio: Option<f32>) {
    let Some(ratio) = ratio else { return };
    if crop.height < 0.001 {
        return;
    }
    let current = crop.width / crop.height;
    if (current - ratio).abs() < 0.001 {
        return;
    }
    let cx = crop.x + crop.width / 2.0;
    let cy = crop.y + crop.height / 2.0;
    let (new_w, new_h) = if current > ratio {
        (crop.height * ratio, crop.height)
    } else {
        (crop.width, crop.width / ratio)
    };
    crop.width = new_w.min(1.0);
    crop.height = new_h.min(1.0);
    crop.x = (cx - crop.width / 2.0).clamp(0.0, 1.0 - crop.width);
    crop.y = (cy - crop.height / 2.0).clamp(0.0, 1.0 - crop.height);
}

// ---------------------------------------------------------------------------
// Drawing helpers
// ---------------------------------------------------------------------------

fn draw_fitted_image(
    ui: &mut egui::Ui,
    tex: &egui::TextureHandle,
    max_w: f32,
    max_h: f32,
    zoom: f32,
    pan_offset: egui::Vec2,
) -> egui::Rect {
    let tex_size = tex.size_vec2();
    let fit_scale = (max_w / tex_size.x).min(max_h / tex_size.y);
    let fit_size = tex_size * fit_scale;

    // Allocate the fit-sized area (layout stays stable regardless of zoom)
    let (viewport_rect, _) = ui.allocate_exact_size(fit_size, egui::Sense::hover());

    // The zoomed image rect, centered in the viewport and shifted by pan
    let zoomed_size = fit_size * zoom;
    let center = viewport_rect.center() + pan_offset;
    let img_rect = egui::Rect::from_center_size(center, zoomed_size);

    // Compute UV rect: which portion of the texture is visible within the viewport
    // Map viewport edges to UV coordinates relative to img_rect
    let uv_min_x = (viewport_rect.min.x - img_rect.min.x) / img_rect.width();
    let uv_min_y = (viewport_rect.min.y - img_rect.min.y) / img_rect.height();
    let uv_max_x = (viewport_rect.max.x - img_rect.min.x) / img_rect.width();
    let uv_max_y = (viewport_rect.max.y - img_rect.min.y) / img_rect.height();

    let uv = egui::Rect::from_min_max(
        egui::pos2(uv_min_x.max(0.0), uv_min_y.max(0.0)),
        egui::pos2(uv_max_x.min(1.0), uv_max_y.min(1.0)),
    );

    // The screen rect where we actually paint (intersection of img_rect and viewport)
    let paint_rect = egui::Rect::from_min_max(
        egui::pos2(
            viewport_rect.min.x.max(img_rect.min.x),
            viewport_rect.min.y.max(img_rect.min.y),
        ),
        egui::pos2(
            viewport_rect.max.x.min(img_rect.max.x),
            viewport_rect.max.y.min(img_rect.max.y),
        ),
    );

    ui.painter()
        .image(tex.id(), paint_rect, uv, egui::Color32::WHITE);

    // Return the full img_rect (not clipped) so callers can map coordinates correctly
    img_rect
}

/// Draw the crop overlay. `interactive` controls handle visibility:
/// true for the pending (editable) crop, false for the applied (read-only) crop.
fn draw_crop_overlay(
    ui: &mut egui::Ui,
    img_rect: egui::Rect,
    crop_screen: egui::Rect,
    interactive: bool,
) {
    let painter = ui.painter();
    let dim = egui::Color32::from_black_alpha(if interactive { 120 } else { 80 });

    // Darken outside crop — four strips
    painter.rect_filled(
        egui::Rect::from_min_max(
            img_rect.left_top(),
            egui::pos2(img_rect.right(), crop_screen.top()),
        ),
        0.0,
        dim,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(img_rect.left(), crop_screen.bottom()),
            img_rect.right_bottom(),
        ),
        0.0,
        dim,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(img_rect.left(), crop_screen.top()),
            egui::pos2(crop_screen.left(), crop_screen.bottom()),
        ),
        0.0,
        dim,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(crop_screen.right(), crop_screen.top()),
            egui::pos2(img_rect.right(), crop_screen.bottom()),
        ),
        0.0,
        dim,
    );

    // Border
    let border_color = if interactive {
        egui::Color32::WHITE
    } else {
        egui::Color32::from_white_alpha(160)
    };
    painter.rect_stroke(
        crop_screen,
        0.0,
        egui::Stroke::new(1.5, border_color),
        egui::StrokeKind::Middle,
    );

    if interactive {
        // Corner handles
        let hs = HANDLE_SIZE;
        let corners = [
            crop_screen.left_top(),
            crop_screen.right_top(),
            crop_screen.right_bottom(),
            crop_screen.left_bottom(),
        ];
        for c in &corners {
            painter.rect_filled(
                egui::Rect::from_center_size(*c, egui::vec2(hs, hs)),
                0.0,
                egui::Color32::WHITE,
            );
        }

        // Rule of thirds
        let third_stroke = egui::Stroke::new(0.5, egui::Color32::from_white_alpha(120));
        for i in 1..3 {
            let t = i as f32 / 3.0;
            let x = crop_screen.left() + t * crop_screen.width();
            let y = crop_screen.top() + t * crop_screen.height();
            painter.line_segment(
                [
                    egui::pos2(x, crop_screen.top()),
                    egui::pos2(x, crop_screen.bottom()),
                ],
                third_stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(crop_screen.left(), y),
                    egui::pos2(crop_screen.right(), y),
                ],
                third_stroke,
            );
        }
    }
}

fn show_transform_section(
    ui: &mut egui::Ui,
    state: &mut EditState,
    needs_process: &mut bool,
    last_slider_change: &mut Option<Instant>,
) {
    ui.label(egui::RichText::new("Transform").strong());
    ui.add_space(4.0);

    // Rotate
    ui.horizontal(|ui| {
        ui.label("Rotate");
        if ui.button("◀ 90°").clicked() {
            state.rotate = (state.rotate - 90).rem_euclid(360);
            *needs_process = true;
            *last_slider_change = None;
        }
        if ui.button("180°").clicked() {
            state.rotate = (state.rotate + 180).rem_euclid(360);
            *needs_process = true;
            *last_slider_change = None;
        }
        if ui.button("90° ▶").clicked() {
            state.rotate = (state.rotate + 90).rem_euclid(360);
            *needs_process = true;
            *last_slider_change = None;
        }
        if state.rotate != 0 {
            ui.weak(format!("({}°)", state.rotate));
        }
    });

    // Flip
    ui.horizontal(|ui| {
        ui.label("Flip");
        let flip_h = ui.selectable_label(state.flip_h, "↔ H");
        if flip_h.clicked() {
            state.flip_h = !state.flip_h;
            *needs_process = true;
            *last_slider_change = None;
        }
        let flip_v = ui.selectable_label(state.flip_v, "↕ V");
        if flip_v.clicked() {
            state.flip_v = !state.flip_v;
            *needs_process = true;
            *last_slider_change = None;
        }
    });

    // Straighten
    ui.horizontal(|ui| {
        ui.label("Straighten");
        let resp = ui.add(
            egui::Slider::new(&mut state.straighten, -15.0_f32..=15.0_f32)
                .suffix("°")
                .fixed_decimals(1)
                .clamping(egui::SliderClamping::Always),
        );
        if resp.changed() {
            *needs_process = true;
            *last_slider_change = Some(Instant::now());
        }
        if state.straighten != 0.0 && ui.small_button("↺").clicked() {
            state.straighten = 0.0;
            *needs_process = true;
            *last_slider_change = None;
        }
    });

    // Keystone — Vertical
    ui.horizontal(|ui| {
        ui.label("Vertical");
        let resp = ui.add(
            egui::Slider::new(&mut state.keystone.vertical, -0.5_f32..=0.5_f32)
                .fixed_decimals(2)
                .clamping(egui::SliderClamping::Always),
        );
        if resp.changed() {
            *needs_process = true;
            *last_slider_change = Some(Instant::now());
        }
        if state.keystone.vertical != 0.0 && ui.small_button("↺").clicked() {
            state.keystone.vertical = 0.0;
            *needs_process = true;
            *last_slider_change = None;
        }
    });

    // Keystone — Horizontal
    ui.horizontal(|ui| {
        ui.label("Horizontal");
        let resp = ui.add(
            egui::Slider::new(&mut state.keystone.horizontal, -0.5_f32..=0.5_f32)
                .fixed_decimals(2)
                .clamping(egui::SliderClamping::Always),
        );
        if resp.changed() {
            *needs_process = true;
            *last_slider_change = Some(Instant::now());
        }
        if state.keystone.horizontal != 0.0 && ui.small_button("↺").clicked() {
            state.keystone.horizontal = 0.0;
            *needs_process = true;
            *last_slider_change = None;
        }
    });

    // Reset all
    let dirty = state.rotate != 0
        || state.flip_h
        || state.flip_v
        || state.straighten != 0.0
        || state.keystone.vertical != 0.0
        || state.keystone.horizontal != 0.0;
    if dirty {
        ui.add_space(4.0);
        if ui.small_button("Reset transforms").clicked() {
            state.rotate = 0;
            state.flip_h = false;
            state.flip_v = false;
            state.straighten = 0.0;
            state.keystone.vertical = 0.0;
            state.keystone.horizontal = 0.0;
            *needs_process = true;
            *last_slider_change = None;
        }
    }
}

fn show_color_section(
    ui: &mut egui::Ui,
    state: &mut EditState,
    needs_process: &mut bool,
    last_slider_change: &mut Option<Instant>,
) {
    ui.label(egui::RichText::new("Color").strong());
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        ui.label("Exposure");
        let resp = ui.add(
            egui::Slider::new(&mut state.exposure, -3.0_f32..=3.0_f32)
                .suffix(" EV")
                .fixed_decimals(2)
                .clamping(egui::SliderClamping::Always),
        );
        if resp.changed() {
            *needs_process = true;
            *last_slider_change = Some(Instant::now());
        }
        if state.exposure != 0.0 && ui.small_button("↺").clicked() {
            state.exposure = 0.0;
            *needs_process = true;
            *last_slider_change = None;
        }
    });

    ui.horizontal(|ui| {
        ui.label("Contrast");
        let resp = ui.add(
            egui::Slider::new(&mut state.contrast, -1.0_f32..=1.0_f32)
                .fixed_decimals(2)
                .clamping(egui::SliderClamping::Always),
        );
        if resp.changed() {
            *needs_process = true;
            *last_slider_change = Some(Instant::now());
        }
        if state.contrast != 0.0 && ui.small_button("↺").clicked() {
            state.contrast = 0.0;
            *needs_process = true;
            *last_slider_change = None;
        }
    });

    ui.horizontal(|ui| {
        ui.label("Highlights");
        let resp = ui.add(
            egui::Slider::new(&mut state.highlights, -1.0_f32..=1.0_f32)
                .fixed_decimals(2)
                .clamping(egui::SliderClamping::Always),
        );
        if resp.changed() {
            *needs_process = true;
            *last_slider_change = Some(Instant::now());
        }
        if state.highlights != 0.0 && ui.small_button("↺").clicked() {
            state.highlights = 0.0;
            *needs_process = true;
            *last_slider_change = None;
        }
    });

    ui.horizontal(|ui| {
        ui.label("Shadows");
        let resp = ui.add(
            egui::Slider::new(&mut state.shadows, -1.0_f32..=1.0_f32)
                .fixed_decimals(2)
                .clamping(egui::SliderClamping::Always),
        );
        if resp.changed() {
            *needs_process = true;
            *last_slider_change = Some(Instant::now());
        }
        if state.shadows != 0.0 && ui.small_button("↺").clicked() {
            state.shadows = 0.0;
            *needs_process = true;
            *last_slider_change = None;
        }
    });

    ui.horizontal(|ui| {
        ui.label("Sharpness");
        let resp = ui.add(
            egui::Slider::new(&mut state.sharpness, 0.0_f32..=2.0_f32)
                .fixed_decimals(2)
                .clamping(egui::SliderClamping::Always),
        );
        if resp.changed() {
            *needs_process = true;
            *last_slider_change = Some(Instant::now());
        }
        if state.sharpness != 0.0 && ui.small_button("↺").clicked() {
            state.sharpness = 0.0;
            *needs_process = true;
            *last_slider_change = None;
        }
    });

    ui.horizontal(|ui| {
        ui.label("Temperature");
        let resp = ui.add(
            egui::Slider::new(&mut state.temperature, -1.0_f32..=1.0_f32)
                .fixed_decimals(2)
                .clamping(egui::SliderClamping::Always),
        );
        if resp.changed() {
            *needs_process = true;
            *last_slider_change = Some(Instant::now());
        }
        if state.temperature != 0.0 && ui.small_button("↺").clicked() {
            state.temperature = 0.0;
            *needs_process = true;
            *last_slider_change = None;
        }
    });

    ui.horizontal(|ui| {
        ui.label("Saturation");
        let resp = ui.add(
            egui::Slider::new(&mut state.saturation, -1.0_f32..=1.0_f32)
                .fixed_decimals(2)
                .clamping(egui::SliderClamping::Always),
        );
        if resp.changed() {
            *needs_process = true;
            *last_slider_change = Some(Instant::now());
        }
        if state.saturation != 0.0 && ui.small_button("↺").clicked() {
            state.saturation = 0.0;
            *needs_process = true;
            *last_slider_change = None;
        }
    });

    ui.horizontal(|ui| {
        ui.label("Hue Shift");
        let resp = ui.add(
            egui::Slider::new(&mut state.hue_shift, -180.0_f32..=180.0_f32)
                .suffix("°")
                .fixed_decimals(1)
                .clamping(egui::SliderClamping::Always),
        );
        if resp.changed() {
            *needs_process = true;
            *last_slider_change = Some(Instant::now());
        }
        if state.hue_shift != 0.0 && ui.small_button("↺").clicked() {
            state.hue_shift = 0.0;
            *needs_process = true;
            *last_slider_change = None;
        }
    });

    ui.add_space(6.0);
    ui.label(egui::RichText::new("Selective Color").strong());
    const HUE_LABELS: [&str; 8] = [
        "Red", "Orange", "Yellow", "Green", "Cyan", "Blue", "Purple", "Pink",
    ];
    for (idx, label) in HUE_LABELS.iter().enumerate() {
        let adj = &mut state.selective_color[idx];
        let base = selective_base_color(idx);
        let bg = selective_bg_color(base);
        let label_color = selective_label_color(base);
        egui::Frame::group(ui.style()).fill(bg).show(ui, |ui| {
            egui::CollapsingHeader::new(egui::RichText::new(*label).strong().color(label_color))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Hue");
                        let resp = ui.add(
                            egui::Slider::new(&mut adj.hue, -45.0_f32..=45.0_f32)
                                .suffix("°")
                                .fixed_decimals(1)
                                .clamping(egui::SliderClamping::Always),
                        );
                        if resp.changed() {
                            *needs_process = true;
                            *last_slider_change = Some(Instant::now());
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Saturation");
                        let resp = ui.add(
                            egui::Slider::new(&mut adj.saturation, -1.0_f32..=1.0_f32)
                                .fixed_decimals(2)
                                .clamping(egui::SliderClamping::Always),
                        );
                        if resp.changed() {
                            *needs_process = true;
                            *last_slider_change = Some(Instant::now());
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Lightness");
                        let resp = ui.add(
                            egui::Slider::new(&mut adj.lightness, -1.0_f32..=1.0_f32)
                                .fixed_decimals(2)
                                .clamping(egui::SliderClamping::Always),
                        );
                        if resp.changed() {
                            *needs_process = true;
                            *last_slider_change = Some(Instant::now());
                        }
                    });
                });
        });
        ui.add_space(4.0);
    }

    ui.add_space(6.0);
    ui.label(egui::RichText::new("Graduated Filter").strong());
    let mut grad_enabled = state.graduated_filter.is_some();
    if ui.checkbox(&mut grad_enabled, "Enable").changed() {
        if grad_enabled {
            if state.graduated_filter.is_none() {
                state.graduated_filter = Some(GradFilter {
                    top: 0.0,
                    bottom: 0.6,
                    exposure: -0.7,
                });
            }
        } else {
            state.graduated_filter = None;
        }
        *needs_process = true;
        *last_slider_change = None;
    }

    if let Some(ref mut grad) = state.graduated_filter {
        ui.horizontal(|ui| {
            ui.label("Top");
            let resp = ui.add(
                egui::Slider::new(&mut grad.top, 0.0_f32..=1.0_f32)
                    .fixed_decimals(2)
                    .clamping(egui::SliderClamping::Always),
            );
            if resp.changed() {
                *needs_process = true;
                *last_slider_change = Some(Instant::now());
            }
        });
        ui.horizontal(|ui| {
            ui.label("Bottom");
            let resp = ui.add(
                egui::Slider::new(&mut grad.bottom, 0.0_f32..=1.0_f32)
                    .fixed_decimals(2)
                    .clamping(egui::SliderClamping::Always),
            );
            if resp.changed() {
                *needs_process = true;
                *last_slider_change = Some(Instant::now());
            }
        });
        if grad.bottom < grad.top + 0.01 {
            grad.bottom = (grad.top + 0.01).min(1.0);
        }
        if grad.top > grad.bottom - 0.01 {
            grad.top = (grad.bottom - 0.01).max(0.0);
        }
        ui.horizontal(|ui| {
            ui.label("Exposure");
            let resp = ui.add(
                egui::Slider::new(&mut grad.exposure, -3.0_f32..=3.0_f32)
                    .suffix(" EV")
                    .fixed_decimals(2)
                    .clamping(egui::SliderClamping::Always),
            );
            if resp.changed() {
                *needs_process = true;
                *last_slider_change = Some(Instant::now());
            }
            if grad.exposure != 0.0 && ui.small_button("↺").clicked() {
                grad.exposure = 0.0;
                *needs_process = true;
                *last_slider_change = None;
            }
        });
    }

    let selective_dirty = state.selective_color.iter().any(|adj| {
        adj.hue.abs() > 0.001 || adj.saturation.abs() > 0.001 || adj.lightness.abs() > 0.001
    });
    let color_dirty = state.exposure != 0.0
        || state.contrast != 0.0
        || state.highlights != 0.0
        || state.shadows != 0.0
        || state.temperature != 0.0
        || state.saturation != 0.0
        || state.hue_shift != 0.0
        || selective_dirty
        || state.graduated_filter.is_some();
    if color_dirty {
        ui.add_space(4.0);
        if ui.small_button("Reset color").clicked() {
            state.exposure = 0.0;
            state.contrast = 0.0;
            state.highlights = 0.0;
            state.shadows = 0.0;
            state.temperature = 0.0;
            state.saturation = 0.0;
            state.hue_shift = 0.0;
            state.selective_color = Default::default();
            state.graduated_filter = None;
            *needs_process = true;
            *last_slider_change = None;
        }
    }
}

fn selective_base_color(idx: usize) -> egui::Color32 {
    match idx {
        0 => egui::Color32::from_rgb(220, 64, 64),   // Red
        1 => egui::Color32::from_rgb(226, 140, 55),  // Orange
        2 => egui::Color32::from_rgb(224, 197, 67),  // Yellow
        3 => egui::Color32::from_rgb(74, 170, 86),   // Green
        4 => egui::Color32::from_rgb(70, 176, 195),  // Cyan
        5 => egui::Color32::from_rgb(72, 120, 220),  // Blue
        6 => egui::Color32::from_rgb(145, 98, 208),  // Purple
        7 => egui::Color32::from_rgb(216, 102, 168), // Pink
        _ => egui::Color32::GRAY,
    }
}

fn selective_bg_color(base: egui::Color32) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 48)
}

fn selective_label_color(base: egui::Color32) -> egui::Color32 {
    let luminance = 0.2126 * base.r() as f32 + 0.7152 * base.g() as f32 + 0.0722 * base.b() as f32;
    if luminance > 160.0 {
        egui::Color32::BLACK
    } else {
        egui::Color32::WHITE
    }
}

fn show_exif(ui: &mut egui::Ui, meta: &crate::metadata::ImageMetadata) {
    ui.label(egui::RichText::new("EXIF").strong());
    ui.add_space(4.0);
    egui::Grid::new("exif_grid")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            let mut row = |label: &str, value: Option<String>| {
                if let Some(v) = value {
                    ui.label(egui::RichText::new(label).weak());
                    ui.label(v);
                    ui.end_row();
                }
            };

            let camera = match (&meta.camera_make, &meta.camera_model) {
                (Some(make), Some(model)) => Some(format!("{} {}", make, model)),
                (Some(make), None) => Some(make.clone()),
                (None, Some(model)) => Some(model.clone()),
                _ => None,
            };

            row("Camera", camera);
            row("Lens", meta.lens.clone());
            row("Date", meta.date_taken.clone());
            row("Shutter", meta.shutter_speed.clone());
            row("Aperture", meta.aperture.clone());
            row("ISO", meta.iso.map(|v| v.to_string()));
            row("Focal length", meta.focal_length.clone());
        });
}

#[cfg(test)]
mod tests {
    use image::{DynamicImage, ImageBuffer, Rgba};
    use std::path::Path;

    use super::{
        INTERACTIVE_PREVIEW_MAX, PreviewBackend, bump_requested_generation_for_pending_changes,
        downscale_for_interactive, edit_state_signature, process_preview_with_backend,
        process_preview_with_backend_and_gpu_hook, source_signature,
    };
    use crate::state::EditState;

    #[test]
    fn bumps_generation_when_pending_changes_arrive_during_processing() {
        let mut requested = 4_u64;
        bump_requested_generation_for_pending_changes(true, true, Some(4), &mut requested);
        assert_eq!(requested, 5);
    }

    #[test]
    fn does_not_bump_generation_when_processing_is_idle() {
        let mut requested = 7_u64;
        bump_requested_generation_for_pending_changes(false, true, Some(7), &mut requested);
        assert_eq!(requested, 7);
    }

    #[test]
    fn does_not_bump_generation_more_than_once_for_same_inflight_job() {
        let mut requested = 9_u64;
        bump_requested_generation_for_pending_changes(true, true, Some(9), &mut requested);
        bump_requested_generation_for_pending_changes(true, true, Some(9), &mut requested);
        assert_eq!(requested, 10);
    }

    #[test]
    fn downscale_for_interactive_reduces_long_edge() {
        let img = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
            INTERACTIVE_PREVIEW_MAX * 2,
            INTERACTIVE_PREVIEW_MAX,
            Rgba([255, 0, 0, 255]),
        ));
        let out = downscale_for_interactive(img);
        assert!(out.width() <= INTERACTIVE_PREVIEW_MAX);
        assert!(out.height() <= INTERACTIVE_PREVIEW_MAX);
    }

    #[test]
    fn downscale_for_interactive_keeps_small_images() {
        let img = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
            INTERACTIVE_PREVIEW_MAX / 2,
            INTERACTIVE_PREVIEW_MAX / 2,
            Rgba([255, 0, 0, 255]),
        ));
        let out = downscale_for_interactive(img);
        assert_eq!(out.width(), INTERACTIVE_PREVIEW_MAX / 2);
        assert_eq!(out.height(), INTERACTIVE_PREVIEW_MAX / 2);
    }

    #[test]
    fn unsupported_state_in_auto_mode_falls_back_to_cpu() {
        let img = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(4, 4, Rgba([32, 64, 128, 255])));
        let mut state = EditState::default();
        state.rotate = 90;

        let expected = crate::processing::transform::apply(&img, &state);
        let out = process_preview_with_backend(&img, &state, PreviewBackend::Auto);
        assert_eq!(out.to_rgba8().into_raw(), expected.to_rgba8().into_raw());
    }

    #[test]
    fn auto_mode_falls_back_to_cpu_when_gpu_is_unavailable() {
        let img = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(4, 4, Rgba([80, 90, 100, 255])));
        let mut state = EditState::default();
        state.exposure = 0.3;
        state.contrast = 0.2;

        let expected = crate::processing::transform::apply(&img, &state);
        let out = process_preview_with_backend_and_gpu_hook(
            &img,
            &state,
            PreviewBackend::Auto,
            |_source, _state| None,
        );
        assert_eq!(out.to_rgba8().into_raw(), expected.to_rgba8().into_raw());
    }

    #[test]
    fn cpu_mode_bypasses_gpu_hook() {
        let img = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(3, 3, Rgba([20, 30, 40, 255])));
        let mut state = EditState::default();
        state.exposure = 0.4;

        let expected = crate::processing::transform::apply(&img, &state);
        let out = process_preview_with_backend_and_gpu_hook(
            &img,
            &state,
            PreviewBackend::Cpu,
            |_source, _state| panic!("gpu path should not be called in cpu mode"),
        );
        assert_eq!(out.to_rgba8().into_raw(), expected.to_rgba8().into_raw());
    }

    #[test]
    fn edit_state_signature_changes_when_edit_changes() {
        let base = EditState::default();
        let mut changed = EditState::default();
        changed.exposure = 0.5;
        assert_ne!(edit_state_signature(&base), edit_state_signature(&changed));
    }

    #[test]
    fn source_signature_is_stable_for_same_path() {
        let path = Path::new("/tmp/photograph-nonexistent-raw.raf");
        assert_eq!(source_signature(path), source_signature(path));
    }
}

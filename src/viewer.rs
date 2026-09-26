use std::{
    sync::Arc,
    collections::hash_map::DefaultHasher,
    collections::{HashMap, VecDeque},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

use image::{DynamicImage, RgbaImage};
use rayon::prelude::*;

use crate::state::{EditState, GradFilter, Mask, Rect, Spot, Stroke};

/// Downscale loaded images to this longest-edge size for the preview.
const PREVIEW_MAX: u32 = 1920;
/// During active slider drags, process a lower-resolution preview for responsiveness.
const INTERACTIVE_PREVIEW_MAX: u32 = 960;
const DEBOUNCE: Duration = Duration::from_millis(300);
const INTERACTIVE_REFRESH: Duration = Duration::from_millis(90);
const PREVIEW_CACHE_CAPACITY: usize = 24;
/// Byte budget for cached rendered previews.
const PREVIEW_CACHE_BYTES: usize = 512 * 1024 * 1024;

/// Size in screen pixels for crop corner drag handles.
const HANDLE_SIZE: f32 = 8.0;

enum BgResult {
    Loaded {
        path: PathBuf,
        img: DynamicImage,
    },
    LoadFailed(PathBuf),
    /// A rendered preview, already converted for display on the worker
    /// thread so the UI thread only has to upload it.
    Processed {
        generation: u64,
        cache_key: PreviewCacheKey,
        image: Arc<egui::ColorImage>,
    },
    /// An edited thumbnail was written for this photo (or failed to be).
    ThumbnailSaved(PathBuf),
    /// Split view's "before" image, rendered for the state with `signature`.
    Original {
        signature: u64,
        image: Arc<egui::ColorImage>,
    },
}

/// Converts a rendered RGBA image for display. For opaque images — every
/// photo the pipeline produces unless the source has transparency — the
/// bytes are already egui's `Color32` (premultiplied RGBA equals straight
/// RGBA at alpha 255), so they're copied in bulk, in parallel, instead of
/// converted pixel by pixel (~60ms for a 20 MP render). The buffer can't be
/// reused outright: `Color32` is 4-byte aligned, a `Vec<u8>` isn't.
fn color_image_from_rgba(rgba: RgbaImage) -> egui::ColorImage {
    const CHUNK: usize = 1 << 20; // multiple of 4, so pixels never straddle chunks
    let size = [rgba.width() as usize, rgba.height() as usize];
    let raw = rgba.into_raw();
    let opaque = raw
        .par_chunks(CHUNK)
        .all(|chunk| chunk.chunks_exact(4).all(|p| p[3] == 255));
    if !opaque {
        return egui::ColorImage::from_rgba_unmultiplied(size, &raw);
    }
    // Zeroed by the allocator (no fill pass); the parallel copy then
    // touches the pages from all cores at once.
    let mut pixels: Vec<egui::Color32> = bytemuck::zeroed_vec(raw.len() / 4);
    bytemuck::cast_slice_mut::<egui::Color32, u8>(&mut pixels)
        .par_chunks_mut(CHUNK)
        .zip(raw.par_chunks(CHUNK))
        .for_each(|(dst, src)| dst.copy_from_slice(src));
    egui::ColorImage::new(size, pixels)
}

fn scale_to_cap(img: DynamicImage, cap: u32) -> DynamicImage {
    if img.width() > cap || img.height() > cap {
        img.thumbnail(cap, cap)
    } else {
        img
    }
}

fn load_preview_stages_with_hooks<FPreview, FFull>(
    path: &Path,
    cap: u32,
    open_preview_with_source: FPreview,
    open_full: FFull,
) -> anyhow::Result<Vec<DynamicImage>>
where
    FPreview: Fn(&Path) -> anyhow::Result<(DynamicImage, crate::thumbnail::PreviewSource)>,
    FFull: Fn(&Path) -> anyhow::Result<DynamicImage>,
{
    let (img, source) = open_preview_with_source(path)?;
    let mut stages = vec![scale_to_cap(img, cap)];

    // For RAW files loaded from embedded preview payloads, schedule
    // a second-stage full decode to converge toward full-quality preview.
    if crate::thumbnail::is_raw_image(path) && source == crate::thumbnail::PreviewSource::Embedded {
        if let Ok(full) = open_full(path) {
            stages.push(scale_to_cap(full, cap));
        }
    }

    Ok(stages)
}

fn load_preview_stages(path: &Path, cap: u32) -> anyhow::Result<Vec<DynamicImage>> {
    load_preview_stages_with_hooks(
        path,
        cap,
        crate::thumbnail::open_image_for_preview_with_source,
        crate::thumbnail::open_image,
    )
}

fn send_loaded_preview_stages(path: PathBuf, cap: u32, tx: &mpsc::SyncSender<BgResult>) {
    match load_preview_stages(&path, cap) {
        Ok(stages) => {
            for img in stages {
                // Convert once here: renders then use the RGBA8 buffer
                // directly instead of converting on every pass.
                let img = DynamicImage::ImageRgba8(img.into_rgba8());
                let _ = tx.send(BgResult::Loaded {
                    path: path.clone(),
                    img,
                });
            }
        }
        Err(_) => {
            let _ = tx.send(BgResult::LoadFailed(path));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewBackend {
    Cpu,
    Auto,
    GpuPipeline,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ProcessQuality {
    Interactive,
    Final,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct PreviewCacheKey {
    source_signature: u64,
    /// Which loaded preview was rendered (see `Viewer::preview_revision`).
    preview_revision: u64,
    edit_signature: u64,
    input_width: u32,
    input_height: u32,
    quality: ProcessQuality,
}

#[derive(Clone)]
struct PreviewCacheEntry {
    image: Arc<egui::ColorImage>,
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
    /// Target width/height in pixels; `Original` is the image's own aspect.
    fn pixel_ratio(self, image_aspect: f32) -> Option<f32> {
        match self {
            CropAspect::Free => None,
            CropAspect::Square => Some(1.0),
            CropAspect::Photo4x3 => Some(4.0 / 3.0),
            CropAspect::Wide16x9 => Some(16.0 / 9.0),
            CropAspect::Original => Some(image_aspect),
        }
    }

    /// The ratio in crop-rect units. Crop rects are fractions of the
    /// (rotated) image's width and height, so a pixel ratio `r` on an image
    /// of aspect `a` is `r / a` there — e.g. 1:1 on a 3:2 photo is a rect
    /// 2/3 as wide (as a fraction) as it is tall. `None` for Free, or when
    /// the image's aspect isn't known yet.
    fn normalized_ratio(self, image_aspect: Option<f32>) -> Option<f32> {
        let image_aspect = image_aspect.filter(|a| a.is_finite() && *a > 0.0)?;
        self.pixel_ratio(image_aspect).map(|r| r / image_aspect)
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
    /// 0 = TL, 1 = TR, 2 = BR, 3 = BL.
    Corner(u8),
    /// Midpoint handle that moves one edge: 0 = top, 1 = right, 2 = bottom, 3 = left.
    Edge(u8),
    Interior,
}

/// Which circle of a spot is being dragged.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SpotHandle {
    Target,
    Source,
}

/// An in-progress drag of one spot circle.
#[derive(Clone, Copy, Debug)]
struct SpotDrag {
    index: usize,
    handle: SpotHandle,
    /// Pointer minus circle center at grab time, normalized, so the circle
    /// doesn't jump to center on the pointer.
    grab_offset: [f32; 2],
}

/// Default radius for new spots, as a fraction of the image's shorter side.
const DEFAULT_SPOT_SIZE: f32 = 0.02;
/// Default mask brush radius and feather.
const DEFAULT_BRUSH_SIZE: f32 = 0.05;
const DEFAULT_BRUSH_FEATHER: f32 = 0.5;
/// A stroke records a new point once the brush has moved this fraction of
/// its radius, keeping sidecars small (ADR-0019).
const STROKE_POINT_SPACING: f32 = 0.25;
/// Longest side of the editor's mask overlay texture.
const MASK_OVERLAY_MAX: u32 = 512;

/// Image viewer/editor window state, including async preview processing.
pub struct Viewer {
    id: usize,
    preview_backend: PreviewBackend,
    current_path: Option<PathBuf>,
    preview: Option<DynamicImage>,
    /// Bumped whenever `preview` is replaced. A RAW first shows its embedded
    /// camera JPEG, then its full develop at the same size; render caches
    /// key on this so the develop isn't mistaken for the JPEG's renders.
    preview_revision: u64,
    pub edit_state: EditState,
    /// Photos whose sidecar was written or removed, for thumbnail refreshes.
    changed_sidecars: Vec<PathBuf>,
    needs_process: bool,
    needs_final_process: bool,
    last_slider_change: Option<Instant>,
    last_interactive_process: Option<Instant>,
    texture: Option<egui::TextureHandle>,
    original_texture: Option<egui::TextureHandle>,
    /// `original_signature` of the state `original_texture` was rendered for.
    original_rendered: Option<u64>,
    /// `original_signature` of an in-flight "before" render, if any.
    original_in_flight: Option<u64>,
    split_view: bool,
    /// Split view shows the "before" image as shot (uncropped, unrotated)
    /// instead of with the edit's geometry.
    split_original_crop: bool,
    crop_mode: bool,
    crop_aspect: CropAspect,
    /// Visual-only crop selection — not applied to processing until user confirms.
    pending_crop: Option<Rect>,
    /// Active drag operation on the pending crop rect.
    crop_drag: Option<DragTarget>,
    /// Normalized drag start position (for interior moves).
    crop_drag_start_pos: Option<egui::Pos2>,
    /// Pending crop rect snapshot at drag start (interior moves offset from
    /// it; corner drags anchor on it).
    crop_drag_start_rect: Option<Rect>,
    /// Normalized position where the initial drag began (for creating new rects).
    crop_create_origin: Option<egui::Pos2>,
    /// Spot removal tool active (ADR-0018): preview drawn without geometry.
    spot_mode: bool,
    /// Radius for new spots (and the selected one), fraction of the shorter side.
    spot_size: f32,
    selected_spot: Option<usize>,
    spot_drag: Option<SpotDrag>,
    /// Mask painting tool active (ADR-0019): same view as the spot tool.
    mask_mode: bool,
    selected_mask: Option<usize>,
    brush_size: f32,
    brush_feather: f32,
    brush_erase: bool,
    show_mask_overlay: bool,
    /// A stroke is being painted into the selected mask.
    painting: bool,
    /// The selected mask's coverage as a tinted texture, with the signature
    /// of the mask it was drawn from.
    mask_overlay: Option<(u64, egui::TextureHandle)>,
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
            preview_revision: 0,
            edit_state: EditState::default(),
            changed_sidecars: Vec::new(),
            needs_process: false,
            needs_final_process: false,
            last_slider_change: None,
            last_interactive_process: None,
            texture: None,
            original_texture: None,
            original_rendered: None,
            original_in_flight: None,
            split_view: false,
            split_original_crop: false,
            crop_mode: false,
            crop_aspect: CropAspect::Free,
            pending_crop: None,
            crop_drag: None,
            crop_drag_start_pos: None,
            crop_drag_start_rect: None,
            crop_create_origin: None,
            spot_mode: false,
            spot_size: DEFAULT_SPOT_SIZE,
            selected_spot: None,
            spot_drag: None,
            mask_mode: false,
            selected_mask: None,
            brush_size: DEFAULT_BRUSH_SIZE,
            brush_feather: DEFAULT_BRUSH_FEATHER,
            brush_erase: false,
            show_mask_overlay: true,
            painting: false,
            mask_overlay: None,
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
    /// Persists current edits to the sidecar file, if any.
    /// Writes the current photo's edits to its sidecar (removing it when
    /// there are none). Photos whose sidecar changed are queued for
    /// `take_changed_sidecars`, so the library can refresh their thumbnails.
    ///
    /// Photos with edits also get a thumbnail showing them, saved beside the
    /// sidecar — whenever the edits changed, or if it's missing (e.g. edits
    /// made before thumbnails were saved). It's rendered in the background
    /// from the in-memory preview, and the photo is only queued once the
    /// file is written, so the library never re-reads the old one.
    pub fn save_edits(&mut self) {
        let Some(path) = self.current_path.clone() else {
            return;
        };
        let changed = self.edit_state.sync_sidecar(&path).unwrap_or(false);
        let has_edits = self.edit_state.has_edits();
        let missing = !crate::state::edited_thumbnail_path(&path).exists();
        if has_edits && (changed || missing) && self.write_edited_thumbnail(&path) {
            return; // queued when the thumbnail lands (`BgResult::ThumbnailSaved`)
        }
        if changed {
            self.changed_sidecars.push(path);
        }
    }

    /// Renders and saves `path`'s edited thumbnail on a worker thread.
    /// Returns false if there's no preview to render it from.
    fn write_edited_thumbnail(&self, path: &Path) -> bool {
        let Some(preview) = self.preview.clone() else {
            return false;
        };
        let state = self.edit_state.clone();
        let path = path.to_path_buf();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let size = crate::thumbnail::THUMB_SIZE;
            // Edits are resolution-independent, so render a small copy.
            let small = DynamicImage::ImageRgba8(preview.thumbnail(size * 2, size * 2).into_rgba8());
            let rendered = crate::processing::gpu_pipeline::try_apply(&small, &state).or_else(|| {
                crate::processing::gpu_pipeline::allow_debug_cpu_fallback()
                    .then(|| crate::processing::transform::apply(&small, &state))
            });
            if let Some(rendered) = rendered {
                let thumb_path = crate::state::edited_thumbnail_path(&path);
                if let Some(dir) = thumb_path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = rendered.thumbnail(size, size).save(&thumb_path);
            }
            let _ = tx.send(BgResult::ThumbnailSaved(path));
        });
        true
    }

    /// Photos whose saved edits changed since the last call.
    pub fn take_changed_sidecars(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.changed_sidecars)
    }

    fn has_edits(&self) -> bool {
        self.edit_state.has_edits()
    }

    /// The edit state the preview is rendered with. In crop mode the applied
    /// crop is left out, so the full image is on screen and the crop overlay
    /// (in full-image coordinates) lines up with it. In spot and mask mode
    /// straighten and keystone are left out: what remains (rotate, flip,
    /// crop) maps exactly back to source-image coordinates, where spots and
    /// strokes live (ADR-0018, ADR-0019; see `SourceProjection`).
    fn render_state(&self) -> EditState {
        let mut state = self.edit_state.clone();
        if self.crop_mode {
            state.crop = None;
        }
        if self.spot_mode || self.mask_mode {
            state.straighten = 0.0;
            state.keystone = Default::default();
        }
        state
    }

    /// Leaves crop mode, discarding any unapplied selection.
    fn exit_crop_mode(&mut self) {
        self.set_crop_mode(false);
        self.pending_crop = None;
        self.crop_drag = None;
        self.crop_create_origin = None;
    }

    /// Enters or leaves the spot tool. Entering leaves crop mode; either way
    /// the preview re-renders, since spot mode drops geometry (`render_state`).
    fn set_spot_mode(&mut self, on: bool) {
        if self.spot_mode == on {
            return;
        }
        if on {
            self.exit_crop_mode();
            self.set_mask_mode(false);
        }
        self.spot_mode = on;
        self.selected_spot = None;
        self.spot_drag = None;
        self.needs_process = true;
        self.last_slider_change = None;
    }

    /// Enters or leaves the mask tool; like the spot tool it's exclusive with
    /// the other tools and re-renders without straighten and keystone.
    fn set_mask_mode(&mut self, on: bool) {
        if self.mask_mode == on {
            return;
        }
        if on {
            self.exit_crop_mode();
            self.set_spot_mode(false);
            if self.selected_mask.is_none() && !self.edit_state.masks.is_empty() {
                self.selected_mask = Some(0);
            }
        }
        self.mask_mode = on;
        self.painting = false;
        self.needs_process = true;
        self.last_slider_change = None;
    }

    /// Adds a new, empty mask named "Mask N" and selects it.
    fn add_mask(&mut self) -> usize {
        let n = (1..)
            .find(|n| !self.edit_state.masks.iter().any(|m| m.name == format!("Mask {n}")))
            .unwrap_or(1);
        self.edit_state.masks.push(Mask {
            name: format!("Mask {n}"),
            enabled: true,
            strokes: Vec::new(),
            adjust: Default::default(),
        });
        let index = self.edit_state.masks.len() - 1;
        self.selected_mask = Some(index);
        index
    }

    fn delete_selected_mask(&mut self) {
        if let Some(i) = self.selected_mask.take() {
            if i < self.edit_state.masks.len() {
                self.edit_state.masks.remove(i);
                self.painting = false;
                self.selected_mask = if self.edit_state.masks.is_empty() {
                    None
                } else {
                    Some(i.min(self.edit_state.masks.len() - 1))
                };
                self.edits_changed(false);
            }
        }
    }

    /// Marks spot or mask edits for re-render: `dragging` coalesces into interactive
    /// passes like a slider drag, otherwise a final pass runs right away.
    fn edits_changed(&mut self, dragging: bool) {
        self.needs_process = true;
        self.last_slider_change = dragging.then(Instant::now);
    }

    /// Best-matching source for a new spot, searched on the unedited preview
    /// (which shares spot coordinates). `None` keeps the spot's default.
    fn auto_spot_source(&self, spot: &Spot) -> Option<[f32; 2]> {
        let preview = self.preview.as_ref()?;
        let find = |img: &RgbaImage| {
            crate::processing::spots::find_source(
                img,
                spot.target,
                spot.radius,
                &self.edit_state.spots,
            )
        };
        match preview.as_rgba8() {
            Some(rgba) => find(rgba),
            None => find(&preview.to_rgba8()),
        }
    }

    fn delete_selected_spot(&mut self) {
        if let Some(i) = self.selected_spot.take() {
            if i < self.edit_state.spots.len() {
                self.edit_state.spots.remove(i);
                self.spot_drag = None;
                self.edits_changed(false);
            }
        }
    }

    /// Enters or leaves crop mode, re-rendering the preview when that changes
    /// whether the applied crop is shown (see `render_state`).
    fn set_crop_mode(&mut self, on: bool) {
        if self.crop_mode == on {
            return;
        }
        self.crop_mode = on;
        if self.edit_state.crop.is_some() {
            self.needs_process = true;
            self.last_slider_change = None;
        }
    }

    pub fn set_image(&mut self, path: PathBuf, ctx: &egui::Context) {
        if self.current_path.as_ref() == Some(&path) {
            return;
        }
        // Save current edits before switching
        self.save_edits();
        self.current_path = Some(path.clone());
        self.source_signature = source_signature(&path);
        self.preview = None;
        self.texture = None;
        self.original_texture = None;
        self.original_rendered = None;
        self.original_in_flight = None;
        self.edit_state = EditState::load(&path).unwrap_or_default();
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
        self.spot_mode = false;
        self.selected_spot = None;
        self.spot_drag = None;
        self.mask_mode = false;
        self.selected_mask = None;
        self.painting = false;
        self.mask_overlay = None;
        self.zoom = 1.0;
        self.pan_offset = egui::Vec2::ZERO;
        self.preview_max = PREVIEW_MAX;
        self.last_zoom_change = None;
        self.metadata = crate::metadata::read(&path).ok();

        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        let cap = self.preview_max;
        std::thread::spawn(move || {
            send_loaded_preview_stages(path, cap, &tx);
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

        if let Some(image) = self.cached_preview_image(&cache_key) {
            self.texture = Some(ctx.load_texture(
                format!("viewer_tex_{}", self.id),
                egui::ImageData::Color(image),
                egui::TextureOptions::LINEAR,
            ));
            self.processing = false;
            self.in_flight_generation = None;
            return;
        }

        let img = preview;
        let state = self.render_state();
        let preview_backend = self.preview_backend;
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let source = match quality {
                ProcessQuality::Interactive => downscale_for_interactive(img),
                ProcessQuality::Final => img,
            };
            let result = process_preview_with_backend(&source, &state, preview_backend);
            let image = Arc::new(color_image_from_rgba(result.into_rgba8()));
            let _ = tx.send(BgResult::Processed {
                generation,
                cache_key,
                image,
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
            preview_revision: self.preview_revision,
            edit_signature: edit_state_signature(&self.render_state()),
            input_width: preview.width(),
            input_height: preview.height(),
            quality,
        }
    }

    fn cached_preview_image(&mut self, key: &PreviewCacheKey) -> Option<Arc<egui::ColorImage>> {
        let image = Arc::clone(&self.preview_cache.get(key)?.image);
        self.touch_preview_cache_key(key);
        Some(image)
    }

    /// Caches a rendered preview, sharing the displayed image rather than
    /// copying it. Evicts oldest-first beyond `PREVIEW_CACHE_CAPACITY`
    /// entries or `PREVIEW_CACHE_BYTES` (full-resolution previews are ~80 MB
    /// each, so the entry count alone could reach gigabytes).
    fn store_preview_cache(&mut self, key: PreviewCacheKey, image: Arc<egui::ColorImage>) {
        self.preview_cache.insert(key.clone(), PreviewCacheEntry { image });
        self.touch_preview_cache_key(&key);
        let bytes = |cache: &HashMap<PreviewCacheKey, PreviewCacheEntry>| -> usize {
            cache.values().map(|e| e.image.pixels.len() * 4).sum()
        };
        while self.preview_cache.len() > 1
            && (self.preview_cache.len() > PREVIEW_CACHE_CAPACITY
                || bytes(&self.preview_cache) > PREVIEW_CACHE_BYTES)
        {
            match self.preview_cache_lru.pop_front() {
                Some(oldest) => {
                    self.preview_cache.remove(&oldest);
                }
                None => break,
            }
        }
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
                        self.preview_revision = self.preview_revision.wrapping_add(1);
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
                    image,
                } => {
                    self.processing = false;
                    self.in_flight_generation = None;
                    self.store_preview_cache(cache_key, Arc::clone(&image));
                    if generation != self.requested_generation {
                        continue;
                    }
                    self.texture = Some(ctx.load_texture(
                        format!("viewer_tex_{}", self.id),
                        egui::ImageData::Color(image),
                        egui::TextureOptions::LINEAR,
                    ));
                }
                BgResult::ThumbnailSaved(path) => {
                    self.changed_sidecars.push(path);
                }
                BgResult::Original { signature, image } => {
                    // Drop renders for a state that's since changed.
                    if self.original_in_flight == Some(signature) {
                        self.original_in_flight = None;
                        self.set_original_texture(ctx, signature, image);
                    }
                }
            }
        }
    }

    /// The state split view's "before" side is rendered with: the edit's
    /// geometry only (so both sides frame the same area and differ only in
    /// color and retouching), or nothing when showing the original crop.
    fn original_state(&self) -> EditState {
        if self.split_original_crop {
            return EditState::default();
        }
        let e = &self.edit_state;
        EditState {
            rotate: e.rotate,
            flip_h: e.flip_h,
            flip_v: e.flip_v,
            crop: e.crop.clone(),
            straighten: e.straighten,
            keystone: e.keystone.clone(),
            ..EditState::default()
        }
    }

    /// Identifies a "before" render: its state plus the preview it came from
    /// (which changes when the RAW develop replaces the embedded JPEG, or a
    /// higher-resolution preview reloads).
    fn original_signature(&self, preview: &DynamicImage) -> u64 {
        let mut hasher = DefaultHasher::new();
        edit_state_signature(&self.original_state()).hash(&mut hasher);
        (self.preview_revision, preview.width(), preview.height()).hash(&mut hasher);
        hasher.finish()
    }

    /// Keeps split view's "before" texture current, rendering it in the
    /// background when its geometry changes. The previous texture stays on
    /// screen until the new one arrives.
    fn ensure_original_texture(&mut self, ctx: &egui::Context) {
        let Some(preview) = self.preview.clone() else {
            return;
        };
        let signature = self.original_signature(&preview);
        if self.original_rendered == Some(signature) || self.original_in_flight == Some(signature)
        {
            return;
        }
        let state = self.original_state();
        if !crate::processing::gpu_pipeline::has_geometry(&state) {
            // Nothing to apply (no geometry, or showing the original crop):
            // the "before" image is the preview itself.
            let image = Arc::new(color_image_from_rgba(preview.to_rgba8()));
            self.set_original_texture(ctx, signature, image);
            return;
        }
        self.original_in_flight = Some(signature);
        let preview_backend = self.preview_backend;
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let result = process_preview_with_backend(&preview, &state, preview_backend);
            let image = Arc::new(color_image_from_rgba(result.into_rgba8()));
            let _ = tx.send(BgResult::Original { signature, image });
            ctx2.request_repaint();
        });
    }

    fn set_original_texture(
        &mut self,
        ctx: &egui::Context,
        signature: u64,
        image: Arc<egui::ColorImage>,
    ) {
        self.original_texture = Some(ctx.load_texture(
            format!("viewer_orig_{}", self.id),
            egui::ImageData::Color(image),
            egui::TextureOptions::LINEAR,
        ));
        self.original_rendered = Some(signature);
    }

    /// The selected aspect ratio in crop-rect units, ready for
    /// `constrain_aspect` / `resize_from_corner` / `resize_from_edge`.
    fn effective_crop_ratio(&self) -> Option<f32> {
        self.crop_aspect.normalized_ratio(self.crop_image_aspect())
    }

    /// Width/height of the source image before geometry, which spots are
    /// relative to. Falls back to square before the preview has loaded.
    fn spot_image_aspect(&self) -> f32 {
        self.preview
            .as_ref()
            .map(|p| p.width() as f32 / p.height().max(1) as f32)
            .unwrap_or(1.0)
    }

    /// Width/height of the image crop rects are relative to: the source
    /// after rotation (crop is applied post-rotation).
    fn crop_image_aspect(&self) -> Option<f32> {
        let preview = self.preview.as_ref()?;
        let (w, h) = (preview.width() as f32, preview.height() as f32);
        Some(match self.edit_state.rotate.rem_euclid(360) {
            90 | 270 => h / w,
            _ => w / h,
        })
    }

    /// Renders the image viewport and kicks off preview processing when needed.
    /// Renders the image viewport. When `editable` is false (fullscreen preview
    /// mode), the split-view/crop/save toolbar and crop interaction are hidden.
    pub fn show_image(&mut self, ui: &mut egui::Ui, editable: bool) {
        // Leaving the editor (e.g. to fullscreen) abandons an in-progress crop,
        // like toggling Crop off, so the view goes back to the applied crop.
        if !editable {
            self.exit_crop_mode();
            self.set_spot_mode(false);
            self.set_mask_mode(false);
        }

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
                        send_loaded_preview_stages(path, cap, &tx);
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

        // Toolbar row (edit mode only)
        if editable {
            ui.horizontal(|ui| {
                if ui.selectable_label(self.split_view, "Split view").clicked() {
                    self.split_view = !self.split_view;
                }
                if self.split_view {
                    ui.checkbox(&mut self.split_original_crop, "Show original crop")
                        .on_hover_text(
                            "Show the before image as shot, without the edit's crop and rotation",
                        );
                }
                if ui.selectable_label(self.crop_mode, "Crop").clicked() {
                    self.set_spot_mode(false);
                    self.set_mask_mode(false);
                    self.set_crop_mode(!self.crop_mode);
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
                if ui
                    .selectable_label(self.spot_mode, "Spot")
                    .on_hover_text("Spot removal: click a blemish to cover it")
                    .clicked()
                {
                    self.set_spot_mode(!self.spot_mode);
                }
                if ui
                    .selectable_label(self.mask_mode, "Mask")
                    .on_hover_text("Paint masks to adjust parts of the photo")
                    .clicked()
                {
                    self.set_mask_mode(!self.mask_mode);
                }

                if ui
                    .add_enabled(self.has_edits(), egui::Button::new("Save"))
                    .clicked()
                {
                    self.save_edits();
                }

                if self.processing || self.reloading_preview {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spinner();
                    });
                }
            });
        }

        if editable && self.mask_mode {
            self.show_mask_window(ui.ctx());
        }

        // Spot mode toolbar: size, delete, clear
        if editable && self.spot_mode {
            ui.horizontal(|ui| {
                ui.label("Size:");
                if ui
                    .add(
                        egui::Slider::new(&mut self.spot_size, 0.001..=0.15)
                            .logarithmic(true)
                            .show_value(false),
                    )
                    .changed()
                {
                    let aspect = self.spot_image_aspect();
                    let size = self.spot_size;
                    if let Some(spot) =
                        self.selected_spot.and_then(|i| self.edit_state.spots.get_mut(i))
                    {
                        spot.radius = size;
                        spot.target = crate::state::clamp_center(spot.target, size, aspect);
                        spot.source = crate::state::clamp_center(spot.source, size, aspect);
                        self.edits_changed(true);
                    }
                }
                if ui
                    .add_enabled(self.selected_spot.is_some(), egui::Button::new("Delete"))
                    .clicked()
                {
                    self.delete_selected_spot();
                }
                if ui
                    .add_enabled(!self.edit_state.spots.is_empty(), egui::Button::new("Clear all"))
                    .clicked()
                {
                    self.edit_state.spots.clear();
                    self.selected_spot = None;
                    self.edits_changed(false);
                }
                ui.weak("Click to add · drag circles to adjust · Delete removes");
            });
        }

        // Crop mode toolbar: aspect ratio + apply/cancel/reset
        if editable && self.crop_mode {
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
                    self.set_crop_mode(false);
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
                if editable && self.crop_mode {
                    // Disable zoom/pan while cropping
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
                    // The spot and mask tools supply their own, and pan themselves.
                    let source_tool = editable && (self.spot_mode || self.mask_mode);
                    let resp = if editable && self.spot_mode {
                        self.handle_spot_interaction(ui, img_rect, viewport_rect)
                    } else if editable && self.mask_mode {
                        self.handle_mask_interaction(ui, img_rect, viewport_rect)
                    } else {
                        let sense = if self.zoom > 1.0 {
                            egui::Sense::click_and_drag()
                        } else {
                            egui::Sense::click()
                        };
                        ui.interact(viewport_rect, ui.id().with("zoom_pan"), sense)
                    };

                    // Scroll-to-zoom and pinch-to-zoom (only when hovered)
                    if resp.hovered() {
                        // Mouse wheel zoom
                        let scroll_delta = ui.input(|i| i.smooth_scroll_delta.y);
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
                    if self.zoom > 1.0 && !source_tool {
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

    /// Where spots appear on screen for the current preview (`img_rect` is the
    /// full, possibly zoomed image rect).
    fn source_projection(&self, img_rect: egui::Rect) -> SourceProjection {
        SourceProjection {
            img_rect,
            source_aspect: self.spot_image_aspect(),
            rotate: self.edit_state.rotate,
            flip_h: self.edit_state.flip_h,
            flip_v: self.edit_state.flip_v,
            crop: self.edit_state.crop.clone(),
        }
    }

    /// Spot tool interaction: click empty image to add a spot, click a circle
    /// to select it, drag a target or source circle to move it, drag empty
    /// image to pan when zoomed, Delete or Backspace to remove the selected
    /// spot, Escape to deselect. Returns the interaction response so the
    /// caller can apply scroll/pinch zoom.
    fn handle_spot_interaction(
        &mut self,
        ui: &mut egui::Ui,
        img_rect: egui::Rect,
        viewport_rect: egui::Rect,
    ) -> egui::Response {
        let resp = ui.interact(
            viewport_rect.intersect(img_rect),
            ui.id().with("spot_interact"),
            egui::Sense::click_and_drag(),
        );
        let proj = self.source_projection(img_rect);
        let aspect = proj.source_aspect;

        if resp.drag_started() {
            // Hit-test where the button went down: by the time a drag
            // registers the pointer may already have left a small circle.
            let press = ui.input(|i| i.pointer.press_origin());
            if let Some(pos) = press {
                let hit = spot_hit_test(&self.edit_state.spots, self.selected_spot, pos, &proj);
                if let Some((index, handle)) = hit {
                    let spot = &self.edit_state.spots[index];
                    let center = match handle {
                        SpotHandle::Target => spot.target,
                        SpotHandle::Source => spot.source,
                    };
                    let p = proj.to_source(pos);
                    self.selected_spot = Some(index);
                    self.spot_size = spot.radius;
                    self.spot_drag = Some(SpotDrag {
                        index,
                        handle,
                        grab_offset: [p[0] - center[0], p[1] - center[1]],
                    });
                }
            }
        }
        let panning = resp.dragged() && self.spot_drag.is_none();
        if panning && self.zoom > 1.0 {
            self.pan_offset += resp.drag_delta();
        }
        if let (Some(drag), Some(pos)) = (self.spot_drag, resp.interact_pointer_pos()) {
            if resp.dragged() {
                if let Some(spot) = self.edit_state.spots.get_mut(drag.index) {
                    let p = proj.to_source(pos);
                    let moved = crate::state::clamp_center(
                        [p[0] - drag.grab_offset[0], p[1] - drag.grab_offset[1]],
                        spot.radius,
                        aspect,
                    );
                    match drag.handle {
                        SpotHandle::Target => spot.target = moved,
                        SpotHandle::Source => spot.source = moved,
                    }
                    self.edits_changed(true);
                }
            }
        }
        if resp.drag_stopped() && self.spot_drag.take().is_some() {
            self.edits_changed(false);
        }

        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                match spot_hit_test(&self.edit_state.spots, self.selected_spot, pos, &proj) {
                    Some((index, _)) => {
                        self.selected_spot = Some(index);
                        self.spot_size = self.edit_state.spots[index].radius;
                    }
                    None => {
                        let mut spot = Spot::new(proj.to_source(pos), self.spot_size, aspect);
                        if let Some(source) = self.auto_spot_source(&spot) {
                            spot.source = source;
                        }
                        self.edit_state.spots.push(spot);
                        self.selected_spot = Some(self.edit_state.spots.len() - 1);
                        self.edits_changed(false);
                    }
                }
            }
        }

        if resp.hovered() || self.spot_drag.is_some() {
            let typing = ui.ctx().egui_wants_keyboard_input();
            let delete = ui.input(|i| {
                i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)
            });
            if delete && !typing {
                self.delete_selected_spot();
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.selected_spot = None;
            }

            let hovered = resp.hover_pos().and_then(|pos| {
                spot_hit_test(&self.edit_state.spots, self.selected_spot, pos, &proj)
            });
            let cursor = if self.spot_drag.is_some() || panning {
                egui::CursorIcon::Grabbing
            } else if hovered.is_some() {
                egui::CursorIcon::Grab
            } else {
                egui::CursorIcon::Crosshair
            };
            ui.ctx().set_cursor_icon(cursor);
        }

        let painter = ui.painter().with_clip_rect(viewport_rect);
        let accent = ui.visuals().selection.bg_fill;
        draw_spot_overlay(&painter, accent, &proj, &self.edit_state.spots, self.selected_spot);
        resp
    }

    /// Mask tool interaction: primary-button drag (or click) paints — or
    /// erases — a stroke into the selected mask, creating a mask if there is
    /// none; middle- or right-button drag pans when zoomed. Draws the
    /// selected mask's coverage overlay and the brush outline. Returns the
    /// interaction response so the caller can apply scroll/pinch zoom.
    fn handle_mask_interaction(
        &mut self,
        ui: &mut egui::Ui,
        img_rect: egui::Rect,
        viewport_rect: egui::Rect,
    ) -> egui::Response {
        let resp = ui.interact(
            viewport_rect.intersect(img_rect),
            ui.id().with("mask_interact"),
            egui::Sense::click_and_drag(),
        );
        let proj = self.source_projection(img_rect);
        let aspect = proj.source_aspect;

        let panning = resp.dragged_by(egui::PointerButton::Middle)
            || resp.dragged_by(egui::PointerButton::Secondary);
        if panning && self.zoom > 1.0 {
            self.pan_offset += resp.drag_delta();
        }

        let primary_press = resp.drag_started_by(egui::PointerButton::Primary)
            || resp.clicked_by(egui::PointerButton::Primary);
        if primary_press && !self.painting {
            // Start where the button went down, not where the drag registered.
            let start = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
            if let Some(pos) = start {
                let index = match self.selected_mask {
                    Some(i) if i < self.edit_state.masks.len() => i,
                    _ => self.add_mask(),
                };
                self.edit_state.masks[index].strokes.push(Stroke {
                    points: vec![proj.to_source(pos)],
                    radius: self.brush_size,
                    feather: self.brush_feather,
                    erase: self.brush_erase,
                });
                self.painting = true;
                self.edits_changed(true);
            }
        }
        if self.painting && resp.dragged_by(egui::PointerButton::Primary) {
            if let (Some(pos), Some(stroke)) = (
                resp.interact_pointer_pos(),
                self.selected_mask
                    .and_then(|i| self.edit_state.masks.get_mut(i))
                    .and_then(|m| m.strokes.last_mut()),
            ) {
                let p = proj.to_source(pos);
                let last = *stroke.points.last().unwrap_or(&p);
                if source_distance(p, last, aspect) >= stroke.radius * STROKE_POINT_SPACING {
                    stroke.points.push(p);
                    self.edits_changed(true);
                }
            }
        }
        if self.painting
            && (resp.drag_stopped() || resp.clicked() || !ui.input(|i| i.pointer.primary_down()))
        {
            self.painting = false;
            self.edits_changed(false);
        }

        let painter = ui.painter().with_clip_rect(viewport_rect);
        if self.show_mask_overlay {
            if let Some(tex) = self.selected_mask_overlay(ui.ctx(), aspect) {
                draw_source_texture(&painter, &proj, tex.id());
            }
        }

        if resp.hovered() || self.painting {
            ui.ctx().set_cursor_icon(if panning {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Crosshair
            });
            if let Some(pos) = resp.hover_pos() {
                let r = proj.radius_px(self.brush_size);
                let shadow = egui::Stroke::new(3.0, egui::Color32::from_black_alpha(110));
                let color = if self.brush_erase {
                    egui::Color32::from_rgb(255, 120, 120)
                } else {
                    egui::Color32::WHITE
                };
                painter.circle_stroke(pos, r, shadow);
                painter.circle_stroke(pos, r, egui::Stroke::new(1.5, color));
                let inner = r * (1.0 - self.brush_feather);
                if inner > 2.0 && inner < r - 2.0 {
                    painter.circle_stroke(pos, inner, egui::Stroke::new(1.0, color.gamma_multiply(0.5)));
                }
            }
        }
        resp
    }

    /// The selected mask's coverage as a translucent tint, cached until the
    /// mask changes.
    fn selected_mask_overlay(&mut self, ctx: &egui::Context, aspect: f32) -> Option<&egui::TextureHandle> {
        let mask = self.selected_mask.and_then(|i| self.edit_state.masks.get(i))?;
        let signature = {
            let mut hasher = DefaultHasher::new();
            for stroke in &mask.strokes {
                (stroke.radius.to_bits(), stroke.feather.to_bits(), stroke.erase).hash(&mut hasher);
                for p in &stroke.points {
                    (p[0].to_bits(), p[1].to_bits()).hash(&mut hasher);
                }
            }
            aspect.to_bits().hash(&mut hasher);
            hasher.finish()
        };
        if self.mask_overlay.as_ref().map(|(sig, _)| *sig) != Some(signature) {
            let (w, h) = crate::processing::masks::raster_size(aspect, MASK_OVERLAY_MAX);
            let coverage = crate::processing::masks::rasterize(mask, w, h);
            let pixels = coverage
                .iter()
                .map(|c| egui::Color32::from_rgba_unmultiplied(255, 70, 70, (c * 140.0) as u8))
                .collect();
            let image = egui::ColorImage::new([w as usize, h as usize], pixels);
            let tex = ctx.load_texture(
                format!("viewer_mask_overlay_{}", self.id),
                image,
                egui::TextureOptions::LINEAR,
            );
            self.mask_overlay = Some((signature, tex));
        }
        self.mask_overlay.as_ref().map(|(_, tex)| tex)
    }

    /// The Masks window shown while the mask tool is active: create, select,
    /// rename and delete masks, plus the brush. The selected mask's
    /// adjustments live in the normal adjustments panel (`show_mask_controls`).
    fn show_mask_window(&mut self, ctx: &egui::Context) {
        egui::Window::new("Masks")
            .id(egui::Id::new(("mask_window", self.id)))
            .default_width(230.0)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("+").on_hover_text("New mask").clicked() {
                        self.add_mask();
                    }
                    if ui
                        .add_enabled(self.selected_mask.is_some(), egui::Button::new("Delete"))
                        .clicked()
                    {
                        self.delete_selected_mask();
                    }
                });
                if self.edit_state.masks.is_empty() {
                    ui.weak("No masks yet. Paint to create one.");
                }
                for i in 0..self.edit_state.masks.len() {
                    let selected = self.selected_mask == Some(i);
                    let row = mask_list_row(ui, &mut self.edit_state.masks[i], selected);
                    if row.selected {
                        self.selected_mask = Some(i);
                    }
                    if row.visibility_changed {
                        self.edits_changed(false);
                    }
                }
                if let Some(i) = self.selected_mask.filter(|&i| i < self.edit_state.masks.len()) {
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        ui.text_edit_singleline(&mut self.edit_state.masks[i].name);
                    });
                }

                ui.separator();
                egui::Grid::new(("brush_grid", self.id)).num_columns(2).show(ui, |ui| {
                    ui.label("Size");
                    ui.add(
                        egui::Slider::new(&mut self.brush_size, 0.002..=0.3)
                            .logarithmic(true)
                            .show_value(false),
                    );
                    ui.end_row();
                    ui.label("Feather");
                    ui.add(egui::Slider::new(&mut self.brush_feather, 0.0..=1.0).show_value(false));
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.brush_erase, false, "Paint");
                    ui.selectable_value(&mut self.brush_erase, true, "Erase");
                    ui.checkbox(&mut self.show_mask_overlay, "Overlay");
                });
                ui.weak("Drag to paint · right-drag to pan");
            });
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
            // Handles straddle the crop edge, so when the crop touches the
            // image edge half of each handle lies outside the image. Give each
            // handle its own interaction area so those halves still count as
            // hovering this viewer.
            let mut is_this_viewer = is_this_viewer;
            for (i, (_, r)) in handle_rects(crop_screen).into_iter().enumerate() {
                let resp = ui.interact(
                    r,
                    ui.id().with(("crop_handle", i)),
                    egui::Sense::click_and_drag(),
                );
                is_this_viewer |= resp.hovered() || resp.dragged();
            }
            // Always draw interactive overlay (handles + thirds) for visible crop
            draw_crop_overlay(ui, img_rect, crop_screen, true);

            // Cursor feedback: the active drag's cursor, else whatever is hovered
            if is_this_viewer {
                let target = self.crop_drag.or_else(|| {
                    pointer
                        .hover_pos()
                        .and_then(|pos| crop_hit_target(pos, crop_screen))
                });
                if let Some(t) = target {
                    let dragging = self.crop_drag.is_some();
                    ui.ctx().set_cursor_icon(crop_cursor(t, dragging));
                }
            }

            // Handle drag initiation — only for the interacted viewer
            if is_this_viewer {
                if let Some(pos) = pointer.interact_pos() {
                    if pointer.any_pressed() && self.crop_drag.is_none() {
                        if let Some(t) = crop_hit_target(pos, crop_screen) {
                            // Auto-promote applied crop to pending on grab
                            if !has_pending {
                                self.pending_crop = self.edit_state.crop.clone();
                            }
                            self.crop_drag = Some(t);
                            self.crop_drag_start_pos = Some(screen_to_norm_pos(pos, img_rect));
                            self.crop_drag_start_rect = self.pending_crop.clone();
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
                                // Resize from the drag-start rect so the anchor
                                // stays put even after the crop flips past it.
                                if let (Some(start_rect), Some(pc)) =
                                    (&self.crop_drag_start_rect, &mut self.pending_crop)
                                {
                                    if let Some(resized) =
                                        resize_from_corner(start_rect, idx, nx, ny, aspect_ratio)
                                    {
                                        *pc = resized;
                                    }
                                }
                            }
                            DragTarget::Edge(idx) => {
                                if let Some(ref mut pc) = self.pending_crop {
                                    resize_from_edge(pc, idx, nx, ny, aspect_ratio);
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
                        let pointer = (n.x.clamp(0.0, 1.0), n.y.clamp(0.0, 1.0));
                        *crop = anchored_rect((origin.x, origin.y), pointer, aspect_ratio);
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
                if self.mask_mode {
                    self.show_mask_controls(ui);
                    ui.separator();
                    if let Some(ref meta) = self.metadata {
                        show_exif(ui, meta);
                    }
                    return;
                }

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

    /// The adjustments panel in mask mode: the color sliders, applied to the
    /// selected mask instead of the whole photo.
    fn show_mask_controls(&mut self, ui: &mut egui::Ui) {
        let Some(i) = self.selected_mask.filter(|&i| i < self.edit_state.masks.len()) else {
            ui.label(egui::RichText::new("Masks").strong());
            ui.weak("Paint on the photo, or press + in the Masks window, to create a mask.");
            return;
        };
        let accent = ui.visuals().selection.bg_fill;
        let name = self.edit_state.masks[i].name.clone();
        ui.label(egui::RichText::new(format!("Editing mask: {name}")).strong().color(accent));
        ui.weak("These adjustments apply inside the mask. Leave Mask mode to edit the whole photo.");
        ui.add_space(4.0);
        let adjust = &mut self.edit_state.masks[i].adjust;
        show_basic_color_sliders(
            ui,
            BasicColor {
                exposure: &mut adjust.exposure,
                contrast: &mut adjust.contrast,
                highlights: &mut adjust.highlights,
                shadows: &mut adjust.shadows,
                temperature: &mut adjust.temperature,
                saturation: &mut adjust.saturation,
                hue_shift: &mut adjust.hue_shift,
            },
            None,
            &mut self.needs_process,
            &mut self.last_slider_change,
        );
        ui.add_space(6.0);
        show_selective_color(
            ui,
            &mut adjust.selective_color,
            &mut self.needs_process,
            &mut self.last_slider_change,
        );
    }

    fn show_crop_section(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Crop").strong());
        ui.add_space(4.0);

        // Aspect ratio selector
        ui.horizontal_wrapped(|ui| {
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
                    self.set_crop_mode(false);
                    self.crop_drag = None;
                    self.needs_process = true;
                    self.last_slider_change = None;
                }
                if ui.button("Cancel").clicked() {
                    self.pending_crop = None;
                    self.set_crop_mode(false);
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
                    self.set_crop_mode(true);
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
    let allow_cpu_fallback = crate::processing::gpu_pipeline::allow_debug_cpu_fallback();
    process_preview_with_backend_and_gpu_hook(
        source,
        state,
        preview_backend,
        allow_cpu_fallback,
        crate::processing::gpu_pipeline::try_apply,
    )
}

fn process_preview_with_backend_and_gpu_hook<F>(
    source: &DynamicImage,
    state: &EditState,
    preview_backend: PreviewBackend,
    allow_cpu_fallback: bool,
    gpu_apply: F,
) -> DynamicImage
where
    F: Fn(&DynamicImage, &EditState) -> Option<DynamicImage>,
{
    let apply_gpu_or_fallback = |source: &DynamicImage, state: &EditState| match gpu_apply(
        source, state,
    ) {
        Some(img) => img,
        None if allow_cpu_fallback => crate::processing::transform::apply(source, state),
        None => panic!(
            "photograph: gpu pipeline failed while CPU fallback is disabled (set {}=1 for debug fallback)",
            crate::processing::gpu_pipeline::DEBUG_ALLOW_CPU_FALLBACK_ENV
        ),
    };

    match preview_backend {
        PreviewBackend::Cpu if allow_cpu_fallback => {
            crate::processing::transform::apply(source, state)
        }
        PreviewBackend::Cpu | PreviewBackend::Auto | PreviewBackend::GpuPipeline => {
            apply_gpu_or_fallback(source, state)
        }
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

/// Maps between source-image coordinates (where spots live) and the screen,
/// for a preview rendered with rotate, flip and crop but no straighten or
/// keystone (see `Viewer::render_state` in spot mode). The pipeline applies
/// rotate (clockwise), then flips, then crop; `to_screen` follows that order
/// and `to_source` inverts it exactly.
struct SourceProjection {
    /// Screen rect of the whole displayed image (zoomed, not clipped).
    img_rect: egui::Rect,
    /// Width/height of the source image before geometry.
    source_aspect: f32,
    rotate: i32,
    flip_h: bool,
    flip_v: bool,
    crop: Option<Rect>,
}

impl SourceProjection {
    fn to_screen(&self, p: [f32; 2]) -> egui::Pos2 {
        let [mut u, mut v] = p;
        (u, v) = match self.rotate.rem_euclid(360) {
            90 => (1.0 - v, u),
            180 => (1.0 - u, 1.0 - v),
            270 => (v, 1.0 - u),
            _ => (u, v),
        };
        if self.flip_h {
            u = 1.0 - u;
        }
        if self.flip_v {
            v = 1.0 - v;
        }
        if let Some(c) = &self.crop {
            u = (u - c.x) / c.width;
            v = (v - c.y) / c.height;
        }
        self.img_rect.min + egui::vec2(u * self.img_rect.width(), v * self.img_rect.height())
    }

    fn to_source(&self, pos: egui::Pos2) -> [f32; 2] {
        let mut u = (pos.x - self.img_rect.min.x) / self.img_rect.width();
        let mut v = (pos.y - self.img_rect.min.y) / self.img_rect.height();
        if let Some(c) = &self.crop {
            u = c.x + u * c.width;
            v = c.y + v * c.height;
        }
        if self.flip_h {
            u = 1.0 - u;
        }
        if self.flip_v {
            v = 1.0 - v;
        }
        let (u, v) = match self.rotate.rem_euclid(360) {
            90 => (v, 1.0 - u),
            180 => (1.0 - u, 1.0 - v),
            270 => (1.0 - v, u),
            _ => (u, v),
        };
        [u, v]
    }

    /// A spot radius (fraction of the source's shorter side) in screen pixels.
    fn radius_px(&self, radius: f32) -> f32 {
        // Width of the rotated image in source-height units.
        let rotated_w = match self.rotate.rem_euclid(360) {
            90 | 270 => 1.0 / self.source_aspect,
            _ => self.source_aspect,
        };
        let crop_w = self.crop.as_ref().map_or(1.0, |c| c.width);
        let px_per_unit = self.img_rect.width() / (crop_w * rotated_w);
        radius * rotated_w.min(1.0) * px_per_unit
    }
}

struct MaskRowResponse {
    selected: bool,
    visibility_changed: bool,
}

/// One row of the Masks list: the name on the left (click the row to select
/// it) and a visibility checkbox on the right.
fn mask_list_row(ui: &mut egui::Ui, mask: &mut Mask, selected: bool) -> MaskRowResponse {
    let height = ui.spacing().interact_size.y;
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click());
    let visuals = ui.visuals();
    let (fill, text_color) = if selected {
        (visuals.selection.bg_fill, visuals.selection.stroke.color)
    } else if resp.hovered() {
        (visuals.widgets.hovered.weak_bg_fill, visuals.text_color())
    } else {
        (egui::Color32::TRANSPARENT, visuals.text_color())
    };
    let text_color = if mask.enabled { text_color } else { text_color.gamma_multiply(0.5) };
    ui.painter().rect_filled(rect, 3.0, fill);
    let text_rect = egui::Rect::from_min_max(
        rect.min + egui::vec2(6.0, 0.0),
        egui::pos2(rect.right() - height - 4.0, rect.bottom()),
    );
    ui.painter().with_clip_rect(text_rect).text(
        text_rect.left_center(),
        egui::Align2::LEFT_CENTER,
        &mask.name,
        egui::TextStyle::Body.resolve(ui.style()),
        text_color,
    );

    // The checkbox is added after the row, so it takes clicks over it.
    let checkbox_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - height, rect.top()),
        egui::vec2(height, height),
    );
    let mut checkbox_ui = ui.new_child(egui::UiBuilder::new().max_rect(checkbox_rect));
    let visibility_changed = checkbox_ui
        .checkbox(&mut mask.enabled, "")
        .on_hover_text(if mask.enabled { "Hide this mask's changes" } else { "Show this mask's changes" })
        .changed();

    MaskRowResponse {
        selected: resp.clicked(),
        visibility_changed,
    }
}

/// Distance between two source-image points in shorter-side units, the
/// unit brush and spot radii use.
fn source_distance(a: [f32; 2], b: [f32; 2], aspect: f32) -> f32 {
    let (sx, sy) = if aspect >= 1.0 { (aspect, 1.0) } else { (1.0, 1.0 / aspect) };
    let (dx, dy) = ((a[0] - b[0]) * sx, (a[1] - b[1]) * sy);
    (dx * dx + dy * dy).sqrt()
}

/// Paints a texture covering the whole source image onto the screen through
/// `proj` (rotate, flip and crop are axis-aligned, so its four corners map
/// to a screen rectangle; the painter's clip rect trims the cropped parts).
fn draw_source_texture(painter: &egui::Painter, proj: &SourceProjection, texture: egui::TextureId) {
    let mut mesh = egui::Mesh::with_texture(texture);
    for (source, uv) in [
        ([0.0, 0.0], egui::pos2(0.0, 0.0)),
        ([1.0, 0.0], egui::pos2(1.0, 0.0)),
        ([1.0, 1.0], egui::pos2(1.0, 1.0)),
        ([0.0, 1.0], egui::pos2(0.0, 1.0)),
    ] {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: proj.to_screen(source),
            uv,
            color: egui::Color32::WHITE,
        });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

/// Which spot circle is under `pos`. The selected spot's source circle is
/// checked first (it's the only source drawn), then targets, topmost (last
/// added) first.
fn spot_hit_test(
    spots: &[Spot],
    selected: Option<usize>,
    pos: egui::Pos2,
    proj: &SourceProjection,
) -> Option<(usize, SpotHandle)> {
    let inside = |center: [f32; 2], radius: f32| {
        // A minimum grab radius keeps tiny spots clickable.
        proj.to_screen(center).distance(pos) <= proj.radius_px(radius).max(HANDLE_SIZE)
    };
    if let Some(i) = selected {
        if let Some(spot) = spots.get(i) {
            if inside(spot.source, spot.radius) {
                return Some((i, SpotHandle::Source));
            }
        }
    }
    spots
        .iter()
        .enumerate()
        .rev()
        .find(|(_, s)| inside(s.target, s.radius))
        .map(|(i, _)| (i, SpotHandle::Target))
}

/// Draws every spot's target circle; the selected spot also gets its source
/// circle and a line joining the two.
fn draw_spot_overlay(
    painter: &egui::Painter,
    accent: egui::Color32,
    proj: &SourceProjection,
    spots: &[Spot],
    selected: Option<usize>,
) {
    let shadow = egui::Stroke::new(3.0, egui::Color32::from_black_alpha(110));
    for (i, spot) in spots.iter().enumerate() {
        let target = proj.to_screen(spot.target);
        let r = proj.radius_px(spot.radius);
        let is_selected = selected == Some(i);
        let color = if is_selected { accent } else { egui::Color32::WHITE };
        if is_selected {
            let source = proj.to_screen(spot.source);
            let dir = (source - target).normalized();
            if (source - target).length() > 2.0 * r {
                let line = [target + dir * r, source - dir * r];
                painter.line_segment(line, shadow);
                painter.line_segment(line, egui::Stroke::new(1.0, egui::Color32::WHITE));
            }
            painter.circle_stroke(source, r, shadow);
            painter.circle_stroke(source, r, egui::Stroke::new(1.0, egui::Color32::WHITE));
        }
        painter.circle_stroke(target, r, shadow);
        painter.circle_stroke(target, r, egui::Stroke::new(1.5, color));
    }
}

/// Which part of the crop rect, if any, a drag starting at `pos` would grab.
/// Corners win over edge handles (they overlap on small crops), and both win
/// over the interior.
fn crop_hit_target(pos: egui::Pos2, crop_screen: egui::Rect) -> Option<DragTarget> {
    handle_rects(crop_screen)
        .into_iter()
        .find(|(_, r)| r.contains(pos))
        .map(|(t, _)| t)
        .or_else(|| crop_screen.contains(pos).then_some(DragTarget::Interior))
}

fn crop_cursor(target: DragTarget, dragging: bool) -> egui::CursorIcon {
    match target {
        // TL/BR resize along one diagonal, TR/BL along the other
        DragTarget::Corner(0 | 2) => egui::CursorIcon::ResizeNwSe,
        DragTarget::Corner(_) => egui::CursorIcon::ResizeNeSw,
        DragTarget::Edge(0 | 2) => egui::CursorIcon::ResizeVertical,
        DragTarget::Edge(_) => egui::CursorIcon::ResizeHorizontal,
        DragTarget::Interior if dragging => egui::CursorIcon::Grabbing,
        DragTarget::Interior => egui::CursorIcon::Grab,
    }
}

/// Anchor points of the eight crop handles: corners first (TL, TR, BR, BL),
/// then edge midpoints (top, right, bottom, left).
fn handle_points(crop_screen: egui::Rect) -> [(DragTarget, egui::Pos2); 8] {
    let c = crop_screen.center();
    [
        (DragTarget::Corner(0), crop_screen.left_top()),
        (DragTarget::Corner(1), crop_screen.right_top()),
        (DragTarget::Corner(2), crop_screen.right_bottom()),
        (DragTarget::Corner(3), crop_screen.left_bottom()),
        (DragTarget::Edge(0), egui::pos2(c.x, crop_screen.top())),
        (DragTarget::Edge(1), egui::pos2(crop_screen.right(), c.y)),
        (DragTarget::Edge(2), egui::pos2(c.x, crop_screen.bottom())),
        (DragTarget::Edge(3), egui::pos2(crop_screen.left(), c.y)),
    ]
}

/// Hit areas for the eight crop handles, centered on each anchor — so they
/// reach outside the image when the crop touches its edge.
fn handle_rects(crop_screen: egui::Rect) -> [(DragTarget, egui::Rect); 8] {
    let size = egui::vec2(HANDLE_SIZE * 3.0, HANDLE_SIZE * 3.0);
    handle_points(crop_screen).map(|(t, p)| (t, egui::Rect::from_center_size(p, size)))
}

// ---------------------------------------------------------------------------
// Crop geometry
// ---------------------------------------------------------------------------

/// `start` with one corner dragged to the pointer and the opposite corner
/// anchored, or `None` if that would collapse the crop below minimum size.
fn resize_from_corner(
    start: &Rect,
    corner: u8,
    nx: f32,
    ny: f32,
    aspect: Option<f32>,
) -> Option<Rect> {
    let (x1, y1, x2, y2) = (start.x, start.y, start.x + start.width, start.y + start.height);
    let anchor = match corner {
        0 => (x2, y2), // TL drags, BR anchored
        1 => (x1, y2), // TR drags, BL anchored
        2 => (x1, y1), // BR drags, TL anchored
        3 => (x2, y1), // BL drags, TR anchored
        _ => return None,
    };
    let resized = anchored_rect(anchor, (nx, ny), aspect);
    (resized.width >= 0.01 && resized.height >= 0.01).then_some(resized)
}

/// The rect spanning `anchor` to `pointer`, clamped to the image. With a
/// ratio (in crop-rect units) the rect grows to reach the pointer along
/// whichever axis it's further out on, then shrinks as a whole if that
/// overflows the image — the anchor never moves. Crossing the anchor flips
/// the rect to the other side, like dragging past the opposite corner.
fn anchored_rect(anchor: (f32, f32), pointer: (f32, f32), aspect: Option<f32>) -> Rect {
    let (ax, ay) = anchor;
    let (dx, dy) = (pointer.0 - ax, pointer.1 - ay);
    let room_w = if dx < 0.0 { ax } else { 1.0 - ax };
    let room_h = if dy < 0.0 { ay } else { 1.0 - ay };
    let (mut w, mut h) = (dx.abs().min(room_w), dy.abs().min(room_h));

    if let Some(ratio) = aspect {
        if w > h * ratio {
            h = w / ratio;
        } else {
            w = h * ratio;
        }
        if w > room_w {
            w = room_w;
            h = w / ratio;
        }
        if h > room_h {
            h = room_h;
            w = h * ratio;
        }
    }

    Rect {
        x: if dx < 0.0 { ax - w } else { ax },
        y: if dy < 0.0 { ay - h } else { ay },
        width: w,
        height: h,
    }
}

/// Moves one edge of the crop to the pointer. With a fixed aspect ratio the
/// perpendicular dimension follows, staying centered on its axis; if that
/// would overflow the image, the dragged edge stops where it still fits.
fn resize_from_edge(crop: &mut Rect, edge: u8, nx: f32, ny: f32, aspect: Option<f32>) {
    const MIN: f32 = 0.01;
    let (mut x1, mut y1, mut x2, mut y2) =
        (crop.x, crop.y, crop.x + crop.width, crop.y + crop.height);
    match edge {
        0 => y1 = ny.clamp(0.0, y2 - MIN),
        1 => x2 = nx.clamp(x1 + MIN, 1.0),
        2 => y2 = ny.clamp(y1 + MIN, 1.0),
        3 => x1 = nx.clamp(0.0, x2 - MIN),
        _ => return,
    }

    if let Some(ratio) = aspect {
        if edge % 2 == 0 {
            // Height was dragged; width follows.
            let mut w = (y2 - y1) * ratio;
            if w > 1.0 {
                w = 1.0;
                let h = w / ratio;
                if edge == 0 { y1 = y2 - h } else { y2 = y1 + h }
            }
            let cx = (x1 + x2) / 2.0;
            x1 = (cx - w / 2.0).clamp(0.0, 1.0 - w);
            x2 = x1 + w;
        } else {
            // Width was dragged; height follows.
            let mut h = (x2 - x1) / ratio;
            if h > 1.0 {
                h = 1.0;
                let w = h * ratio;
                if edge == 3 { x1 = x2 - w } else { x2 = x1 + w }
            }
            let cy = (y1 + y2) / 2.0;
            y1 = (cy - h / 2.0).clamp(0.0, 1.0 - h);
            y2 = y1 + h;
        }
    }

    crop.x = x1;
    crop.y = y1;
    crop.width = x2 - x1;
    crop.height = y2 - y1;
}

/// Shrinks `crop` around its center to `ratio`, given in crop-rect units
/// (see `CropAspect::normalized_ratio`), not pixels.
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

        // Edge handles: bars along the edge, so they read as one-axis resizes
        let (long, short) = (hs * 2.0, hs * 0.6);
        let c = crop_screen.center();
        for (p, size) in [
            (egui::pos2(c.x, crop_screen.top()), egui::vec2(long, short)),
            (
                egui::pos2(crop_screen.right(), c.y),
                egui::vec2(short, long),
            ),
            (
                egui::pos2(c.x, crop_screen.bottom()),
                egui::vec2(long, short),
            ),
            (egui::pos2(crop_screen.left(), c.y), egui::vec2(short, long)),
        ] {
            painter.rect_filled(
                egui::Rect::from_center_size(p, size),
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
    ui.horizontal_wrapped(|ui| {
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

/// The color sliders shared by the whole-photo panel and a mask's panel.
struct BasicColor<'a> {
    exposure: &'a mut f32,
    contrast: &'a mut f32,
    highlights: &'a mut f32,
    shadows: &'a mut f32,
    temperature: &'a mut f32,
    saturation: &'a mut f32,
    hue_shift: &'a mut f32,
}

/// One labeled slider with a reset (↺) button shown when it's off zero.
fn adjust_slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
    needs_process: &mut bool,
    last_slider_change: &mut Option<Instant>,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        let resp = ui.add(
            egui::Slider::new(value, range)
                .suffix(suffix)
                .fixed_decimals(2)
                .clamping(egui::SliderClamping::Always),
        );
        if resp.changed() {
            *needs_process = true;
            *last_slider_change = Some(Instant::now());
        }
        if *value != 0.0 && ui.small_button("↺").clicked() {
            *value = 0.0;
            *needs_process = true;
            *last_slider_change = None;
        }
    });
}

/// Exposure through hue shift. `sharpness` is only offered for the whole
/// photo; masks don't carry it (ADR-0019).
fn show_basic_color_sliders(
    ui: &mut egui::Ui,
    c: BasicColor,
    sharpness: Option<&mut f32>,
    needs_process: &mut bool,
    last_slider_change: &mut Option<Instant>,
) {
    let (np, lsc) = (needs_process, last_slider_change);
    adjust_slider(ui, "Exposure", c.exposure, -3.0..=3.0, " EV", np, lsc);
    adjust_slider(ui, "Contrast", c.contrast, -1.0..=1.0, "", np, lsc);
    adjust_slider(ui, "Highlights", c.highlights, -1.0..=1.0, "", np, lsc);
    adjust_slider(ui, "Shadows", c.shadows, -1.0..=1.0, "", np, lsc);
    if let Some(sharpness) = sharpness {
        adjust_slider(ui, "Sharpness", sharpness, 0.0..=2.0, "", np, lsc);
    }
    adjust_slider(ui, "Temperature", c.temperature, -1.0..=1.0, "", np, lsc);
    adjust_slider(ui, "Saturation", c.saturation, -1.0..=1.0, "", np, lsc);

    ui.horizontal(|ui| {
        ui.label("Hue Shift");
        if *c.hue_shift != 0.0 && ui.small_button("↺").clicked() {
            *c.hue_shift = 0.0;
            *np = true;
            *lsc = None;
        }
    });
    if hue_slider(ui, c.hue_shift, 0.0, 180.0) {
        *np = true;
        *lsc = Some(Instant::now());
    }
}

/// The eight selective color bands, for the whole photo or a mask.
fn show_selective_color(
    ui: &mut egui::Ui,
    bands: &mut [crate::state::HslAdjust; 8],
    needs_process: &mut bool,
    last_slider_change: &mut Option<Instant>,
) {
    ui.label(egui::RichText::new("Selective Color").strong());
    const HUE_LABELS: [&str; 8] = [
        "Red", "Orange", "Yellow", "Green", "Cyan", "Blue", "Purple", "Pink",
    ];
    for (idx, label) in HUE_LABELS.iter().enumerate() {
        let adj = &mut bands[idx];
        let base = selective_base_color(idx);
        let bg = selective_bg_color(base);
        let label_color = selective_label_color(base);
        egui::Frame::group(ui.style()).fill(bg).show(ui, |ui| {
            egui::CollapsingHeader::new(egui::RichText::new(*label).strong().color(label_color))
                .show(ui, |ui| {
                    if hue_slider(ui, &mut adj.hue, SELECTIVE_CENTER_HUES[idx], 45.0) {
                        *needs_process = true;
                        *last_slider_change = Some(Instant::now());
                    }
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
}

fn show_color_section(
    ui: &mut egui::Ui,
    state: &mut EditState,
    needs_process: &mut bool,
    last_slider_change: &mut Option<Instant>,
) {
    ui.label(egui::RichText::new("Color").strong());
    ui.add_space(4.0);

    show_basic_color_sliders(
        ui,
        BasicColor {
            exposure: &mut state.exposure,
            contrast: &mut state.contrast,
            highlights: &mut state.highlights,
            shadows: &mut state.shadows,
            temperature: &mut state.temperature,
            saturation: &mut state.saturation,
            hue_shift: &mut state.hue_shift,
        },
        Some(&mut state.sharpness),
        needs_process,
        last_slider_change,
    );

    ui.add_space(6.0);
    show_selective_color(ui, &mut state.selective_color, needs_process, last_slider_change);

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

const SELECTIVE_CENTER_HUES: [f32; 8] = [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 285.0, 330.0];

fn hue_to_rgb(hue_deg: f32) -> egui::Color32 {
    let h = ((hue_deg % 360.0) + 360.0) % 360.0;
    let s: f32 = 0.85;
    let v: f32 = 0.9;
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    egui::Color32::from_rgb(
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

/// Custom hue slider: a gradient-filled track with a draggable handle.
/// `value` is the current offset in degrees (e.g. -45..=45 or -180..=180).
/// `center_hue` is the base hue in degrees. `half_range` is half the slider range.
/// Returns true if the value changed.
fn hue_slider(ui: &mut egui::Ui, value: &mut f32, center_hue: f32, half_range: f32) -> bool {
    let track_height = 14.0;
    let handle_radius = 7.0;
    let desired = egui::vec2(ui.available_width(), track_height);
    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click_and_drag());

    let old_value = *value;

    if response.dragged() || response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let t = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            *value = -half_range + t * 2.0 * half_range;
            if value.abs() < half_range * 0.02 {
                *value = 0.0;
            }
            *value = (*value * 10.0).round() / 10.0;
        }
    }

    let changed = (*value - old_value).abs() > 0.001;

    if !ui.is_rect_visible(rect) {
        return changed;
    }

    // Paint gradient track
    let num_segments = 24;
    let mut mesh = egui::Mesh::default();
    for i in 0..num_segments {
        let t0 = i as f32 / num_segments as f32;
        let t1 = (i + 1) as f32 / num_segments as f32;
        let hue0 = center_hue - half_range + t0 * 2.0 * half_range;
        let hue1 = center_hue - half_range + t1 * 2.0 * half_range;
        let c0 = hue_to_rgb(hue0);
        let c1 = hue_to_rgb(hue1);
        let x0 = rect.left() + t0 * rect.width();
        let x1 = rect.left() + t1 * rect.width();
        let idx = mesh.vertices.len() as u32;
        mesh.vertices.push(egui::epaint::Vertex {
            pos: egui::pos2(x0, rect.top()),
            uv: egui::epaint::WHITE_UV,
            color: c0,
        });
        mesh.vertices.push(egui::epaint::Vertex {
            pos: egui::pos2(x1, rect.top()),
            uv: egui::epaint::WHITE_UV,
            color: c1,
        });
        mesh.vertices.push(egui::epaint::Vertex {
            pos: egui::pos2(x1, rect.bottom()),
            uv: egui::epaint::WHITE_UV,
            color: c1,
        });
        mesh.vertices.push(egui::epaint::Vertex {
            pos: egui::pos2(x0, rect.bottom()),
            uv: egui::epaint::WHITE_UV,
            color: c0,
        });
        mesh.indices
            .extend_from_slice(&[idx, idx + 1, idx + 2, idx, idx + 2, idx + 3]);
    }
    ui.painter().add(egui::Shape::mesh(mesh));

    // Center tick mark
    let center_x = rect.left() + rect.width() * 0.5;
    ui.painter().line_segment(
        [
            egui::pos2(center_x, rect.top()),
            egui::pos2(center_x, rect.bottom()),
        ],
        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(80)),
    );

    // Handle
    let t = (*value + half_range) / (2.0 * half_range);
    let handle_x = rect.left() + t * rect.width();
    let handle_center = egui::pos2(handle_x, rect.center().y);

    ui.painter().circle_filled(
        handle_center + egui::vec2(0.0, 1.0),
        handle_radius + 1.0,
        egui::Color32::from_black_alpha(80),
    );

    let handle_color = if response.dragged() {
        egui::Color32::WHITE
    } else if response.hovered() {
        egui::Color32::from_gray(240)
    } else {
        egui::Color32::from_gray(220)
    };
    ui.painter()
        .circle_filled(handle_center, handle_radius, handle_color);
    ui.painter().circle_stroke(
        handle_center,
        handle_radius,
        egui::Stroke::new(1.0, egui::Color32::from_gray(80)),
    );

    // Show value on hover/drag via egui tooltip (no manual painting above track)
    if response.hovered() || response.dragged() {
        response.on_hover_text(format!("{:.1}°", *value));
    }

    changed
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
        CropAspect, DragTarget, HANDLE_SIZE, INTERACTIVE_PREVIEW_MAX, PreviewBackend, SpotHandle,
        SourceProjection, spot_hit_test,
        bump_requested_generation_for_pending_changes, crop_hit_target, downscale_for_interactive,
        anchored_rect, constrain_aspect, edit_state_signature, load_preview_stages_with_hooks,
        resize_from_corner, resize_from_edge,
        process_preview_with_backend_and_gpu_hook, source_signature,
    };
    use crate::state::EditState;

    #[test]
    fn full_image_crop_corners_are_grabbable_on_both_sides_of_the_edge() {
        let img = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(400.0, 300.0));
        let outside = img.left_top() - egui::vec2(HANDLE_SIZE, HANDLE_SIZE);
        let inside = img.left_top() + egui::vec2(HANDLE_SIZE, HANDLE_SIZE);
        for pos in [outside, inside] {
            assert!(matches!(
                crop_hit_target(pos, img),
                Some(DragTarget::Corner(0))
            ));
        }
        assert!(matches!(
            crop_hit_target(img.right_bottom() + egui::vec2(4.0, 4.0), img),
            Some(DragTarget::Corner(2))
        ));
        assert!(matches!(
            crop_hit_target(img.center(), img),
            Some(DragTarget::Interior)
        ));
        assert!(crop_hit_target(img.left_top() - egui::vec2(20.0, 20.0), img).is_none());
    }

    /// Pixel aspect of a normalized crop on an image of the given aspect.
    fn pixel_aspect(c: &crate::state::Rect, image_aspect: f32) -> f32 {
        c.width / c.height * image_aspect
    }

    #[test]
    fn aspect_presets_produce_their_pixel_ratio_on_a_3x2_image() {
        let image = 1.5;
        for (aspect, want) in [
            (CropAspect::Square, 1.0),
            (CropAspect::Photo4x3, 4.0 / 3.0),
            (CropAspect::Wide16x9, 16.0 / 9.0),
            (CropAspect::Original, 1.5),
        ] {
            let mut c = crop(0.0, 0.0, 1.0, 1.0);
            constrain_aspect(&mut c, aspect.normalized_ratio(Some(image)));
            let got = pixel_aspect(&c, image);
            assert!((got - want).abs() < 1e-4, "{}: got {got}, want {want}", aspect.label());
            assert!(c.width <= 1.0 && c.height <= 1.0);
        }
    }

    #[test]
    fn original_keeps_full_image_crop_untouched() {
        let mut c = crop(0.0, 0.0, 1.0, 1.0);
        constrain_aspect(&mut c, CropAspect::Original.normalized_ratio(Some(1.5)));
        assert_crop(&c, 0.0, 0.0, 1.0, 1.0);
    }

    #[test]
    fn square_on_portrait_image_trims_height() {
        // 2:3 portrait: a square spans the full width and 2/3 of the height.
        let mut c = crop(0.0, 0.0, 1.0, 1.0);
        constrain_aspect(&mut c, CropAspect::Square.normalized_ratio(Some(2.0 / 3.0)));
        assert_crop(&c, 0.0, 1.0 / 6.0, 1.0, 2.0 / 3.0);
    }

    #[test]
    fn free_or_unknown_image_aspect_means_unconstrained() {
        assert_eq!(CropAspect::Free.normalized_ratio(Some(1.5)), None);
        assert_eq!(CropAspect::Square.normalized_ratio(None), None);
    }

    #[test]
    fn locked_corner_drag_keeps_opposite_corner_fixed() {
        let ratio = CropAspect::Square.normalized_ratio(Some(1.5));
        for corner in 0..4u8 {
            let mut c = crop(0.2, 0.2, 0.4, 0.6);
            constrain_aspect(&mut c, ratio);
            let (x1, y1, x2, y2) = (c.x, c.y, c.x + c.width, c.y + c.height);
            let (anchor, pointer) = match corner {
                0 => ((x2, y2), (0.1, 0.3)),
                1 => ((x1, y2), (0.9, 0.3)),
                2 => ((x1, y1), (0.5, 0.9)),
                _ => ((x2, y1), (0.1, 0.9)),
            };
            let c = resize_from_corner(&c, corner, pointer.0, pointer.1, ratio).unwrap();
            let corners = [
                (c.x, c.y),
                (c.x + c.width, c.y),
                (c.x, c.y + c.height),
                (c.x + c.width, c.y + c.height),
            ];
            assert!(
                corners
                    .iter()
                    .any(|p| (p.0 - anchor.0).abs() < 1e-5 && (p.1 - anchor.1).abs() < 1e-5),
                "corner {corner}: anchor {anchor:?} moved, got {:?}",
                (c.x, c.y, c.width, c.height)
            );
            assert!((pixel_aspect(&c, 1.5) - 1.0).abs() < 1e-4, "corner {corner} lost 1:1");
        }
    }

    #[test]
    fn locked_corner_drag_shrinks_to_fit_image_without_moving_anchor() {
        // 1:1 on a 3:2 image is 2/3 in crop units. Anchored at the top-left,
        // dragging to the far bottom-right is limited by height.
        let r = anchored_rect((0.0, 0.0), (1.0, 1.0), Some(2.0 / 3.0));
        assert_crop(&r, 0.0, 0.0, 2.0 / 3.0, 1.0);
    }

    #[test]
    fn free_corner_drag_follows_pointer_and_flips_past_anchor() {
        let start = crop(0.2, 0.2, 0.4, 0.4);
        let c = resize_from_corner(&start, 2, 0.1, 0.1, None).unwrap(); // BR past TL anchor
        assert_crop(&c, 0.1, 0.1, 0.1, 0.1);
        // Onto the anchor itself: too small, so the caller keeps the last rect.
        assert!(resize_from_corner(&start, 2, 0.2, 0.2, None).is_none());
    }

    #[test]
    fn edge_midpoints_hit_edge_handles_with_corners_taking_priority() {
        let img = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
        let c = img.center();
        let cases = [
            (egui::pos2(c.x, img.top() - 4.0), 0),
            (egui::pos2(img.right() + 4.0, c.y), 1),
            (egui::pos2(c.x, img.bottom() - 4.0), 2),
            (egui::pos2(img.left() + 4.0, c.y), 3),
        ];
        for (pos, edge) in cases {
            assert!(matches!(
                crop_hit_target(pos, img),
                Some(DragTarget::Edge(e)) if e == edge
            ));
        }
        // On a crop too small to separate them, the corner wins.
        let tiny = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(10.0, 10.0));
        assert!(matches!(
            crop_hit_target(tiny.left_top(), tiny),
            Some(DragTarget::Corner(0))
        ));
    }

    fn crop(x: f32, y: f32, width: f32, height: f32) -> crate::state::Rect {
        crate::state::Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn assert_crop(c: &crate::state::Rect, x: f32, y: f32, w: f32, h: f32) {
        let eps = 1e-5;
        assert!(
            (c.x - x).abs() < eps
                && (c.y - y).abs() < eps
                && (c.width - w).abs() < eps
                && (c.height - h).abs() < eps,
            "got ({}, {}, {}, {}), want ({x}, {y}, {w}, {h})",
            c.x,
            c.y,
            c.width,
            c.height
        );
    }

    #[test]
    fn free_edge_drag_moves_only_that_edge() {
        let mut c = crop(0.0, 0.0, 1.0, 1.0);
        resize_from_edge(&mut c, 0, 0.9, 0.2, None); // top: x ignored
        assert_crop(&c, 0.0, 0.2, 1.0, 0.8);
        resize_from_edge(&mut c, 1, 0.7, 0.9, None); // right: y ignored
        assert_crop(&c, 0.0, 0.2, 0.7, 0.8);
        resize_from_edge(&mut c, 3, 0.1, 0.5, None); // left
        assert_crop(&c, 0.1, 0.2, 0.6, 0.8);
        resize_from_edge(&mut c, 2, 0.5, 0.6, None); // bottom
        assert_crop(&c, 0.1, 0.2, 0.6, 0.4);
    }

    #[test]
    fn edge_drag_cannot_cross_the_opposite_edge() {
        let mut c = crop(0.2, 0.2, 0.5, 0.5);
        resize_from_edge(&mut c, 3, 0.95, 0.5, None); // left dragged past right
        assert!(c.width >= 0.01 - 1e-6);
        assert!((c.x + c.width - 0.7).abs() < 1e-5, "right edge must stay put");
    }

    #[test]
    fn locked_edge_drag_scales_other_axis_around_its_center() {
        let mut c = crop(0.2, 0.2, 0.4, 0.4);
        resize_from_edge(&mut c, 2, 0.5, 0.5, Some(1.0)); // bottom up: h 0.3
        assert_crop(&c, 0.25, 0.2, 0.3, 0.3);
    }

    #[test]
    fn locked_edge_drag_stops_when_other_axis_hits_image_bounds() {
        let mut c = crop(0.0, 0.0, 1.0, 0.5);
        // Ratio 2: dragging the bottom edge down would need width > 1.
        resize_from_edge(&mut c, 2, 0.5, 0.9, Some(2.0));
        assert_crop(&c, 0.0, 0.0, 1.0, 0.5);
    }

    #[test]
    fn crop_mode_renders_without_applied_crop() {
        let mut v = super::Viewer::new(0, PreviewBackend::Auto);
        v.edit_state.crop = Some(crop(0.5, 0.0, 0.5, 1.0));
        assert!(v.render_state().crop.is_some());

        v.set_crop_mode(true);
        assert!(v.render_state().crop.is_none(), "crop mode must show the full image");
        assert!(v.needs_process, "entering crop mode must re-render");
        assert!(v.edit_state.crop.is_some(), "applied crop itself is kept");

        v.needs_process = false;
        v.set_crop_mode(false);
        assert!(v.render_state().crop.is_some());
        assert!(v.needs_process, "leaving crop mode must re-render");
    }

    #[test]
    fn spot_mode_drops_only_non_invertible_geometry() {
        let mut v = super::Viewer::new(0, PreviewBackend::Auto);
        v.edit_state.rotate = 90;
        v.edit_state.flip_h = true;
        v.edit_state.straighten = 3.0;
        v.edit_state.keystone.vertical = 0.2;
        v.edit_state.crop = Some(crop(0.1, 0.1, 0.5, 0.5));
        v.edit_state.exposure = 0.7;

        v.set_spot_mode(true);
        let r = v.render_state();
        assert_eq!(r.straighten, 0.0);
        assert_eq!(r.keystone.vertical, 0.0);
        // Exactly invertible geometry stays, so spots are placed on the
        // oriented, cropped view.
        assert_eq!((r.rotate, r.flip_h), (90, true));
        assert!(r.crop.is_some());
        assert_eq!(r.exposure, 0.7, "color edits stay on while placing spots");
        assert!(v.needs_process);
        assert_eq!(v.edit_state.straighten, 3.0, "saved geometry is untouched");
    }

    #[test]
    fn split_before_side_keeps_geometry_but_drops_other_edits_by_default() {
        let mut v = super::Viewer::new(0, PreviewBackend::Auto);
        v.edit_state.rotate = 90;
        v.edit_state.straighten = 2.0;
        v.edit_state.crop = Some(crop(0.1, 0.1, 0.5, 0.5));
        v.edit_state.exposure = 0.8;
        v.edit_state.spots.push(crate::state::Spot::new([0.5, 0.5], 0.05, 1.0));

        let before = v.original_state();
        assert_eq!((before.rotate, before.straighten), (90, 2.0));
        assert!(before.crop.is_some());
        assert_eq!(before.exposure, 0.0);
        assert!(before.spots.is_empty(), "retouching is part of the edit, not the before");
    }

    #[test]
    fn show_original_crop_renders_the_before_side_as_shot() {
        let mut v = super::Viewer::new(0, PreviewBackend::Auto);
        v.edit_state.rotate = 90;
        v.edit_state.crop = Some(crop(0.1, 0.1, 0.5, 0.5));
        v.split_original_crop = true;
        let before = v.original_state();
        assert_eq!(before.rotate, 0);
        assert!(before.crop.is_none());
    }

    #[test]
    fn before_side_rerenders_when_geometry_changes_but_not_color() {
        let mut v = super::Viewer::new(0, PreviewBackend::Auto);
        let preview = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(8, 6, Rgba([1, 2, 3, 255])));
        let base = v.original_signature(&preview);
        v.edit_state.exposure = 1.0;
        assert_eq!(v.original_signature(&preview), base, "color edits don't touch the before side");
        v.edit_state.crop = Some(crop(0.0, 0.0, 0.5, 0.5));
        assert_ne!(v.original_signature(&preview), base);
        let bigger = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(16, 12, Rgba([1, 2, 3, 255])));
        assert_ne!(
            v.original_signature(&bigger),
            v.original_signature(&preview),
            "a reloaded higher-res preview needs a new before render"
        );
    }

    #[test]
    fn mask_tool_is_exclusive_with_crop_and_spot() {
        let mut v = super::Viewer::new(0, PreviewBackend::Auto);
        v.set_spot_mode(true);
        v.set_mask_mode(true);
        assert!(v.mask_mode && !v.spot_mode);
        v.set_crop_mode(true);
        v.pending_crop = Some(crop(0.0, 0.0, 0.5, 0.5));
        v.set_mask_mode(false);
        v.set_mask_mode(true);
        assert!(!v.crop_mode && v.pending_crop.is_none());
        v.set_spot_mode(true);
        assert!(!v.mask_mode);
    }

    #[test]
    fn mask_mode_renders_like_spot_mode() {
        let mut v = super::Viewer::new(0, PreviewBackend::Auto);
        v.edit_state.rotate = 90;
        v.edit_state.straighten = 3.0;
        v.edit_state.crop = Some(crop(0.1, 0.1, 0.5, 0.5));
        v.set_mask_mode(true);
        let r = v.render_state();
        assert_eq!((r.rotate, r.straighten), (90, 0.0));
        assert!(r.crop.is_some());
    }

    #[test]
    fn new_masks_get_unique_names_and_are_selected() {
        let mut v = super::Viewer::new(0, PreviewBackend::Auto);
        v.add_mask();
        v.add_mask();
        v.edit_state.masks[0].name = "Face".into();
        let i = v.add_mask();
        let names: Vec<&str> = v.edit_state.masks.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["Face", "Mask 2", "Mask 1"]);
        assert_eq!(v.selected_mask, Some(i));
    }

    #[test]
    fn deleting_a_mask_selects_a_neighbour() {
        let mut v = super::Viewer::new(0, PreviewBackend::Auto);
        v.add_mask();
        v.add_mask();
        v.selected_mask = Some(1);
        v.delete_selected_mask();
        assert_eq!(v.edit_state.masks.len(), 1);
        assert_eq!(v.selected_mask, Some(0));
        v.delete_selected_mask();
        assert!(v.edit_state.masks.is_empty());
        assert_eq!(v.selected_mask, None);
    }

    #[test]
    fn source_distance_uses_shorter_side_units() {
        // 2:1 image: half the width is one short side.
        assert!((super::source_distance([0.0, 0.5], [0.5, 0.5], 2.0) - 1.0).abs() < 1e-6);
        assert!((super::source_distance([0.5, 0.0], [0.5, 1.0], 2.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn spot_and_crop_modes_are_exclusive() {
        let mut v = super::Viewer::new(0, PreviewBackend::Auto);
        v.set_crop_mode(true);
        v.pending_crop = Some(crop(0.0, 0.0, 0.5, 0.5));
        v.set_spot_mode(true);
        assert!(!v.crop_mode);
        assert!(v.pending_crop.is_none(), "entering spot mode discards the unapplied crop");
    }

    #[test]
    fn deleting_the_selected_spot_removes_only_it() {
        let mut v = super::Viewer::new(0, PreviewBackend::Auto);
        for x in [0.2, 0.5, 0.8] {
            v.edit_state.spots.push(crate::state::Spot::new([x, 0.5], 0.05, 1.0));
        }
        v.selected_spot = Some(1);
        v.delete_selected_spot();
        let xs: Vec<f32> = v.edit_state.spots.iter().map(|s| s.target[0]).collect();
        assert_eq!(xs, vec![0.2, 0.8]);
        assert_eq!(v.selected_spot, None);
    }

    fn projection(
        img_rect: egui::Rect,
        source_aspect: f32,
        rotate: i32,
        flip_h: bool,
        flip_v: bool,
        crop: Option<crate::state::Rect>,
    ) -> SourceProjection {
        SourceProjection {
            img_rect,
            source_aspect,
            rotate,
            flip_h,
            flip_v,
            crop,
        }
    }

    #[test]
    fn spot_projection_round_trips_through_every_orientation_and_crop() {
        let img = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(300.0, 200.0));
        let p = [0.3, 0.6];
        for rotate in [0, 90, 180, 270] {
            for (fh, fv) in [(false, false), (true, false), (false, true), (true, true)] {
                for crop in [None, Some(crop(0.1, 0.2, 0.7, 0.6))] {
                    let proj = projection(img, 1.5, rotate, fh, fv, crop);
                    let back = proj.to_source(proj.to_screen(p));
                    assert!(
                        (back[0] - p[0]).abs() < 1e-5 && (back[1] - p[1]).abs() < 1e-5,
                        "rotate {rotate} flip {fh}/{fv}: {back:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn spot_projection_rotates_clockwise_like_the_pipeline() {
        let img = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        // Source top-left lands top-right after a clockwise 90° turn.
        let proj = projection(img, 1.0, 90, false, false, None);
        assert_eq!(proj.to_screen([0.0, 0.0]), egui::pos2(100.0, 0.0));
    }

    #[test]
    fn spot_radius_scales_with_crop_zoom() {
        // 3:2 source shown uncropped 300px wide: shorter side is 200px.
        let img = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(300.0, 200.0));
        let full = projection(img, 1.5, 0, false, false, None);
        assert!((full.radius_px(0.1) - 20.0).abs() < 1e-3);
        // Same screen size showing a half-width crop: twice as big on screen.
        let cropped = projection(img, 1.5, 0, false, false, Some(crop(0.0, 0.0, 0.5, 0.5)));
        assert!((cropped.radius_px(0.1) - 40.0).abs() < 1e-3);
        // Rotated 90°: the displayed image is 2:3; shorter side is its width.
        let tall = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 300.0));
        let rotated = projection(tall, 1.5, 90, false, false, None);
        assert!((rotated.radius_px(0.1) - 20.0).abs() < 1e-3);
    }

    #[test]
    fn spot_hit_test_prefers_selected_source_then_topmost_target() {
        let img = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 400.0));
        let proj = projection(img, 1.0, 0, false, false, None);
        let mut a = crate::state::Spot::new([0.25, 0.25], 0.05, 1.0);
        a.source = [0.5, 0.5];
        let b = crate::state::Spot::new([0.5, 0.5], 0.05, 1.0); // target over a's source
        let spots = vec![a, b];
        let center = egui::pos2(200.0, 200.0);
        assert_eq!(spot_hit_test(&spots, None, center, &proj), Some((1, SpotHandle::Target)));
        assert_eq!(spot_hit_test(&spots, Some(0), center, &proj), Some((0, SpotHandle::Source)));
        assert_eq!(
            spot_hit_test(&spots, None, egui::pos2(100.0, 100.0), &proj),
            Some((0, SpotHandle::Target))
        );
        assert_eq!(spot_hit_test(&spots, None, egui::pos2(390.0, 10.0), &proj), None);
    }

    #[test]
    fn toggling_crop_mode_without_applied_crop_skips_rerender() {
        let mut v = super::Viewer::new(0, PreviewBackend::Auto);
        v.set_crop_mode(true);
        assert!(!v.needs_process);
    }

    #[test]
    fn saving_edits_writes_a_thumbnail_showing_them_and_reset_removes_it() {
        if !crate::processing::gpu_pipeline::is_available()
            && !crate::processing::gpu_pipeline::allow_debug_cpu_fallback()
        {
            return;
        }
        let dir = std::env::temp_dir().join(format!("photograph-save-thumb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("grey.png");
        let mut v = super::Viewer::new(0, PreviewBackend::Auto);
        v.current_path = Some(photo.clone());
        v.preview = Some(DynamicImage::ImageRgba8(ImageBuffer::from_pixel(64, 48, Rgba([100, 100, 100, 255]))));
        v.edit_state.exposure = 1.0;
        let ctx = egui::Context::default();

        v.save_edits();
        assert!(v.take_changed_sidecars().is_empty(), "not reported before the thumbnail lands");
        let mut changed = Vec::new();
        for _ in 0..200 {
            v.drain(&ctx);
            changed = v.take_changed_sidecars();
            if !changed.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(changed, vec![photo.clone()]);
        let thumb = image::open(crate::state::edited_thumbnail_path(&photo)).unwrap().to_rgba8();
        let (w, h) = thumb.dimensions();
        assert!(thumb.get_pixel(w / 2, h / 2).0[0] > 150, "thumbnail shows the edit");

        v.edit_state.exposure = 0.0;
        v.save_edits();
        assert_eq!(v.take_changed_sidecars(), vec![photo.clone()], "reset reported at once");
        assert!(!crate::state::edited_thumbnail_path(&photo).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_replaced_preview_of_the_same_size_gets_new_render_keys() {
        // A RAW's embedded JPEG and its full develop arrive at the same size.
        let mut v = super::Viewer::new(0, PreviewBackend::Auto);
        let tx = v.tx.clone();
        let jpeg_look = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(8, 6, Rgba([200, 90, 60, 255])));
        let develop = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(8, 6, Rgba([150, 110, 100, 255])));
        let path = std::path::PathBuf::from("/photos/a.CR3");
        v.current_path = Some(path.clone());
        let ctx = egui::Context::default();

        tx.send(super::BgResult::Loaded { path: path.clone(), img: jpeg_look.clone() }).unwrap();
        v.drain(&ctx);
        let key_jpeg = v.build_preview_cache_key(&jpeg_look, super::ProcessQuality::Final);
        let before_jpeg = v.original_signature(&jpeg_look);

        tx.send(super::BgResult::Loaded { path, img: develop.clone() }).unwrap();
        v.drain(&ctx);
        let key_develop = v.build_preview_cache_key(&develop, super::ProcessQuality::Final);
        assert!(key_jpeg != key_develop, "same size and edits, different preview");
        assert_ne!(before_jpeg, v.original_signature(&develop));
    }

    #[test]
    fn fast_display_conversion_matches_egui_for_opaque_and_transparent() {
        let opaque = image::RgbaImage::from_fn(37, 23, |x, y| {
            Rgba([(x * 7) as u8, (y * 11) as u8, (x * y) as u8, 255])
        });
        let fast = super::color_image_from_rgba(opaque.clone());
        let egui_way = egui::ColorImage::from_rgba_unmultiplied([37, 23], opaque.as_raw());
        assert_eq!(fast.pixels, egui_way.pixels);
        assert_eq!(fast.size, [37, 23]);

        let mut transparent = opaque.clone();
        transparent.get_pixel_mut(3, 4).0[3] = 100;
        let fast = super::color_image_from_rgba(transparent.clone());
        let egui_way = egui::ColorImage::from_rgba_unmultiplied([37, 23], transparent.as_raw());
        assert_eq!(fast.pixels, egui_way.pixels, "non-opaque images get egui's premultiplication");
    }

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
    fn auto_mode_uses_cpu_fallback_when_gpu_is_unavailable_and_debug_enabled() {
        let img = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(4, 4, Rgba([80, 90, 100, 255])));
        let mut state = EditState::default();
        state.exposure = 0.3;
        state.contrast = 0.2;

        let expected = crate::processing::transform::apply(&img, &state);
        let out = process_preview_with_backend_and_gpu_hook(
            &img,
            &state,
            PreviewBackend::Auto,
            true,
            |_source, _state| None,
        );
        assert_eq!(out.to_rgba8().into_raw(), expected.to_rgba8().into_raw());
    }

    #[test]
    fn auto_mode_panics_when_gpu_is_unavailable_and_debug_fallback_is_disabled() {
        let img = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(4, 4, Rgba([80, 90, 100, 255])));
        let state = EditState::default();

        let result = std::panic::catch_unwind(|| {
            process_preview_with_backend_and_gpu_hook(
                &img,
                &state,
                PreviewBackend::Auto,
                false,
                |_source, _state| None,
            )
        });
        assert!(result.is_err());
    }

    #[test]
    fn cpu_mode_uses_cpu_only_when_debug_fallback_enabled() {
        let img = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(3, 3, Rgba([20, 30, 40, 255])));
        let mut state = EditState::default();
        state.exposure = 0.4;

        let expected = crate::processing::transform::apply(&img, &state);
        let out = process_preview_with_backend_and_gpu_hook(
            &img,
            &state,
            PreviewBackend::Cpu,
            true,
            |_source, _state| panic!("gpu path should not be called in cpu mode"),
        );
        assert_eq!(out.to_rgba8().into_raw(), expected.to_rgba8().into_raw());
    }

    #[test]
    fn cpu_mode_uses_gpu_path_when_debug_fallback_is_disabled() {
        let img = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(3, 3, Rgba([20, 30, 40, 255])));
        let state = EditState::default();

        let out = process_preview_with_backend_and_gpu_hook(
            &img,
            &state,
            PreviewBackend::Cpu,
            false,
            |_source, _state| {
                Some(DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
                    3,
                    3,
                    Rgba([200, 100, 50, 255]),
                )))
            },
        );
        assert_eq!(out.to_rgba8().get_pixel(0, 0).0, [200, 100, 50, 255]);
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

    #[test]
    fn raw_embedded_preview_adds_full_quality_stage() {
        let path = Path::new("/tmp/test.raf");
        let out = load_preview_stages_with_hooks(
            path,
            2000,
            |_path| {
                Ok((
                    DynamicImage::ImageRgba8(ImageBuffer::from_pixel(1200, 800, Rgba([1, 2, 3, 255]))),
                    crate::thumbnail::PreviewSource::Embedded,
                ))
            },
            |_path| {
                Ok(DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
                    4000,
                    3000,
                    Rgba([9, 9, 9, 255]),
                )))
            },
        )
        .expect("staged preview load should succeed");

        assert_eq!(out.len(), 2);
        assert_eq!(out[0].width(), 1200);
        assert_eq!(out[0].height(), 800);
        assert_eq!(out[1].width(), 2000);
        assert_eq!(out[1].height(), 1500);
    }

    #[test]
    fn raw_full_preview_source_does_not_add_second_stage() {
        let path = Path::new("/tmp/test.raf");
        let out = load_preview_stages_with_hooks(
            path,
            2000,
            |_path| {
                Ok((
                    DynamicImage::ImageRgba8(ImageBuffer::from_pixel(1800, 1200, Rgba([1, 2, 3, 255]))),
                    crate::thumbnail::PreviewSource::FullDevelop,
                ))
            },
            |_path| {
                panic!("full decode should not run when preview source is already full quality")
            },
        )
        .expect("staged preview load should succeed");

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].width(), 1800);
        assert_eq!(out[0].height(), 1200);
    }

    #[test]
    fn raw_embedded_preview_keeps_first_stage_if_full_decode_fails() {
        let path = Path::new("/tmp/test.raf");
        let out = load_preview_stages_with_hooks(
            path,
            2000,
            |_path| {
                Ok((
                    DynamicImage::ImageRgba8(ImageBuffer::from_pixel(1600, 1066, Rgba([1, 2, 3, 255]))),
                    crate::thumbnail::PreviewSource::Embedded,
                ))
            },
            |_path| anyhow::bail!("full decode failed"),
        )
        .expect("staged preview load should keep embedded stage");

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].width(), 1600);
        assert_eq!(out[0].height(), 1066);
    }
}

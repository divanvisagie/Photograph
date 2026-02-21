use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

use image::DynamicImage;

use crate::state::EditState;

/// Downscale loaded images to this longest-edge size for the preview.
const PREVIEW_MAX: u32 = 1920;
const DEBOUNCE: Duration = Duration::from_millis(300);

enum BgResult {
    Loaded { path: PathBuf, img: DynamicImage },
    LoadFailed(PathBuf),
    Processed(Vec<u8>, usize, usize),
}

pub struct Viewer {
    current_path: Option<PathBuf>,
    /// Downscaled original kept in memory for non-destructive re-processing.
    preview: Option<DynamicImage>,
    pub edit_state: EditState,
    /// Process needed but not yet kicked off.
    needs_process: bool,
    /// Set when a slider is being moved; cleared once debounce elapses.
    last_slider_change: Option<Instant>,
    texture: Option<egui::TextureHandle>,
    loading: bool,
    processing: bool,
    pub metadata: Option<crate::metadata::ImageMetadata>,
    tx: mpsc::SyncSender<BgResult>,
    rx: mpsc::Receiver<BgResult>,
}

impl Viewer {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel(8);
        Self {
            current_path: None,
            preview: None,
            edit_state: EditState::default(),
            needs_process: false,
            last_slider_change: None,
            texture: None,
            loading: false,
            processing: false,
            metadata: None,
            tx,
            rx,
        }
    }

    pub fn set_image(&mut self, path: PathBuf, ctx: &egui::Context) {
        if self.current_path.as_ref() == Some(&path) {
            return;
        }
        self.current_path = Some(path.clone());
        self.preview = None;
        self.texture = None;
        self.edit_state = EditState::default();
        self.needs_process = false;
        self.last_slider_change = None;
        self.loading = true;
        self.processing = false;
        self.metadata = crate::metadata::read(&path).ok();

        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            match crate::thumbnail::open_image(&path) {
                Ok(img) => {
                    let preview = if img.width() > PREVIEW_MAX || img.height() > PREVIEW_MAX {
                        img.thumbnail(PREVIEW_MAX, PREVIEW_MAX)
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

    fn trigger_process(&mut self, ctx: &egui::Context) {
        let Some(ref preview) = self.preview else { return };
        if self.processing {
            return;
        }
        self.processing = true;
        self.needs_process = false;
        self.last_slider_change = None;

        let img = preview.clone();
        let state = self.edit_state.clone();
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let result = crate::processing::transform::apply(&img, &state);
            let rgba = result.to_rgba8();
            let w = rgba.width() as usize;
            let h = rgba.height() as usize;
            let _ = tx.send(BgResult::Processed(rgba.into_raw(), w, h));
            ctx2.request_repaint();
        });
    }

    fn drain(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                BgResult::Loaded { path, img } => {
                    if self.current_path.as_ref() == Some(&path) {
                        self.preview = Some(img);
                        self.loading = false;
                        self.needs_process = true;
                    }
                }
                BgResult::LoadFailed(path) => {
                    if self.current_path.as_ref() == Some(&path) {
                        self.loading = false;
                    }
                }
                BgResult::Processed(data, w, h) => {
                    self.processing = false;
                    let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &data);
                    self.texture = Some(ctx.load_texture(
                        "viewer_tex",
                        img,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
        }
    }

    pub fn show_panel(&mut self, ctx: &egui::Context) {
        self.drain(ctx);

        // Kick off processing when ready, respecting debounce for sliders.
        if self.needs_process && !self.processing && self.preview.is_some() {
            let debounce_done = self
                .last_slider_change
                .map(|t| t.elapsed() >= DEBOUNCE)
                .unwrap_or(true);
            if debounce_done {
                self.trigger_process(ctx);
            } else {
                ctx.request_repaint_after(DEBOUNCE);
            }
        }

        // Snapshot display state before the closure borrows self for edits.
        let path = self.current_path.clone();
        let loading = self.loading;
        let processing = self.processing;
        let texture = self.texture.clone();
        let metadata = self.metadata.clone();

        egui::SidePanel::right("viewer_panel")
            .min_width(340.0)
            .default_width(460.0)
            .show(ctx, |ui| {
                let Some(ref path) = path else {
                    ui.centered_and_justified(|ui| {
                        ui.label("Select an image");
                    });
                    return;
                };

                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                ui.label(egui::RichText::new(&name).strong().size(13.0));
                ui.separator();

                egui::ScrollArea::vertical()
                    .id_salt("viewer_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // Image display
                        let avail_w = ui.available_width();
                        let img_max_h = (ui.ctx().screen_rect().height() * 0.55).max(180.0);

                        if loading || (processing && texture.is_none()) {
                            ui.allocate_ui(egui::vec2(avail_w, img_max_h), |ui| {
                                ui.centered_and_justified(|ui| {
                                    ui.spinner();
                                });
                            });
                        } else if let Some(ref tex) = texture {
                            let tex_size = tex.size_vec2();
                            let scale = (avail_w / tex_size.x).min(img_max_h / tex_size.y);
                            let display = tex_size * scale;
                            let (img_rect, _) =
                                ui.allocate_exact_size(display, egui::Sense::hover());
                            ui.painter().image(
                                tex.id(),
                                img_rect,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                egui::Color32::WHITE,
                            );
                            // Spinner overlay while re-processing an existing image
                            if processing {
                                ui.painter().rect_filled(
                                    img_rect,
                                    0.0,
                                    egui::Color32::from_black_alpha(80),
                                );
                            }
                        } else {
                            ui.allocate_ui(egui::vec2(avail_w, 40.0), |ui| {
                                ui.label("⚠ Could not open image");
                            });
                        }

                        ui.add_space(8.0);
                        ui.separator();

                        // Transform controls
                        show_transform_section(ui, &mut self.edit_state, &mut self.needs_process, &mut self.last_slider_change);

                        ui.separator();

                        // EXIF
                        if let Some(ref meta) = metadata {
                            show_exif(ui, meta);
                        } else {
                            ui.label(egui::RichText::new("No EXIF data").weak());
                        }
                    });
            });
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

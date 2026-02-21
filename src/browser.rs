use std::{collections::HashMap, path::PathBuf, sync::mpsc};

const CELL: f32 = 170.0;

static IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "tiff", "tif", "webp", "bmp",
    // Raw — will fail gracefully; magick-rust handles these later
    "raf", "dng", "heic", "avif",
];

enum ThumbState {
    Loading,
    Ready(egui::TextureHandle),
    Failed,
}

struct ThumbResult {
    path: PathBuf,
    rgba: Option<(Vec<u8>, usize, usize)>,
}

pub struct Browser {
    pub current_dir: PathBuf,
    subdirs: Vec<(PathBuf, String)>,
    pub images: Vec<(PathBuf, String)>,
    pending_nav: Option<PathBuf>,
    thumbnails: HashMap<PathBuf, ThumbState>,
    tx: mpsc::SyncSender<ThumbResult>,
    rx: mpsc::Receiver<ThumbResult>,
    pub selected: Option<PathBuf>,
}

impl Browser {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel(64);
        let mut b = Self {
            current_dir: dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
            subdirs: Vec::new(),
            images: Vec::new(),
            pending_nav: None,
            thumbnails: HashMap::new(),
            tx,
            rx,
            selected: None,
        };
        b.scan();
        b
    }

    fn scan(&mut self) {
        self.subdirs.clear();
        self.images.clear();
        self.thumbnails.clear();

        let Ok(rd) = std::fs::read_dir(&self.current_dir) else { return };

        for entry in rd.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                self.subdirs.push((path, name));
            } else if is_image(&path) {
                self.images.push((path, name));
            }
        }

        self.subdirs.sort_by(|a, b| a.1.cmp(&b.1));
        self.images.sort_by(|a, b| a.1.cmp(&b.1));
    }

    fn queue_pending_thumbs(&mut self, ctx: &egui::Context) {
        let to_queue: Vec<PathBuf> = self
            .images
            .iter()
            .filter(|(p, _)| !self.thumbnails.contains_key(p))
            .map(|(p, _)| p.clone())
            .collect();

        for path in to_queue {
            self.thumbnails.insert(path.clone(), ThumbState::Loading);
            let tx = self.tx.clone();
            let ctx2 = ctx.clone();
            let cache_dir = self.current_dir.join(".thumbnails");
            std::thread::spawn(move || {
                let result = generate_thumb(&path, &cache_dir);
                let _ = tx.send(ThumbResult { path, rgba: result });
                ctx2.request_repaint();
            });
        }
    }

    fn drain_channel(&mut self, ctx: &egui::Context) {
        while let Ok(ThumbResult { path, rgba }) = self.rx.try_recv() {
            let state = match rgba {
                Some((data, w, h)) => {
                    let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &data);
                    let tex = ctx.load_texture(
                        path.to_string_lossy().as_ref(),
                        img,
                        egui::TextureOptions::LINEAR,
                    );
                    ThumbState::Ready(tex)
                }
                None => ThumbState::Failed,
            };
            self.thumbnails.insert(path, state);
        }
    }

    pub fn show_nav(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("browser_nav").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button("⬆")
                    .on_hover_text("Parent directory")
                    .clicked()
                {
                    if let Some(p) = self.current_dir.parent() {
                        self.pending_nav = Some(p.to_path_buf());
                    }
                }
                ui.monospace(self.current_dir.display().to_string());
            });
        });
    }

    pub fn show_grid(&mut self, ctx: &egui::Context) {
        if let Some(nav) = self.pending_nav.take() {
            self.current_dir = nav;
            self.selected = None;
            self.scan();
        }

        self.drain_channel(ctx);
        self.queue_pending_thumbs(ctx);

        let mut new_sel: Option<PathBuf> = None;
        let mut nav_to: Option<PathBuf> = None;
        let current_sel = self.selected.clone();

        egui::CentralPanel::default().show(ctx, |ui| {
            // Subdirectory buttons
            if !self.subdirs.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for (path, name) in &self.subdirs {
                        if ui.button(format!("📁 {}", name)).clicked() {
                            nav_to = Some(path.clone());
                        }
                    }
                });
                ui.separator();
            }

            if self.images.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label("No images in this directory");
                });
                return;
            }

            let avail_w = ui.available_width();
            let cols = ((avail_w / (CELL + 8.0)) as usize).max(1);

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new("image_grid")
                        .num_columns(cols)
                        .spacing([8.0, 8.0])
                        .show(ui, |ui| {
                            for (i, (path, name)) in self.images.iter().enumerate() {
                                let is_sel = current_sel.as_ref() == Some(path);
                                let thumb = match self.thumbnails.get(path) {
                                    Some(ThumbState::Ready(tex)) => {
                                        Some((tex.id(), tex.size_vec2()))
                                    }
                                    _ => None,
                                };

                                if draw_thumb_cell(ui, name, thumb, is_sel) {
                                    new_sel = Some(path.clone());
                                }

                                if (i + 1) % cols == 0 {
                                    ui.end_row();
                                }
                            }
                        });
                });
        });

        if let Some(nav) = nav_to {
            self.pending_nav = Some(nav);
        }
        if let Some(sel) = new_sel {
            self.selected = Some(sel);
        }
    }
}

fn draw_thumb_cell(
    ui: &mut egui::Ui,
    name: &str,
    thumb: Option<(egui::TextureId, egui::Vec2)>,
    selected: bool,
) -> bool {
    let (resp, painter) =
        ui.allocate_painter(egui::vec2(CELL, CELL + 22.0), egui::Sense::click());
    let rect = resp.rect;

    // Background
    if selected {
        painter.rect_filled(rect, 4.0, ui.visuals().selection.bg_fill);
    } else if resp.hovered() {
        painter.rect_filled(rect, 4.0, ui.visuals().widgets.hovered.bg_fill);
    }

    // Image area
    let img_rect = egui::Rect::from_min_size(rect.min, egui::vec2(CELL, CELL));
    match thumb {
        Some((tex_id, tex_size)) => {
            let scale = (CELL / tex_size.x).min(CELL / tex_size.y);
            let display = tex_size * scale;
            let offset = (egui::vec2(CELL, CELL) - display) * 0.5;
            let draw_rect = egui::Rect::from_min_size(img_rect.min + offset, display);
            painter.image(
                tex_id,
                draw_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        None => {
            painter.rect_filled(img_rect, 4.0, egui::Color32::from_gray(40));
            painter.text(
                img_rect.center(),
                egui::Align2::CENTER_CENTER,
                "…",
                egui::FontId::proportional(22.0),
                egui::Color32::GRAY,
            );
        }
    }

    // Filename label
    let label_pos = egui::pos2(rect.center().x, img_rect.max.y + 11.0);
    let name_short = if name.len() > 24 { &name[..24] } else { name };
    painter.text(
        label_pos,
        egui::Align2::CENTER_CENTER,
        name_short,
        egui::FontId::proportional(11.0),
        ui.visuals().text_color(),
    );

    resp.clicked()
}

fn generate_thumb(path: &PathBuf, cache_dir: &PathBuf) -> Option<(Vec<u8>, usize, usize)> {
    let thumb_path = crate::thumbnail::cache_path(path, cache_dir);

    let img = if thumb_path.exists() {
        image::open(&thumb_path).ok()?
    } else {
        let full = image::open(path).ok()?;
        let t = full.thumbnail(
            crate::thumbnail::THUMB_SIZE,
            crate::thumbnail::THUMB_SIZE,
        );
        let _ = std::fs::create_dir_all(cache_dir);
        let _ = t.save(&thumb_path);
        t
    };

    let rgba = img.to_rgba8();
    let w = rgba.width() as usize;
    let h = rgba.height() as usize;
    Some((rgba.into_raw(), w, h))
}

fn is_image(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

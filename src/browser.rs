use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::mpsc,
};

const CELL: f32 = 170.0;
const FILMSTRIP_CELL: f32 = 64.0;
const MAX_THUMB_JOBS: usize = 4;

enum ThumbState {
    Loading,
    Ready(egui::TextureHandle),
    Failed,
}

struct ThumbResult {
    path: PathBuf,
    rgba: Option<(Vec<u8>, usize, usize)>,
}

/// File browser state for directory navigation and thumbnail selection.
pub struct Browser {
    pub current_dir: PathBuf,
    subdirs: Vec<(PathBuf, String)>,
    pub images: Vec<(PathBuf, String)>,
    pending_nav: Option<PathBuf>,
    thumbnails: HashMap<PathBuf, ThumbState>,
    tx: mpsc::SyncSender<ThumbResult>,
    rx: mpsc::Receiver<ThumbResult>,
    pub selected: Option<PathBuf>,
    marked: HashSet<PathBuf>,
    path_edit: String,
    locations: Vec<(PathBuf, String)>,
    network_locations: Vec<(PathBuf, String)>,
    scan_error: Option<String>,
}

impl Browser {
    /// Creates a browser rooted at `initial_dir` or a reasonable fallback directory.
    pub fn new(initial_dir: Option<PathBuf>) -> Self {
        let dir = initial_dir.filter(|p| p.is_dir()).unwrap_or_else(|| {
            let pictures = dirs::picture_dir()
                .or_else(|| dirs::home_dir().map(|h| h.join("Pictures")))
                .filter(|p| p.is_dir());
            pictures.unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")))
        });
        let (tx, rx) = mpsc::sync_channel(64);
        let mut b = Self {
            path_edit: dir.display().to_string(),
            current_dir: dir,
            subdirs: Vec::new(),
            images: Vec::new(),
            pending_nav: None,
            thumbnails: HashMap::new(),
            tx,
            rx,
            selected: None,
            marked: HashSet::new(),
            locations: Vec::new(),
            network_locations: Vec::new(),
            scan_error: None,
        };
        b.scan_locations();
        b.scan_network_locations();
        b.scan();
        b
    }

    fn scan(&mut self) {
        self.subdirs.clear();
        self.images.clear();
        self.thumbnails.clear();
        self.scan_error = None;

        let rd = match std::fs::read_dir(&self.current_dir) {
            Ok(rd) => rd,
            Err(e) => {
                let msg = if e.kind() == std::io::ErrorKind::PermissionDenied {
                    if std::env::var_os("SNAP").is_some() {
                        format!(
                            "Cannot read this directory (permission denied). \
                             If installed as a Snap, run:\n\
                             sudo snap connect photograph:removable-media"
                        )
                    } else {
                        format!("Cannot read this directory: permission denied")
                    }
                } else {
                    format!("Cannot read this directory: {e}")
                };
                self.scan_error = Some(msg);
                return;
            }
        };

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

    fn scan_locations(&mut self) {
        self.locations.clear();

        if let Some(home) = dirs::home_dir() {
            self.locations.push((home, "Home".into()));
        }

        let user = std::env::var("USER").unwrap_or_default();
        let search_dirs = [
            format!("/media/{user}"),
            "/mnt".into(),
            format!("/run/media/{user}"),
        ];

        for parent in &search_dirs {
            let parent = PathBuf::from(parent);
            if let Ok(entries) = std::fs::read_dir(&parent) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        self.locations.push((path, name));
                    }
                }
            }
        }
    }

    /// Finds currently-mounted network shares: active GVfs mounts (the way
    /// GNOME/Nautilus surfaces `smb://`/`sftp://` connections) plus classic
    /// NFS/CIFS/sshfs entries from `/proc/mounts`.
    fn scan_network_locations(&mut self) {
        self.network_locations.clear();
        let mut seen = HashSet::new();

        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            let gvfs_dir = PathBuf::from(runtime_dir).join("gvfs");
            if let Ok(entries) = std::fs::read_dir(&gvfs_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() && seen.insert(path.clone()) {
                        let raw = entry.file_name().to_string_lossy().into_owned();
                        self.network_locations.push((path, friendly_gvfs_label(&raw)));
                    }
                }
            }
        }

        const NETWORK_FS_TYPES: &[&str] = &[
            "nfs",
            "nfs4",
            "cifs",
            "smb3",
            "smbfs",
            "fuse.sshfs",
            "fuse.rclone",
            "davfs",
            "fuse.davfs2",
            "afs",
            "9p",
            "ftpfs",
            "fuse.curlftpfs",
        ];
        if let Ok(contents) = std::fs::read_to_string("/proc/mounts") {
            for line in contents.lines() {
                let mut fields = line.split_whitespace();
                let Some(_device) = fields.next() else {
                    continue;
                };
                let Some(mount_point) = fields.next() else {
                    continue;
                };
                let Some(fs_type) = fields.next() else {
                    continue;
                };
                if !NETWORK_FS_TYPES.contains(&fs_type) {
                    continue;
                }
                let path = PathBuf::from(unescape_mount_field(mount_point));
                if !path.is_dir() || !seen.insert(path.clone()) {
                    continue;
                }
                let label = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| mount_point.to_string());
                self.network_locations.push((path, label));
            }
        }

        self.network_locations.sort_by(|a, b| a.1.cmp(&b.1));
    }

    fn navigate(&mut self, dir: PathBuf) {
        self.pending_nav = Some(dir);
    }

    /// Toggles whether `path` is marked for batch export.
    pub fn toggle_mark(&mut self, path: PathBuf) {
        if !self.marked.remove(&path) {
            self.marked.insert(path);
        }
    }

    /// Paths currently marked for batch export.
    pub fn marked_paths(&self) -> Vec<PathBuf> {
        self.marked.iter().cloned().collect()
    }

    /// Number of paths currently marked for batch export.
    pub fn marked_count(&self) -> usize {
        self.marked.len()
    }

    /// Whether `path` is currently marked for batch export.
    pub fn is_marked(&self, path: &std::path::Path) -> bool {
        self.marked.contains(path)
    }

    fn queue_pending_thumbs(&mut self, ctx: &egui::Context) {
        let in_flight = self
            .thumbnails
            .values()
            .filter(|state| matches!(state, ThumbState::Loading))
            .count();
        if in_flight >= MAX_THUMB_JOBS {
            return;
        }
        let slots = MAX_THUMB_JOBS - in_flight;

        let to_queue: Vec<PathBuf> = self
            .images
            .iter()
            .filter(|(p, _)| !self.thumbnails.contains_key(p))
            .take(slots)
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

    /// Drain thumbnail results and queue pending thumbnails.
    /// Call every frame before rendering windows.
    pub fn poll(&mut self, ctx: &egui::Context) {
        if let Some(nav) = self.pending_nav.take() {
            self.current_dir = nav;
            self.path_edit = self.current_dir.display().to_string();
            self.selected = None;
            self.marked.clear();
            self.scan_locations();
            self.scan_network_locations();
            self.scan();
        }

        self.drain_channel(ctx);
        self.queue_pending_thumbs(ctx);
    }

    /// Renders the left-hand navigation sidebar: locations, then the current
    /// directory's subfolders, as vertical lists.
    pub fn show_sidebar(&mut self, ui: &mut egui::Ui) {
        let mut nav_to: Option<PathBuf> = None;

        if !self.locations.is_empty() {
            ui.label(egui::RichText::new("LOCATIONS").weak().small());
            for (path, label) in &self.locations {
                let is_current = *path == self.current_dir;
                if ui
                    .selectable_label(is_current, format!("\u{1F5C2} {}", label))
                    .clicked()
                {
                    nav_to = Some(path.clone());
                }
            }
            ui.add_space(8.0);
        }

        if !self.network_locations.is_empty() {
            ui.label(egui::RichText::new("NETWORK").weak().small());
            for (path, label) in &self.network_locations {
                let is_current = *path == self.current_dir;
                if ui
                    .selectable_label(is_current, format!("\u{1F310} {}", label))
                    .clicked()
                {
                    nav_to = Some(path.clone());
                }
            }
            ui.add_space(8.0);
        }

        ui.separator();
        ui.add_space(4.0);

        // Editable path bar + up button
        ui.horizontal(|ui| {
            if ui
                .button("\u{2B06}")
                .on_hover_text("Parent directory")
                .clicked()
            {
                if let Some(p) = self.current_dir.parent() {
                    nav_to = Some(p.to_path_buf());
                }
            }

            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.path_edit)
                    .desired_width(ui.available_width())
                    .font(egui::TextStyle::Monospace),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let candidate = PathBuf::from(&self.path_edit);
                if candidate.is_dir() {
                    nav_to = Some(candidate);
                } else {
                    // Revert to current dir if invalid
                    self.path_edit = self.current_dir.display().to_string();
                }
            }
        });

        ui.add_space(4.0);
        ui.separator();

        if !self.subdirs.is_empty() {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("FOLDERS").weak().small());
            egui::ScrollArea::vertical()
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (path, name) in &self.subdirs {
                        if ui.button(format!("\u{1F4C1} {}", name)).clicked() {
                            nav_to = Some(path.clone());
                        }
                    }
                });
        }

        if let Some(nav) = nav_to {
            self.navigate(nav);
        }
    }

    /// Renders the thumbnail grid (Library mode central panel content).
    /// Plain click selects+opens a photo; Ctrl/Cmd-click toggles it as
    /// marked for batch export without changing the open photo.
    pub fn show_contents(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        let mut new_sel: Option<PathBuf> = None;
        let current_sel = self.selected.clone();

        if let Some(err) = &self.scan_error {
            ui.centered_and_justified(|ui| {
                ui.label(err.as_str());
            });
        } else if self.images.is_empty() && self.subdirs.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label("No images in this directory");
            });
        } else {
            let avail_w = ui.available_width();
            let cols = ((avail_w / (CELL + 8.0)) as usize).max(1);
            let mut toggled_mark: Option<PathBuf> = None;

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new("image_grid")
                        .num_columns(cols)
                        .spacing([8.0, 8.0])
                        .show(ui, |ui| {
                            for (i, (path, name)) in self.images.iter().enumerate() {
                                let is_sel = current_sel.as_ref() == Some(path);
                                let is_marked = self.marked.contains(path);
                                let thumb = match self.thumbnails.get(path) {
                                    Some(ThumbState::Ready(tex)) => {
                                        Some((tex.id(), tex.size_vec2()))
                                    }
                                    _ => None,
                                };

                                let clicked = draw_thumb_cell(
                                    ui,
                                    name,
                                    thumb,
                                    is_sel,
                                    is_marked,
                                    CELL,
                                    true,
                                );
                                if clicked {
                                    let ctrl_held =
                                        ui.input(|i| i.modifiers.ctrl || i.modifiers.mac_cmd);
                                    if ctrl_held {
                                        toggled_mark = Some(path.clone());
                                    } else {
                                        new_sel = Some(path.clone());
                                    }
                                }

                                if (i + 1) % cols == 0 {
                                    ui.end_row();
                                }
                            }
                        });
                });

            if let Some(path) = toggled_mark {
                self.toggle_mark(path);
            }
        }

        if let Some(sel) = new_sel {
            self.selected = Some(sel);
        }
    }

    /// Renders a horizontal filmstrip of the current directory's images at a
    /// smaller size, reusing the same thumbnail cache as the grid. Returns
    /// the clicked path, if any, so the caller can switch the active photo.
    pub fn show_filmstrip(&mut self, ui: &mut egui::Ui, active: Option<&std::path::Path>) -> Option<PathBuf> {
        let mut clicked_path = None;
        egui::ScrollArea::horizontal()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (path, name) in &self.images {
                        let is_active = active == Some(path.as_path());
                        let is_marked = self.marked.contains(path);
                        let thumb = match self.thumbnails.get(path) {
                            Some(ThumbState::Ready(tex)) => Some((tex.id(), tex.size_vec2())),
                            _ => None,
                        };
                        if draw_thumb_cell(
                            ui,
                            name,
                            thumb,
                            is_active,
                            is_marked,
                            FILMSTRIP_CELL,
                            false,
                        ) {
                            clicked_path = Some(path.clone());
                        }
                    }
                });
            });
        clicked_path
    }
}

fn draw_thumb_cell(
    ui: &mut egui::Ui,
    name: &str,
    thumb: Option<(egui::TextureId, egui::Vec2)>,
    selected: bool,
    marked: bool,
    cell: f32,
    show_label: bool,
) -> bool {
    let cell_height = if show_label { cell + 22.0 } else { cell };
    let (resp, painter) = ui.allocate_painter(egui::vec2(cell, cell_height), egui::Sense::click());
    let rect = resp.rect;

    // Background
    if selected {
        painter.rect_filled(rect, 4.0, ui.visuals().selection.bg_fill);
    } else if resp.hovered() {
        painter.rect_filled(rect, 4.0, ui.visuals().widgets.hovered.bg_fill);
    }

    // Image area
    let img_rect = egui::Rect::from_min_size(rect.min, egui::vec2(cell, cell));
    match thumb {
        Some((tex_id, tex_size)) => {
            let scale = (cell / tex_size.x).min(cell / tex_size.y);
            let display = tex_size * scale;
            let offset = (egui::vec2(cell, cell) - display) * 0.5;
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
                "\u{2026}",
                egui::FontId::proportional(22.0),
                egui::Color32::GRAY,
            );
        }
    }

    // Marked-for-export badge
    if marked {
        let badge_center = img_rect.right_top() + egui::vec2(-10.0, 10.0);
        painter.circle_filled(badge_center, 8.0, ui.visuals().selection.bg_fill);
        painter.text(
            badge_center,
            egui::Align2::CENTER_CENTER,
            "\u{2713}",
            egui::FontId::proportional(10.0),
            egui::Color32::WHITE,
        );
    }

    // Filename label
    if show_label {
        let label_pos = egui::pos2(rect.center().x, img_rect.max.y + 11.0);
        let name_short = if name.len() > 24 { &name[..24] } else { name };
        painter.text(
            label_pos,
            egui::Align2::CENTER_CENTER,
            name_short,
            egui::FontId::proportional(11.0),
            ui.visuals().text_color(),
        );
    }

    resp.clicked()
}

fn generate_thumb(path: &PathBuf, cache_dir: &PathBuf) -> Option<(Vec<u8>, usize, usize)> {
    let thumb_path = crate::thumbnail::cache_path(path, cache_dir);

    let img = if thumb_path.exists() {
        image::open(&thumb_path).ok()?
    } else {
        let full = crate::thumbnail::open_image_for_preview(path).ok()?;
        let t = full.thumbnail(crate::thumbnail::THUMB_SIZE, crate::thumbnail::THUMB_SIZE);
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
    crate::thumbnail::is_supported_image(path)
}

/// Turns a raw GVfs mount directory name (e.g.
/// `smb-share:server=nas,share=photos`) into a readable label like
/// `photos on nas`. Falls back to the raw name for unrecognized schemes.
fn friendly_gvfs_label(raw: &str) -> String {
    let Some((scheme, rest)) = raw.split_once(':') else {
        return raw.to_string();
    };

    let mut server = None;
    let mut share = None;
    for kv in rest.split(',') {
        if let Some((key, value)) = kv.split_once('=') {
            match key {
                "server" | "host" => server = server.or(Some(value)),
                "share" => share = Some(value),
                _ => {}
            }
        }
    }

    match (share, server) {
        (Some(share), Some(server)) => format!("{share} on {server}"),
        (None, Some(server)) => format!("{server} ({scheme})"),
        _ => raw.to_string(),
    }
}

/// Decodes the octal escapes (`\040` for space, etc.) `/proc/mounts` uses
/// for whitespace and backslashes in mount-point paths.
fn unescape_mount_field(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            if let Ok(code) = u8::from_str_radix(&field[i + 1..i + 4], 8) {
                out.push(code);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_gvfs_label_formats_smb_share() {
        assert_eq!(
            friendly_gvfs_label("smb-share:server=nas,share=photos"),
            "photos on nas"
        );
    }

    #[test]
    fn friendly_gvfs_label_formats_sftp_with_host_only() {
        assert_eq!(
            friendly_gvfs_label("sftp:host=example.com"),
            "example.com (sftp)"
        );
    }

    #[test]
    fn friendly_gvfs_label_falls_back_for_unknown_scheme() {
        assert_eq!(friendly_gvfs_label("unknown-thing"), "unknown-thing");
    }

    #[test]
    fn unescape_mount_field_decodes_octal_space() {
        assert_eq!(unescape_mount_field(r"/mnt/My\040Share"), "/mnt/My Share");
    }

    #[test]
    fn unescape_mount_field_passes_through_plain_path() {
        assert_eq!(unescape_mount_field("/mnt/nas"), "/mnt/nas");
    }
}

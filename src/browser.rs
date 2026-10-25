use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::mpsc,
};

use patina::tree::FolderTree;

use crate::locations::{self, Location};
use crate::network::{self, Network};

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
    /// The focused/open photo — drives the Edit target and highlight.
    pub selected: Option<PathBuf>,
    /// Anchor for shift-click range selection — the last plain- or Ctrl-clicked item.
    select_anchor: Option<PathBuf>,
    /// Working set built via Ctrl-click (toggle) or Shift-click (range); shown as
    /// checkboxes, drives the filmstrip, and is what Render targets.
    pub selection: HashSet<PathBuf>,
    path_edit: String,
    /// Set when the path bar holds text that isn't a navigable path.
    path_error: Option<String>,
    /// Photo to focus once a pending navigation lands (path bar given a file).
    pending_select: Option<PathBuf>,
    storage_locations: Vec<Location>,
    network_locations: Vec<Location>,
    scan_error: Option<String>,
    /// The sidebar's folder tree, rooted at the location that holds the
    /// current folder (Patina's `FolderTree`, see ADR-0026).
    tree: FolderTree,
    /// GVfs, for the network's servers and shares (ADR-0029).
    network: Network,
    /// Network shares mounted through GVfs, by Photograph or anything else.
    network_mounts: Vec<network::Mount>,
    /// Shown in place of the photos while browsing the network or a server.
    network_view: Option<NetworkView>,
    /// A share being mounted, to open once it is.
    connecting: Option<String>,
    /// Servers' questions, answered one at a time.
    asking: Vec<Asking>,
    /// An error for the status bar, taken by the app.
    message: Option<String>,
}

/// The network's servers, or a server's shares.
struct NetworkView {
    uri: String,
    /// `None` while GVfs looks.
    listing: Option<Result<Vec<network::Place>, String>>,
}

/// A server's question for the user, being answered.
struct Asking {
    ask: network::Ask,
    user: String,
    domain: String,
    password: String,
    anonymous: bool,
    save: network::Save,
    /// Whether a field has been given the keys.
    focused: bool,
}

impl Browser {
    /// Creates a browser rooted at `initial_dir` or a reasonable fallback directory.
    pub fn new(ctx: &egui::Context, initial_dir: Option<PathBuf>) -> Self {
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
            select_anchor: None,
            selection: HashSet::new(),
            path_error: None,
            pending_select: None,
            storage_locations: Vec::new(),
            network_locations: Vec::new(),
            scan_error: None,
            tree: FolderTree::new(PathBuf::new(), String::new()),
            network: Network::new(ctx),
            network_mounts: Vec::new(),
            network_view: None,
            connecting: None,
            asking: Vec::new(),
            message: None,
        };
        b.scan_locations();
        let (root, label) = b.location_for(&b.current_dir);
        b.tree.set_root(root, label);
        b.tree.reveal(&b.current_dir.clone());
        b.scan();
        b
    }

    /// The places the sidebar lists, as (path, label): Home, Pictures,
    /// Computer, then storage and network mounts.
    fn locations(&self) -> Vec<(PathBuf, String)> {
        let mut out = Vec::new();
        if let Some(home) = dirs::home_dir() {
            out.push((home, "Home".to_string()));
        }
        if let Some(pictures) = dirs::picture_dir().filter(|p| p.is_dir()) {
            out.push((pictures, "Pictures".to_string()));
        }
        out.push((PathBuf::from("/"), "Computer".to_string()));
        for loc in self.storage_locations.iter().chain(&self.network_locations) {
            out.push((loc.path.clone(), loc.label.clone()));
        }
        for mount in &self.network_mounts {
            out.push((mount.path.clone(), mount.name.clone()));
        }
        out
    }

    /// The most specific sidebar location containing `path`, for rooting
    /// the tree when you navigate by path bar or start up.
    fn location_for(&self, path: &Path) -> (PathBuf, String) {
        self.locations()
            .into_iter()
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .unwrap_or_else(|| (PathBuf::from("/"), "Computer".to_string()))
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
                    "Cannot read this directory: permission denied".to_string()
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
        let home = dirs::home_dir();
        let (storage, network) = locations::mounted_locations(home.as_deref());
        self.storage_locations = storage;
        self.network_locations = network;
    }

    fn navigate(&mut self, dir: PathBuf) {
        self.pending_nav = Some(dir);
    }

    /// Opens a URI: the network or a server is listed in place of the
    /// photos, and a share is mounted, then opened as a folder.
    fn open_uri(&mut self, uri: &str) {
        let uri = uri.trim().to_string();
        if network::is_listing(&uri) {
            self.path_edit = uri.clone();
            self.path_error = None;
            self.images.clear();
            self.subdirs.clear();
            self.thumbnails.clear();
            self.scan_error = None;
            self.selected = None;
            self.select_anchor = None;
            self.selection.clear();
            self.network.list(&uri);
            self.network_view = Some(NetworkView { uri, listing: None });
        } else {
            self.network.mount(&uri);
            self.connecting = Some(uri);
        }
    }

    /// The GVfs share whose folder is `dir`, if it's one's top.
    fn mount_at(&self, dir: &Path) -> Option<&network::Mount> {
        self.network_mounts.iter().find(|m| m.path == dir)
    }

    /// An error to show in the status bar, once.
    pub fn take_message(&mut self) -> Option<String> {
        self.message.take()
    }

    fn network_event(&mut self, event: network::Event) {
        match event {
            network::Event::Listed { uri, result } => {
                if let Some(view) = self.network_view.as_mut().filter(|v| v.uri == uri) {
                    view.listing = Some(result);
                }
            }
            network::Event::Mounted { uri, result } => {
                if self.connecting.as_ref() != Some(&uri) {
                    return;
                }
                self.connecting = None;
                match result {
                    Ok(path) => self.navigate(path),
                    Err(Some(e)) => self.message = Some(format!("Couldn't connect to {uri}: {e}")),
                    Err(None) => {}
                }
            }
            network::Event::Unmounted { result } => {
                if let Err(Some(e)) = result {
                    self.message = Some(format!("Couldn't unmount: {e}"));
                }
            }
            network::Event::Mounts(mounts) => {
                // Out of a share that's gone, to its server.
                let gone = self
                    .network_mounts
                    .iter()
                    .find(|m| {
                        self.network_view.is_none()
                            && self.current_dir.starts_with(&m.path)
                            && !mounts.iter().any(|n| n.path == m.path)
                    })
                    .map(|m| network::server_of(&m.uri));
                self.network_mounts = mounts;
                if let Some(server) = gone {
                    self.open_uri(&server);
                }
            }
            network::Event::Ask(ask) => {
                let (user, domain) = match &ask.kind {
                    network::AskKind::Password { user, domain, .. } => {
                        (user.clone(), domain.clone())
                    }
                    network::AskKind::Question(_) => Default::default(),
                };
                self.asking.push(Asking {
                    ask,
                    user,
                    domain,
                    password: String::new(),
                    anonymous: false,
                    save: network::Save::ForSession,
                    focused: false,
                });
            }
            network::Event::Withdrawn(id) => self.asking.retain(|a| a.ask.id != id),
        }
    }

    /// Toggles whether `path` is in the working selection (checkbox, filmstrip,
    /// Render target).
    pub fn toggle_selection(&mut self, path: PathBuf) {
        if !self.selection.remove(&path) {
            self.selection.insert(path);
        }
    }

    /// Paths currently in the working selection.
    pub fn selected_paths(&self) -> Vec<PathBuf> {
        self.selection.iter().cloned().collect()
    }

    /// Number of paths currently in the working selection.
    pub fn selection_count(&self) -> usize {
        self.selection.len()
    }

    /// Whether `path` is currently in the working selection.
    pub fn is_selected(&self, path: &std::path::Path) -> bool {
        self.selection.contains(path)
    }

    /// Drops `path`'s thumbnail so it's regenerated — e.g. after its edits
    /// changed — on the next poll.
    pub fn refresh_thumbnail(&mut self, path: &std::path::Path) {
        self.thumbnails.remove(path);
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
        for event in self.network.events() {
            self.network_event(event);
        }
        if let Some(nav) = self.pending_nav.take() {
            self.network_view = None;
            self.current_dir = nav;
            self.path_edit = self.current_dir.display().to_string();
            self.path_error = None;
            self.selected = None;
            self.select_anchor = None;
            self.selection.clear();
            self.scan_locations();
            if !self.current_dir.starts_with(self.tree.root()) {
                let (root, label) = self.location_for(&self.current_dir);
                self.tree.set_root(root, label);
            }
            // Entering a folder re-lists it, so the tree picks up changes.
            self.tree.refresh(&self.current_dir);
            self.tree.reveal(&self.current_dir);
            self.scan();
            if let Some(file) = self.pending_select.take() {
                if self.images.iter().any(|(p, _)| *p == file) {
                    self.select_anchor = Some(file.clone());
                    self.selected = Some(file);
                }
            }
        }

        self.tree.poll();
        self.drain_channel(ctx);
        self.queue_pending_thumbs(ctx);
    }

    /// Renders the left-hand navigation sidebar: locations, the path bar,
    /// then a folder tree under the chosen location.
    pub fn show_sidebar(&mut self, ui: &mut egui::Ui) {
        let mut nav_to: Option<PathBuf> = None;
        // A clicked location becomes the tree's root, even if it's inside another.
        let mut new_root: Option<(PathBuf, String)> = None;

        ui.label(egui::RichText::new("LOCATIONS").weak().small());
        let mut places: Vec<(PathBuf, &str, &str)> = Vec::new();
        if let Some(home) = dirs::home_dir() {
            places.push((home, "\u{1F3E0}", "Home"));
        }
        if let Some(pictures) = dirs::picture_dir().filter(|p| p.is_dir()) {
            places.push((pictures, "\u{1F5BC}", "Pictures"));
        }
        places.push((PathBuf::from("/"), "\u{1F4BB}", "Computer"));
        // Nothing local is current while the network is shown.
        let local = self.network_view.is_none();
        for (path, icon, label) in places {
            let is_current = local && path == self.tree.root();
            if ui
                .selectable_label(is_current, format!("{icon} {label}"))
                .on_hover_text(path.display().to_string())
                .clicked()
            {
                new_root = Some((path.clone(), label.to_string()));
                nav_to = Some(path);
            }
        }
        ui.add_space(8.0);

        if !self.storage_locations.is_empty() {
            ui.label(egui::RichText::new("STORAGE").weak().small());
            for loc in &self.storage_locations {
                let is_current = local && loc.path == self.tree.root();
                if ui
                    .selectable_label(is_current, format!("\u{1F4BE} {}", loc.label))
                    .on_hover_text(&loc.detail)
                    .clicked()
                {
                    new_root = Some((loc.path.clone(), loc.label.clone()));
                    nav_to = Some(loc.path.clone());
                }
            }
            ui.add_space(8.0);
        }

        // The network through GVfs, Linux's (ADR-0029), then what's mounted:
        // classic mounts, then GVfs shares, which can be unmounted here.
        let browsable = cfg!(target_os = "linux");
        if browsable || !self.network_locations.is_empty() {
            ui.label(egui::RichText::new("NETWORK").weak().small());
        }
        let mut open_uri: Option<String> = None;
        let mut unmount: Option<String> = None;
        if browsable {
            let shown = self.network_view.is_some();
            if ui
                .selectable_label(shown, "\u{1F5A7} Network")
                .on_hover_text("Servers on the network, and their shares")
                .clicked()
            {
                open_uri = Some(network::ROOT.to_string());
            }
        }
        for loc in &self.network_locations {
            let is_current = local && loc.path == self.tree.root();
            if ui
                .selectable_label(is_current, format!("\u{1F310} {}", loc.label))
                .on_hover_text(&loc.detail)
                .clicked()
            {
                new_root = Some((loc.path.clone(), loc.label.clone()));
                nav_to = Some(loc.path.clone());
            }
        }
        for mount in &self.network_mounts {
            let is_current = local && mount.path == self.tree.root();
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(is_current, format!("\u{1F310} {}", mount.name))
                    .on_hover_text(&mount.uri)
                    .clicked()
                {
                    new_root = Some((mount.path.clone(), mount.name.clone()));
                    nav_to = Some(mount.path.clone());
                }
                let eject = ui
                    .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(egui::Button::new("\u{23CF}").frame(false))
                            .on_hover_text(format!("Unmount {}", mount.name))
                            .clicked()
                    })
                    .inner;
                if eject {
                    unmount = Some(mount.uri.clone());
                }
            });
        }
        if let Some(uri) = &self.connecting {
            ui.label(
                egui::RichText::new(format!("Connecting to {uri}\u{2026}"))
                    .weak()
                    .small(),
            );
        }
        if browsable || !self.network_locations.is_empty() {
            ui.add_space(8.0);
        }

        ui.separator();
        ui.add_space(4.0);

        // Editable path bar + up button
        ui.horizontal(|ui| {
            // Up from a share's top is its server, and from a server the network.
            let up_uri = match &self.network_view {
                Some(view) if view.uri != network::ROOT => Some(network::server_of(&view.uri)),
                Some(_) => None,
                None => self
                    .mount_at(&self.current_dir)
                    .map(|m| network::server_of(&m.uri)),
            };
            let has_parent = up_uri.is_some()
                || (self.network_view.is_none() && self.current_dir.parent().is_some());
            if ui
                .add_enabled(has_parent, egui::Button::new("\u{2B06}"))
                .on_hover_text("Parent directory")
                .clicked()
            {
                if let Some(uri) = up_uri {
                    open_uri = Some(uri);
                } else if let Some(p) = self.current_dir.parent() {
                    nav_to = Some(p.to_path_buf());
                }
            }

            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.path_edit)
                    .desired_width(ui.available_width())
                    .font(egui::TextStyle::Monospace),
            );
            if resp.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                && network::is_uri(&self.path_edit)
            {
                open_uri = Some(self.path_edit.trim().to_string());
            } else if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let candidate =
                    locations::expand_typed_path(&self.path_edit, dirs::home_dir().as_deref());
                if candidate.is_dir() {
                    nav_to = Some(candidate);
                } else if candidate.is_file() {
                    // A photo path: open its folder with the photo focused.
                    if let Some(parent) = candidate.parent() {
                        nav_to = Some(parent.to_path_buf());
                        self.pending_select = Some(candidate);
                    }
                } else {
                    // Keep the typed text so it can be corrected.
                    self.path_error = Some(format!("No such folder: {}", candidate.display()));
                }
            }
            if resp.changed() {
                self.path_error = None;
            }
        });
        if let Some(err) = &self.path_error {
            ui.colored_label(ui.visuals().error_fg_color, err);
        }

        ui.add_space(4.0);
        ui.separator();

        ui.add_space(4.0);
        ui.label(egui::RichText::new("FOLDERS").weak().small());
        // The tree scrolls on its own, so revealing a deep folder keeps the
        // locations and path bar in view.
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let current = if local {
                    self.current_dir.as_path()
                } else {
                    Path::new("")
                };
                if let Some(path) = self.tree.show(ui, current) {
                    nav_to = Some(path);
                }
            });

        if let Some((root, label)) = new_root {
            self.tree.set_root(root, label);
        }
        if let Some(nav) = nav_to {
            self.navigate(nav);
        }
        if let Some(uri) = open_uri {
            self.open_uri(&uri);
        }
        if let Some(uri) = unmount {
            self.network.unmount(&uri);
        }
    }

    /// The network's servers or a server's shares, as a grid of places:
    /// a server opens its shares, a share mounts and opens.
    fn show_network(&mut self, ui: &mut egui::Ui) {
        let Some(view) = &self.network_view else {
            return;
        };
        let mut open: Option<String> = None;
        match &view.listing {
            None => {
                ui.centered_and_justified(|ui| {
                    ui.label("Looking\u{2026}");
                });
            }
            Some(Err(e)) => {
                ui.centered_and_justified(|ui| {
                    ui.label(format!("Can't list {}: {e}", view.uri));
                });
            }
            Some(Ok(places)) if places.iter().all(|p| p.hidden()) => {
                ui.centered_and_justified(|ui| {
                    ui.label(if view.uri == network::ROOT {
                        "No servers found. Type one into the path bar, as smb://host/"
                    } else {
                        "No shares on this server"
                    });
                });
            }
            Some(Ok(places)) => {
                let avail_w = ui.available_width();
                let cols = ((avail_w / (CELL + 8.0)) as usize).max(1);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Grid::new("network_grid")
                            .num_columns(cols)
                            .spacing([8.0, 8.0])
                            .show(ui, |ui| {
                                let shown = places.iter().filter(|p| !p.hidden());
                                for (i, place) in shown.enumerate() {
                                    let connecting = self.connecting.as_ref() == Some(&place.uri);
                                    if draw_place_cell(ui, place, connecting)
                                        .on_hover_text(&place.uri)
                                        .clicked()
                                    {
                                        open = Some(place.uri.clone());
                                    }
                                    if (i + 1) % cols == 0 {
                                        ui.end_row();
                                    }
                                }
                            });
                    });
            }
        }
        if let Some(uri) = open {
            self.open_uri(&uri);
        }
    }

    /// A server's question: a password, as Nautilus asks for one, or a
    /// choice such as whether to trust a host.
    pub fn show_dialogs(&mut self, ctx: &egui::Context) {
        let Some(asking) = self.asking.first_mut() else {
            return;
        };
        let mut answer = None;
        let modal = egui::Modal::new(egui::Id::new(("ask", asking.ask.id))).show(ctx, |ui| {
            ui.set_width(380.0);
            let mut lines = asking.ask.message.lines().filter(|l| !l.trim().is_empty());
            ui.heading(lines.next().unwrap_or("Connect"));
            for line in lines {
                ui.label(line);
            }
            ui.add_space(8.0);
            match &asking.ask.kind {
                network::AskKind::Password {
                    need_user,
                    need_domain,
                    need_password,
                    anonymous,
                    can_save,
                    ..
                } => {
                    if *anonymous {
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut asking.anonymous, false, "Registered user");
                            ui.radio_value(&mut asking.anonymous, true, "Anonymous");
                        });
                        ui.add_space(4.0);
                    }
                    ui.add_enabled_ui(!asking.anonymous, |ui| {
                        // The keys go to the first field that wants something.
                        let mut focus = !std::mem::replace(&mut asking.focused, true);
                        egui::Grid::new("credentials")
                            .num_columns(2)
                            .spacing([8.0, 6.0])
                            .show(ui, |ui| {
                                let mut field =
                                    |ui: &mut egui::Ui,
                                     label: &str,
                                     text: &mut String,
                                     password: bool| {
                                        ui.label(label);
                                        let resp = ui.add(
                                            egui::TextEdit::singleline(text)
                                                .password(password)
                                                .desired_width(260.0),
                                        );
                                        if focus && (text.is_empty() || password) {
                                            resp.request_focus();
                                            focus = false;
                                        }
                                        ui.end_row();
                                    };
                                if *need_user {
                                    field(ui, "Username", &mut asking.user, false);
                                }
                                if *need_domain {
                                    field(ui, "Domain", &mut asking.domain, false);
                                }
                                if *need_password {
                                    field(ui, "Password", &mut asking.password, true);
                                }
                            });
                        if *can_save {
                            ui.add_space(6.0);
                            ui.radio_value(
                                &mut asking.save,
                                network::Save::Never,
                                "Forget password immediately",
                            );
                            ui.radio_value(
                                &mut asking.save,
                                network::Save::ForSession,
                                "Remember until you log out",
                            );
                            ui.radio_value(
                                &mut asking.save,
                                network::Save::Permanently,
                                "Remember forever",
                            );
                        }
                    });
                    ui.add_space(12.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Connect").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            answer = Some(network::Answer::Password {
                                anonymous: asking.anonymous,
                                user: asking.user.clone(),
                                domain: asking.domain.clone(),
                                password: std::mem::take(&mut asking.password),
                                save: asking.save,
                            });
                        }
                        if ui.button("Cancel").clicked() {
                            answer = Some(network::Answer::Cancel);
                        }
                    });
                }
                network::AskKind::Question(choices) => {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        for (i, choice) in choices.iter().enumerate().rev() {
                            if ui.button(choice).clicked() {
                                answer = Some(network::Answer::Choice(i));
                            }
                        }
                    });
                }
            }
        });
        if modal.should_close() {
            answer = answer.or(Some(network::Answer::Cancel));
        }
        if let Some(answer) = answer {
            let asking = self.asking.remove(0);
            if matches!(answer, network::Answer::Cancel) && self.asking.is_empty() {
                self.connecting = None;
            }
            self.network.answer(asking.ask.id, answer);
        }
    }

    /// Renders the thumbnail grid (Library mode central panel content).
    /// Plain click focuses a photo and resets the selection to just that one;
    /// Ctrl/Cmd-click toggles a photo in/out of the selection (and moves the
    /// range anchor there); Shift-click range-selects from the anchor. The
    /// selection drives the checkbox badge, the filmstrip, and Render's
    /// target set. Double-click opens the photo fullscreen — the returned
    /// path, if any, is that open request.
    pub fn show_contents(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) -> Option<PathBuf> {
        let mut plain_click: Option<PathBuf> = None;
        let mut ctrl_click: Option<PathBuf> = None;
        let mut shift_click: Option<PathBuf> = None;
        let mut open_request: Option<PathBuf> = None;

        if self.network_view.is_some() {
            self.show_network(ui);
            return None;
        }
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

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new("image_grid")
                        .num_columns(cols)
                        .spacing([8.0, 8.0])
                        .show(ui, |ui| {
                            for (i, (path, name)) in self.images.iter().enumerate() {
                                let is_focused = self.selected.as_ref() == Some(path);
                                let is_checked = self.selection.contains(path);
                                let thumb = match self.thumbnails.get(path) {
                                    Some(ThumbState::Ready(tex)) => {
                                        Some((tex.id(), tex.size_vec2()))
                                    }
                                    _ => None,
                                };

                                let resp = draw_thumb_cell(
                                    ui, name, thumb, is_focused, is_checked, CELL, true,
                                );
                                if resp.double_clicked() {
                                    open_request = Some(path.clone());
                                } else if resp.clicked() {
                                    let shift_held = ui.input(|i| i.modifiers.shift);
                                    let ctrl_held =
                                        ui.input(|i| i.modifiers.ctrl || i.modifiers.mac_cmd);
                                    if ctrl_held {
                                        ctrl_click = Some(path.clone());
                                    } else if shift_held {
                                        shift_click = Some(path.clone());
                                    } else {
                                        plain_click = Some(path.clone());
                                    }
                                }

                                if (i + 1) % cols == 0 {
                                    ui.end_row();
                                }
                            }
                        });
                });
        }

        if let Some(path) = plain_click {
            self.selected = Some(path.clone());
            self.select_anchor = Some(path.clone());
            self.selection.clear();
            self.selection.insert(path);
        }
        if let Some(path) = ctrl_click {
            self.selected = Some(path.clone());
            self.select_anchor = Some(path.clone());
            self.toggle_selection(path);
        }
        if let Some(path) = shift_click {
            self.extend_selection_to(path);
        }

        open_request
    }

    /// Extends the range selection from `select_anchor` (or `selected` if no
    /// anchor yet) up to `path`, inclusive, in folder order.
    fn extend_selection_to(&mut self, path: PathBuf) {
        let anchor = self.select_anchor.clone().or_else(|| self.selected.clone());
        let Some(anchor) = anchor else {
            self.selected = Some(path.clone());
            self.select_anchor = Some(path.clone());
            self.selection.clear();
            self.selection.insert(path);
            return;
        };
        let anchor_idx = self.images.iter().position(|(p, _)| *p == anchor);
        let click_idx = self.images.iter().position(|(p, _)| *p == path);
        if let (Some(a), Some(c)) = (anchor_idx, click_idx) {
            let (lo, hi) = if a <= c { (a, c) } else { (c, a) };
            self.selection = self.images[lo..=hi]
                .iter()
                .map(|(p, _)| p.clone())
                .collect();
        }
        self.selected = Some(path);
    }

    /// Renders a horizontal filmstrip of the current selection at a smaller
    /// size, reusing the same thumbnail cache as the grid. Returns the
    /// clicked path, if any, so the caller can switch the active photo.
    pub fn show_filmstrip(
        &mut self,
        ui: &mut egui::Ui,
        active: Option<&std::path::Path>,
    ) -> Option<PathBuf> {
        let mut clicked_path = None;
        let selection: Vec<(PathBuf, String)> = self
            .images
            .iter()
            .filter(|(p, _)| self.selection.contains(p))
            .cloned()
            .collect();
        egui::ScrollArea::horizontal()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (path, name) in &selection {
                        let is_active = active == Some(path.as_path());
                        let is_checked = self.selection.contains(path);
                        let thumb = match self.thumbnails.get(path) {
                            Some(ThumbState::Ready(tex)) => Some((tex.id(), tex.size_vec2())),
                            _ => None,
                        };
                        if draw_thumb_cell(
                            ui,
                            name,
                            thumb,
                            is_active,
                            is_checked,
                            FILMSTRIP_CELL,
                            false,
                        )
                        .clicked()
                        {
                            clicked_path = Some(path.clone());
                        }
                    }
                });
            });
        clicked_path
    }
}

/// A server or share in the network grid: a glyph where a photo would be,
/// and its name.
fn draw_place_cell(ui: &mut egui::Ui, place: &network::Place, connecting: bool) -> egui::Response {
    let (resp, painter) = ui.allocate_painter(egui::vec2(CELL, CELL + 22.0), egui::Sense::click());
    let rect = resp.rect;
    if resp.hovered() {
        painter.rect_filled(rect, 4.0, ui.visuals().widgets.hovered.bg_fill);
    }
    let img_rect = egui::Rect::from_min_size(rect.min, egui::vec2(CELL, CELL));
    let glyph = if connecting {
        "\u{2026}"
    } else if place.is_server() {
        "\u{1F5A7}"
    } else {
        "\u{1F4C1}"
    };
    painter.text(
        img_rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(56.0),
        ui.visuals().weak_text_color(),
    );
    let name: String = place.name.chars().take(24).collect();
    painter.text(
        egui::pos2(rect.center().x, img_rect.max.y + 11.0),
        egui::Align2::CENTER_CENTER,
        name,
        egui::FontId::proportional(11.0),
        ui.visuals().text_color(),
    );
    resp
}

fn draw_thumb_cell(
    ui: &mut egui::Ui,
    name: &str,
    thumb: Option<(egui::TextureId, egui::Vec2)>,
    selected: bool,
    marked: bool,
    cell: f32,
    show_label: bool,
) -> egui::Response {
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

    resp
}

/// A photo's grid thumbnail: the edited thumbnail saved beside its sidecar
/// when it has one (see `Viewer::save_edits`), otherwise the plain
/// thumbnail from the camera preview, cached in `cache_dir`.
fn generate_thumb(path: &PathBuf, cache_dir: &PathBuf) -> Option<(Vec<u8>, usize, usize)> {
    let edited = crate::state::edited_thumbnail_path(path);
    let img = match edited.exists().then(|| image::open(&edited).ok()).flatten() {
        Some(img) => img,
        None => plain_thumb(path, cache_dir)?,
    };
    let rgba = img.to_rgba8();
    let w = rgba.width() as usize;
    let h = rgba.height() as usize;
    Some((rgba.into_raw(), w, h))
}

fn plain_thumb(path: &PathBuf, cache_dir: &PathBuf) -> Option<image::DynamicImage> {
    let thumb_path = crate::thumbnail::cache_path(path, cache_dir);
    if thumb_path.exists() {
        return image::open(&thumb_path).ok();
    }
    let full = crate::thumbnail::open_image_for_preview(path).ok()?;
    let t = image::DynamicImage::ImageRgba8(crate::processing::resize::downscale_rgba8(
        &full,
        crate::thumbnail::THUMB_SIZE,
    ));
    let _ = std::fs::create_dir_all(cache_dir);
    let _ = t.save(&thumb_path);
    Some(t)
}

fn is_image(path: &std::path::Path) -> bool {
    crate::thumbnail::is_supported_image(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_edited_thumbnail_is_preferred_while_it_exists() {
        let dir = std::env::temp_dir().join(format!("photograph-thumbs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".edits")).unwrap();
        let cache = dir.join(".thumbnails");
        let photo = dir.join("grey.png");
        image::RgbaImage::from_pixel(64, 48, image::Rgba([100, 100, 100, 255]))
            .save(&photo)
            .unwrap();
        let center = |t: &(Vec<u8>, usize, usize)| t.0[((t.2 / 2) * t.1 + t.1 / 2) * 4];

        assert_eq!(
            center(&generate_thumb(&photo, &cache).unwrap()),
            100,
            "plain"
        );
        let edited = crate::state::edited_thumbnail_path(&photo);
        image::RgbaImage::from_pixel(32, 24, image::Rgba([220, 220, 220, 255]))
            .save_with_format(&edited, image::ImageFormat::WebP)
            .unwrap();
        assert_eq!(
            center(&generate_thumb(&photo, &cache).unwrap()),
            220,
            "edited"
        );
        std::fs::remove_file(&edited).unwrap();
        assert_eq!(
            center(&generate_thumb(&photo, &cache).unwrap()),
            100,
            "plain again"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

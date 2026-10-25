//! The network through GVfs, as Nautilus and Sector F's Files have it
//! (ADR-0029): the servers it finds (`network:///`, from DNS-SD and SMB
//! browsing), a server's shares, mounting one, with a password when it asks,
//! and unmounting. GIO runs its own main loop on a thread here. A mounted
//! share is an ordinary folder under GVfs's FUSE bridge,
//! `/run/user/<uid>/gvfs`, so the browser opens it like any other.
//!
//! Linux only: elsewhere `Network` finds nothing and mounts nothing.

// Elsewhere nothing makes the events and answers.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::PathBuf;

/// What the sidebar's Network place shows: every server GVfs finds.
pub const ROOT: &str = "network:///";

pub enum Event {
    /// The servers on the network, or a server's shares.
    Listed {
        uri: String,
        result: Result<Vec<Place>, String>,
    },
    /// A share is mounted, at its folder; `Err(None)` when the user cancelled.
    Mounted {
        uri: String,
        result: Result<PathBuf, Option<String>>,
    },
    Unmounted {
        result: Result<(), Option<String>>,
    },
    /// The network shares mounted now, after any change.
    Mounts(Vec<Mount>),
    /// A server wants a password or an answer before it mounts.
    Ask(Ask),
    /// The server stopped waiting for the answer to `Ask` with this id.
    Withdrawn(u64),
}

/// A server on the network, or a share on a server.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub name: String,
    pub uri: String,
}

impl Place {
    /// Windows' administrative shares: C$, IPC$, print$.
    pub fn hidden(&self) -> bool {
        self.name.ends_with('$')
    }

    /// Whether opening it lists shares (a server) rather than mounting it.
    pub fn is_server(&self) -> bool {
        is_listing(&self.uri)
    }
}

/// A mounted network share.
#[derive(Debug, Clone, PartialEq)]
pub struct Mount {
    /// GVfs's name for it: "music on urd.local".
    pub name: String,
    pub uri: String,
    /// Its folder under the FUSE bridge.
    pub path: PathBuf,
}

pub struct Ask {
    pub id: u64,
    pub message: String,
    pub kind: AskKind,
}

pub enum AskKind {
    Password {
        user: String,
        domain: String,
        need_user: bool,
        need_domain: bool,
        need_password: bool,
        anonymous: bool,
        can_save: bool,
    },
    Question(Vec<String>),
}

pub enum Answer {
    Password {
        anonymous: bool,
        user: String,
        domain: String,
        password: String,
        save: Save,
    },
    Choice(usize),
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Save {
    Never,
    ForSession,
    Permanently,
}

pub use imp::Network;

/// Whether `uri` is listed as servers or shares (the network, or a server);
/// anything else is mounted and opened as a folder.
pub fn is_listing(uri: &str) -> bool {
    let Some((scheme, rest)) = uri.split_once("://") else {
        return false;
    };
    match scheme {
        "network" => true,
        "smb" => rest
            .split_once('/')
            .is_none_or(|(_, path)| path.trim_matches('/').is_empty()),
        _ => false,
    }
}

/// Whether text typed into the path bar is a URI rather than a path.
pub fn is_uri(text: &str) -> bool {
    text.trim().split_once("://").is_some_and(|(scheme, _)| {
        !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// The server a share is on, to go up to: `smb://host/share` is on
/// `smb://host/`. Other kinds of mount go up to the network.
pub fn server_of(uri: &str) -> String {
    match uri.split_once("://") {
        Some(("smb", rest)) => format!("smb://{}/", rest.split('/').next().unwrap_or_default()),
        _ => ROOT.to_string(),
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::sync::mpsc::{Receiver, Sender, channel};

    use gio::glib;
    use gio::prelude::*;

    use super::{Answer, Ask, AskKind, Event, Mount, Place, Save};

    pub struct Network {
        send: async_channel::Sender<Request>,
        events: Receiver<Event>,
    }

    enum Request {
        List(String),
        Mount(String),
        Unmount(String),
        Answer(u64, Answer),
    }

    /// Everything on the network thread.
    struct State {
        events: Sender<Event>,
        ctx: egui::Context,
        waiting: RefCell<HashMap<u64, async_channel::Sender<Answer>>>,
        next: Cell<u64>,
    }

    impl State {
        fn send(&self, event: Event) {
            let _ = self.events.send(event);
            self.ctx.request_repaint();
        }

        /// Puts a question to the user; the answer comes on the receiver.
        fn ask(&self, message: &str, kind: AskKind) -> (u64, async_channel::Receiver<Answer>) {
            let id = self.next.get();
            self.next.set(id + 1);
            let (send, answer) = async_channel::bounded(1);
            self.waiting.borrow_mut().insert(id, send);
            self.send(Event::Ask(Ask {
                id,
                message: message.to_string(),
                kind,
            }));
            (id, answer)
        }
    }

    impl Network {
        pub fn new(ctx: &egui::Context) -> Self {
            let (send, requests) = async_channel::unbounded();
            let (events_send, events) = channel();
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name("network".into())
                .spawn(move || {
                    let context = glib::MainContext::new();
                    let main_loop = glib::MainLoop::new(Some(&context), false);
                    let run = context.with_thread_default(|| {
                        let state = Rc::new(State {
                            events: events_send,
                            ctx,
                            waiting: RefCell::new(HashMap::new()),
                            next: Cell::new(0),
                        });
                        // Signals reach the context that's the thread's default when it's made.
                        let monitor = gio::VolumeMonitor::get();
                        state.send(Event::Mounts(mounts(&monitor)));
                        let changed = {
                            let state = state.clone();
                            move |monitor: &gio::VolumeMonitor, _: &gio::Mount| {
                                state.send(Event::Mounts(mounts(monitor)))
                            }
                        };
                        monitor.connect_mount_added(changed.clone());
                        monitor.connect_mount_removed(changed.clone());
                        monitor.connect_mount_changed(changed);
                        let quit = main_loop.clone();
                        glib::MainContext::ref_thread_default().spawn_local(async move {
                            while let Ok(request) = requests.recv().await {
                                let state = state.clone();
                                match request {
                                    Request::Answer(id, answer) => {
                                        if let Some(send) = state.waiting.borrow_mut().remove(&id) {
                                            let _ = send.try_send(answer);
                                        }
                                    }
                                    Request::List(uri) => spawn(list(state, uri)),
                                    Request::Mount(uri) => spawn(mount(state, uri)),
                                    Request::Unmount(uri) => spawn(unmount(state, uri)),
                                }
                            }
                            quit.quit();
                        });
                        main_loop.run();
                        drop(monitor);
                    });
                    if let Err(e) = run {
                        tracing::warn!("can't reach the network: {e}");
                    }
                })
                .ok();
            Network { send, events }
        }

        /// Lists a server's shares, or the servers, as an `Event::Listed`.
        pub fn list(&self, uri: &str) {
            let _ = self.send.try_send(Request::List(uri.to_string()));
        }

        pub fn mount(&self, uri: &str) {
            let _ = self.send.try_send(Request::Mount(uri.to_string()));
        }

        pub fn unmount(&self, uri: &str) {
            let _ = self.send.try_send(Request::Unmount(uri.to_string()));
        }

        pub fn answer(&self, id: u64, answer: Answer) {
            let _ = self.send.try_send(Request::Answer(id, answer));
        }

        pub fn events(&self) -> Vec<Event> {
            self.events.try_iter().collect()
        }
    }

    fn spawn(future: impl std::future::Future<Output = ()> + 'static) {
        glib::MainContext::ref_thread_default().spawn_local(future);
    }

    /// The network mounts: not the local disks the monitor also knows, nor
    /// mounts another stands in for.
    fn mounts(monitor: &gio::VolumeMonitor) -> Vec<Mount> {
        let mut found: Vec<Mount> = monitor
            .mounts()
            .iter()
            .filter(|m| !m.is_shadowed())
            .filter_map(|m| {
                let root = m.root();
                if root.uri_scheme().is_some_and(|s| s == "file") {
                    return None;
                }
                Some(Mount {
                    name: m.name().to_string(),
                    uri: root.uri().to_string(),
                    path: root.path()?,
                })
            })
            .collect();
        found.sort_by_key(|m| m.name.to_lowercase());
        found.dedup_by(|a, b| a.uri == b.uri);
        found
    }

    async fn list(state: Rc<State>, uri: String) {
        let file = gio::File::for_uri(&uri);
        let mut result = children(&file).await;
        // A server has to be mounted to list its shares, which may take a password.
        if result
            .as_ref()
            .is_err_and(|e| e.matches(gio::IOErrorEnum::NotMounted))
        {
            result = match file
                .mount_enclosing_volume_future(gio::MountMountFlags::NONE, Some(&operation(&state)))
                .await
            {
                Ok(()) => children(&file).await,
                Err(e) => Err(e),
            };
        }
        let result = result.map_err(|e| {
            if e.matches(gio::IOErrorEnum::FailedHandled) {
                "Not connected".to_string()
            } else {
                e.message().to_string()
            }
        });
        state.send(Event::Listed { uri, result });
    }

    async fn children(file: &gio::File) -> Result<Vec<Place>, glib::Error> {
        let attributes = "standard::name,standard::display-name,standard::target-uri";
        let enumerator = file
            .enumerate_children_future(
                attributes,
                gio::FileQueryInfoFlags::NONE,
                glib::Priority::DEFAULT,
            )
            .await?;
        let mut places = Vec::new();
        loop {
            let infos = enumerator
                .next_files_future(64, glib::Priority::DEFAULT)
                .await?;
            if infos.is_empty() {
                places.sort_by_key(|p: &Place| p.name.to_lowercase());
                return Ok(places);
            }
            for info in infos {
                let uri = info
                    .attribute_string("standard::target-uri")
                    .map(|u| u.to_string())
                    .unwrap_or_else(|| file.child(info.name()).uri().to_string());
                places.push(Place {
                    name: info.display_name().to_string(),
                    uri,
                });
            }
        }
    }

    async fn mount(state: Rc<State>, uri: String) {
        let file = gio::File::for_uri(&uri);
        let result = match file
            .mount_enclosing_volume_future(gio::MountMountFlags::NONE, Some(&operation(&state)))
            .await
        {
            Ok(()) => Ok(()),
            Err(e) if e.matches(gio::IOErrorEnum::AlreadyMounted) => Ok(()),
            Err(e) => Err(e),
        };
        let result = result.map_err(quiet).and_then(|()| {
            file.path().ok_or_else(|| {
                Some(
                    "GVfs's FUSE bridge (gvfsd-fuse) isn't running, so the share has no folder"
                        .into(),
                )
            })
        });
        state.send(Event::Mounted { uri, result });
    }

    async fn unmount(state: Rc<State>, uri: String) {
        let file = gio::File::for_uri(&uri);
        // A quick call to GVfs, on this thread rather than the window's.
        let result = match file.find_enclosing_mount(None::<&gio::Cancellable>) {
            Ok(mount) => {
                mount
                    .unmount_with_operation_future(
                        gio::MountUnmountFlags::NONE,
                        Some(&operation(&state)),
                    )
                    .await
            }
            Err(e) => Err(e),
        };
        state.send(Event::Unmounted {
            result: result.map_err(quiet),
        });
    }

    /// An error worth showing, or none when the user cancelled.
    fn quiet(e: glib::Error) -> Option<String> {
        (!e.matches(gio::IOErrorEnum::FailedHandled)).then(|| e.message().to_string())
    }

    /// Asks the user what a server asks: a password, or a question such as
    /// whether to trust a host's key.
    fn operation(state: &Rc<State>) -> gio::MountOperation {
        let op = gio::MountOperation::new();
        // The question the window shows, to take back if the server gives up.
        let asked = Rc::new(Cell::new(None::<u64>));

        let (s, a) = (state.clone(), asked.clone());
        op.connect_ask_password(move |op, message, user, domain, flags| {
            let (id, answer) = s.ask(
                message,
                AskKind::Password {
                    user: user.to_string(),
                    domain: domain.to_string(),
                    need_user: flags.contains(gio::AskPasswordFlags::NEED_USERNAME),
                    need_domain: flags.contains(gio::AskPasswordFlags::NEED_DOMAIN),
                    need_password: flags.contains(gio::AskPasswordFlags::NEED_PASSWORD),
                    anonymous: flags.contains(gio::AskPasswordFlags::ANONYMOUS_SUPPORTED),
                    can_save: flags.contains(gio::AskPasswordFlags::SAVING_SUPPORTED),
                },
            );
            a.set(Some(id));
            let op = op.clone();
            spawn(async move {
                match answer.recv().await {
                    Ok(Answer::Password {
                        anonymous,
                        user,
                        domain,
                        password,
                        save,
                    }) => {
                        op.set_anonymous(anonymous);
                        if !anonymous {
                            op.set_username(Some(&user));
                            op.set_domain(Some(&domain));
                            op.set_password(Some(&password));
                        }
                        op.set_password_save(match save {
                            Save::Never => gio::PasswordSave::Never,
                            Save::ForSession => gio::PasswordSave::ForSession,
                            Save::Permanently => gio::PasswordSave::Permanently,
                        });
                        op.reply(gio::MountOperationResult::Handled);
                    }
                    _ => op.reply(gio::MountOperationResult::Aborted),
                }
            });
        });

        // gio has no typed binding for ask-question: its choices are a string array.
        let (s, a) = (state.clone(), asked.clone());
        op.connect_local("ask-question", false, move |args| {
            let op = args[0].get::<gio::MountOperation>().ok()?;
            let message = args[1].get::<String>().ok()?;
            let choices = args[2]
                .get::<glib::StrV>()
                .ok()?
                .iter()
                .map(|c| c.to_string())
                .collect();
            let (id, answer) = s.ask(&message, AskKind::Question(choices));
            a.set(Some(id));
            spawn(async move {
                match answer.recv().await {
                    Ok(Answer::Choice(i)) => {
                        op.set_choice(i as i32);
                        op.reply(gio::MountOperationResult::Handled);
                    }
                    _ => op.reply(gio::MountOperationResult::Aborted),
                }
            });
            None
        });

        let s = state.clone();
        op.connect_aborted(move |_| {
            if let Some(id) = asked.take() {
                s.waiting.borrow_mut().remove(&id);
                s.send(Event::Withdrawn(id));
            }
        });
        op
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::{Answer, Event};

    /// GVfs is Linux's: elsewhere there's no network to browse.
    pub struct Network;

    impl Network {
        pub fn new(_ctx: &egui::Context) -> Self {
            Network
        }

        pub fn list(&self, _uri: &str) {}

        pub fn mount(&self, _uri: &str) {}

        pub fn unmount(&self, _uri: &str) {}

        pub fn answer(&self, _id: u64, _answer: Answer) {}

        pub fn events(&self) -> Vec<Event> {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_network_and_servers_are_listed_and_shares_mounted() {
        assert!(is_listing(ROOT));
        assert!(is_listing("smb://urd.local/"));
        assert!(is_listing("smb://urd.local:445"));
        assert!(!is_listing("smb://urd.local/photos"));
        assert!(!is_listing("smb://urd.local/photos/"));
        assert!(!is_listing("sftp://urd.local:22/"));
    }

    #[test]
    fn shares_go_up_to_their_server() {
        assert_eq!(server_of("smb://urd.local/photos/"), "smb://urd.local/");
        assert_eq!(server_of("sftp://urd.local/"), ROOT);
    }

    #[test]
    fn uris_are_told_from_paths() {
        assert!(is_uri("smb://urd.local/photos"));
        assert!(is_uri(" network:/// "));
        assert!(!is_uri("/home/divan/Pictures"));
        assert!(!is_uri("~/a://b"));
    }
}

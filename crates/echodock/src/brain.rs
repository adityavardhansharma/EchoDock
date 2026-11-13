//! The dock's worker thread. It owns everything slow: Hyprland queries, desktop entries, icon
//! lookup, screen capture and launching. It turns Hyprland's state into one `Snapshot` per
//! monitor and hands each to that monitor's overlay; the overlays send `Action`s back.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use echodock_core::apps::{self, Icons, Library};
use echodock_core::config::{Config, Visibility, WindowScope};
use echodock_core::hypr::{self, Client, Monitor};
use rune::OverlayHandle;

use crate::glass::Backdrop;

use crate::dock::Dock;

#[derive(Clone, Debug, PartialEq)]
pub struct Win {
    pub address: String,
    pub title: String,
    pub workspace: i64,
    pub minimized: bool,
    pub focused: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Apps,
    App,
    Separator,
    Trash,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// Desktop id, or the window class for an app without an entry.
    pub key: String,
    pub kind: Kind,
    pub name: String,
    pub icon: Option<PathBuf>,
    pub pinned: bool,
    pub has_entry: bool,
    pub windows: Vec<Win>,
    /// Desktop actions (jump list) by name.
    pub actions: Vec<String>,
}

impl Item {
    pub fn running(&self) -> bool {
        !self.windows.is_empty()
    }
    pub fn focused(&self) -> bool {
        self.windows.iter().any(|w| w.focused)
    }
    fn fixed(kind: Kind, key: &str, name: &str) -> Item {
        Item { key: key.into(), kind, name: name.into(), icon: None, pinned: false, has_entry: false, windows: Vec::new(), actions: Vec::new() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub items: Vec<Item>,
    /// A fullscreen window covers the monitor's current workspace.
    pub fullscreen: bool,
    /// A window overlaps where the dock sits (for `Visibility::Dodge`).
    pub overlapped: bool,
    pub workspace: i64,
}

pub enum Action {
    Activate(String),
    Launch(String),
    LaunchAction(String, usize),
    Focus(String),
    Close(String),
    Quit(String),
    Pin(String, bool),
    MinimizeAll(String),
    OpenApps,
    OpenTrash,
    /// Capture the band behind the dock: monitor, then x, y, w, h in monitor-local points.
    Capture(String, [f32; 4], [f32; 4], Backdrop),
}

pub enum Msg {
    Refresh,
    Config(Config),
    Act(Action),
    Ctl(String, Sender<String>),
}

pub struct Overlay {
    pub monitor: String,
    pub handle: OverlayHandle<Dock>,
}

struct Brain {
    cfg: Config,
    lib: Library,
    icons: Icons,
    overlays: Vec<Overlay>,
    /// Running apps that aren't pinned, in the order they first appeared, kept for the session.
    order: Vec<String>,
    clients: Vec<Client>,
    monitors: Vec<Monitor>,
    active: Option<String>,
    last: HashMap<String, Snapshot>,
    lib_loaded: Instant,
}

pub fn spawn(cfg: Config, overlays: Vec<Overlay>, tx: Sender<Msg>, rx: Receiver<Msg>) {
    std::thread::Builder::new().name("echodock-brain".into()).spawn(move || {
        let mut b = Brain {
            cfg,
            lib: Library::load(),
            icons: Icons::new(),
            overlays,
            order: Vec::new(),
            clients: Vec::new(),
            monitors: Vec::new(),
            active: None,
            last: HashMap::new(),
            lib_loaded: Instant::now(),
        };
        let _ = tx;
        b.refresh();
        loop {
            let Ok(msg) = rx.recv() else { return };
            let mut refresh = false;
            let mut handle = |b: &mut Brain, msg: Msg| match msg {
                Msg::Refresh => refresh = true,
                Msg::Config(c) => {
                    let monitors_changed = c.behavior.monitors != b.cfg.behavior.monitors;
                    b.cfg = c.clone();
                    for o in &b.overlays {
                        let c = c.clone();
                        o.handle.update(move |s, tx| s.set_config(c, tx));
                    }
                    if monitors_changed { restart(); }
                    refresh = true;
                }
                Msg::Act(a) => b.act(a),
                Msg::Ctl(cmd, reply) => {
                    let r = b.ctl(&cmd);
                    let _ = reply.send(r);
                    refresh = true;
                }
            };
            handle(&mut b, msg);
            // Hyprland sends bursts (a window opening is several events): settle them into one read.
            let deadline = Instant::now() + Duration::from_millis(25);
            while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                match rx.recv_timeout(left) {
                    Ok(m) => handle(&mut b, m),
                    Err(_) => break,
                }
            }
            if refresh { b.refresh(); }
        }
    }).expect("brain thread");
}

/// Replaces the process with a fresh copy (monitors came or went).
pub fn restart() -> ! {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().unwrap_or_else(|_| "echodock".into());
    let err = std::process::Command::new(exe).args(std::env::args().skip(1)).exec();
    eprintln!("echodock: restart failed: {err}");
    std::process::exit(1);
}

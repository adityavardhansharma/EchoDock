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

impl Brain {
    fn refresh(&mut self) {
        // New apps installed show up without a restart.
        if self.lib_loaded.elapsed() > Duration::from_secs(20) {
            self.lib = Library::load();
            self.lib_loaded = Instant::now();
        }
        self.clients = hypr::clients();
        self.monitors = hypr::monitors();
        self.active = hypr::active_address();
        let names: Vec<String> = self.overlays.iter().map(|o| o.monitor.clone()).collect();
        for name in names {
            let Some(m) = self.monitors.iter().find(|m| m.name == name).cloned() else { continue };
            let snap = self.snapshot(&m);
            if self.last.get(&name) == Some(&snap) { continue; }
            self.last.insert(name.clone(), snap.clone());
            if let Some(o) = self.overlays.iter().find(|o| o.monitor == name) {
                o.handle.update(move |s, tx| s.set_snapshot(snap, tx));
            }
        }
    }

    fn entry_key(&self, c: &Client) -> (String, Option<&apps::Entry>) {
        let e = self.lib.for_class(&c.class).or_else(|| self.lib.for_class(&c.initial_class));
        match e {
            Some(e) => (e.id.clone(), Some(e)),
            None => (if c.class.is_empty() { c.initial_class.clone() } else { c.class.clone() }, None),
        }
    }

    fn snapshot(&mut self, m: &Monitor) -> Snapshot {
        let cfg = self.cfg.clone();
        let ws = m.active_workspace.id;
        let mut items: Vec<Item> = Vec::new();
        if cfg.items.apps_button { items.push(Item::fixed(Kind::Apps, "@apps", "Applications")); }
        let mut pinned_count = 0;
        for id in &cfg.items.pinned {
            let Some(e) = self.lib.get(id).cloned() else { continue };
            let icon = self.icons.lookup(&e.icon);
            items.push(Item { key: e.id.clone(), kind: Kind::App, name: e.name.clone(), icon, pinned: true, has_entry: true, windows: Vec::new(), actions: e.actions.iter().map(|a| a.name.clone()).collect() });
            pinned_count += 1;
        }
        let mut fullscreen = false;
        let mut overlapped = false;
        let (mw, mh) = m.logical_size();
        let a = &cfg.appearance;
        let dock_h = a.icon_size + 2.0 * a.padding + a.margin;
        let n = (cfg.items.pinned.len() + 4) as f32;
        let dock_w = n * (a.icon_size + a.spacing) + 2.0 * a.padding;
        let dock = [m.x as f32 + (mw - dock_w) / 2.0, m.y as f32 + mh - dock_h, dock_w, dock_h];
        let clients = self.clients.clone();
        for c in &clients {
            let minimized = c.workspace.name == hypr::MINIMIZED;
            let visible_here = c.monitor == m.id && c.workspace.id == ws;
            if visible_here && c.fullscreen >= 2 { fullscreen = true; }
            if visible_here && cfg.behavior.visibility == Visibility::Dodge {
                let (x, y, w, h) = (c.at[0] as f32, c.at[1] as f32, c.size[0] as f32, c.size[1] as f32);
                if x < dock[0] + dock[2] && x + w > dock[0] && y < dock[1] + dock[3] && y + h > dock[1] { overlapped = true; }
            }
            if c.workspace.id < 1 && !minimized { continue; }
            let keep = match cfg.behavior.scope {
                WindowScope::All => true,
                WindowScope::Workspace => visible_here || minimized,
                WindowScope::Monitor => c.monitor == m.id || minimized,
            };
            if !keep { continue; }
            let (key, entry) = self.entry_key(c);
            let entry = entry.cloned();
            if entry.as_ref().is_some_and(|e| e.no_display) && !minimized && !cfg.items.pinned.contains(&key) { continue; }
            let win = Win {
                address: c.address.clone(),
                title: c.title.clone(),
                workspace: c.workspace.id,
                minimized,
                focused: self.active.as_deref() == Some(c.address.as_str()),
            };
            if let Some(it) = items.iter_mut().find(|i| i.key == key) {
                it.windows.push(win);
                continue;
            }
            if !cfg.behavior.show_running { continue; }
            if !self.order.contains(&key) { self.order.push(key.clone()); }
            let (name, icon_name, actions) = match &entry {
                Some(e) => (e.name.clone(), e.icon.clone(), e.actions.iter().map(|a| a.name.clone()).collect()),
                None => (key.clone(), key.clone(), Vec::new()),
            };
            let icon = self.icons.lookup(&icon_name).or_else(|| self.icons.lookup(&key.to_lowercase()));
            items.push(Item { key, kind: Kind::App, name, icon, pinned: false, has_entry: entry.is_some(), windows: vec![win], actions });
        }
        // Running apps keep the slot they joined in.
        let first_running = items.iter().position(|i| i.kind == Kind::App && !i.pinned).unwrap_or(items.len());
        items[first_running..].sort_by_key(|i| self.order.iter().position(|k| *k == i.key).unwrap_or(usize::MAX));
        if cfg.appearance.separator && pinned_count > 0 && first_running < items.len() {
            items.insert(first_running, Item::fixed(Kind::Separator, "@sep", ""));
        }
        if cfg.items.trash {
            if cfg.appearance.separator { items.push(Item::fixed(Kind::Separator, "@sep-trash", "")); }
            let mut t = Item::fixed(Kind::Trash, "@trash", "Trash");
            t.icon = self.icons.lookup("user-trash-full").filter(|_| trash_full()).or_else(|| self.icons.lookup("user-trash"));
            items.push(t);
        }
        for it in &mut items {
            if it.kind == Kind::App && it.icon.is_none() { it.icon = self.icons.lookup("application-x-executable"); }
        }
        Snapshot { items, fullscreen, overlapped, workspace: ws }
    }

    fn focused_monitor(&self) -> Option<&Monitor> {
        self.monitors.iter().find(|m| m.focused).or(self.monitors.first())
    }

    fn windows_of(&self, key: &str) -> Vec<Client> {
        self.clients.iter().filter(|c| (c.workspace.id >= 1 || c.workspace.name == hypr::MINIMIZED) && self.entry_key(c).0 == key).cloned().collect()
    }

    fn act(&mut self, a: Action) {
        let ws = self.focused_monitor().map(|m| m.active_workspace.id);
        match a {
            Action::Activate(key) => {
                // A refresh may be stale by a few ms; read the windows now.
                self.clients = hypr::clients();
                self.active = hypr::active_address();
                let wins = self.windows_of(&key);
                if wins.is_empty() {
                    if let Some(e) = self.lib.get(&key) { apps::launch(e); }
                    return;
                }
                // Windows on this workspace first, then minimized, then elsewhere; oldest focus first.
                let mut order: Vec<&Client> = wins.iter().collect();
                let rank = |c: &Client| if c.workspace.name == hypr::MINIMIZED { 1 } else if Some(c.workspace.id) == ws { 0 } else { 2 };
                order.sort_by_key(|c| (rank(c), c.focus_history_id));
                let focused = order.iter().position(|c| self.active.as_deref() == Some(c.address.as_str()));
                let target = match focused {
                    Some(i) if order.len() == 1 && self.cfg.behavior.click_minimizes => {
                        hypr::minimize(&order[i].address, order[i].workspace.id);
                        return;
                    }
                    Some(i) if self.cfg.behavior.click_cycles => order[(i + 1) % order.len()],
                    Some(i) => order[i],
                    None => order[0],
                };
                hypr::focus(&target.address, target.workspace.name == hypr::MINIMIZED, ws);
            }
            Action::Launch(key) => if let Some(e) = self.lib.get(&key) { apps::launch(e) },
            Action::LaunchAction(key, i) => if let Some(a) = self.lib.get(&key).and_then(|e| e.actions.get(i)) { apps::launch_action(a) },
            Action::Focus(address) => {
                let minimized = self.clients.iter().any(|c| c.address == address && c.workspace.name == hypr::MINIMIZED);
                hypr::focus(&address, minimized, ws);
            }
            Action::Close(address) => hypr::close(&address),
            Action::Quit(key) => for c in self.windows_of(&key) { hypr::close(&c.address) },
            Action::MinimizeAll(key) => for c in self.windows_of(&key) {
                if c.workspace.name != hypr::MINIMIZED { hypr::minimize(&c.address, c.workspace.id) }
            },
            Action::Pin(key, on) => {
                let mut c = self.cfg.clone();
                c.items.pinned.retain(|k| *k != key);
                if on { c.items.pinned.push(key); }
                self.cfg = c.clone();
                if let Err(e) = c.save() { eprintln!("echodock: saving pins: {e}"); }
            }
            Action::OpenApps => apps::run(&self.cfg.items.apps_command),
            Action::OpenTrash => apps::run(&self.cfg.items.trash_command),
            Action::Capture(monitor, rect, screen, backdrop) => {
                let Some(m) = self.monitors.iter().find(|m| m.name == monitor) else { return };
                let (x, y) = ((m.x as f32 + screen[0]).round(), (m.y as f32 + screen[1]).round());
                let handle = self.overlays.iter().find(|o| o.monitor == monitor).map(|o| o.handle.clone());
                std::thread::spawn(move || {
                    capture(x, y, rect[2].round(), rect[3].round(), rect, backdrop);
                    // Redraw with the new backdrop.
                    if let Some(h) = handle { h.update(|_, tx| tx.wake_after(Duration::ZERO)); }
                });
            }
        }
    }

    /// `echodock ctl` commands.
    fn ctl(&mut self, cmd: &str) -> String {
        let mut words = cmd.split_whitespace();
        let verb = words.next().unwrap_or("");
        let arg = words.next();
        let nth_app = |b: &mut Brain, n: usize| -> Option<String> {
            let m = b.focused_monitor()?.clone();
            b.snapshot(&m).items.into_iter().filter(|i| i.kind == Kind::App).nth(n.checked_sub(1)?).map(|i| i.key)
        };
        match verb {
            "activate" | "new" => {
                let Some(n) = arg.and_then(|a| a.parse::<usize>().ok()) else { return "usage: activate <n>".into() };
                let Some(key) = nth_app(self, n) else { return format!("no app {n}") };
                self.act(if verb == "new" { Action::Launch(key.clone()) } else { Action::Activate(key.clone()) });
                format!("ok {key}")
            }
            "autohide" => {
                let mut c = self.cfg.clone();
                let on = match arg {
                    Some("on") => true,
                    Some("off") => false,
                    _ => c.behavior.visibility != Visibility::AutoHide,
                };
                c.behavior.visibility = if on { Visibility::AutoHide } else { Visibility::Always };
                match c.save() { Ok(()) => format!("autohide {}", if on { "on" } else { "off" }), Err(e) => format!("error: {e}") }
            }
            "reveal" | "toggle" => {
                let Some(m) = self.focused_monitor().map(|m| m.name.clone()) else { return "no monitor".into() };
                let toggle = verb == "toggle";
                if let Some(o) = self.overlays.iter().find(|o| o.monitor == m) { o.handle.update(move |s, tx| s.summon(toggle, tx)); }
                "ok".into()
            }
            "pin" | "unpin" => {
                let Some(id) = arg else { return format!("usage: {verb} <desktop-id>") };
                let id = id.trim_end_matches(".desktop").to_owned();
                if verb == "pin" && self.lib.get(&id).is_none() { return format!("no app {id}") }
                self.act(Action::Pin(id, verb == "pin"));
                "ok".into()
            }
            "list" => {
                let Some(m) = self.focused_monitor().cloned() else { return String::new() };
                self.snapshot(&m).items.iter().filter(|i| i.kind == Kind::App).enumerate()
                    .map(|(i, it)| format!("{}:{}{}{}", i + 1, it.key, if it.pinned { "*" } else { "" }, if it.running() { "+" } else { "" }))
                    .collect::<Vec<_>>().join(" ")
            }
            "apps" => { self.act(Action::OpenApps); "ok".into() }
            "minimize" => {
                self.clients = hypr::clients();
                let Some(addr) = hypr::active_address() else { return "no window".into() };
                let Some(c) = self.clients.iter().find(|c| c.address == addr) else { return "no window".into() };
                if c.workspace.id < 1 { return "not on a workspace".into() }
                hypr::minimize(&c.address, c.workspace.id);
                "ok".into()
            }
            "reload" => { self.lib = Library::load(); self.icons = Icons::new(); self.last.clear(); "ok".into() }
            "quit" => std::process::exit(0),
            "restart" => restart(),
            "ping" => "pong".into(),
            _ => "commands: activate <n> | new <n> | apps | minimize | reveal | toggle | autohide [on|off] | pin <id> | unpin <id> | list | reload | restart | quit".into(),
        }
    }
}

fn trash_full() -> bool {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    std::fs::read_dir(home.join(".local/share/Trash/files")).is_ok_and(|mut d| d.next().is_some())
}

/// Grabs a region of the screen (global logical points) into `pixels` with grim.
fn capture(x: f32, y: f32, w: f32, h: f32, band: [f32; 4], backdrop: Backdrop) {
    let out = std::process::Command::new("grim").args(["-g", &format!("{x},{y} {w}x{h}"), "-t", "ppm", "-"]).output();
    let Ok(out) = out else { return };
    if !out.status.success() { return; }
    if let Some((pw, ph, rgba)) = ppm_to_rgba(&out.stdout) { backdrop.set(band, pw, ph, rgba); }
}

fn ppm_to_rgba(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    // "P6\n<w> <h>\n<max>\n" then RGB bytes.
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 && i < data.len() {
        while i < data.len() && data[i].is_ascii_whitespace() { i += 1; }
        if data.get(i) == Some(&b'#') { while i < data.len() && data[i] != b'\n' { i += 1; } continue; }
        let start = i;
        while i < data.len() && !data[i].is_ascii_whitespace() { i += 1; }
        fields.push(std::str::from_utf8(&data[start..i]).ok()?.to_owned());
    }
    i += 1;
    if fields.first().map(String::as_str) != Some("P6") { return None; }
    let (w, h): (u32, u32) = (fields[1].parse().ok()?, fields[2].parse().ok()?);
    let rgb = data.get(i..i + (w * h * 3) as usize)?;
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for p in rgb.chunks_exact(3) { rgba.extend_from_slice(&[p[0], p[1], p[2], 255]); }
    Some((w, h, rgba))
}

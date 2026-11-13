//! EchoDock Settings: every dock option in one window. Changes save as you make them and the
//! running dock picks them up live (it watches its config file), popping up so you see them.

use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use echodock_core::apps::Library;
use echodock_core::config::{Config, Indicator, Monitors, Theme, Visibility, WindowScope};
use echodock_core::ipc;
use rune::{AppSpec, Color, Cursor, Cx, El, ScrollSpec, Spring, TextStyle, col, el, row, text};

#[derive(Clone, Copy, PartialEq)]
enum Section {
    Appearance,
    Glass,
    Behavior,
    Apps,
    Shortcuts,
    About,
}

const SECTIONS: [(Section, &str); 6] = [
    (Section::Appearance, "Appearance"),
    (Section::Glass, "Liquid Glass"),
    (Section::Behavior, "Behavior"),
    (Section::Apps, "Apps"),
    (Section::Shortcuts, "Shortcuts"),
    (Section::About, "About"),
];

struct Settings {
    cfg: Config,
    section: Section,
    lib: Library,
    saver: Sender<Config>,
    /// The slider being dragged: its key and the value it had when the drag began.
    dragging: Option<&'static str>,
    show_all_apps: bool,
}

impl Settings {
    fn change(&mut self, f: impl FnOnce(&mut Config)) {
        f(&mut self.cfg);
        self.cfg = self.cfg.clone().sanitized();
        let _ = self.saver.send(self.cfg.clone());
    }
}

/// Saves the latest config shortly after changes stop, off the UI thread, and shows the dock.
fn saver() -> Sender<Config> {
    let (tx, rx) = channel::<Config>();
    std::thread::spawn(move || {
        while let Ok(mut c) = rx.recv() {
            while let Ok(newer) = rx.recv_timeout(Duration::from_millis(140)) { c = newer; }
            if let Err(e) = c.save() { eprintln!("echodock-settings: {e}"); }
            let _ = ipc::send("reveal");
        }
    });
    tx
}

fn init() -> Settings {
    Settings { cfg: Config::load(), section: Section::Appearance, lib: Library::load(), saver: saver(), dragging: None, show_all_apps: false }
}

// Palette: Omarchy-dark with a cool accent.
const BG: Color = Color { r: 0.075, g: 0.078, b: 0.09, a: 1.0 };
const SIDEBAR: Color = Color { r: 0.095, g: 0.098, b: 0.112, a: 1.0 };
const CARD: Color = Color { r: 0.12, g: 0.124, b: 0.14, a: 1.0 };
const LINE: Color = Color { r: 1.0, g: 1.0, b: 1.0, a: 0.07 };
const INK: Color = Color { r: 0.93, g: 0.94, b: 0.96, a: 1.0 };
const DIM: Color = Color { r: 0.93, g: 0.94, b: 0.96, a: 0.55 };
const ACCENT: Color = Color { r: 0.38, g: 0.62, b: 1.0, a: 1.0 };

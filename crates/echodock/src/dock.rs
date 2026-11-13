//! One monitor's dock: an overlay along the bottom edge, transparent and click-through except
//! where the dock, its menus and the reveal strip are. Every animation here (reveal, the
//! magnification wave, launch bounces) is stepped by hand in `poll`, so the frozen backdrop the
//! glass refracts can be kept still on screen while the dock slides over it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Duration;

use echodock_core::config::{Config, Indicator, Theme, Visibility};
use rune::{Color, Cursor, Cx, El, Image, ImageCache, Shadow, Spring, TextStyle, Tx, Waker, col, el, row, text};

use crate::brain::{Action, Item, Kind, Msg, Snapshot};
use crate::glass::Backdrop;
use crate::live::Live;

/// Room above the magnified icons for the name label.
const LABEL_ROOM: f32 = 40.0;
/// Room for a context menu above the dock.
const MENU_ROOM: f32 = 340.0;
const MENU_W: f32 = 240.0;
const ROW_H: f32 = 28.0;
const SEP_W: f32 = 13.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Hidden,
    /// The pointer is on the edge; the dock comes up at `due`.
    Arming { due: f64 },
    Shown,
    /// The pointer left; the dock goes at `due` unless it comes back.
    Leaving { due: f64 },
}

pub struct Dock {
    pub monitor: String,
    /// Surface size in points.
    pub size: (f32, f32),
    /// The monitor's height in points: the surface sits at its bottom.
    pub screen_h: f32,
    pub scale: f32,
    pub cfg: Config,
    pub snap: Snapshot,
    pub light: bool,
    tx: Sender<Msg>,
    cache: Option<ImageCache>,
    images: HashMap<PathBuf, Image>,
    phase: Phase,
    /// 0 hidden … 1 shown, eased toward the target in `poll`.
    pub reveal: f32,
    /// 0 … 1: how much of the magnification wave applies. It fades in once the dock has
    /// arrived, so icons don't swell while it is still sliding up under the pointer.
    mag: f32,
    pointer: Option<f32>,
    in_zone: bool,
    in_strip: bool,
    in_menu: bool,
    hovered: Option<String>,
    menu: Option<String>,
    menu_row: Option<usize>,
    /// Displayed icon size per item key.
    sizes: HashMap<String, f32>,
    launching: HashMap<String, f64>,
    /// The screen behind the dock, captured as it comes up (band in surface points).
    pub backdrop: Backdrop,
    /// Follows what is behind the dock while it is up.
    pub live: Option<Live>,
    live_on: bool,
    now: f64,
    last: Option<f64>,
    /// Shown by a shortcut until the pointer comes and goes, or this clock time.
    summoned: Option<f64>,
}

impl Dock {
    pub fn new(monitor: String, size: (f32, f32), scale: f32, cfg: Config, light: bool, tx: Sender<Msg>, backdrop: Backdrop) -> Dock {
        Dock {
            screen_h: size.1, monitor, size, scale, cfg, light, tx,
            snap: Snapshot { items: Vec::new(), fullscreen: false, overlapped: false, workspace: 0 },
            cache: None, images: HashMap::new(), phase: Phase::Hidden, reveal: 0.0, mag: 0.0, pointer: None,
            in_zone: false, in_strip: false, in_menu: false, hovered: None, menu: None, menu_row: None,
            sizes: HashMap::new(), launching: HashMap::new(), backdrop, live: None, live_on: false,
            now: 0.0, last: None, summoned: None,
        }
    }

    fn send(&self, a: Action) {
        let _ = self.tx.send(Msg::Act(a));
    }

    pub fn dark(&self) -> bool {
        match self.cfg.appearance.theme {
            Theme::Dark => true,
            Theme::Light => false,
            Theme::Auto => !self.light,
        }
    }

    pub fn set_config(&mut self, cfg: Config, tx: &mut Tx) {
        self.cfg = cfg;
        self.images.clear();
        self.load_icons();
        tx.wake_after(Duration::ZERO);
    }

    pub fn set_snapshot(&mut self, snap: Snapshot, tx: &mut Tx) {
        for it in &snap.items {
            if it.running() { self.launching.remove(&it.key); }
        }
        if self.menu.as_ref().is_some_and(|k| !snap.items.iter().any(|i| &i.key == k)) { self.menu = None; }
        self.snap = snap;
        self.load_icons();
        tx.wake_after(Duration::ZERO);
    }

    fn load_icons(&mut self) {
        let Some(cache) = &self.cache else { return };
        let px = self.cfg.appearance.magnified_size.max(self.cfg.appearance.icon_size);
        for it in &self.snap.items {
            if let Some(p) = &it.icon && !self.images.contains_key(p) {
                self.images.insert(p.clone(), cache.file(p, px, self.scale));
            }
        }
    }

    /// From a keybinding: bring the dock up (or, toggling, put it away).
    pub fn summon(&mut self, toggle: bool, tx: &mut Tx) {
        if toggle && self.visible() {
            self.summoned = None;
            self.phase = Phase::Hidden;
            self.menu = None;
        } else {
            self.capture();
            self.phase = Phase::Shown;
            self.summoned = Some(tx.clock() + 3.0);
        }
        tx.wake_after(Duration::ZERO);
    }

    /// The dock hides on its own: auto-hide, a dodging dock with a window in its way, or a
    /// fullscreen window.
    fn auto(&self) -> bool {
        let b = &self.cfg.behavior;
        self.snap.fullscreen || b.visibility == Visibility::AutoHide || (b.visibility == Visibility::Dodge && self.snap.overlapped)
    }

    fn strip_active(&self) -> bool {
        self.auto() && (!self.snap.fullscreen || self.cfg.behavior.reveal_over_fullscreen)
    }

    pub fn visible(&self) -> bool {
        !self.auto() || matches!(self.phase, Phase::Shown | Phase::Leaving { .. })
    }

    fn inside(&self) -> bool {
        self.in_zone || self.in_strip || self.in_menu
    }

    fn pill_h(&self) -> f32 {
        self.cfg.appearance.icon_size + 2.0 * self.cfg.appearance.padding
    }

    /// The band in monitor points.
    pub fn band_screen(&self) -> [f32; 4] {
        let [x, y, w, h] = self.band_rect();
        [x, y + (self.screen_h - self.size.1), w, h]
    }

    /// The band of screen the glass may refract, in surface points.
    fn band_rect(&self) -> [f32; 4] {
        let a = &self.cfg.appearance;
        let h = self.pill_h() + a.margin + 48.0;
        [0.0, self.size.1 - h, self.size.0, h]
    }

    fn capture(&mut self) {
        if !self.cfg.glass.refraction { return; }
        if let Some(l) = &self.live && l.available.load(std::sync::atomic::Ordering::Relaxed) {
            self.live_on = true;
            l.set(true, self.band_rect(), self.band_screen(), self.cfg.glass.live_fps);
            return;
        }
        self.send(Action::Capture(self.monitor.clone(), self.band_rect(), self.band_screen(), self.backdrop.clone()));
    }

    fn on_strip(&mut self, inside: bool, tx: &mut Tx) {
        self.in_strip = inside;
        self.pointer_changed(tx);
    }

    fn pointer_changed(&mut self, tx: &mut Tx) {
        let now = tx.clock();
        let b = self.cfg.behavior.clone();
        if self.inside() {
            match self.phase {
                Phase::Hidden if self.strip_active() && self.in_strip => {
                    self.capture();
                    self.phase = Phase::Arming { due: now + b.reveal_delay_ms as f64 / 1000.0 };
                    tx.wake_after(Duration::from_millis(b.reveal_delay_ms));
                }
                Phase::Leaving { .. } => self.phase = Phase::Shown,
                _ => {}
            }
        } else {
            self.summoned = None;
            match self.phase {
                Phase::Arming { .. } => self.phase = Phase::Hidden,
                Phase::Shown if self.menu.is_none() => {
                    self.phase = Phase::Leaving { due: now + b.hide_delay_ms as f64 / 1000.0 };
                    tx.wake_after(Duration::from_millis(b.hide_delay_ms));
                }
                _ => {}
            }
        }
        tx.wake_after(Duration::ZERO);
    }

    /// Rest width of a slot.
    fn rest(&self, it: &Item) -> f32 {
        if it.kind == Kind::Separator { SEP_W } else { self.cfg.appearance.icon_size }
    }

    /// Icon sizes the magnification wave wants for the pointer where it is.
    fn targets(&self) -> Vec<f32> {
        let a = &self.cfg.appearance;
        let rests: Vec<f32> = self.snap.items.iter().map(|i| self.rest(i)).collect();
        let Some(px) = self.pointer.filter(|_| a.magnification && self.menu.is_none()) else { return rests };
        let total: f32 = rests.iter().sum::<f32>() + a.spacing * (rests.len().saturating_sub(1)) as f32 + 2.0 * a.padding;
        let mut x = (self.size.0 - total) / 2.0 + a.padding;
        let unit = a.icon_size + a.spacing;
        let mut out = Vec::with_capacity(rests.len());
        for (it, r) in self.snap.items.iter().zip(&rests) {
            let c = x + r / 2.0;
            x += r + a.spacing;
            if it.kind == Kind::Separator { out.push(*r); continue; }
            let d = (px - c).abs() / unit;
            let f = if d < a.magnification_reach { 0.5 * (1.0 + (std::f32::consts::PI * d / a.magnification_reach).cos()) } else { 0.0 };
            out.push(a.icon_size + (a.magnified_size - a.icon_size) * f * self.mag);
        }
        out
    }

    fn size_of(&self, it: &Item) -> f32 {
        self.sizes.get(&it.key).copied().unwrap_or_else(|| self.rest(it))
    }

    /// Slot rects along the dock as drawn: (x, width) per item, and the dock's x and width.
    fn slots(&self) -> (Vec<(f32, f32)>, f32, f32) {
        let a = &self.cfg.appearance;
        let ws: Vec<f32> = self.snap.items.iter().map(|i| self.size_of(i)).collect();
        let total = ws.iter().sum::<f32>() + a.spacing * (ws.len().saturating_sub(1)) as f32 + 2.0 * a.padding;
        let x0 = ((self.size.0 - total) / 2.0).round();
        let mut x = x0 + a.padding;
        let slots = ws.iter().map(|w| { let s = (x, *w); x += w + a.spacing; s }).collect();
        (slots, x0, total)
    }

    /// How far down the dock is slid for the current reveal.
    fn slide(&self) -> f32 {
        let travel = self.pill_h() + self.cfg.appearance.margin + 24.0;
        travel * (1.0 - ease(self.reveal, self.visible()))
    }

    pub fn pill_rect(&self) -> [f32; 4] {
        let (_, x0, w) = self.slots();
        let h = self.pill_h();
        [x0, self.size.1 - self.cfg.appearance.margin - h + self.slide(), w, h]
    }

    fn activate(&mut self, key: &str, kind: Kind, now: f64) {
        self.menu = None;
        match kind {
            Kind::Apps => self.send(Action::OpenApps),
            Kind::Trash => self.send(Action::OpenTrash),
            Kind::Separator => {}
            Kind::App => {
                let running = self.snap.items.iter().any(|i| i.key == key && i.running());
                if !running && self.cfg.behavior.bounce { self.launching.insert(key.to_owned(), now); }
                self.send(Action::Activate(key.to_owned()));
            }
        }
    }
}

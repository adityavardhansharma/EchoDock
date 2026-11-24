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
use crate::thumbs::Thumbs;

/// Room above the magnified icons for the name label.
const LABEL_ROOM: f32 = 40.0;
/// Room for a context menu above the dock.
const MENU_ROOM: f32 = 340.0;
const MENU_W: f32 = 240.0;
const ROW_H: f32 = 28.0;
const SEP_W: f32 = 13.0;
/// How long the previews stay after the pointer leaves them and their icon.
const PREVIEW_GRACE_S: f64 = 0.3;
/// Padding inside a preview card and around the row of cards.
const CARD_PAD: f32 = 10.0;
const CARD_GAP: f32 = 6.0;
const PANEL_PAD: f32 = 8.0;
const CARD_TITLE_H: f32 = 24.0;
/// How long the previews take to fade in.
const PREVIEW_IN_S: f64 = 0.16;

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
    /// Captures window previews on request.
    pub thumbs: Option<Thumbs>,
    /// The app whose windows are shown as previews.
    preview: Option<String>,
    /// The app whose previews open at this clock time, if the pointer stays on it.
    preview_due: Option<(String, f64)>,
    /// When the pointer left both the icon and the previews; they close a moment later.
    preview_left: Option<f64>,
    in_preview: bool,
    /// When the previews opened, for their fade-in.
    preview_opened: f64,
    /// The pointer is on the card's close button.
    close_hot: bool,
    /// The preview under the pointer, by window address.
    card: Option<String>,
    /// Wheel travel over an icon not yet turned into a window step, and when the last step was.
    wheel: (f32, f64),
    /// The latest preview of each window, by address.
    previews: HashMap<String, Preview>,
}

/// A window's preview. A new capture decodes off-thread, so the one on screen stays until it's in.
struct Preview {
    shown: Option<Image>,
    next: Option<Image>,
}

impl Preview {
    /// Swaps in a newer capture once it has decoded; true if it did.
    fn settle(&mut self) -> bool {
        if self.next.as_ref().is_some_and(Image::is_ready) { self.shown = self.next.take(); true } else { false }
    }

    fn image(&self) -> Option<&Image> {
        self.shown.as_ref().filter(|i| i.is_ready() && i.error().is_none())
    }
}

impl Dock {
    pub fn new(monitor: String, size: (f32, f32), scale: f32, cfg: Config, light: bool, tx: Sender<Msg>, backdrop: Backdrop) -> Dock {
        Dock {
            screen_h: size.1, monitor, size, scale, cfg, light, tx,
            snap: Snapshot { items: Vec::new(), fullscreen: false, overlapped: false, workspace: 0 },
            cache: None, images: HashMap::new(), phase: Phase::Hidden, reveal: 0.0, mag: 0.0, pointer: None,
            in_zone: false, in_strip: false, in_menu: false, hovered: None, menu: None, menu_row: None,
            sizes: HashMap::new(), launching: HashMap::new(), backdrop, live: None, live_on: false,
            now: 0.0, last: None, summoned: None, thumbs: None, preview: None, preview_due: None, preview_left: None, in_preview: false, preview_opened: 0.0, close_hot: false, card: None, wheel: (0.0, 0.0), previews: HashMap::new(),
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
        self.previews.retain(|a, _| snap.items.iter().any(|i| i.windows.iter().any(|w| &w.address == a)));
        self.snap = snap;
        self.load_icons();
        tx.wake_after(Duration::ZERO);
    }

    /// A new capture of a window from the preview thread.
    pub fn set_thumb(&mut self, address: String, w: u32, h: u32, rgba: Vec<u8>, tx: &mut Tx) {
        let Some(cache) = &self.cache else { return };
        let img = cache.rgba(w, h, rgba);
        let p = self.previews.entry(address).or_insert(Preview { shown: None, next: None });
        if p.shown.is_none() { p.shown = Some(img); } else { p.next = Some(img); }
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

    /// From `echodock ctl preview`: bring the dock up with this app's windows showing.
    pub fn show_preview(&mut self, key: String, tx: &mut Tx) {
        self.summon(false, tx);
        self.menu = None;
        if self.preview.is_none() { self.preview_opened = self.now; }
        self.preview = Some(key);
        self.preview_left = None;
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
        self.in_zone || self.in_strip || self.in_menu || self.in_preview
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

    /// The pointer came onto (or left) an icon: a running app's previews open after a rest.
    fn hover_icon(&mut self, key: &str, inside: bool, tx: &mut Tx) {
        if inside {
            self.hovered = Some(key.to_owned());
            let running = self.snap.items.iter().any(|i| i.key == key && i.kind == Kind::App && i.running());
            // With previews already open, they follow the pointer from icon to icon at once.
            if self.preview.is_some() && self.preview.as_deref() != Some(key) {
                if running { self.preview = Some(key.to_owned()); self.card = None; } else { self.close_preview(); }
                self.preview_left = None;
            } else if self.cfg.behavior.previews && running && self.menu.is_none() && self.preview.as_deref() != Some(key) {
                let due = self.now + self.cfg.behavior.preview_delay_ms as f64 / 1000.0;
                self.preview_due = Some((key.to_owned(), due));
                tx.wake_at(due);
            }
        } else {
            if self.hovered.as_deref() == Some(key) { self.hovered = None; }
            if self.preview_due.as_ref().is_some_and(|(k, _)| k == key) { self.preview_due = None; }
        }
        tx.wake_after(Duration::ZERO);
    }

    /// Tells the capture thread which windows' previews are on screen, at what size.
    fn request_thumbs(&self) {
        let Some(t) = &self.thumbs else { return };
        let item = self.preview.as_ref().and_then(|k| self.snap.items.iter().find(|i| &i.key == k));
        match item {
            Some(it) => {
                let (_, tw, th) = card_size(self, it.windows.len());
                let px = |v: f32| (v * self.scale).round() as u32;
                t.want(it.windows.iter().map(|w| w.address.clone()).collect(), (px(tw), px(th)));
            }
            None => t.want(Vec::new(), (0, 0)),
        }
    }

    fn close_preview(&mut self) {
        if !self.in_zone { self.pointer = None; }
        self.preview = None;
        self.preview_due = None;
        self.preview_left = None;
        self.in_preview = false;
        self.card = None;
    }

    /// Opens, keeps or closes the previews as the pointer moves; called every frame.
    fn step_preview(&mut self, clock: f64, tx: &mut Tx) -> bool {
        let mut changed = false;
        if let Some((key, due)) = self.preview_due.clone() {
            if self.hovered.as_deref() != Some(key.as_str()) || self.menu.is_some() {
                self.preview_due = None;
            } else if clock >= due {
                if self.preview.is_none() { self.preview_opened = clock; }
                self.preview = Some(key);
                self.preview_due = None;
                self.preview_left = None;
                changed = true;
            } else {
                tx.wake_at(due);
            }
        }
        if let Some(key) = self.preview.clone() {
            let still = self.snap.items.iter().any(|i| i.key == key && i.running());
            // Opened by a shortcut, they stay as long as the dock does.
            let near = self.in_preview || self.hovered.as_deref() == Some(key.as_str()) || self.summoned.is_some();
            if !still || self.menu.is_some() || !self.visible() {
                self.close_preview();
                changed = true;
            } else if near {
                self.preview_left = None;
            } else {
                // A short grace, so the pointer can cross from the icon to the previews.
                let left = *self.preview_left.get_or_insert(clock);
                if clock - left > PREVIEW_GRACE_S {
                    self.close_preview();
                    changed = true;
                } else {
                    tx.wake_at(left + PREVIEW_GRACE_S);
                }
            }
        }
        changed
    }

    /// The wheel over a running app's icon steps through its windows: one step per notch, or
    /// per 40 points of trackpad travel, and no faster than every 120 ms.
    fn scroll(&mut self, key: &str, dy: f32) -> bool {
        if !self.cfg.behavior.scroll_cycles || !self.snap.items.iter().any(|i| i.key == key && i.running()) { return false; }
        self.wheel.0 += dy;
        if self.wheel.0.abs() >= 40.0 && self.now - self.wheel.1 > 0.12 {
            // Scrolling down (content moving up) goes forward.
            self.send(Action::Cycle(key.to_owned(), self.wheel.0 < 0.0));
            self.wheel = (0.0, self.now);
        }
        true
    }

    fn activate(&mut self, key: &str, kind: Kind, now: f64) {
        self.menu = None;
        self.close_preview();
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

/// Rising: ease-out (fast start, gentle landing). Leaving: ease-in (gentle start, quick exit).
fn ease(t: f32, rising: bool) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if rising { 1.0 - (1.0 - t).powi(3) } else { t * t * (3.0 - 2.0 * t) }
}

const RISE_S: f32 = 0.28;
const FALL_S: f32 = 0.22;
const MAG_IN_S: f32 = 0.16;

pub fn setup(s: &mut Dock, waker: Waker) {
    s.cache = Some(ImageCache::new(waker));
    s.load_icons();
}

/// Steps timers and animations from the frame clock.
pub fn poll(s: &mut Dock, clock: f64, tx: &mut Tx) -> bool {
    let dt = s.last.map_or(0.0, |l| (clock - l).clamp(0.0, 0.1)) as f32;
    s.last = Some(clock);
    s.now = clock;
    let mut changed = false;
    match s.phase {
        Phase::Arming { due } if clock >= due => { s.phase = Phase::Shown; changed = true; }
        Phase::Leaving { due } if clock >= due && !s.inside() => {
            s.phase = Phase::Hidden;
            s.menu = None;
            changed = true;
        }
        Phase::Leaving { .. } if s.inside() => s.phase = Phase::Shown,
        Phase::Arming { due } | Phase::Leaving { due } => tx.wake_at(due),
        _ => {}
    }
    if let Some(until) = s.summoned && clock >= until && !s.inside() {
        s.summoned = None;
        if s.phase == Phase::Shown { s.phase = Phase::Leaving { due: clock }; }
        tx.wake_after(Duration::ZERO);
        changed = true;
    } else if let Some(until) = s.summoned {
        tx.wake_at(until);
    }
    let target = if s.visible() { 1.0 } else { 0.0 };
    let step = dt / if target > s.reveal { RISE_S } else { FALL_S };
    let next = if target > s.reveal { (s.reveal + step).min(target) } else { (s.reveal - step).max(target) };
    if next != s.reveal { s.reveal = next; changed = true; }
    // Magnification waits for the dock to land, then fades in.
    let mag_target = if s.reveal >= 1.0 && s.pointer.is_some() { 1.0 } else if s.pointer.is_none() { 1.0 } else { 0.0 };
    let mag_target = if s.reveal < 1.0 { 0.0 } else { mag_target };
    let mag_next = if mag_target > s.mag { (s.mag + dt / MAG_IN_S).min(1.0) } else { mag_target };
    if mag_next != s.mag { s.mag = mag_next; changed = true; }
    // Icons follow the magnification wave quickly but never jump.
    let targets = s.targets();
    let km = 1.0 - (-dt * 24.0).exp();
    let keys: Vec<String> = s.snap.items.iter().map(|i| i.key.clone()).collect();
    for (key, t) in keys.iter().zip(targets) {
        let cur = s.sizes.get(key).copied().unwrap_or(t);
        let mut v = cur + (t - cur) * if dt == 0.0 { 1.0 } else { km };
        if (v - t).abs() < 0.05 { v = t; }
        if v != cur || !s.sizes.contains_key(key) { s.sizes.insert(key.clone(), v); changed = true; }
    }
    s.sizes.retain(|k, _| keys.contains(k));
    let want_live = s.cfg.glass.refraction && (s.reveal > 0.0 || matches!(s.phase, Phase::Arming { .. }));
    if want_live != s.live_on && let Some(l) = &s.live {
        if want_live && !l.available.load(std::sync::atomic::Ordering::Relaxed) {
            // Still captures only: nothing to keep running.
        } else {
            // A live rate of 0 means a single capture as the dock shows.
            let fps = if s.cfg.glass.live_fps <= 0.0 { 0.0001 } else { s.cfg.glass.live_fps };
            l.set(want_live, s.band_rect(), s.band_screen(), fps);
            s.live_on = want_live;
        }
    }
    for p in s.previews.values_mut() {
        if p.settle() { changed = true; }
    }
    if s.step_preview(clock, tx) { changed = true; }
    s.request_thumbs();
    s.launching.retain(|_, t| clock - *t < 6.0);
    if !s.launching.is_empty() { changed = true; }
    let fading = s.preview.is_some() && clock - s.preview_opened < PREVIEW_IN_S;
    if fading { changed = true; }
    let animating = fading || s.reveal != target || s.mag != mag_target || !s.launching.is_empty()
        || s.snap.items.iter().zip(s.targets()).any(|(i, t)| s.sizes.get(&i.key).is_some_and(|v| *v != t));
    if animating { tx.wake_after(Duration::ZERO); }
    changed
}

struct Palette {
    ink: Color,
    /// The previews' panel: deeper and more solid than a menu, so the previews read on any backdrop.
    panel: Color,
    tint: Color,
    menu: Color,
    line: Color,
}

fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            ink: Color::WHITE,
            panel: Color { r: 0.012, g: 0.012, b: 0.016, a: 0.94 },
            tint: Color { r: 0.07, g: 0.07, b: 0.09, a: 1.0 },
            menu: Color { r: 0.11, g: 0.11, b: 0.13, a: 0.86 },
            line: Color::WHITE.alpha(0.14),
        }
    } else {
        let black = Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
        Palette { ink: black, panel: Color { r: 0.93, g: 0.93, b: 0.95, a: 0.95 }, tint: Color::WHITE, menu: Color { r: 0.97, g: 0.97, b: 0.98, a: 0.9 }, line: black.alpha(0.12) }
    }
}

pub fn view(s: &Dock, _: &mut Cx<Dock>) -> El {
    let a = &s.cfg.appearance;
    let g = &s.cfg.glass;
    let pal = palette(s.dark());
    let (sw, sh) = s.size;
    // Every position here is computed per frame by `poll`; Rune's own layout springs on top of
    // that would make the icons trail the glass.
    let mut root = el().key("root").w(sw).h(sh).motion(Spring::NONE);

    // The reveal strip along the bottom edge.
    if s.strip_active() {
        root = root.child(el().key("strip").absolute(0.0, sh - s.cfg.behavior.edge_size).size(sw, s.cfg.behavior.edge_size).hit()
            .on_hover_change(|s: &mut Dock, inside, tx| s.on_strip(inside, tx)));
    }
    if s.reveal <= 0.0 || s.snap.items.is_empty() {
        return root;
    }

    let [px, py, pw, ph] = s.pill_rect();
    let radius = a.corner_radius.min(ph / 2.0);
    let (slots, _, _) = s.slots();
    let shown = s.visible();

    // Shadow under the glass.
    let refract = g.refraction && s.backdrop.ready();
    if g.shadow > 0.0 && !refract {
        root = root.child(el().key("shadow").absolute(px, py).size(pw, ph).radius(radius).fill(pal.tint.alpha(0.01))
            .box_shadow(Shadow::core_animation(0.0, 10.0, 28.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.45 * g.shadow }))
            .box_shadow(Shadow::core_animation(0.0, 1.0, 3.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.25 * g.shadow })));
    }
    // Liquid glass: the compositor draws the lens (the frozen screen behind, refracted) under
    // everything here; this adds the tint over it, or a frosted fill without refraction.
    if !refract {
        root = root.child(el().key("tint").absolute(px, py).size(pw, ph).radius(radius).fill(pal.tint.alpha(0.45 + g.tint * 0.4)));
        root = root.child(el().key("rim").absolute(px, py).size(pw, ph).radius(radius)
            .linear_gradient(std::f32::consts::PI, [(0.0, Color::WHITE.alpha(0.10 * g.rim)), (0.5, Color::WHITE.alpha(0.0)), (1.0, Color::WHITE.alpha(0.03 * g.rim))])
            .border(1.0, Color::WHITE.alpha(if s.dark() { 0.16 } else { 0.5 } * g.rim.max(0.2))));
    }

    // The zone the pointer magnifies and keeps the dock up in: the glass plus the room the big icons reach.
    let grow = if a.magnification { a.magnified_size - a.icon_size } else { 0.0 };
    root = root.child(el().key("zone").absolute(px, py - grow).size(pw, ph + grow + a.margin).hit()
        .on_hover_change(|s: &mut Dock, inside, tx| {
            s.in_zone = inside;
            // The wave holds still while the pointer goes up into the previews, so they don't
            // slide away from under it.
            if !inside && s.preview.is_none() { s.pointer = None; }
            if !inside { s.hovered = None; }
            s.pointer_changed(tx);
        })
        .on_pointer_move(|s: &mut Dock, p| s.pointer = Some(p.x)));

    // Icons.
    let bottom = py + ph - a.padding;
    for (it, (x, w)) in s.snap.items.iter().zip(slots.iter().copied()) {
        if it.kind == Kind::Separator {
            root = root.child(el().key(format!("sep-{}", it.key)).absolute(x + w / 2.0 - 0.5, py + a.padding * 0.8).size(1.0, ph - a.padding * 1.6).fill(pal.line));
            continue;
        }
        let bounce = s.launching.get(&it.key).map_or(0.0, |t0| {
            let t = (s.now - t0) as f32;
            (t * std::f32::consts::PI * 1.6).sin().abs() * a.icon_size * 0.38
        });
        let y = bottom - w - bounce;
        let key = it.key.clone();
        let kind = it.kind;
        let mut icon = el().key(format!("icon-{}", it.key)).absolute(x, y).size(w, w).hit().cursor(Cursor::Pointer)
            .on_hover_change({ let key = key.clone(); move |s: &mut Dock, inside, tx| s.hover_icon(&key, inside, tx) })
            .on_press({ let key = key.clone(); move |s: &mut Dock| s.activate(&key, kind, s.now) })
            .on_middle_press({ let key = key.clone(); move |s: &mut Dock, _tx| {
                if kind == Kind::App { s.launching.insert(key.clone(), s.now); s.send(Action::Launch(key.clone())); }
            }})
            .on_wheel({ let key = key.clone(); move |s: &mut Dock, e, _tx| kind == Kind::App && s.scroll(&key, e.dy) })
            .on_secondary_press({ let key = key.clone(); move |s: &mut Dock, _p, tx| {
                s.menu = if s.menu.as_deref() == Some(key.as_str()) { None } else { Some(key.clone()) };
                s.menu_row = None;
                s.close_preview();
                tx.wake_after(Duration::ZERO);
            }});
        icon = match kind {
            Kind::Apps => icon.child(apps_glyph(w, pal.ink)),
            _ => match it.icon.as_ref().and_then(|p| s.images.get(p)) {
                Some(img) => icon.child(el().w(w).h(w).pixels(img.pixels())
                    .opacity(if a.dim_idle && !it.running() && it.kind == Kind::App { 0.62 } else { 1.0 })),
                None => icon.child(letter_tile(&it.name, w, pal.ink)),
            },
        };
        root = root.child(icon);
        // Running indicator: one per window (up to three) when counting, else one per app.
        if it.running() && a.indicator != Indicator::None {
            let n = if a.indicator_count { it.windows.len().min(3) } else { 1 };
            let bar = a.indicator == Indicator::Bar;
            let (iw, ih) = match (bar, n) {
                (true, 1) => (if it.focused() { 16.0 } else { 8.0 }, 3.0),
                (true, _) => (6.0, 3.0),
                _ => (4.0, 4.0),
            };
            let gap = if bar { 2.0 } else { 3.0 };
            let row_w = n as f32 * iw + (n - 1) as f32 * gap;
            let dot = pal.ink.alpha(if it.focused() || !bar { 0.85 } else { 0.55 });
            for k in 0..n {
                let dx = x + w / 2.0 - row_w / 2.0 + k as f32 * (iw + gap);
                root = root.child(el().key(format!("dot-{}-{k}", it.key)).absolute(dx, py + ph - a.padding / 2.0 - ih / 2.0 + 1.0)
                    .size(iw, ih).radius(ih / 2.0).fill(dot));
            }
        }
    }

    // Name label over the hovered icon.
    if a.labels && shown && s.menu.is_none() && s.preview.is_none() && let Some(h) = &s.hovered
        && let Some((it, (x, w))) = s.snap.items.iter().zip(slots.iter().copied()).find(|(i, _)| &i.key == h) {
        let size = s.size_of(it);
        let label = row().key("label").absolute(0.0, 0.0).h(26.0).px(11.0).center_y().radius(8.0).fill(pal.menu).border(1.0, pal.line)
            .child(text(it.name.clone()).font(TextStyle::new(13.0, 520.0)).color(pal.ink.alpha(0.9)));
        let lw = (it.name.chars().count() as f32 * 7.2 + 24.0).min(320.0);
        let lx = (x + w / 2.0 - lw / 2.0).clamp(4.0, sw - lw - 4.0);
        root = root.child(el().key("label-at").absolute(lx, bottom - size - 36.0).w(lw).child(label.absolute(0.0, 0.0).w(lw).center()));
    }

    // Window previews.
    if let Some(key) = &s.preview && let Some((it, (x, w))) = s.snap.items.iter().zip(slots.iter().copied()).find(|(i, _)| &i.key == key) {
        let top = bottom - a.magnified_size.max(a.icon_size) - 10.0;
        root = root.child(previews(s, it, x + w / 2.0, top, &pal));
    }

    // Context menu.
    if let Some(key) = &s.menu && let Some((it, (x, w))) = s.snap.items.iter().zip(slots.iter().copied()).find(|(i, _)| &i.key == key) {
        root = root.child(menu(s, it, x + w / 2.0, py - 10.0, &pal));
    }
    root
}

/// Card width and the preview box inside it, fitted so every window's card fits on screen.
fn card_size(s: &Dock, n: usize) -> (f32, f32, f32) {
    let n = n.max(1) as f32;
    let room = s.size.0 - 24.0 - 2.0 * PANEL_PAD - (n - 1.0) * CARD_GAP;
    let cw = s.cfg.behavior.preview_size.min(room / n).max(96.0);
    let tw = cw - 2.0 * CARD_PAD;
    (cw, tw, (tw * 0.6).round())
}

/// An app's windows as a row of cards above its icon: each the app's icon and the window's
/// title over a framed live preview. The row fades and rises in as it opens.
fn previews(s: &Dock, it: &Item, cx: f32, bottom: f32, pal: &Palette) -> El {
    let n = it.windows.len();
    let (cw, tw, th) = card_size(s, n);
    let ch = CARD_PAD + CARD_TITLE_H + 8.0 + th + CARD_PAD;
    let pw = n as f32 * cw + (n as f32 - 1.0) * CARD_GAP + 2.0 * PANEL_PAD;
    let ph = ch + 2.0 * PANEL_PAD;
    let t = ease(((s.now - s.preview_opened) / PREVIEW_IN_S) as f32, true);
    let x = (cx - pw / 2.0).clamp(6.0, s.size.0 - pw - 6.0);
    let y = (bottom - ph).max(4.0) + (1.0 - t) * 8.0;
    let dark = s.dark();
    let mut c = el().key("previews").absolute(x, y).size(pw, ph).radius(20.0).opacity(t)
        .fill(pal.panel)
        .linear_gradient(std::f32::consts::PI, [(0.0, pal.ink.alpha(if dark { 0.07 } else { 0.5 })), (0.35, pal.ink.alpha(0.0)), (1.0, pal.ink.alpha(0.0))])
        .border(1.0, pal.ink.alpha(if dark { 0.12 } else { 0.08 }))
        .box_shadow(Shadow::core_animation(0.0, 18.0, 44.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.42 }))
        .box_shadow(Shadow::core_animation(0.0, 2.0, 6.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.22 })).hit()
        .on_hover_change(|s: &mut Dock, inside, tx| { s.in_preview = inside; if !inside { s.card = None; } s.pointer_changed(tx); });
    let app_icon = |size: f32| match it.icon.as_ref().and_then(|p| s.images.get(p)) {
        Some(img) => el().size(size, size).pixels(img.pixels()),
        None => letter_tile(&it.name, size, pal.ink),
    };
    for (k, win) in it.windows.iter().enumerate() {
        let address = win.address.clone();
        let hot = s.card.as_deref() == Some(address.as_str());
        let kx = PANEL_PAD + k as f32 * (cw + CARD_GAP);
        // The preview, fitted to the window's shape, or the app's icon until one arrives.
        let aspect = if win.size[1] > 0 { win.size[0] as f32 / win.size[1] as f32 } else { 1.6 };
        let (iw, ih) = if tw / th > aspect { (th * aspect, th) } else { (tw, tw / aspect) };
        let (ox, oy) = ((tw - iw) / 2.0, (th - ih) / 2.0);
        let shot = match s.previews.get(&win.address).and_then(|p| p.image()) {
            Some(img) => el().size(iw, ih).clip_rounded(9.0).child(el().size(iw, ih).pixels(img.pixels())),
            None => el().size(iw, ih).radius(9.0).fill(pal.ink.alpha(0.06)).center().child(app_icon((ih * 0.4).round().min(56.0))),
        };
        // Minimized windows look set aside; one on another workspace says which.
        let shot = shot.opacity(if win.minimized { 0.4 } else { 1.0 });
        let frame = el().absolute(ox, oy).size(iw, ih).radius(9.0)
            .border(if hot { 2.0 } else { 1.0 }, pal.ink.alpha(if hot { 0.55 } else if win.focused { 0.3 } else { 0.1 }))
            .box_shadow(Shadow::core_animation(0.0, if hot { 8.0 } else { 3.0 }, if hot { 18.0 } else { 8.0 }, Color { r: 0.0, g: 0.0, b: 0.0, a: if hot { 0.4 } else { 0.25 } }))
            .child(shot);
        let tag = if win.minimized { Some("Minimized".to_owned()) } else if win.workspace != s.snap.workspace { Some(format!("Workspace {}", win.workspace)) } else { None };
        let tag = tag.map(|t| {
            let w = t.chars().count() as f32 * 6.0 + 16.0;
            el().absolute(ox + iw - w - 6.0, oy + ih - 26.0).size(w, 20.0).radius(10.0).fill(Color { r: 0.0, g: 0.0, b: 0.0, a: 0.62 })
                .border(1.0, Color::WHITE.alpha(0.12)).center()
                .child(text(t).font(TextStyle::new(10.5, 600.0)).color(Color::WHITE.alpha(0.92)))
        });
        let body = el().absolute(CARD_PAD, CARD_PAD + CARD_TITLE_H + 8.0).size(tw, th).child(frame).children(tag);
        // Title row: the app's icon, the window's title, and a close button under the pointer.
        let title = if win.title.is_empty() { it.name.clone() } else { win.title.clone() };
        let ib = 18.0;
        let mut head = el().absolute(CARD_PAD, CARD_PAD).size(tw, CARD_TITLE_H)
            .child(el().absolute(0.0, (CARD_TITLE_H - ib) / 2.0).size(ib, ib).child(app_icon(ib)))
            .child(el().absolute(ib + 7.0, 0.0).size(tw - ib - 7.0 - if hot { 26.0 } else { 0.0 }, CARD_TITLE_H).center_y()
                .child(text(title).font(TextStyle::new(12.5, if win.focused { 620.0 } else { 520.0 })).color(pal.ink.alpha(0.92)).truncate()));
        if hot {
            let b = 22.0;
            head = head.child(el().key(format!("close-{address}")).absolute(tw - b, (CARD_TITLE_H - b) / 2.0).size(b, b).radius(b / 2.0)
                .fill(if s.close_hot { Color { r: 0.9, g: 0.22, b: 0.2, a: 1.0 } } else { pal.ink.alpha(0.12) }).center().cursor(Cursor::Pointer).hit()
                .on_hover_change(|s: &mut Dock, inside, tx| { s.close_hot = inside; tx.wake_after(Duration::ZERO); })
                .on_press({ let address = address.clone(); move |s: &mut Dock| { s.close_hot = false; s.send(Action::Close(address.clone())) } })
                .child(text("✕").font(TextStyle::new(10.5, 700.0)).color(if s.close_hot { Color::WHITE } else { pal.ink.alpha(0.8) })));
        }
        let card = el().key(format!("card-{address}")).absolute(kx, PANEL_PAD).size(cw, ch).radius(14.0).cursor(Cursor::Pointer).hit()
            .fill(pal.ink.alpha(if hot { 0.1 } else { 0.0 }))
            .on_hover_change({ let address = address.clone(); move |s: &mut Dock, inside, tx| {
                if inside { s.card = Some(address.clone()); } else if s.card.as_deref() == Some(address.as_str()) { s.card = None; s.close_hot = false; }
                tx.wake_after(Duration::ZERO);
            }})
            .on_press({ let address = address.clone(); move |s: &mut Dock| { s.close_preview(); s.send(Action::Focus(address.clone())); } })
            // Middle-click closes the window, as on a browser tab.
            .on_middle_press({ let address = address.clone(); move |s: &mut Dock, tx| { s.send(Action::Close(address.clone())); tx.wake_after(Duration::ZERO); } })
            .child(head)
            .child(body);
        // The focused window: a short bar under its preview.
        let card = if win.focused {
            card.child(el().absolute(cw / 2.0 - 10.0, ch - 5.0).size(20.0, 3.0).radius(1.5).fill(pal.ink.alpha(0.8)))
        } else { card };
        c = c.child(card);
    }
    c
}

/// One row of a menu: what it says and what it does.
enum Entry {
    Header(String),
    Line,
    Do(String, MenuAction, bool),
}

#[derive(Clone)]
enum MenuAction {
    Focus(String),
    Launch(String),
    LaunchAction(String, usize),
    MinimizeAll(String),
    Pin(String, bool),
    Quit(String),
    Autohide,
    Settings,
    OpenTrash,
}

fn entries(s: &Dock, it: &Item) -> Vec<Entry> {
    let mut v = Vec::new();
    match it.kind {
        Kind::Apps => {
            v.push(Entry::Header("EchoDock".into()));
            v.push(Entry::Do("Auto-hide".into(), MenuAction::Autohide, s.cfg.behavior.visibility == Visibility::AutoHide));
            v.push(Entry::Do("Dock Settings…".into(), MenuAction::Settings, false));
        }
        Kind::Trash => v.push(Entry::Do("Open".into(), MenuAction::OpenTrash, false)),
        Kind::Separator => {}
        Kind::App => {
            v.push(Entry::Header(it.name.clone()));
            for w in it.windows.iter().take(8) {
                let mut t = if w.title.is_empty() { it.name.clone() } else { w.title.clone() };
                if t.chars().count() > 34 { t = t.chars().take(33).collect::<String>() + "…"; }
                if w.minimized { t = format!("◌ {t}"); }
                v.push(Entry::Do(t, MenuAction::Focus(w.address.clone()), w.focused));
            }
            if it.running() { v.push(Entry::Line); }
            for (i, name) in it.actions.iter().enumerate().take(6) {
                v.push(Entry::Do(name.clone(), MenuAction::LaunchAction(it.key.clone(), i), false));
            }
            if !it.actions.is_empty() { v.push(Entry::Line); }
            if it.has_entry { v.push(Entry::Do(if it.running() { "New Window" } else { "Open" }.into(), MenuAction::Launch(it.key.clone()), false)); }
            if it.running() { v.push(Entry::Do("Minimize All".into(), MenuAction::MinimizeAll(it.key.clone()), false)); }
            if it.has_entry {
                v.push(Entry::Do(if it.pinned { "Remove from Dock" } else { "Keep in Dock" }.into(), MenuAction::Pin(it.key.clone(), !it.pinned), false));
            }
            if it.running() {
                v.push(Entry::Line);
                let n = it.windows.len();
                v.push(Entry::Do(if n > 1 { format!("Quit ({n} windows)") } else { "Quit".into() }, MenuAction::Quit(it.key.clone()), false));
            }
        }
    }
    v
}

fn run_menu(s: &mut Dock, a: MenuAction) {
    s.menu = None;
    match a {
        MenuAction::Focus(addr) => s.send(Action::Focus(addr)),
        MenuAction::Launch(k) => { s.launching.insert(k.clone(), s.now); s.send(Action::Launch(k)) }
        MenuAction::LaunchAction(k, i) => s.send(Action::LaunchAction(k, i)),
        MenuAction::MinimizeAll(k) => s.send(Action::MinimizeAll(k)),
        MenuAction::Pin(k, on) => s.send(Action::Pin(k, on)),
        MenuAction::Quit(k) => s.send(Action::Quit(k)),
        MenuAction::Autohide => { let _ = s.tx.send(Msg::Ctl("autohide".into(), std::sync::mpsc::channel().0)); }
        MenuAction::Settings => echodock_core::apps::run("echodock-settings"),
        MenuAction::OpenTrash => s.send(Action::OpenTrash),
    }
}

fn menu(s: &Dock, it: &Item, cx: f32, bottom: f32, pal: &Palette) -> El {
    let rows = entries(s, it);
    let h: f32 = rows.iter().map(|r| match r { Entry::Line => 9.0, _ => ROW_H }).sum::<f32>() + 12.0;
    let x = (cx - MENU_W / 2.0).clamp(6.0, s.size.0 - MENU_W - 6.0);
    let y = (bottom - h).max(4.0);
    let mut c = col().key("menu").absolute(x, y).w(MENU_W).padding(6.0).radius(14.0).fill(pal.menu).border(1.0, pal.line)
        .box_shadow(Shadow::core_animation(0.0, 8.0, 24.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.35 })).hit()
        .on_hover_change(|s: &mut Dock, inside, tx| { s.in_menu = inside; s.pointer_changed(tx); });
    for (i, r) in rows.into_iter().enumerate() {
        c = c.child(match r {
            Entry::Header(t) => row().h(ROW_H).px(10.0).center_y()
                .child(text(t).font(TextStyle::new(12.0, 650.0)).color(pal.ink.alpha(0.55))),
            Entry::Line => el().h(9.0).w_full().center_y().child(el().h(1.0).w_full().fill(pal.line)),
            Entry::Do(t, act, checked) => {
                let hot = s.menu_row == Some(i);
                row().key(format!("menu-row-{i}")).h(ROW_H).w_full().px(10.0).center_y().radius(7.0).cursor(Cursor::Pointer)
                    .fill(if hot { pal.ink.alpha(0.12) } else { pal.ink.alpha(0.0) })
                    .on_hover_change(move |s: &mut Dock, inside, _| {
                        if inside { s.menu_row = Some(i); } else if s.menu_row == Some(i) { s.menu_row = None; }
                    })
                    .on_press(move |s: &mut Dock| run_menu(s, act.clone()))
                    .child(text(t).font(TextStyle::new(13.0, if checked { 600.0 } else { 450.0 })).color(pal.ink.alpha(0.92)))
                    .child(el().grow(1.0))
                    .children(checked.then(|| text("✓").font(TextStyle::new(13.0, 600.0)).color(pal.ink.alpha(0.7))))
            }
        });
    }
    c
}

/// The apps button: a 3×3 grid of rounded squares, drawn so it stays crisp at any size.
fn apps_glyph(w: f32, ink: Color) -> El {
    let sq = (w * 0.2).round();
    let gap = (w * 0.09).round();
    let total = sq * 3.0 + gap * 2.0;
    let o = (w - total) / 2.0;
    let mut g = el().w(w).h(w);
    for i in 0..9 {
        let (cx, cy) = ((i % 3) as f32, (i / 3) as f32);
        g = g.child(el().absolute(o + cx * (sq + gap), o + cy * (sq + gap)).size(sq, sq).radius(sq * 0.32).fill(ink.alpha(0.88)));
    }
    g
}

/// An app without an icon: its initial on a tile.
fn letter_tile(name: &str, w: f32, ink: Color) -> El {
    let letter = name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "?".into());
    el().w(w).h(w).radius(w * 0.24).fill(ink.alpha(0.14)).center()
        .child(text(letter).font(TextStyle::new((w * 0.5).round(), 650.0)).color(ink.alpha(0.9)))
}

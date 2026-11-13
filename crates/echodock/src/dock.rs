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
    s.launching.retain(|_, t| clock - *t < 6.0);
    if !s.launching.is_empty() { changed = true; }
    let animating = s.reveal != target || s.mag != mag_target || !s.launching.is_empty()
        || s.snap.items.iter().zip(s.targets()).any(|(i, t)| s.sizes.get(&i.key).is_some_and(|v| *v != t));
    if animating { tx.wake_after(Duration::ZERO); }
    changed
}

struct Palette {
    ink: Color,
    tint: Color,
    menu: Color,
    line: Color,
}

fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            ink: Color::WHITE,
            tint: Color { r: 0.07, g: 0.07, b: 0.09, a: 1.0 },
            menu: Color { r: 0.11, g: 0.11, b: 0.13, a: 0.86 },
            line: Color::WHITE.alpha(0.14),
        }
    } else {
        let black = Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
        Palette { ink: black, tint: Color::WHITE, menu: Color { r: 0.97, g: 0.97, b: 0.98, a: 0.9 }, line: black.alpha(0.12) }
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
            if !inside { s.pointer = None; s.hovered = None; }
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
            .on_hover_change({ let key = key.clone(); move |s: &mut Dock, inside, _tx| {
                if inside { s.hovered = Some(key.clone()); } else if s.hovered.as_deref() == Some(key.as_str()) { s.hovered = None; }
            }})
            .on_press({ let key = key.clone(); move |s: &mut Dock| s.activate(&key, kind, s.now) })
            .on_middle_press({ let key = key.clone(); move |s: &mut Dock, _tx| {
                if kind == Kind::App { s.launching.insert(key.clone(), s.now); s.send(Action::Launch(key.clone())); }
            }})
            .on_secondary_press({ let key = key.clone(); move |s: &mut Dock, _p, tx| {
                s.menu = if s.menu.as_deref() == Some(key.as_str()) { None } else { Some(key.clone()) };
                s.menu_row = None;
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
        // Running indicator.
        if it.running() && a.indicator != Indicator::None {
            let (iw, ih) = if a.indicator == Indicator::Bar { (if it.focused() { 16.0 } else { 8.0 }, 3.0) } else { (4.0, 4.0) };
            let dot = pal.ink.alpha(if it.focused() || a.indicator == Indicator::Dot { 0.85 } else { 0.55 });
            root = root.child(el().key(format!("dot-{}", it.key)).absolute(x + w / 2.0 - iw / 2.0, py + ph - a.padding / 2.0 - ih / 2.0 + 1.0)
                .size(iw, ih).radius(ih / 2.0).fill(dot));
        }
    }

    // Name label over the hovered icon.
    if a.labels && shown && s.menu.is_none() && let Some(h) = &s.hovered
        && let Some((it, (x, w))) = s.snap.items.iter().zip(slots.iter().copied()).find(|(i, _)| &i.key == h) {
        let size = s.size_of(it);
        let label = row().key("label").absolute(0.0, 0.0).h(26.0).px(11.0).center_y().radius(8.0).fill(pal.menu).border(1.0, pal.line)
            .child(text(it.name.clone()).font(TextStyle::new(13.0, 520.0)).color(pal.ink.alpha(0.9)));
        let lw = (it.name.chars().count() as f32 * 7.2 + 24.0).min(320.0);
        let lx = (x + w / 2.0 - lw / 2.0).clamp(4.0, sw - lw - 4.0);
        root = root.child(el().key("label-at").absolute(lx, bottom - size - 36.0).w(lw).child(label.absolute(0.0, 0.0).w(lw).center()));
    }

    // Context menu.
    if let Some(key) = &s.menu && let Some((it, (x, w))) = s.snap.items.iter().zip(slots.iter().copied()).find(|(i, _)| &i.key == key) {
        root = root.child(menu(s, it, x + w / 2.0, py - 10.0, &pal));
    }
    root
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

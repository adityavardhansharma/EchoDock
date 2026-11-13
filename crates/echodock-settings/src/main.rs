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

fn t(s: impl Into<String>, size: f32, weight: f32, c: Color) -> El {
    text(s.into()).font(TextStyle::new(size, weight)).color(c)
}

fn view(s: &Settings, _: &mut Cx<Settings>) -> El {
    let mut side = col().key("sidebar").w(210.0).h_full().fill(SIDEBAR).pt(22.0).px(12.0).gap(2.0)
        .child(row().px(10.0).pb(18.0).gap(10.0).center_y()
            .child(dock_logo())
            .child(col().child(t("EchoDock", 16.0, 680.0, INK)).child(t("Settings", 12.0, 450.0, DIM))));
    for (sec, name) in SECTIONS {
        let on = s.section == sec;
        side = side.child(row().key(format!("nav-{name}")).h(34.0).w_full().px(12.0).center_y().radius(9.0).cursor(Cursor::Pointer)
            .fill(if on { ACCENT.alpha(0.18) } else { INK.alpha(0.0) })
            .hover(if on { ACCENT.alpha(0.22) } else { INK.alpha(0.05) })
            .on_press(move |s: &mut Settings| s.section = sec)
            .child(t(name, 13.5, if on { 600.0 } else { 480.0 }, if on { INK } else { INK.alpha(0.78) })));
    }
    side = side.child(el().grow(1.0)).child(
        row().key("preview").h(34.0).w_full().center().radius(9.0).fill(INK.alpha(0.06)).hover(INK.alpha(0.1)).cursor(Cursor::Pointer).mb(16.0)
            .on_press(|_: &mut Settings| { std::thread::spawn(|| { let _ = ipc::send("reveal"); }); })
            .child(t("Show Dock", 13.0, 560.0, INK)));

    let body = match s.section {
        Section::Appearance => appearance(s),
        Section::Glass => glass(s),
        Section::Behavior => behavior(s),
        Section::Apps => apps(s),
        Section::Shortcuts => shortcuts(),
        Section::About => about(),
    };
    let content = col().key("content").grow(1.0).h_full().scroll(ScrollSpec { stick_bottom: false })
        .child(col().w_full().px(34.0).pt(28.0).pb(40.0).gap(18.0).child(body));
    row().key("root").w_full().h_full().fill(BG).motion(Spring::SNAPPY).child(side).child(el().w(1.0).h_full().fill(LINE)).child(content)
}

fn dock_logo() -> El {
    el().size(34.0, 34.0).radius(10.0).linear_gradient(std::f32::consts::PI, [(0.0, Color { r: 0.45, g: 0.7, b: 1.0, a: 1.0 }), (1.0, Color { r: 0.25, g: 0.35, b: 0.95, a: 1.0 })])
        .child(el().absolute(6.0, 21.0).size(22.0, 7.0).radius(3.5).fill(Color::WHITE.alpha(0.85)))
}

fn header(title: &str, sub: &str) -> El {
    col().gap(4.0).pb(4.0).child(t(title, 24.0, 700.0, INK)).child(t(sub, 13.0, 450.0, DIM))
}

fn card(rows: Vec<El>) -> El {
    let n = rows.len();
    let mut c = col().w_full().radius(14.0).fill(CARD).border(1.0, LINE);
    for (i, r) in rows.into_iter().enumerate() {
        c = c.child(r);
        if i + 1 < n { c = c.child(row().w_full().pl(16.0).child(el().h(1.0).grow(1.0).fill(LINE))); }
    }
    c
}

fn group(title: &str, rows: Vec<El>) -> El {
    col().w_full().gap(8.0).child(t(title.to_uppercase(), 11.0, 650.0, DIM).ml(4.0)).child(card(rows))
}

fn setting(label: &str, help: &str, control: El) -> El {
    row().w_full().min_h(54.0).px(16.0).py(10.0).gap(16.0).center_y()
        .child(col().grow(1.0).gap(3.0).child(t(label, 13.5, 540.0, INK)).children((!help.is_empty()).then(|| t(help, 12.0, 430.0, DIM).w_full().wrap())))
        .child(control)
}

fn switch(on: bool, f: fn(&mut Config)) -> El {
    let knob = el().size(20.0, 20.0).radius(10.0).fill(Color::WHITE).absolute(if on { 20.0 } else { 2.0 }, 2.0)
        .box_shadow(rune::Shadow::core_animation(0.0, 1.0, 3.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.3 }));
    el().size(42.0, 24.0).radius(12.0).cursor(Cursor::Pointer).fill(if on { ACCENT } else { INK.alpha(0.16) }).motion(Spring::SNAPPY)
        .on_press(move |s: &mut Settings| s.change(f)).child(knob)
}

fn segmented<T: Copy + PartialEq + 'static>(opts: &[(&'static str, T)], cur: T, f: fn(&mut Config, T)) -> El {
    let mut r = row().h(30.0).padding(3.0).gap(2.0).radius(9.0).fill(INK.alpha(0.07));
    for &(name, v) in opts {
        let on = v == cur;
        r = r.child(row().h(24.0).px(12.0).center().radius(7.0).cursor(Cursor::Pointer)
            .fill(if on { INK.alpha(0.16) } else { INK.alpha(0.0) }).hover(INK.alpha(if on { 0.18 } else { 0.06 }))
            .on_press(move |s: &mut Settings| s.change(|c| f(c, v)))
            .child(t(name, 12.5, if on { 600.0 } else { 480.0 }, if on { INK } else { INK.alpha(0.7) })));
    }
    r
}

/// A slider: drag or click anywhere on the track.
fn slider(key: &'static str, v: f32, min: f32, max: f32, step: f32, unit: &str, f: fn(&mut Config, f32)) -> El {
    const W: f32 = 200.0;
    let k = ((v - min) / (max - min)).clamp(0.0, 1.0);
    let label = if step < 1.0 { format!("{v:.2}{unit}") } else { format!("{v:.0}{unit}") };
    let set = move |s: &mut Settings, x: f32, w: f32| {
        let raw = min + (x / w.max(1.0)).clamp(0.0, 1.0) * (max - min);
        let snapped = (raw / step).round() * step;
        s.change(|c| f(c, snapped));
    };
    let track = el().key(format!("slider-{key}")).size(W, 22.0).cursor(Cursor::Pointer)
        .on_drag(move |s: &mut Settings, d, _tx| {
            match d.phase {
                rune::Phase::Down => s.dragging = Some(key),
                rune::Phase::Up => s.dragging = None,
                rune::Phase::Move => {}
            }
            set(s, d.x, d.w);
        })
        .child(el().absolute(0.0, 9.0).size(W, 4.0).radius(2.0).fill(INK.alpha(0.14)))
        .child(el().absolute(0.0, 9.0).size(W * k, 4.0).radius(2.0).fill(ACCENT))
        .child(el().absolute(W * k - 9.0, 2.0).size(18.0, 18.0).radius(9.0).fill(Color::WHITE)
            .box_shadow(rune::Shadow::core_animation(0.0, 1.0, 3.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.35 })));
    row().gap(12.0).center_y().child(track).child(t(label, 12.5, 520.0, DIM).w(58.0))
}

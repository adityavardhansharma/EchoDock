//! echodock: a macOS-style dock for Omarchy / Hyprland, drawn with Rune.
//!
//! `echodock`            run the dock (one per session)
//! `echodock ctl <cmd>`  talk to the running dock (`echodock ctl help`)

mod brain;
mod dock;
mod glass;
mod live;

use std::sync::mpsc;

use echodock_core::config::{self, Config};
use echodock_core::{hypr, ipc};
use rune::{App, AppSpec, Cx, El, OverlayInput, OverlayOptions, OverlayPlacement, OverlaySpec, RunOptions, ScreenEdge, WaylandLayer, WindowSpec, el};

use brain::{Msg, Overlay};
use dock::Dock;

/// Height reserved above the dock for labels and menus.
fn surface_height(cfg: &Config) -> f32 {
    let a = &cfg.appearance;
    (a.margin + a.icon_size + 2.0 * a.padding + (a.magnified_size - a.icon_size).max(0.0) + 40.0 + 340.0).ceil()
}

fn light_theme() -> bool {
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    home.join(".local/state/omarchy/current/theme/light.mode").exists()
}

struct Root;

fn root_view(_: &Root, _: &mut Cx<Root>) -> El {
    el()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("ctl") {
        let cmd = args[2..].join(" ");
        match ipc::send(if cmd.is_empty() { "help" } else { &cmd }) {
            Ok(reply) => println!("{reply}"),
            Err(e) => {
                eprintln!("echodock is not running ({e})");
                std::process::exit(1);
            }
        }
        return;
    }
    if args.iter().any(|a| a == "--version") {
        println!("echodock {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let cfg = Config::load();
    if !config::path().exists() && let Err(e) = cfg.save() {
        eprintln!("echodock: writing default config: {e}");
    }
    let (tx, rx) = mpsc::channel::<Msg>();
    {
        let tx = tx.clone();
        if let Err(e) = ipc::serve(move |cmd| {
            let (rtx, rrx) = mpsc::channel();
            if tx.send(Msg::Ctl(cmd.to_owned(), rtx)).is_err() { return "error".into(); }
            rrx.recv_timeout(std::time::Duration::from_secs(3)).unwrap_or_else(|_| "timeout".into())
        }) {
            eprintln!("echodock: {e}");
            std::process::exit(1);
        }
    }

    let mut monitors = hypr::monitors();
    if cfg.behavior.monitors == config::Monitors::Focused {
        monitors.retain(|m| m.focused);
    }
    if monitors.is_empty() {
        eprintln!("echodock: no Hyprland monitors (is HYPRLAND_INSTANCE_SIGNATURE set?)");
        std::process::exit(1);
    }
    let light = light_theme();
    rune::run_windows_with(RunOptions { accessory: true }, move |_| {
        let mut app = App::new(AppSpec::new("echodock", (1.0, 1.0), || Root, root_view)).windowless();
        let mut overlays = Vec::new();
        let height = surface_height(&cfg);
        for m in &monitors {
            let (w, _) = m.logical_size();
            let options = OverlayOptions {
                output_name: Some(m.name.clone()),
                wayland_layer: if cfg.behavior.reveal_over_fullscreen { WaylandLayer::Overlay } else { WaylandLayer::Top },
                input: OverlayInput::Regions,
                placement: Some(OverlayPlacement::Edge { edge: ScreenEdge::Bottom, margin: 0.0, align: rune::Align::Center, inset: 0.0 }),
                ..Default::default()
            };
            let backdrop = glass::Backdrop::default();
            let state = Dock::new(m.name.clone(), (w, height), m.scale as f32, cfg.clone(), light, tx.clone(), backdrop.clone());
            let spec = OverlaySpec::new("echodock", (w, height), dock::view).options(options)
                .setup(dock::setup).poll(dock::poll).compositor(glass::Glass::default()).srgb_blend(true);
            let mut state = state;
            state.screen_h = m.logical_size().1;
            let handle = app.overlay(spec, state);
            handle.show();
            let live = live::spawn(m.name.clone(), handle.clone(), backdrop);
            handle.update(move |s, _| s.live = Some(live));
            overlays.push(Overlay { monitor: m.name.clone(), handle });
        }
        brain::spawn(cfg.clone(), overlays, tx.clone(), rx);
        {
            let tx = tx.clone();
            config::watch(move |c| {
                let _ = tx.send(Msg::Config(c));
            });
        }
        let names: Vec<String> = monitors.iter().map(|m| m.name.clone()).collect();
        hypr::listen(move |name, _data| match name {
            "monitoradded" | "monitoraddedv2" | "monitorremoved" | "monitorremovedv2" => {
                let now: Vec<String> = hypr::monitors().into_iter().map(|m| m.name).collect();
                if now != names { brain::restart(); }
            }
            "openwindow" | "closewindow" | "movewindow" | "movewindowv2" | "activewindowv2" | "workspacev2" | "focusedmonv2"
            | "fullscreen" | "changefloatingmode" | "windowtitlev2" | "configreloaded" | "moveworkspacev2" | "destroyworkspacev2" => {
                let _ = tx.send(Msg::Refresh);
            }
            _ => {}
        });
        vec![WindowSpec::from_app("echodock", app)]
    });
}

//! `~/.config/echodock/config.toml`: everything the dock does, each feature behind a switch.
//! The daemon re-reads the file when it changes, so the settings app (or a text editor) applies
//! changes live.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub appearance: Appearance,
    pub glass: Glass,
    pub behavior: Behavior,
    pub items: Items,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Indicator {
    Dot,
    Bar,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    Auto,
    Dark,
    Light,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// Resting icon size in points.
    pub icon_size: f32,
    /// macOS-style magnification wave under the pointer.
    pub magnification: bool,
    /// Icon size at the pointer when magnifying, in points.
    pub magnified_size: f32,
    /// How many neighbouring icons the wave reaches on each side.
    pub magnification_reach: f32,
    /// Gap between icons in points.
    pub spacing: f32,
    /// Padding inside the dock around the icons.
    pub padding: f32,
    /// Distance between the dock and the screen edge.
    pub margin: f32,
    /// Corner radius of the dock; capped at half its height.
    pub corner_radius: f32,
    pub indicator: Indicator,
    /// Faded icons for pinned apps that are not running.
    pub dim_idle: bool,
    /// App name above the hovered icon.
    pub labels: bool,
    /// Divider between pinned and running apps.
    pub separator: bool,
    pub theme: Theme,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Glass {
    /// Refract a capture of the screen behind the dock (real Liquid Glass). Off: tinted frost only.
    pub refraction: bool,
    /// Blur of the backdrop, 0 (clear) to 1 (frosted).
    pub frost: f32,
    /// Bevel width in points: how far in from the rim light bends.
    pub bevel: f32,
    /// Lens depth: how strongly the rim bends light.
    pub depth: f32,
    /// Colour split at the rim.
    pub dispersion: f32,
    /// Tint over the glass, 0 to 1.
    pub tint: f32,
    /// Rim highlight strength, 0 to 1.
    pub rim: f32,
    /// Drop shadow strength, 0 to 1.
    pub shadow: f32,
    /// How often the glass re-reads what is behind it while the dock is up (0: once, as it shows).
    pub live_fps: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Visibility {
    /// Hidden until the pointer reaches the screen edge.
    AutoHide,
    /// Always on screen.
    Always,
    /// Always on screen, except while a window overlaps it.
    Dodge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WindowScope {
    /// Apps running on any workspace (macOS).
    All,
    /// Only apps with a window on the monitor's current workspace.
    Workspace,
    /// Only apps with a window on this monitor.
    Monitor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Monitors {
    All,
    Focused,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Behavior {
    pub visibility: Visibility,
    /// How long the pointer rests on the edge before the dock comes up, in milliseconds.
    pub reveal_delay_ms: u64,
    /// How long after the pointer leaves before the dock goes, in milliseconds.
    pub hide_delay_ms: u64,
    /// Thickness of the invisible strip along the edge that reveals the dock, in points.
    pub edge_size: f32,
    /// Show up over fullscreen windows when the pointer reaches the edge.
    pub reveal_over_fullscreen: bool,
    /// Bounce an app's icon while it launches.
    pub bounce: bool,
    /// Clicking a running app's icon again cycles through its windows.
    pub click_cycles: bool,
    /// Clicking the focused app's only window minimizes it (Windows taskbar).
    pub click_minimizes: bool,
    /// Which running apps the dock lists.
    pub scope: WindowScope,
    pub monitors: Monitors,
    /// Show running apps that are not pinned.
    pub show_running: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Items {
    /// Pinned desktop ids, in dock order.
    pub pinned: Vec<String>,
    /// The "all apps" grid button at the start of the dock.
    pub apps_button: bool,
    /// Trash at the end of the dock.
    pub trash: bool,
    /// What the apps button runs.
    pub apps_command: String,
    /// What the trash runs.
    pub trash_command: String,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            icon_size: 48.0,
            magnification: true,
            magnified_size: 80.0,
            magnification_reach: 2.5,
            spacing: 6.0,
            padding: 9.0,
            margin: 8.0,
            corner_radius: 24.0,
            indicator: Indicator::Dot,
            dim_idle: false,
            labels: true,
            separator: true,
            theme: Theme::Auto,
        }
    }
}

impl Default for Glass {
    fn default() -> Self {
        Glass { refraction: true, frost: 0.0, bevel: 22.0, depth: 34.0, dispersion: 0.12, tint: 0.0, rim: 1.0, shadow: 0.5, live_fps: 30.0 }
    }
}

impl Default for Behavior {
    fn default() -> Self {
        Behavior {
            visibility: Visibility::AutoHide,
            reveal_delay_ms: 120,
            hide_delay_ms: 450,
            edge_size: 3.0,
            reveal_over_fullscreen: false,
            bounce: true,
            click_cycles: true,
            click_minimizes: false,
            scope: WindowScope::All,
            monitors: Monitors::All,
            show_running: true,
        }
    }
}

impl Default for Items {
    fn default() -> Self {
        Items {
            pinned: Vec::new(),
            apps_button: true,
            trash: false,
            // Omarchy's own app grid lives in its dock, which EchoDock replaces.
            apps_command: if crate::apps::which("echospot") { "echospot toggle" } else { "omarchy-menu toggle" }.into(),
            trash_command: "xdg-open trash:///".into(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config { appearance: Appearance::default(), glass: Glass::default(), behavior: Behavior::default(), items: Items::default() }
    }
}

pub fn dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"))
        .join("echodock")
}

pub fn path() -> PathBuf {
    dir().join("config.toml")
}

impl Config {
    /// The saved config, or defaults (with Omarchy's pins carried over) when there is none or
    /// it doesn't parse.
    pub fn load() -> Config {
        match std::fs::read_to_string(path()) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(c) => c.sanitized(),
                Err(e) => {
                    eprintln!("echodock: {}: {e}", path().display());
                    Config::default()
                }
            },
            Err(_) => {
                let mut c = Config::default();
                c.items.pinned = omarchy_pins();
                c
            }
        }
    }

    /// Writes atomically, so the daemon never reads half a file.
    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(dir())?;
        // f32 fields print as f64 (0.35 → 0.3499999940395355); round them back for people.
        let mut value = toml::Value::try_from(self).map_err(std::io::Error::other)?;
        round_floats(&mut value);
        let text = toml::to_string_pretty(&value).map_err(std::io::Error::other)?;
        let tmp = dir().join(".config.toml.tmp");
        std::fs::write(&tmp, format!("# echodock settings. Edit here or with echodock-settings; changes apply live.\n\n{text}"))?;
        std::fs::rename(tmp, path())
    }

    pub fn sanitized(mut self) -> Config {
        let a = &mut self.appearance;
        a.icon_size = a.icon_size.clamp(16.0, 128.0);
        a.magnified_size = a.magnified_size.clamp(a.icon_size, 192.0);
        a.magnification_reach = a.magnification_reach.clamp(1.0, 6.0);
        a.spacing = a.spacing.clamp(0.0, 32.0);
        a.padding = a.padding.clamp(0.0, 32.0);
        a.margin = a.margin.clamp(0.0, 64.0);
        a.corner_radius = a.corner_radius.clamp(0.0, 96.0);
        let g = &mut self.glass;
        for v in [&mut g.frost, &mut g.tint, &mut g.rim, &mut g.shadow, &mut g.dispersion] {
            *v = v.clamp(0.0, 1.0);
        }
        g.bevel = g.bevel.clamp(0.0, 40.0);
        g.depth = g.depth.clamp(0.0, 40.0);
        g.live_fps = g.live_fps.clamp(0.0, 60.0);
        let b = &mut self.behavior;
        b.reveal_delay_ms = b.reveal_delay_ms.min(3000);
        b.hide_delay_ms = b.hide_delay_ms.min(5000);
        b.edge_size = b.edge_size.clamp(1.0, 24.0);
        let mut seen = Vec::new();
        self.items.pinned.retain(|id| !id.is_empty() && !seen.contains(id) && { seen.push(id.clone()); true });
        self
    }
}

fn round_floats(v: &mut toml::Value) {
    match v {
        toml::Value::Float(f) => *f = (*f * 1000.0).round() / 1000.0,
        toml::Value::Table(t) => t.iter_mut().for_each(|(_, v)| round_floats(v)),
        toml::Value::Array(a) => a.iter_mut().for_each(round_floats),
        _ => {}
    }
}

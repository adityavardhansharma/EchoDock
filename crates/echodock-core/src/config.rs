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

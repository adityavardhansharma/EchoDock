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

//! The live backdrop. A screen capture would include the dock itself, so instead EchoDock builds
//! what is behind it: the wallpaper, then every window on the monitor's current workspace that
//! reaches the band behind the dock, each captured on its own through Hyprland's toplevel export
//! and laid down in stacking order. It runs only while the dock is on screen, so the glass
//! follows workspace switches, moving windows and playing video, and costs nothing when hidden.

use std::os::fd::AsFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use echodock_core::hypr::{self, Client, Monitor};
use rune::OverlayHandle;
use wayland_client::protocol::{wl_buffer::WlBuffer, wl_registry::WlRegistry, wl_shm, wl_shm::WlShm, wl_shm_pool::WlShmPool};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop, globals::{GlobalListContents, registry_queue_init}};

use crate::dock::Dock;
use crate::glass::Backdrop;

#[allow(unused_imports, dead_code, non_camel_case_types, non_snake_case, clippy::all)]
mod export {
    use wayland_client;
    use wayland_client::protocol::*;
    use wayland_protocols_wlr::foreign_toplevel::v1::client::*;
    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        use wayland_protocols_wlr::foreign_toplevel::v1::client::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/hyprland-toplevel-export-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("protocols/hyprland-toplevel-export-v1.xml");
}
use export::hyprland_toplevel_export_frame_v1::{self as frame, HyprlandToplevelExportFrameV1 as Frame};
use export::hyprland_toplevel_export_manager_v1::HyprlandToplevelExportManagerV1 as Export;

/// Shared with the dock: whether it is on screen, and where the band behind it is.
#[derive(Clone)]
pub struct Live {
    inner: Arc<(Mutex<LiveState>, Condvar)>,
    pub available: Arc<AtomicBool>,
}

#[derive(Default)]
struct LiveState {
    active: bool,
    band: [f32; 4],
    screen: [f32; 4],
    fps: f32,
}

impl Live {
    /// Starts or stops following; `band` is in surface points.
    pub fn set(&self, active: bool, band: [f32; 4], screen: [f32; 4], fps: f32) {
        let (m, cv) = &*self.inner;
        let mut s = m.lock().unwrap();
        let wake = active && !s.active;
        s.active = active;
        s.band = band;
        s.screen = screen;
        s.fps = fps;
        if wake { cv.notify_all(); }
    }
}

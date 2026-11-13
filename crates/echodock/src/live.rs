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

/// One shm buffer, reused while the window keeps its size.
struct Shm {
    buffer: WlBuffer,
    map: *mut u8,
    len: usize,
    key: (u32, u32, u32, u32),
}

unsafe impl Send for Shm {}

impl Drop for Shm {
    fn drop(&mut self) {
        self.buffer.destroy();
        unsafe { let _ = rustix::mm::munmap(self.map.cast(), self.len); }
    }
}

#[derive(Default)]
struct Grab {
    offer: Option<(wl_shm::Format, u32, u32, u32)>,
    buffer_done: bool,
    inverted: bool,
    ready: bool,
    failed: bool,
}

struct State {
    grab: Grab,
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &WlRegistry, _: wayland_client::protocol::wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: ignore Export);

impl Dispatch<Frame, ()> for State {
    fn event(s: &mut Self, f: &Frame, event: frame::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            frame::Event::Buffer { format: WEnum::Value(format), width, height, stride } => {
                if s.grab.offer.is_none() { s.grab.offer = Some((format, width, height, stride)); }
            }
            frame::Event::BufferDone => s.grab.buffer_done = true,
            frame::Event::Flags { flags: WEnum::Value(flags) } => s.grab.inverted = flags.contains(frame::Flags::YInvert),
            frame::Event::Ready { .. } => { s.grab.ready = true; f.destroy(); }
            frame::Event::Failed => { s.grab.failed = true; f.destroy(); }
            _ => {}
        }
    }
}

struct Wallpaper {
    path: PathBuf,
    size: (u32, u32),
    rgba: Vec<u8>,
    checked: Instant,
}

fn wallpaper_path() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    std::fs::canonicalize(home.join(".local/state/omarchy/current/background")).ok()
}

/// The wallpaper scaled to cover the monitor, as Omarchy draws it.
fn load_wallpaper(path: &PathBuf, w: u32, h: u32) -> Option<Vec<u8>> {
    let img = image::open(path).ok()?;
    Some(img.resize_to_fill(w, h, image::imageops::FilterType::Triangle).to_rgba8().into_raw())
}

pub fn spawn(monitor: String, handle: OverlayHandle<Dock>, backdrop: Backdrop) -> Live {
    let live = Live { inner: Arc::new((Mutex::new(LiveState::default()), Condvar::new())), available: Arc::new(AtomicBool::new(true)) };
    let me = live.clone();
    std::thread::Builder::new().name(format!("echodock-live-{monitor}")).spawn(move || {
        if let Err(e) = run(&monitor, &handle, &backdrop, &me) {
            eprintln!("echodock: live glass unavailable ({e}); using still captures");
            me.available.store(false, Ordering::Relaxed);
        }
    }).expect("live backdrop thread");
    live
}

fn run(monitor: &str, handle: &OverlayHandle<Dock>, backdrop: &Backdrop, live: &Live) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
    let qh = queue.handle();
    let shm: WlShm = globals.bind(&qh, 1..=1, ())?;
    let export: Export = globals.bind(&qh, 1..=2, ()).map_err(|_| "Hyprland has no toplevel export")?;
    let mut state = State { grab: Grab::default() };
    let mut buffers: std::collections::HashMap<String, Shm> = std::collections::HashMap::new();
    let mut wall: Option<Wallpaper> = None;
    let mut band_rgba: Vec<u8> = Vec::new();
    loop {
        // Sleep until the dock shows.
        let (band, screen, fps) = {
            let (m, cv) = &*live.inner;
            let mut s = m.lock().unwrap();
            while !s.active { s = cv.wait(s).unwrap(); }
            (s.band, s.screen, s.fps.max(0.0001))
        };
        let started = Instant::now();
        let Some(m) = hypr::monitors().into_iter().find(|x| x.name == monitor) else {
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };
        let scale = m.scale as f32;
        let (mw, mh) = (m.width as u32, m.height as u32);
        // Wallpaper, reloaded when Omarchy switches it.
        let stale = wall.as_ref().is_none_or(|w| w.size != (mw, mh) || (w.checked.elapsed() > Duration::from_secs(2) && wallpaper_path().as_ref() != Some(&w.path)));
        if stale {
            if let Some(p) = wallpaper_path() && let Some(rgba) = load_wallpaper(&p, mw, mh) {
                wall = Some(Wallpaper { path: p, size: (mw, mh), rgba, checked: Instant::now() });
            }
        } else if let Some(w) = &mut wall && w.checked.elapsed() > Duration::from_secs(2) {
            w.checked = Instant::now();
        }
        // Where the band is on the monitor, in pixels.
        let by = (screen[1] * scale).round().max(0.0) as u32;
        let bh = ((screen[3] * scale).round() as u32).min(mh.saturating_sub(by));
        let bw = mw;
        band_rgba.resize((bw * bh * 4) as usize, 0);
        match &wall {
            Some(w) => band_rgba.copy_from_slice(&w.rgba[(by * mw * 4) as usize..((by + bh) * mw * 4) as usize]),
            None => band_rgba.fill(0),
        }
        // Windows behind the band, bottom to top.
        let band_top = m.y as f32 + screen[1];
        let band_bottom = band_top + screen[3];
        let mut wins: Vec<Client> = hypr::clients().into_iter().filter(|c| {
            let on = c.workspace.id == m.active_workspace.id || (m.special_workspace.id != 0 && c.workspace.id == m.special_workspace.id);
            let y0 = c.at[1] as f32;
            on && !c.hidden && y0 < band_bottom && y0 + c.size[1] as f32 > band_top
        }).collect();
        wins.sort_by_key(|c| (c.workspace.id < 0, c.fullscreen > 0, c.floating, -c.focus_history_id));
        debug(&format!("ws {} band {:?}: {} windows", m.active_workspace.id, screen, wins.len()));
        let mut seen = Vec::new();
        for c in &wins {
            seen.push(c.address.clone());
            if let Some((px, fmt, stride, inverted)) = grab(&conn, &mut queue, &qh, &shm, &export, &mut state, &mut buffers, c)? {
                composite(&mut band_rgba, bw, bh, by, &m, c, px, fmt, stride, inverted, scale);
            }
        }
        buffers.retain(|k, _| seen.contains(k));
        backdrop.set(band, bw, bh, band_rgba.clone());
        handle.update(|_, tx| tx.wake_after(Duration::ZERO));
        let frame = Duration::from_secs_f32(1.0 / fps);
        if let Some(left) = frame.checked_sub(started.elapsed()) { std::thread::sleep(left); }
    }
}

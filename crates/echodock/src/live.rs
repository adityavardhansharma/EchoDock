//! The live backdrop. A screen capture would include the dock itself, so instead EchoDock builds
//! what is behind it: the wallpaper, then every window on the monitor's current workspace that
//! reaches the band behind the dock, each captured on its own through Hyprland's toplevel export
//! and laid down in stacking order. It runs only while the dock is on screen, so the glass
//! follows workspace switches, moving windows and playing video, and costs nothing when hidden.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use echodock_core::hypr::{self, Client, Monitor};
use rune::OverlayHandle;
use crate::dock::Dock;
use crate::export::{Capturer, Shot};
use crate::glass::Backdrop;

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
    let mut cap = Capturer::connect()?;
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
            if let Some(shot) = cap.grab(&c.address)? {
                composite(&mut band_rgba, bw, bh, by, &m, c, &shot, scale);
            }
        }
        cap.retain(&seen);
        backdrop.set(band, bw, bh, band_rgba.clone());
        handle.update(|_, tx| tx.wake_after(Duration::ZERO));
        let frame = Duration::from_secs_f32(1.0 / fps);
        if let Some(left) = frame.checked_sub(started.elapsed()) { std::thread::sleep(left); }
    }
}

fn debug(msg: &str) {
    if std::env::var_os("ECHODOCK_DEBUG").is_some() { eprintln!("echodock live: {msg}"); }
}

/// Lays a window's pixels over the band (premultiplied source-over), only the rows it covers.
fn composite(dst: &mut [u8], bw: u32, bh: u32, by: u32, m: &Monitor, c: &Client, shot: &Shot, scale: f32) {
    let Some((ri, bi, has_alpha)) = shot.layout() else { return };
    let (px, fw, fh) = (shot.px, shot.width, shot.height);
    // Where the window lands, in monitor pixels.
    let x0 = ((c.at[0] as f32 - m.x as f32) * scale).round() as i64;
    let y0 = ((c.at[1] as f32 - m.y as f32) * scale).round() as i64;
    let ww = ((c.size[0] as f32) * scale).round().max(1.0) as i64;
    let wh = ((c.size[1] as f32) * scale).round().max(1.0) as i64;
    for ty in 0..bh as i64 {
        let my = by as i64 + ty;
        let wy = my - y0;
        if wy < 0 || wy >= wh { continue; }
        let src_row = shot.row((wy * fh as i64 / wh) as u32);
        let dst_row = (ty as u32 * bw * 4) as usize;
        let xa = x0.max(0);
        let xb = (x0 + ww).min(bw as i64);
        for mx in xa..xb {
            let sx = ((mx - x0) * fw as i64 / ww) as usize;
            let s = src_row + sx * 4;
            let d = dst_row + mx as usize * 4;
            if s + 3 >= px.len() { break; }
            let a = if has_alpha { px[s + 3] as u32 } else { 255 };
            if a == 255 {
                dst[d] = px[s + ri];
                dst[d + 1] = px[s + 1];
                dst[d + 2] = px[s + bi];
            } else if a > 0 {
                let inv = 255 - a;
                dst[d] = (px[s + ri] as u32 + dst[d] as u32 * inv / 255).min(255) as u8;
                dst[d + 1] = (px[s + 1] as u32 + dst[d + 1] as u32 * inv / 255).min(255) as u8;
                dst[d + 2] = (px[s + bi] as u32 + dst[d + 2] as u32 * inv / 255).min(255) as u8;
            }
            dst[d + 3] = 255;
        }
    }
}

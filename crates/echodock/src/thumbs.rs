//! Window previews. While the dock shows an app's windows, this thread captures each one
//! through the toplevel export (so windows on other workspaces and behind others come out whole),
//! shrinks it to preview size and hands it to the dock, then does it again about once a second
//! so the previews stay current. It sleeps while no previews are open.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rune::OverlayHandle;

use crate::dock::Dock;
use crate::export::{Capturer, Shot};

/// How often open previews are captured again.
const REFRESH: Duration = Duration::from_millis(1000);

#[derive(Default)]
struct Want {
    /// Windows to capture, by address. Empty: nothing open, sleep.
    windows: Vec<String>,
    /// The largest preview size in pixels, width and height.
    max: (u32, u32),
    /// Bumped on every change, so a pass in progress starts over.
    generation: u64,
}

/// Shared with the dock: which windows it wants previews of.
#[derive(Clone)]
pub struct Thumbs {
    inner: Arc<(Mutex<Want>, Condvar)>,
}

impl Thumbs {
    /// Asks for previews of these windows, at most `max` pixels; an empty list stops capturing.
    pub fn want(&self, windows: Vec<String>, max: (u32, u32)) {
        let (m, cv) = &*self.inner;
        let mut w = m.lock().unwrap();
        if w.windows == windows && w.max == max { return; }
        w.windows = windows;
        w.max = max;
        w.generation += 1;
        cv.notify_all();
    }
}

pub fn spawn(monitor: &str, handle: OverlayHandle<Dock>) -> Thumbs {
    let thumbs = Thumbs { inner: Arc::new((Mutex::new(Want::default()), Condvar::new())) };
    let me = thumbs.clone();
    std::thread::Builder::new().name(format!("echodock-thumbs-{monitor}")).spawn(move || {
        if let Err(e) = run(&handle, &me) {
            eprintln!("echodock: window previews unavailable ({e}); showing icons instead");
        }
    }).expect("thumbnail thread");
    thumbs
}

fn run(handle: &OverlayHandle<Dock>, thumbs: &Thumbs) -> Result<(), Box<dyn std::error::Error>> {
    let mut cap = Capturer::connect()?;
    let (m, cv) = &*thumbs.inner;
    let mut done_generation = u64::MAX;
    let mut last_pass = Instant::now();
    loop {
        let (windows, max, generation) = {
            let mut w = m.lock().unwrap();
            loop {
                if !w.windows.is_empty() {
                    // A new request goes now; the same one again once a refresh is due.
                    if w.generation != done_generation { break; }
                    let left = REFRESH.saturating_sub(last_pass.elapsed());
                    if left.is_zero() { break; }
                    w = cv.wait_timeout(w, left).unwrap().0;
                } else {
                    cap.retain(&[]);
                    w = cv.wait(w).unwrap();
                }
            }
            (w.windows.clone(), w.max, w.generation)
        };
        last_pass = Instant::now();
        for address in &windows {
            // Stop early if the dock wants something else now.
            if m.lock().unwrap().generation != generation { break; }
            let Some(shot) = cap.grab(address)? else { continue };
            let Some((w, h, rgba)) = shrink(&shot, max) else { continue };
            let address = address.clone();
            handle.update(move |s, tx| s.set_thumb(address, w, h, rgba, tx));
        }
        cap.retain(&windows);
        done_generation = generation;
    }
}

/// Fits a capture inside `max` pixels, keeping its shape, as straight-alpha RGBA. Each output
/// pixel averages a grid of up to 4×4 source pixels, enough to keep text from shimmering.
pub fn shrink(shot: &Shot, max: (u32, u32)) -> Option<(u32, u32, Vec<u8>)> {
    let (ri, bi, has_alpha) = shot.layout()?;
    let (sw, sh) = (shot.width, shot.height);
    if sw == 0 || sh == 0 { return None; }
    let k = (max.0 as f32 / sw as f32).min(max.1 as f32 / sh as f32).min(1.0);
    let (w, h) = (((sw as f32 * k).round() as u32).max(1), ((sh as f32 * k).round() as u32).max(1));
    let mut out = vec![0u8; (w * h * 4) as usize];
    let taps = |n: u32, of: u32| ((of as f32 / n as f32).ceil() as u32).clamp(1, 4);
    let (tx, ty) = (taps(w, sw), taps(h, sh));
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0u32; 4];
            for j in 0..ty {
                let sy = (((y as f32 + (j as f32 + 0.5) / ty as f32) / h as f32 * sh as f32) as u32).min(sh - 1);
                let row = shot.row(sy);
                for i in 0..tx {
                    let sx = (((x as f32 + (i as f32 + 0.5) / tx as f32) / w as f32 * sw as f32) as u32).min(sw - 1);
                    let p = row + sx as usize * 4;
                    let Some(px) = shot.px.get(p..p + 4) else { continue };
                    acc[0] += px[ri] as u32;
                    acc[1] += px[1] as u32;
                    acc[2] += px[bi] as u32;
                    acc[3] += if has_alpha { px[3] as u32 } else { 255 };
                }
            }
            let n = tx * ty;
            let a = acc[3] / n;
            let o = ((y * w + x) * 4) as usize;
            // Wayland's shm pixels are premultiplied; the image cache wants straight alpha.
            for c in 0..3 {
                let v = acc[c] / n;
                out[o + c] = if a == 0 { 0 } else { (v * 255 / a).min(255) as u8 };
            }
            out[o + 3] = a as u8;
        }
    }
    Some((w, h, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wayland_client::protocol::wl_shm;

    #[test]
    fn shrinks_to_fit_and_keeps_colour() {
        // 8×4 opaque red in XRGB (bytes B, G, R, X).
        let px: Vec<u8> = (0..32).flat_map(|_| [0u8, 0, 255, 0]).collect();
        let shot = Shot { px: &px, format: wl_shm::Format::Xrgb8888, width: 8, height: 4, stride: 32, inverted: false };
        let (w, h, rgba) = shrink(&shot, (4, 4)).unwrap();
        assert_eq!((w, h), (4, 2));
        assert_eq!(&rgba[..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn small_windows_are_not_enlarged() {
        let px = vec![255u8; 2 * 2 * 4];
        let shot = Shot { px: &px, format: wl_shm::Format::Argb8888, width: 2, height: 2, stride: 8, inverted: false };
        let (w, h, _) = shrink(&shot, (100, 100)).unwrap();
        assert_eq!((w, h), (2, 2));
    }
}

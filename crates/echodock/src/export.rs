//! Window capture through Hyprland's toplevel export: one window's pixels at a time, without
//! the dock in them, wherever the window is (another workspace, behind other windows). The live
//! glass and the window previews each keep their own `Capturer` on their own thread.

use std::collections::HashMap;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use wayland_client::protocol::{wl_buffer::WlBuffer, wl_registry::WlRegistry, wl_shm, wl_shm::WlShm, wl_shm_pool::WlShmPool};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop, globals::{GlobalListContents, registry_queue_init}};

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

pub struct State {
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

/// A captured window: its pixels as the compositor gave them.
pub struct Shot<'a> {
    pub px: &'a [u8],
    pub format: wl_shm::Format,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub inverted: bool,
}

impl Shot<'_> {
    /// Byte offsets of red and blue in a pixel, and whether the fourth byte is alpha; None for
    /// a format the dock doesn't read.
    pub fn layout(&self) -> Option<(usize, usize, bool)> {
        match self.format {
            wl_shm::Format::Argb8888 => Some((2, 0, true)),
            wl_shm::Format::Xrgb8888 => Some((2, 0, false)),
            wl_shm::Format::Abgr8888 => Some((0, 2, true)),
            wl_shm::Format::Xbgr8888 => Some((0, 2, false)),
            _ => None,
        }
    }

    /// The row of the image `y` rows from its top, whichever way up the buffer is.
    pub fn row(&self, y: u32) -> usize {
        let y = if self.inverted { self.height - 1 - y } else { y };
        (y * self.stride) as usize
    }
}

/// A Wayland connection that can capture windows, with a buffer kept per window.
pub struct Capturer {
    conn: Connection,
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    shm: WlShm,
    export: Export,
    state: State,
    buffers: HashMap<String, Shm>,
}

impl Capturer {
    pub fn connect() -> Result<Capturer, Box<dyn std::error::Error>> {
        let conn = Connection::connect_to_env()?;
        let (globals, queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();
        let shm: WlShm = globals.bind(&qh, 1..=1, ())?;
        let export: Export = globals.bind(&qh, 1..=2, ()).map_err(|_| "Hyprland has no toplevel export")?;
        Ok(Capturer { conn, queue, qh, shm, export, state: State { grab: Grab::default() }, buffers: HashMap::new() })
    }

    /// Drops the buffers of windows not in `keep`.
    pub fn retain(&mut self, keep: &[String]) {
        self.buffers.retain(|k, _| keep.contains(k));
    }

    /// Captures one window by its Hyprland address; the pixels stay in its shm buffer. Ok(None)
    /// when this window can't be captured right now.
    pub fn grab(&mut self, address: &str) -> Result<Option<Shot<'_>>, Box<dyn std::error::Error>> {
        let addr = u64::from_str_radix(address.trim_start_matches("0x"), 16).unwrap_or(0);
        self.state.grab = Grab::default();
        let f = self.export.capture_toplevel(0, addr as u32, &self.qh, ());
        let deadline = Instant::now() + Duration::from_millis(400);
        while !(self.state.grab.buffer_done || self.state.grab.failed) {
            if Instant::now() > deadline { f.destroy(); debug(&format!("{address}: no buffer offer")); return Ok(None); }
            self.conn.flush()?;
            self.queue.blocking_dispatch(&mut self.state)?;
        }
        let Some((format, w, h, stride)) = self.state.grab.offer.filter(|_| !self.state.grab.failed) else {
            debug(&format!("{address}: failed before copy (offer {:?})", self.state.grab.offer));
            return Ok(None);
        };
        let key = (format as u32, w, h, stride);
        if self.buffers.get(address).is_none_or(|b| b.key != key) {
            let len = (stride * h) as usize;
            let fd = rustix::fs::memfd_create("echodock-capture", rustix::fs::MemfdFlags::CLOEXEC)?;
            rustix::fs::ftruncate(&fd, len as u64)?;
            let map = unsafe { rustix::mm::mmap(std::ptr::null_mut(), len, rustix::mm::ProtFlags::READ | rustix::mm::ProtFlags::WRITE, rustix::mm::MapFlags::SHARED, &fd, 0)? };
            let pool = self.shm.create_pool(fd.as_fd(), len as i32, &self.qh, ());
            let buffer = pool.create_buffer(0, w as i32, h as i32, stride as i32, format, &self.qh, ());
            pool.destroy();
            self.buffers.insert(address.to_owned(), Shm { buffer, map: map.cast(), len, key });
        }
        let b = &self.buffers[address];
        f.copy(&b.buffer, 1);
        while !(self.state.grab.ready || self.state.grab.failed) {
            if Instant::now() > deadline { return Ok(None); }
            self.conn.flush()?;
            self.queue.blocking_dispatch(&mut self.state)?;
        }
        if self.state.grab.failed { debug(&format!("{address}: copy failed")); return Ok(None); }
        debug(&format!("{address}: {w}x{h} {format:?}"));
        let px = unsafe { std::slice::from_raw_parts(b.map as *const u8, b.len) };
        Ok(Some(Shot { px, format, width: w, height: h, stride, inverted: self.state.grab.inverted }))
    }
}

pub fn debug(msg: &str) {
    if std::env::var_os("ECHODOCK_DEBUG").is_some() { eprintln!("echodock capture: {msg}"); }
}

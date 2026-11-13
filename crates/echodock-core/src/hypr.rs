//! Hyprland over its sockets: JSON reads on `.socket.sock`, the event stream on `.socket2.sock`,
//! and dispatches through `hyprctl` (this Hyprland takes Lua dispatcher expressions).

use serde::Deserialize;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct WorkspaceRef {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Client {
    pub address: String,
    #[serde(default)]
    pub mapped: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub at: [i32; 2],
    #[serde(default)]
    pub size: [i32; 2],
    #[serde(default)]
    pub workspace: WorkspaceRef,
    #[serde(default)]
    pub floating: bool,
    #[serde(default)]
    pub monitor: i64,
    #[serde(default)]
    pub class: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub initial_class: String,
    #[serde(default)]
    pub pid: i64,
    /// 0 none, 1 maximized, 2 fullscreen, 3 both.
    #[serde(default)]
    pub fullscreen: i64,
    #[serde(default, rename = "focusHistoryID")]
    pub focus_history_id: i64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Monitor {
    pub id: i64,
    pub name: String,
    pub width: i64,
    pub height: i64,
    #[serde(default)]
    pub x: i64,
    #[serde(default)]
    pub y: i64,
    #[serde(default = "one")]
    pub scale: f64,
    #[serde(default)]
    pub transform: i64,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub active_workspace: WorkspaceRef,
    #[serde(default)]
    pub special_workspace: WorkspaceRef,
    /// Space other layers reserve: left, top, right, bottom.
    #[serde(default)]
    pub reserved: [i64; 4],
    #[serde(default)]
    pub disabled: bool,
}

fn one() -> f64 {
    1.0
}

impl Monitor {
    /// Size in logical points (what layer surfaces are laid out in).
    pub fn logical_size(&self) -> (f32, f32) {
        let (w, h) = if self.transform % 2 == 1 { (self.height, self.width) } else { (self.width, self.height) };
        ((w as f64 / self.scale).round() as f32, (h as f64 / self.scale).round() as f32)
    }
}

/// Where minimized windows wait (shared with Omarchy's `Super+M`).
pub const MINIMIZED: &str = "special:minimized";

fn socket_dir() -> Option<PathBuf> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let dir = PathBuf::from(runtime).join("hypr").join(&sig);
    if dir.exists() { Some(dir) } else { Some(PathBuf::from("/tmp/hypr").join(sig)) }
}

/// One request on the command socket, its reply as text.
pub fn request(cmd: &str) -> Option<String> {
    let mut s = UnixStream::connect(socket_dir()?.join(".socket.sock")).ok()?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(2))).ok();
    s.write_all(cmd.as_bytes()).ok()?;
    let mut out = String::new();
    s.read_to_string(&mut out).ok()?;
    Some(out)
}

fn json<T: for<'de> Deserialize<'de>>(what: &str) -> Option<T> {
    let text = request(&format!("j/{what}"))?;
    match serde_json::from_str(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("echodock: hyprland {what}: {e}");
            None
        }
    }
}

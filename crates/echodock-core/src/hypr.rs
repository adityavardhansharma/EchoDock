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

pub fn clients() -> Vec<Client> {
    json::<Vec<Client>>("clients").unwrap_or_default().into_iter().filter(|c| c.mapped).collect()
}

pub fn monitors() -> Vec<Monitor> {
    json::<Vec<Monitor>>("monitors").unwrap_or_default().into_iter().filter(|m| !m.disabled).collect()
}

pub fn active_address() -> Option<String> {
    #[derive(Deserialize)]
    struct Active {
        #[serde(default)]
        address: String,
    }
    json::<Active>("activewindow").map(|a| a.address).filter(|a| !a.is_empty())
}

/// The pointer in global logical coordinates.
pub fn cursor() -> Option<(f64, f64)> {
    #[derive(Deserialize)]
    struct Pos {
        x: f64,
        y: f64,
    }
    json::<Pos>("cursorpos").map(|p| (p.x, p.y))
}

/// Runs Lua dispatcher expressions in order, stopping at the first failure.
pub fn dispatch(steps: &[String]) {
    for step in steps {
        let ok = Command::new("hyprctl").arg("dispatch").arg(step).output().map(|o| o.status.success()).unwrap_or(false);
        if !ok {
            eprintln!("echodock: dispatch failed: {step}");
            return;
        }
    }
}

fn addr(address: &str) -> String {
    let a = address.trim();
    if a.starts_with("0x") { a.to_owned() } else { format!("0x{a}") }
}

/// Focuses a window and raises it; a minimized one first comes back to `workspace`.
pub fn focus(address: &str, minimized: bool, workspace: Option<i64>) {
    let target = format!("window = \"address:{}\"", addr(address));
    let mut steps = Vec::new();
    if minimized && let Some(ws) = workspace {
        steps.push(format!("hl.dsp.window.move({{ workspace = \"{ws}\", {target} }})"));
    }
    steps.push(format!("hl.dsp.focus({{ {target} }})"));
    steps.push(format!("hl.dsp.window.alter_zorder({{ mode = \"top\", {target} }})"));
    dispatch(&steps);
}

/// Sends a window to the minimized workspace, as Omarchy's `Super+M` does.
pub fn minimize(address: &str, workspace: i64) {
    let target = format!("window = \"address:{}\"", addr(address));
    dispatch(&[
        format!("hl.dsp.window.tag({{ tag = \"+omarchy-minimized-from-{workspace}\", {target} }})"),
        format!("hl.dsp.window.move({{ workspace = \"{MINIMIZED}\", follow = false, {target} }})"),
        format!("hl.dsp.focus({{ workspace = \"{workspace}\" }})"),
    ]);
}

pub fn close(address: &str) {
    dispatch(&[format!("hl.dsp.window.close({{ window = \"address:{}\" }})", addr(address))]);
}

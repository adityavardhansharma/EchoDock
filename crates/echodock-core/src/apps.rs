//! Desktop entries (`.desktop` files), matching a window's class to the app that owns it, launching,
//! and icon theme lookup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Action {
    pub id: String,
    pub name: String,
    pub exec: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    /// Desktop id: the file name without `.desktop`.
    pub id: String,
    pub name: String,
    pub icon: String,
    pub exec: String,
    pub wm_class: String,
    pub terminal: bool,
    pub no_display: bool,
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, Default)]
pub struct Library {
    pub entries: Vec<Entry>,
    by_id: HashMap<String, usize>,
    /// Lower-cased class, id, wm class, last id segment, exec name → entry index.
    by_key: HashMap<String, usize>,
}

fn data_dirs() -> Vec<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let mut dirs = vec![std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"))];
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(sys.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    dirs.dedup();
    dirs
}

fn parse(path: &Path, id: String) -> Option<Entry> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut e = Entry { id, ..Entry::default() };
    let mut section = String::new();
    let mut actions: Vec<Action> = Vec::new();
    let mut listed: Vec<String> = Vec::new();
    let mut hidden = false;
    let mut is_app = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            section = line.trim_matches(|c| c == '[' || c == ']').to_owned();
            if let Some(a) = section.strip_prefix("Desktop Action ") {
                actions.push(Action { id: a.to_owned(), ..Action::default() });
            }
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if section == "Desktop Entry" {
            match k {
                "Name" => e.name = v.to_owned(),
                "Icon" => e.icon = v.to_owned(),
                "Exec" => e.exec = v.to_owned(),
                "StartupWMClass" => e.wm_class = v.to_owned(),
                "Terminal" => e.terminal = v.eq_ignore_ascii_case("true"),
                "NoDisplay" => e.no_display = v.eq_ignore_ascii_case("true"),
                "Hidden" => hidden = v.eq_ignore_ascii_case("true"),
                "Type" => is_app = v == "Application",
                "Actions" => listed = v.split(';').filter(|s| !s.is_empty()).map(str::to_owned).collect(),
                _ => {}
            }
        } else if section.starts_with("Desktop Action ") && let Some(a) = actions.last_mut() {
            match k {
                "Name" => a.name = v.to_owned(),
                "Exec" => a.exec = v.to_owned(),
                _ => {}
            }
        }
    }
    if !is_app || hidden || e.name.is_empty() {
        return None;
    }
    e.actions = actions.into_iter().filter(|a| listed.contains(&a.id) && !a.name.is_empty() && !a.exec.is_empty()).collect();
    Some(e)
}

/// Splits an Exec line into words, dropping field codes (`%f`, `%U`, ...).
pub fn exec_words(exec: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = exec.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        match c {
            '"' => { quoted = !quoted; any = true; }
            '\\' if quoted => { if let Some(n) = chars.next() { cur.push(n); } }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() { words.push(std::mem::take(&mut cur)); any = false; }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() { words.push(cur); }
    words.into_iter().filter(|w| !(w.len() == 2 && w.starts_with('%'))).map(|w| w.replace("%%", "%")).collect()
}

fn program(exec: &str) -> String {
    let words = exec_words(exec);
    let mut it = words.iter();
    let mut p = it.next().cloned().unwrap_or_default();
    if p == "env" {
        p = it.find(|w| !w.contains('=')).cloned().unwrap_or_default();
    }
    p.rsplit('/').next().unwrap_or("").to_owned()
}

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

impl Library {
    pub fn load() -> Library {
        let mut entries: Vec<Entry> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for dir in data_dirs() {
            let apps = dir.join("applications");
            let mut stack = vec![(apps.clone(), String::new())];
            while let Some((d, prefix)) = stack.pop() {
                let Ok(rd) = std::fs::read_dir(&d) else { continue };
                for f in rd.flatten() {
                    let p = f.path();
                    let name = f.file_name().to_string_lossy().into_owned();
                    if p.is_dir() {
                        stack.push((p, format!("{prefix}{name}-")));
                    } else if let Some(stem) = name.strip_suffix(".desktop") {
                        let id = format!("{prefix}{stem}");
                        // The first directory in XDG order wins; a hidden entry still shadows.
                        if seen.insert(id.clone()) && let Some(e) = parse(&p, id) {
                            entries.push(e);
                        }
                    }
                }
            }
        }
        entries.sort_by_key(|e| e.name.to_lowercase());
        let mut lib = Library { entries, ..Library::default() };
        for (i, e) in lib.entries.iter().enumerate() {
            lib.by_id.insert(e.id.clone(), i);
        }
        // Weaker keys first so stronger ones overwrite them.
        for (i, e) in lib.entries.iter().enumerate() {
            let prog = program(&e.exec);
            if !prog.is_empty() && !e.terminal { lib.by_key.entry(prog.to_lowercase()).or_insert(i); }
            lib.by_key.insert(e.name.to_lowercase(), i);
        }
        for (i, e) in lib.entries.iter().enumerate() {
            if let Some(last) = e.id.rsplit('.').next() { lib.by_key.entry(last.to_lowercase()).or_insert(i); }
            lib.by_key.insert(e.id.to_lowercase(), i);
            if !e.wm_class.is_empty() { lib.by_key.insert(e.wm_class.to_lowercase(), i); }
            // Terminal apps launched with their own app id (Omarchy's TUIs and webapps).
            for w in exec_words(&e.exec) {
                if let Some(app_id) = w.strip_prefix("--app-id=").or_else(|| w.strip_prefix("--class=")) {
                    lib.by_key.insert(app_id.to_lowercase(), i);
                }
            }
        }
        lib
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.by_id.get(id).map(|&i| &self.entries[i])
    }

    /// The entry that owns a window of this class, if any.
    pub fn for_class(&self, class: &str) -> Option<&Entry> {
        let c = class.to_lowercase();
        if let Some(&i) = self.by_key.get(&c) {
            return Some(&self.entries[i]);
        }
        // Omarchy's TUI launcher names a terminal window org.omarchy.<command>.
        if let Some(cmd) = c.strip_prefix("org.omarchy.") {
            return self.entries.iter().find(|e| e.terminal && program(&e.exec).eq_ignore_ascii_case(cmd));
        }
        // Chromium web apps: chrome-<host>__-Default, brave-<host>__-Default.
        if let Some(host) = c.split_once('-').map(|(_, h)| h).and_then(|h| h.split("__").next()) {
            let host = host.replace('_', "/");
            return self.entries.iter().find(|e| e.exec.to_lowercase().contains(&host) && e.exec.contains("--app="));
        }
        None
    }
}

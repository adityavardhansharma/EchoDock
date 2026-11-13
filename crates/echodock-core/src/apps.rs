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

//! A line protocol on `$XDG_RUNTIME_DIR/echodock.sock`: `echodock ctl <command>` and keybindings
//! talk to the running dock through it.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

pub fn socket() -> PathBuf {
    PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_else(|| "/tmp".into())).join("echodock.sock")
}

/// Sends one command and returns the dock's reply.
pub fn send(command: &str) -> std::io::Result<String> {
    let mut s = UnixStream::connect(socket())?;
    s.write_all(command.as_bytes())?;
    s.write_all(b"\n")?;
    s.shutdown(std::net::Shutdown::Write)?;
    let mut reply = String::new();
    BufReader::new(s).read_line(&mut reply)?;
    Ok(reply.trim_end().to_owned())
}

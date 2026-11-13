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

/// Serves commands on its own thread. Fails when another dock already answers on the socket.
pub fn serve(handle: impl Fn(&str) -> String + Send + 'static) -> std::io::Result<()> {
    let path = socket();
    if UnixStream::connect(&path).is_ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, "echodock is already running"));
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    std::thread::Builder::new().name("echodock-ipc".into()).spawn(move || {
        for conn in listener.incoming().flatten() {
            let mut line = String::new();
            let mut reader = BufReader::new(&conn);
            if reader.read_line(&mut line).is_ok() {
                let reply = handle(line.trim());
                let _ = (&conn).write_all(format!("{reply}\n").as_bytes());
            }
        }
    })?;
    Ok(())
}

//! Single-instance forwarding. unix: unix socket; elsewhere: loopback TCP.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

#[cfg(unix)]
mod imp {
    pub use std::os::unix::net::{UnixListener as Listener, UnixStream as Stream};
    use std::path::PathBuf;

    pub fn addr() -> PathBuf {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        // SAFETY-free uid lookup: use the owner of the dir via env fallback.
        let user = std::env::var("USER").unwrap_or_else(|_| "u".into());
        dir.join(format!("md-viewer-{user}.sock"))
    }
    pub fn connect() -> std::io::Result<Stream> {
        Stream::connect(addr())
    }
    pub fn bind() -> std::io::Result<Listener> {
        let a = addr();
        match Listener::bind(&a) {
            Ok(l) => Ok(l),
            Err(_) => {
                // Stale socket (connect already failed): replace it.
                let _ = std::fs::remove_file(&a);
                Listener::bind(&a)
            }
        }
    }
}

#[cfg(not(unix))]
mod imp {
    pub use std::net::{TcpListener as Listener, TcpStream as Stream};
    // TODO(windows): named pipe would avoid a fixed loopback port; TCP is the simple fallback.
    const ADDR: &str = "127.0.0.1:47613";
    pub fn connect() -> std::io::Result<Stream> {
        Stream::connect(ADDR)
    }
    pub fn bind() -> std::io::Result<Listener> {
        Listener::bind(ADDR)
    }
}

/// Try to hand `path` (or an empty line = just raise) to a running instance.
pub fn forward(path: Option<&std::path::Path>) -> bool {
    let Ok(mut s) = imp::connect() else { return false };
    let line = path.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    s.write_all(line.as_bytes()).and_then(|_| s.write_all(b"\n")).is_ok()
}

/// Start the listener thread; `on_open` is called with each forwarded path.
pub fn listen<F: Fn(Option<PathBuf>) + Send + 'static>(on_open: F) {
    let Ok(l) = imp::bind() else { return };
    std::thread::spawn(move || {
        for conn in l.incoming().flatten() {
            let mut line = String::new();
            if BufReader::new(conn).take_line(&mut line) {
                let t = line.trim_end_matches(['\n', '\r']);
                on_open(if t.is_empty() { None } else { Some(PathBuf::from(t)) });
            }
        }
    });
}

trait TakeLine {
    fn take_line(&mut self, out: &mut String) -> bool;
}
impl<R: std::io::Read> TakeLine for BufReader<R> {
    fn take_line(&mut self, out: &mut String) -> bool {
        // bounded read so a rogue local client cannot exhaust memory
        use std::io::Read;
        let mut lim = self.by_ref().take(8192);
        let mut b = BufReader::new(&mut lim);
        b.read_line(out).is_ok() && !out.is_empty()
    }
}

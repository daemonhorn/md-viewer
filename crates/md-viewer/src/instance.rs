//! Single-instance forwarding. unix: unix socket in a private per-user
//! directory; windows: loopback TCP on a random port guarded by a per-user token.
//!
//! Wire protocol (one request per connection):
//!   client -> `MDV1\t<token>\t<raw 0|1>\t<theme or ->\t<path or empty>\n`
//!   server -> `ok\n` once the request was accepted.
//! A client only reports success after receiving the ack, so a squatter that
//! merely accepts connections is treated as "no instance".

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Maximum accepted request line, in bytes.
const MAX_LINE: usize = 8 * 1024;
/// Read/write timeout for each connection (both sides).
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const ACK: &[u8] = b"ok\n";

/// A forwarded launch request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Request {
    pub path: Option<PathBuf>,
    pub raw: bool,
    pub theme: Option<String>,
}

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    pub use std::os::unix::net::{UnixListener as Listener, UnixStream as Stream};
    use std::path::{Path, PathBuf};

    /// Effective uid of this process without libc: `/proc/self` is owned by it
    /// on Linux; elsewhere fall back to the owner of `$HOME`.
    fn current_uid() -> Option<u32> {
        if let Ok(m) = std::fs::metadata("/proc/self") {
            return Some(m.uid());
        }
        let home = std::env::var_os("HOME")?;
        std::fs::metadata(home).ok().map(|m| m.uid())
    }

    pub fn default_dir() -> Option<PathBuf> {
        if let Some(x) = std::env::var_os("XDG_RUNTIME_DIR") {
            let x = PathBuf::from(x);
            if x.is_absolute() && x.is_dir() {
                return Some(x.join("md-viewer"));
            }
        }
        let uid = current_uid()?;
        Some(std::env::temp_dir().join(format!("md-viewer-{uid}")))
    }

    /// The dir must be a real directory (not a symlink) owned by us, mode 0700.
    pub fn verify_dir(dir: &Path) -> io::Result<()> {
        let bad = |m: &str| io::Error::new(io::ErrorKind::PermissionDenied, m.to_string());
        let m = std::fs::symlink_metadata(dir)?;
        if !m.file_type().is_dir() {
            return Err(bad("instance dir is not a directory"));
        }
        let uid = current_uid().ok_or_else(|| bad("cannot determine uid"))?;
        if m.uid() != uid {
            return Err(bad("instance dir has wrong owner"));
        }
        if m.mode() & 0o077 != 0 {
            return Err(bad("instance dir permissions too open"));
        }
        Ok(())
    }

    pub fn prepare_dir(dir: &Path) -> io::Result<()> {
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        verify_dir(dir)
    }

    fn sock(dir: &Path) -> PathBuf {
        dir.join("sock")
    }

    pub fn client_token(_dir: &Path) -> io::Result<String> {
        Ok(String::new()) // access is controlled by the 0700 directory
    }

    pub fn connect(dir: &Path) -> io::Result<Stream> {
        Stream::connect(sock(dir))
    }

    /// Called with the lock held. Replaces a stale socket, never a live one.
    pub fn bind_locked(dir: &Path) -> io::Result<(Listener, String)> {
        let s = sock(dir);
        match Stream::connect(&s) {
            Ok(_) => return Err(io::Error::new(io::ErrorKind::AddrInUse, "instance running")),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                match std::fs::remove_file(&s) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
            Err(e) => return Err(e),
        }
        Ok((Listener::bind(&s)?, String::new()))
    }
}

#[cfg(not(unix))]
mod imp {
    use std::io;
    use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
    pub use std::net::{TcpListener as Listener, TcpStream as Stream};
    use std::path::{Path, PathBuf};

    // TODO(windows): a named pipe with a user-only ACL would be the ideal
    // transport. Until then: random loopback port + secret token, both stored
    // in a file under %LOCALAPPDATA% (inherits a per-user ACL by default).

    pub fn default_dir() -> Option<PathBuf> {
        let base = std::env::var_os("LOCALAPPDATA")?;
        Some(PathBuf::from(base).join("md-viewer").join("instance"))
    }

    pub fn verify_dir(dir: &Path) -> io::Result<()> {
        if std::fs::metadata(dir)?.is_dir() {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::Other, "not a directory"))
        }
    }

    pub fn prepare_dir(dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        verify_dir(dir)
    }

    fn endpoint_file(dir: &Path) -> PathBuf {
        dir.join("endpoint")
    }

    /// Returns (port, token) from the endpoint file.
    fn read_endpoint(dir: &Path) -> io::Result<(u16, String)> {
        let text = std::fs::read_to_string(endpoint_file(dir))?;
        let mut it = text.lines();
        let port = it.next().and_then(|p| p.trim().parse::<u16>().ok());
        let token = it.next().map(|t| t.trim().to_string());
        match (port, token) {
            (Some(p), Some(t)) if !t.is_empty() => Ok((p, t)),
            _ => Err(io::Error::new(io::ErrorKind::InvalidData, "bad endpoint")),
        }
    }

    pub fn client_token(dir: &Path) -> io::Result<String> {
        read_endpoint(dir).map(|e| e.1)
    }

    pub fn connect(dir: &Path) -> io::Result<Stream> {
        let (port, _) = read_endpoint(dir)?;
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        TcpStream::connect_timeout(&addr, super::IO_TIMEOUT)
    }

    fn new_token() -> String {
        use std::hash::{BuildHasher, Hasher};
        let mut out = String::new();
        for _ in 0..2 {
            // RandomState is seeded from OS randomness.
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
            );
            out.push_str(&format!("{:016x}", h.finish()));
        }
        out
    }

    /// Called with the lock held.
    pub fn bind_locked(dir: &Path) -> io::Result<(Listener, String)> {
        if connect(dir).is_ok() {
            return Err(io::Error::new(io::ErrorKind::AddrInUse, "instance running"));
        }
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = l.local_addr()?.port();
        let token = new_token();
        let tmp = dir.join("endpoint.tmp");
        std::fs::write(&tmp, format!("{port}\n{token}\n"))?;
        std::fs::rename(&tmp, endpoint_file(dir))?;
        Ok((l, token))
    }
}

/// Serializes binders through an exclusively-created lock file.
struct Lock(PathBuf);

impl Lock {
    fn acquire(dir: &Path) -> std::io::Result<Lock> {
        use std::io::ErrorKind;
        let p = dir.join("lock");
        for _ in 0..150 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&p)
            {
                Ok(_) => return Ok(Lock(p)),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    // Lock left behind by a crashed process.
                    let stale = std::fs::metadata(&p)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > Duration::from_secs(10));
                    if stale {
                        let _ = std::fs::remove_file(&p);
                    } else {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(
            ErrorKind::TimedOut,
            "instance lock busy",
        ))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn encode(req: &Request, token: &str) -> Option<String> {
    let path = req
        .path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let theme = req.theme.as_deref().unwrap_or("-");
    let bad = |s: &str| s.contains(['\n', '\r']);
    if bad(&path) || bad(theme) || bad(token) || theme.contains('\t') || token.contains('\t') {
        return None;
    }
    let raw = u8::from(req.raw);
    Some(format!("MDV1\t{token}\t{raw}\t{theme}\t{path}\n"))
}

fn decode(line: &str, expected_token: &str) -> Option<Request> {
    let line = line.strip_suffix('\n')?;
    let line = line.strip_suffix('\r').unwrap_or(line);
    let mut it = line.splitn(5, '\t');
    if it.next()? != "MDV1" || it.next()? != expected_token {
        return None;
    }
    let raw = match it.next()? {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    let theme = match it.next()? {
        "-" | "" => None,
        t => Some(t.to_string()),
    };
    let path = it.next()?;
    let path = if path.is_empty() {
        None
    } else {
        let p = PathBuf::from(path);
        if !path_allowed(&p) {
            return None;
        }
        Some(p)
    };
    Some(Request { path, raw, theme })
}

#[cfg(windows)]
fn path_allowed(p: &Path) -> bool {
    let s = p.to_string_lossy();
    // UNC / device paths would trigger SMB authentication; require a real file.
    !(s.starts_with("\\\\") || s.starts_with("//")) && p.is_file()
}

#[cfg(not(windows))]
fn path_allowed(_p: &Path) -> bool {
    true
}

/// Send `req` to the instance whose private dir is `dir`; true only if acked.
fn forward_in(dir: &Path, req: &Request) -> bool {
    if imp::verify_dir(dir).is_err() {
        return false;
    }
    let Ok(token) = imp::client_token(dir) else {
        return false;
    };
    let Some(msg) = encode(req, &token) else {
        return false;
    };
    let Ok(mut s) = imp::connect(dir) else {
        return false;
    };
    let _ = s.set_write_timeout(Some(IO_TIMEOUT));
    let _ = s.set_read_timeout(Some(IO_TIMEOUT));
    if s.write_all(msg.as_bytes()).and_then(|_| s.flush()).is_err() {
        return false;
    }
    let mut ack = [0u8; 3];
    s.read_exact(&mut ack).is_ok() && ack == *ACK
}

/// Bind the listener under the lock; fails (AddrInUse) if an instance is alive.
fn bind_in(dir: &Path) -> std::io::Result<(imp::Listener, String)> {
    imp::prepare_dir(dir)?;
    let _lock = Lock::acquire(dir)?;
    imp::bind_locked(dir)
}

/// Read one bounded, newline-terminated line. None on EOF/oversize/timeout.
fn read_request_line(s: &imp::Stream) -> Option<String> {
    let mut r = std::io::BufReader::new(s.take(MAX_LINE as u64 + 1));
    let mut buf = Vec::new();
    r.read_until(b'\n', &mut buf).ok()?;
    if buf.len() > MAX_LINE || buf.last() != Some(&b'\n') {
        return None;
    }
    String::from_utf8(buf).ok()
}

fn serve_conn<F: Fn(Request)>(mut s: imp::Stream, token: &str, handler: &std::sync::Mutex<F>) {
    let _ = s.set_read_timeout(Some(IO_TIMEOUT));
    let _ = s.set_write_timeout(Some(IO_TIMEOUT));
    let Some(line) = read_request_line(&s) else {
        return;
    };
    let Some(req) = decode(&line, token) else {
        return;
    };
    if let Ok(h) = handler.lock() {
        h(req);
    }
    let _ = s.write_all(ACK);
}

fn listen_in<F: Fn(Request) + Send + 'static>(dir: &Path, handler: F) -> std::io::Result<()> {
    let (l, token) = bind_in(dir)?;
    let handler = std::sync::Arc::new(std::sync::Mutex::new(handler));
    let token: std::sync::Arc<str> = token.into();
    std::thread::spawn(move || {
        for conn in l.incoming().flatten() {
            let (h, t) = (handler.clone(), token.clone());
            // One thread per connection so an idle client cannot wedge the listener.
            std::thread::spawn(move || serve_conn(conn, &t, &h));
        }
    });
    Ok(())
}

/// Like [`forward`], also carrying `--raw` / `--theme`.
pub fn forward_with(req: &Request) -> bool {
    imp::default_dir().is_some_and(|d| forward_in(&d, req))
}

/// Like [`listen`], but receives the full request.
pub fn listen_with<F: Fn(Request) + Send + 'static>(on_open: F) {
    if let Some(d) = imp::default_dir() {
        let _ = listen_in(&d, on_open);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    use std::time::Instant;

    fn tmp(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("mdv-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base.join("d")
    }

    fn req(p: &str) -> Request {
        Request {
            path: Some(PathBuf::from(p)),
            raw: true,
            theme: Some("dark".into()),
        }
    }

    #[test]
    fn round_trip() {
        let d = tmp("rt");
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        listen_in(&d, move |r| tx.lock().unwrap().send(r).unwrap()).unwrap();
        assert!(forward_in(&d, &req("/a b/c\td.md")));
        assert_eq!(rx.recv_timeout(IO_TIMEOUT).unwrap(), req("/a b/c\td.md"));
        assert!(forward_in(&d, &Request::default()));
        assert_eq!(rx.recv_timeout(IO_TIMEOUT).unwrap(), Request::default());
    }

    #[test]
    fn no_instance_is_false() {
        let d = tmp("none");
        assert!(!forward_in(&d, &Request::default()));
        imp::prepare_dir(&d).unwrap();
        assert!(!forward_in(&d, &Request::default()));
    }

    #[test]
    fn stalled_client_does_not_wedge() {
        let d = tmp("stall");
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        listen_in(&d, move |r| tx.lock().unwrap().send(r).unwrap()).unwrap();
        let _idle = imp::connect(&d).unwrap(); // connects, never sends
        let t = Instant::now();
        assert!(forward_in(&d, &req("/x.md")));
        assert!(t.elapsed() < IO_TIMEOUT);
        assert_eq!(
            rx.recv_timeout(IO_TIMEOUT).unwrap().path,
            Some("/x.md".into())
        );
    }

    #[test]
    fn squatter_without_ack_is_not_success() {
        let d = tmp("squat");
        imp::prepare_dir(&d).unwrap();
        let l = UnixListener::bind(d.join("sock")).unwrap();
        std::thread::spawn(move || {
            // accepts and reads but never acks; hold conns open
            let mut keep = Vec::new();
            for c in l.incoming().flatten() {
                keep.push(c);
            }
        });
        assert!(!forward_in(&d, &req("/x.md")));
        // and a second instance must not steal the live-looking socket
        let e = listen_in(&d, |_| {}).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AddrInUse);
    }

    #[test]
    fn stale_socket_is_replaced() {
        let d = tmp("stale");
        imp::prepare_dir(&d).unwrap();
        drop(UnixListener::bind(d.join("sock")).unwrap()); // file stays behind
        assert!(d.join("sock").exists());
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        listen_in(&d, move |r| tx.lock().unwrap().send(r).unwrap()).unwrap();
        assert!(forward_in(&d, &req("/s.md")));
        assert_eq!(
            rx.recv_timeout(IO_TIMEOUT).unwrap().path,
            Some("/s.md".into())
        );
    }

    #[test]
    fn simultaneous_binders_one_wins() {
        let d = tmp("race");
        imp::prepare_dir(&d).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let hs: Vec<_> = (0..8)
            .map(|_| {
                let (d, b) = (d.clone(), barrier.clone());
                std::thread::spawn(move || {
                    b.wait();
                    bind_in(&d)
                })
            })
            .collect();
        let res: Vec<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(res.iter().filter(|r| r.is_ok()).count(), 1);
        for r in res.iter().filter_map(|r| r.as_ref().err()) {
            assert_eq!(r.kind(), std::io::ErrorKind::AddrInUse);
        }
    }

    #[test]
    fn oversize_line_rejected() {
        let d = tmp("big");
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        listen_in(&d, move |r| tx.lock().unwrap().send(r).unwrap()).unwrap();
        let long = "x".repeat(MAX_LINE + 100);
        assert!(!forward_in(&d, &req(&long)));
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        // listener still healthy
        assert!(forward_in(&d, &req("/ok.md")));
    }

    #[test]
    fn insecure_dir_refused() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp("perm");
        imp::prepare_dir(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(imp::verify_dir(&d).is_err());
        assert!(listen_in(&d, |_| {}).is_err());
        assert!(!forward_in(&d, &Request::default()));
    }
}

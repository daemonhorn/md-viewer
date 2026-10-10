//! Path sandbox: map a URL path onto a file strictly inside a root directory.

use std::path::{Component, Path, PathBuf};

use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, CONTROLS};

#[derive(Debug, PartialEq, Eq)]
pub enum SandboxError {
    Invalid,
    Escape,
    NotFound,
}

const SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'\\')
    .add(b'|')
    .add(b'^')
    .add(b'[')
    .add(b']');

/// Resolve `url_path` (percent-encoded, leading `/`) under `root`.
/// Rejects `..`, absolute/drive components, NULs, and symlinks leaving `root`.
pub fn resolve(root: &Path, url_path: &str) -> Result<PathBuf, SandboxError> {
    let decoded = percent_decode_str(url_path)
        .decode_utf8()
        .map_err(|_| SandboxError::Invalid)?;
    if decoded.contains('\0') || decoded.contains('\\') {
        return Err(SandboxError::Invalid);
    }
    let mut rel = PathBuf::new();
    for seg in decoded.split('/') {
        match seg {
            "" | "." => {}
            ".." => return Err(SandboxError::Escape),
            s => {
                // Reject Windows drive prefixes like `C:` (only meaningful on windows;
                // `a:b.md` is a legal unix file name).
                if cfg!(windows)
                    && s.len() >= 2
                    && s.as_bytes()[1] == b':'
                    && s.as_bytes()[0].is_ascii_alphabetic()
                {
                    return Err(SandboxError::Invalid);
                }
                rel.push(s);
            }
        }
    }
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(SandboxError::Invalid);
    }
    let canon_root = root.canonicalize().map_err(|_| SandboxError::NotFound)?;
    let full = canon_root
        .join(&rel)
        .canonicalize()
        .map_err(|_| SandboxError::NotFound)?;
    if full.starts_with(&canon_root) {
        Ok(full)
    } else {
        Err(SandboxError::Escape)
    }
}

/// Read a file previously returned by `resolve`, shrinking the
/// canonicalize-then-read race: the file is opened first and the opened
/// descriptor is then verified to still be a regular file inside `root`.
pub fn read_checked(root: &Path, full: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let canon_root = root.canonicalize()?;
    let mut f = std::fs::File::open(full)?;
    let md = f.metadata()?;
    if !md.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    verify_open(&f, &md, &canon_root, full)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

fn escaped() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::PermissionDenied, "path escaped sandbox")
}

#[cfg(unix)]
fn verify_open(
    f: &std::fs::File,
    md: &std::fs::Metadata,
    canon_root: &Path,
    full: &Path,
) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        // Authoritative: where does the opened descriptor actually live?
        if let Ok(p) = std::fs::read_link(format!("/proc/self/fd/{}", f.as_raw_fd())) {
            return if p.starts_with(canon_root) {
                Ok(())
            } else {
                Err(escaped())
            };
        }
    }
    let _ = f;
    // Portable: the path must still canonicalize inside root to the same inode.
    let again = full.canonicalize()?;
    let m2 = std::fs::metadata(&again)?;
    if again.starts_with(canon_root) && m2.ino() == md.ino() && m2.dev() == md.dev() {
        Ok(())
    } else {
        Err(escaped())
    }
}

#[cfg(not(unix))]
fn verify_open(
    _f: &std::fs::File,
    _md: &std::fs::Metadata,
    canon_root: &Path,
    full: &Path,
) -> std::io::Result<()> {
    if full.canonicalize()?.starts_with(canon_root) {
        Ok(())
    } else {
        Err(escaped())
    }
}

/// Inverse of `resolve`: URL path (with leading `/`) for `file` relative to `root`.
pub fn url_path_for(root: &Path, file: &Path) -> Option<String> {
    let rel = file.strip_prefix(root).ok()?;
    let mut out = String::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => {
                out.push('/');
                out.push_str(&utf8_percent_encode(&s.to_string_lossy(), SEGMENT).to_string());
            }
            _ => return None,
        }
    }
    Some(if out.is_empty() { "/".into() } else { out })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mdv-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("root/sub")).unwrap();
        fs::write(d.join("root/a.md"), "x").unwrap();
        fs::write(d.join("root/sub/b b.png"), "x").unwrap();
        fs::write(d.join("secret.txt"), "s").unwrap();
        d
    }

    #[test]
    fn ok_and_encoded() {
        let d = tmp("ok");
        let root = d.join("root");
        assert!(resolve(&root, "/a.md").is_ok());
        assert!(resolve(&root, "/sub/b%20b.png").is_ok());
        assert!(resolve(&root, "/sub/../a.md").is_err());
        assert_eq!(resolve(&root, "/nope.md"), Err(SandboxError::NotFound));
    }

    #[test]
    fn rejects_dotdot() {
        let d = tmp("dd");
        let root = d.join("root");
        assert_eq!(resolve(&root, "/../secret.txt"), Err(SandboxError::Escape));
        assert_eq!(
            resolve(&root, "/%2e%2e/secret.txt"),
            Err(SandboxError::Escape)
        );
        assert_eq!(
            resolve(&root, "/sub/%2E%2E/%2e%2e/secret.txt"),
            Err(SandboxError::Escape)
        );
        assert_eq!(resolve(&root, "/a%00.md"), Err(SandboxError::Invalid));
        #[cfg(windows)]
        assert_eq!(resolve(&root, "/C:/x"), Err(SandboxError::Invalid));
        assert_eq!(
            resolve(&root, "/..\\secret.txt"),
            Err(SandboxError::Invalid)
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        let d = tmp("sl");
        let root = d.join("root");
        std::os::unix::fs::symlink(d.join("secret.txt"), root.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(&d, root.join("dirlink")).unwrap();
        assert_eq!(resolve(&root, "/link.txt"), Err(SandboxError::Escape));
        assert_eq!(
            resolve(&root, "/dirlink/secret.txt"),
            Err(SandboxError::Escape)
        );
    }

    #[cfg(unix)]
    #[test]
    fn colon_names_allowed_on_unix() {
        let d = tmp("colon");
        let root = d.join("root");
        fs::write(root.join("a:b.md"), "x").unwrap();
        assert!(resolve(&root, "/a:b.md").is_ok());
    }

    #[test]
    fn read_checked_reads_inside_and_rejects_outside() {
        let d = tmp("rc");
        let root = d.join("root").canonicalize().unwrap();
        let ok = resolve(&root, "/a.md").unwrap();
        assert_eq!(read_checked(&root, &ok).unwrap(), b"x");
        let outside = d.join("secret.txt").canonicalize().unwrap();
        assert!(read_checked(&root, &outside).is_err());
        assert!(read_checked(&root, &root).is_err());
    }

    #[test]
    fn url_roundtrip() {
        let d = tmp("rt");
        let root = d.join("root").canonicalize().unwrap();
        let f = root.join("sub/b b.png");
        let u = url_path_for(&root, &f).unwrap();
        assert_eq!(u, "/sub/b%20b.png");
        assert_eq!(resolve(&root, &u).unwrap(), f);
        assert!(url_path_for(&root, &d.join("secret.txt")).is_none());
    }
}

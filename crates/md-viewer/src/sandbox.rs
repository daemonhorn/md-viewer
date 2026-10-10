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
    .add(b' ').add(b'"').add(b'#').add(b'%').add(b'<').add(b'>')
    .add(b'?').add(b'`').add(b'{').add(b'}').add(b'\\').add(b'|').add(b'^').add(b'[').add(b']');

/// Resolve `url_path` (percent-encoded, leading `/`) under `root`.
/// Rejects `..`, absolute/drive components, NULs, and symlinks leaving `root`.
pub fn resolve(root: &Path, url_path: &str) -> Result<PathBuf, SandboxError> {
    let decoded = percent_decode_str(url_path).decode_utf8().map_err(|_| SandboxError::Invalid)?;
    if decoded.contains('\0') || decoded.contains('\\') {
        return Err(SandboxError::Invalid);
    }
    let mut rel = PathBuf::new();
    for seg in decoded.split('/') {
        match seg {
            "" | "." => {}
            ".." => return Err(SandboxError::Escape),
            s => {
                // Reject Windows drive prefixes like `C:` even on unix.
                if s.len() >= 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic() {
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
    let full = canon_root.join(&rel).canonicalize().map_err(|_| SandboxError::NotFound)?;
    if full.starts_with(&canon_root) {
        Ok(full)
    } else {
        Err(SandboxError::Escape)
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
        assert_eq!(resolve(&root, "/%2e%2e/secret.txt"), Err(SandboxError::Escape));
        assert_eq!(resolve(&root, "/sub/%2E%2E/%2e%2e/secret.txt"), Err(SandboxError::Escape));
        assert_eq!(resolve(&root, "/a%00.md"), Err(SandboxError::Invalid));
        assert_eq!(resolve(&root, "/C:/x"), Err(SandboxError::Invalid));
        assert_eq!(resolve(&root, "/..\\secret.txt"), Err(SandboxError::Invalid));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        let d = tmp("sl");
        let root = d.join("root");
        std::os::unix::fs::symlink(d.join("secret.txt"), root.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(&d, root.join("dirlink")).unwrap();
        assert_eq!(resolve(&root, "/link.txt"), Err(SandboxError::Escape));
        assert_eq!(resolve(&root, "/dirlink/secret.txt"), Err(SandboxError::Escape));
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

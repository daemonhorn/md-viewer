//! Request handling for the `mdv://` protocol (independent of wry for testability).

use std::path::{Path, PathBuf};

use crate::page::{self, Theme};
use crate::sandbox::{self, SandboxError};

pub struct State {
    /// Sandbox root: the opened document's directory.
    pub root: Option<PathBuf>,
    /// Document most recently served (used for live reload).
    pub current: Option<PathBuf>,
    pub raw_default: bool,
    pub theme: Theme,
}

pub struct Reply {
    pub status: u16,
    pub mime: &'static str,
    pub body: Vec<u8>,
}

impl Reply {
    fn html(status: u16, s: String) -> Self {
        Reply { status, mime: "text/html; charset=utf-8", body: s.into_bytes() }
    }
    fn text(status: u16, s: &str) -> Self {
        Reply { status, mime: "text/plain; charset=utf-8", body: s.as_bytes().to_vec() }
    }
}

pub fn is_markdown(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("md" | "markdown" | "mdown" | "mkd" | "mdx")
    )
}

pub fn mime_for(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("bmp") => "image/bmp",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("html" | "htm") => "text/plain; charset=utf-8", // never execute local HTML
        Some("txt" | "md" | "markdown") => "text/plain; charset=utf-8",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// Embedded third-party assets.
/// TODO(bundling): vendor mermaid.min.js, katex.min.js, katex.min.css (+fonts) under
/// assets/vendor/ (MIT licensed) and return them here via include_bytes!, ideally
/// behind a cargo feature to keep the default binary small.
fn bundled_asset(_name: &str) -> Option<(&'static str, &'static [u8])> {
    None
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v))
}

/// Build the reply for `path` (URL path) and `query`. Mutates `state.current` when serving a document.
pub fn handle(state: &mut State, path: &str, query: &str) -> Reply {
    match path {
        "/__mdv/style.css" => {
            return Reply { status: 200, mime: "text/css; charset=utf-8", body: page::CSS.as_bytes().to_vec() }
        }
        "/__mdv/app.js" => {
            return Reply { status: 200, mime: "text/javascript; charset=utf-8", body: page::JS.as_bytes().to_vec() }
        }
        "/favicon.ico" => return Reply::text(404, "not found"),
        _ => {}
    }
    if let Some(name) = path.strip_prefix("/__mdv/vendor/") {
        return match bundled_asset(name) {
            Some((mime, b)) => Reply { status: 200, mime, body: b.to_vec() },
            None => Reply::text(404, "asset not bundled"),
        };
    }
    let theme = state.theme;
    let Some(root) = state.root.clone() else {
        return Reply::html(200, page::help_page(theme));
    };
    let full = match sandbox::resolve(&root, path) {
        Ok(p) => p,
        Err(SandboxError::NotFound) => return Reply::text(404, "not found"),
        Err(_) => return Reply::text(403, "forbidden"),
    };
    if full.is_dir() {
        return Reply::text(403, "directory");
    }
    if is_markdown(&full) {
        let raw = match query_param(query, "raw") {
            Some("1") => true,
            Some("0") => false,
            _ => state.raw_default,
        };
        let src = match std::fs::read(&full) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => return Reply::html(500, page::error_page(theme, &e.to_string())),
        };
        state.current = Some(full.clone());
        let title = full.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let html = if raw {
            page::raw_page(&title, &src, theme)
        } else {
            page::rendered_page(&title, &md_core::render(&src), theme)
        };
        return Reply::html(200, html);
    }
    match std::fs::read(&full) {
        Ok(body) => Reply { status: 200, mime: mime_for(&full), body },
        Err(_) => Reply::text(404, "not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup() -> (PathBuf, State) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!(
            "mdv-serve-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("root")).unwrap();
        fs::write(d.join("root/a.md"), "# Title\n\ntext").unwrap();
        fs::write(d.join("root/i.png"), [1u8, 2, 3]).unwrap();
        fs::write(d.join("secret"), "s").unwrap();
        let root = d.join("root").canonicalize().unwrap();
        (d, State { root: Some(root), current: None, raw_default: false, theme: Theme::Auto })
    }

    #[test]
    fn serves_doc_modes() {
        let (_d, mut s) = setup();
        let r = handle(&mut s, "/a.md", "");
        assert_eq!(r.status, 200);
        assert!(String::from_utf8(r.body).unwrap().contains("data-mode=\"rendered\""));
        assert!(s.current.is_some());
        let r = handle(&mut s, "/a.md", "raw=1");
        assert!(String::from_utf8(r.body).unwrap().contains("data-mode=\"raw\""));
        s.raw_default = true;
        let r = handle(&mut s, "/a.md", "");
        assert!(String::from_utf8(r.body).unwrap().contains("data-mode=\"raw\""));
        let r = handle(&mut s, "/a.md", "raw=0");
        assert!(String::from_utf8(r.body).unwrap().contains("data-mode=\"rendered\""));
    }

    #[test]
    fn serves_assets_and_blocks_escape() {
        let (_d, mut s) = setup();
        let r = handle(&mut s, "/i.png", "");
        assert_eq!((r.status, r.mime), (200, "image/png"));
        assert_eq!(handle(&mut s, "/../secret", "").status, 403);
        assert_eq!(handle(&mut s, "/%2e%2e/secret", "").status, 403);
        assert_eq!(handle(&mut s, "/missing.png", "").status, 404);
        assert_eq!(handle(&mut s, "/__mdv/style.css", "").status, 200);
        assert_eq!(handle(&mut s, "/__mdv/vendor/mermaid.min.js", "").status, 404);
    }

    #[test]
    fn no_root_shows_help() {
        let mut s = State { root: None, current: None, raw_default: false, theme: Theme::Auto };
        let r = handle(&mut s, "/", "");
        assert!(String::from_utf8(r.body).unwrap().contains("Drop a Markdown file"));
    }
}

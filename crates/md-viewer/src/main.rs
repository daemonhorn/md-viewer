#![cfg_attr(windows, windows_subsystem = "windows")]

mod instance;
mod page;
mod sandbox;
mod serve;

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use clap::{Parser, Subcommand};
use notify::{RecursiveMode, Watcher};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use wry::http::{header::CONTENT_TYPE, Response};
use wry::WebViewBuilder;

use page::Theme;
use serve::State;

use serve::{classify_nav, Nav, BASE};

/// The release binary uses the windows GUI subsystem (no console flash), so when
/// started from a terminal re-attach to the parent console for CLI output.
#[cfg(windows)]
fn attach_parent_console() {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(pid: u32) -> i32;
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn SetStdHandle(which: u32, h: *mut c_void) -> i32;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            sec: *mut c_void,
            disp: u32,
            flags: u32,
            tmpl: *mut c_void,
        ) -> *mut c_void;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const GENERIC_READ_WRITE: u32 = 0x8000_0000 | 0x4000_0000;
    const SHARE_READ_WRITE: u32 = 3;
    const OPEN_EXISTING: u32 = 3;
    // SAFETY: plain kernel32 calls with valid arguments; failures are ignored
    // (no parent console, e.g. launched from Explorer, simply means no output).
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        let conout: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
        for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let cur = GetStdHandle(which);
            // Do not clobber redirected output (`> file`, pipes).
            if cur.is_null() || cur as isize == -1 {
                let h = CreateFileW(
                    conout.as_ptr(),
                    GENERIC_READ_WRITE,
                    SHARE_READ_WRITE,
                    std::ptr::null_mut(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                );
                if h as isize != -1 && !h.is_null() {
                    SetStdHandle(which, h);
                }
            }
        }
    }
}
#[cfg(not(windows))]
fn attach_parent_console() {}

#[derive(Parser)]
#[command(
    name = "md-viewer",
    version,
    about = "Fast Markdown viewer",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    /// Markdown file to open
    file: Option<PathBuf>,
    /// Sandbox root: files outside this directory are never served (default: the
    /// document's directory). Must contain FILE; lets links like `../x.md` work.
    #[arg(long, value_name = "DIR", requires = "file")]
    root: Option<PathBuf>,
    /// Allow remote (https) images in documents; blocked by default
    #[arg(long)]
    allow_remote_images: bool,
    /// Start in raw source view (toggle with `r`)
    #[arg(long)]
    raw: bool,
    /// Color theme
    #[arg(long, value_enum, default_value = "auto")]
    theme: Theme,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Install desktop integration
    Install {
        /// Replace an existing different .md handler without asking
        #[arg(long)]
        yes: bool,
        /// Install files only; do not change the default application
        #[arg(long)]
        no_assoc: bool,
    },
    /// Remove desktop integration
    Uninstall {
        /// Do not restore the previously recorded default handler
        #[arg(long)]
        no_restore: bool,
    },
    /// Diagnose installation
    Doctor,
}

enum UserEvent {
    Open(Option<PathBuf>),
    Reload,
}

fn doc_url(root: &Path, file: &Path) -> Option<String> {
    Some(format!("{BASE}{}", sandbox::url_path_for(root, file)?))
}

/// Canonicalize a file argument into (sandbox root, file). An explicit `root`
/// must be a directory containing the file.
fn locate(file: &Path, root: Option<&Path>) -> Option<(PathBuf, PathBuf)> {
    let f = file.canonicalize().ok()?;
    if !f.is_file() {
        return None;
    }
    let r = match root {
        Some(r) => {
            let r = r.canonicalize().ok()?;
            if !r.is_dir() || !f.starts_with(&r) {
                return None;
            }
            r
        }
        None => f.parent()?.to_path_buf(),
    };
    Some((r, f))
}

/// Root to use when a running viewer is asked to open `file`: keep the current
/// root if the file is inside it (so already-open pages keep working), else
/// switch to the file's own directory. Limitation: pages from a previous root
/// stop resolving after a switch.
fn root_for_open(current: Option<&Path>, file: &Path, default_root: PathBuf) -> PathBuf {
    match current {
        Some(r) if file.starts_with(r) => r.to_path_buf(),
        _ => default_root,
    }
}

fn make_watcher(
    proxy: EventLoopProxy<UserEvent>,
    state: Arc<Mutex<State>>,
    root: &Path,
) -> Option<notify::RecommendedWatcher> {
    // Debounce: a burst of events (editors write several files) yields one reload.
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        while rx.recv().is_ok() {
            while rx
                .recv_timeout(std::time::Duration::from_millis(150))
                .is_ok()
            {}
            if proxy.send_event(UserEvent::Reload).is_err() {
                break;
            }
        }
    });
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        if ev.kind.is_access() {
            return;
        }
        let cur = state.lock().ok().and_then(|s| s.current.clone());
        let hit = ev
            .paths
            .iter()
            .any(|p| serve::is_markdown(p) || Some(p) == cur.as_ref());
        if hit {
            let _ = tx.send(());
        }
    })
    .ok()?;
    // Recursive so linked pages in subdirectories live-reload too; fall back to the
    // top level if the tree is too large for the OS watch limit.
    if w.watch(root, RecursiveMode::Recursive).is_err() {
        w.watch(root, RecursiveMode::NonRecursive).ok()?;
    }
    Some(w)
}

fn run_admin(c: Cmd) -> i32 {
    use md_install::*;
    let res = match c {
        Cmd::Install { yes, no_assoc } => {
            install(&InstallOptions { yes, no_assoc }, &mut StdioPrompter).map(|r| r.to_string())
        }
        Cmd::Uninstall { no_restore } => {
            uninstall(&UninstallOptions { no_restore }).map(|r| r.to_string())
        }
        Cmd::Doctor => doctor().map(|d| {
            let ok = d.healthy();
            format!("{d}\n{}", if ok { "healthy" } else { "problems found" })
        }),
    };
    match res {
        Ok(out) => {
            println!("{out}");
            0
        }
        Err(e) => {
            eprintln!("md-viewer: {e}");
            1
        }
    }
}

fn main() {
    attach_parent_console();
    let cli = Cli::parse();
    if let Some(c) = cli.cmd {
        std::process::exit(run_admin(c));
    }
    let target = match &cli.file {
        Some(f) => match locate(f, cli.root.as_deref()) {
            Some(t) => Some(t),
            None => {
                eprintln!("md-viewer: cannot open {}", f.display());
                std::process::exit(1);
            }
        },
        None => None,
    };
    // Hand off to a running instance before doing any UI work.
    let fwd = instance::Request {
        path: target.as_ref().map(|t| t.1.clone()),
        raw: cli.raw,
        theme: {
            use clap::ValueEnum;
            cli.theme
                .to_possible_value()
                .map(|v| v.get_name().to_owned())
        },
    };
    if instance::forward_with(&fwd) {
        return;
    }

    let state = Arc::new(Mutex::new(State {
        root: target.as_ref().map(|t| t.0.clone()),
        current: target.as_ref().map(|t| t.1.clone()),
        raw_default: cli.raw,
        theme: cli.theme,
        allow_remote_images: cli.allow_remote_images,
    }));

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let title = target
        .as_ref()
        .and_then(|t| t.1.file_name())
        .map(|n| format!("{} - md-viewer", n.to_string_lossy()))
        .unwrap_or_else(|| "md-viewer".into());
    let window = WindowBuilder::new()
        .with_title(title)
        .with_inner_size(tao::dpi::LogicalSize::new(1000.0, 800.0))
        .build(&event_loop)
        .expect("window");

    let initial = target
        .as_ref()
        .and_then(|(r, f)| doc_url(r, f))
        .unwrap_or_else(|| format!("{BASE}/"));

    let st = state.clone();
    let nav_proxy = proxy.clone();
    let drop_proxy = proxy.clone();
    let builder = WebViewBuilder::new()
        .with_url(&initial)
        .with_custom_protocol("mdv".into(), move |_id, req| {
            let uri = req.uri();
            let reply = match st.lock() {
                Ok(mut s) => serve::handle(&mut s, uri.path(), uri.query().unwrap_or("")),
                Err(_) => {
                    let mut r = serve::Reply::error(500, "state poisoned");
                    r.csp = serve::FILE_CSP.to_string();
                    r
                }
            };
            Response::builder()
                .status(reply.status)
                .header(CONTENT_TYPE, reply.mime)
                .header("Cache-Control", "no-store")
                .header("Content-Security-Policy", reply.csp)
                .header("X-Content-Type-Options", "nosniff")
                .body(Cow::Owned(reply.body))
                .unwrap()
        })
        .with_navigation_handler(move |url| {
            let _ = &nav_proxy;
            match classify_nav(&url, BASE) {
                Nav::Internal => true,
                Nav::External => {
                    let _ = open::that_detached(&url);
                    false
                }
                Nav::Deny => false,
            }
        })
        .with_new_window_req_handler(|url, _| {
            if classify_nav(&url, BASE) == Nav::External {
                let _ = open::that_detached(&url);
            }
            wry::NewWindowResponse::Deny
        })
        .with_drag_drop_handler(move |ev| {
            if let wry::DragDropEvent::Drop { paths, .. } = ev {
                if let Some(p) = paths.into_iter().next() {
                    let _ = drop_proxy.send_event(UserEvent::Open(Some(p)));
                }
            }
            true
        });

    #[cfg(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        builder
            .build_gtk(window.default_vbox().expect("gtk vbox"))
            .expect("webview")
    };
    #[cfg(not(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )))]
    let webview = builder.build(&window).expect("webview");

    // Non-critical setup after the window exists.
    let ip = proxy.clone();
    let ist = state.clone();
    instance::listen_with(move |req| {
        // A forwarded launch may carry --raw / --theme; explicit values override, defaults don't.
        if let Ok(mut s) = ist.lock() {
            if req.raw {
                s.raw_default = true;
            }
            if let Some(t) = req.theme.as_deref() {
                use clap::ValueEnum;
                if let Ok(t) = Theme::from_str(t, true) {
                    if t != Theme::Auto {
                        s.theme = t;
                    }
                }
            }
        }
        let _ = ip.send_event(UserEvent::Open(req.path));
    });
    let mut watcher = state
        .lock()
        .ok()
        .and_then(|s| s.root.clone())
        .and_then(|r| make_watcher(proxy.clone(), state.clone(), &r));

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *flow = ControlFlow::Exit,
            Event::UserEvent(UserEvent::Reload) => {
                let _ = webview.evaluate_script("location.reload()");
            }
            Event::UserEvent(UserEvent::Open(p)) => {
                window.set_minimized(false);
                window.set_focus();
                let Some((root, file)) = p.as_deref().and_then(|p| locate(p, None)) else {
                    return;
                };
                let mut root = root;
                if let Ok(mut s) = state.lock() {
                    root = root_for_open(s.root.as_deref(), &file, root);
                    s.root = Some(root.clone());
                    s.current = Some(file.clone());
                }
                if let Some(u) = doc_url(&root, &file) {
                    let _ = webview.load_url(&u);
                }
                window.set_title(&format!(
                    "{} - md-viewer",
                    file.file_name().unwrap_or_default().to_string_lossy()
                ));
                watcher = make_watcher(proxy.clone(), state.clone(), &root);
            }
            _ => {}
        }
        let _ = &watcher;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_kept_when_file_inside() {
        let cur = Path::new("/a");
        assert_eq!(
            root_for_open(Some(cur), Path::new("/a/b/x.md"), "/a/b".into()),
            PathBuf::from("/a")
        );
        assert_eq!(
            root_for_open(Some(cur), Path::new("/c/x.md"), "/c".into()),
            PathBuf::from("/c")
        );
        assert_eq!(
            root_for_open(None, Path::new("/c/x.md"), "/c".into()),
            PathBuf::from("/c")
        );
    }

    #[test]
    fn locate_honors_explicit_root() {
        let d = std::env::temp_dir().join(format!("mdv-main-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("sub/a.md"), "x").unwrap();
        let f = d.join("sub/a.md");
        let (r, _) = locate(&f, Some(&d)).unwrap();
        assert_eq!(r, d.canonicalize().unwrap());
        assert!(locate(&f, Some(&d.join("other"))).is_none());
        assert!(locate(&d.join("sub/a.md"), Some(&d.join("sub/a.md"))).is_none());
        let (r, _) = locate(&f, None).unwrap();
        assert_eq!(r, d.join("sub").canonicalize().unwrap());
    }
}

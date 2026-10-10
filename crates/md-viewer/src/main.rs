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

#[cfg(windows)]
const BASE: &str = "http://mdv.localhost";
#[cfg(not(windows))]
const BASE: &str = "mdv://localhost";

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

/// Canonicalize a file argument into (sandbox root, file).
fn locate(file: &Path) -> Option<(PathBuf, PathBuf)> {
    let f = file.canonicalize().ok()?;
    if !f.is_file() {
        return None;
    }
    Some((f.parent()?.to_path_buf(), f))
}

fn make_watcher(
    proxy: EventLoopProxy<UserEvent>,
    state: Arc<Mutex<State>>,
    root: &Path,
) -> Option<notify::RecommendedWatcher> {
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
            let _ = proxy.send_event(UserEvent::Reload);
        }
    })
    .ok()?;
    w.watch(root, RecursiveMode::NonRecursive).ok()?;
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
    let cli = Cli::parse();
    if let Some(c) = cli.cmd {
        std::process::exit(run_admin(c));
    }
    let target = match &cli.file {
        Some(f) => match locate(f) {
            Some(t) => Some(t),
            None => {
                eprintln!("md-viewer: cannot open {}", f.display());
                std::process::exit(1);
            }
        },
        None => None,
    };
    // Hand off to a running instance before doing any UI work.
    if instance::forward(target.as_ref().map(|t| t.1.as_path())) {
        return;
    }

    let state = Arc::new(Mutex::new(State {
        root: target.as_ref().map(|t| t.0.clone()),
        current: target.as_ref().map(|t| t.1.clone()),
        raw_default: cli.raw,
        theme: cli.theme,
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
                Err(_) => serve::Reply {
                    status: 500,
                    mime: "text/plain",
                    body: b"state poisoned".to_vec(),
                },
            };
            Response::builder()
                .status(reply.status)
                .header(CONTENT_TYPE, reply.mime)
                .header("Cache-Control", "no-store")
                .body(Cow::Owned(reply.body))
                .unwrap()
        })
        .with_navigation_handler(move |url| {
            let _ = &nav_proxy;
            if url.starts_with(BASE) || url.starts_with("about:") {
                return true;
            }
            if url.starts_with("http://")
                || url.starts_with("https://")
                || url.starts_with("mailto:")
            {
                let _ = open::that_detached(&url);
            }
            false
        })
        .with_new_window_req_handler(|url, _| {
            if url.starts_with("http://")
                || url.starts_with("https://")
                || url.starts_with("mailto:")
            {
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
    instance::listen(move |p| {
        let _ = ip.send_event(UserEvent::Open(p));
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
                let Some((root, file)) = p.as_deref().and_then(locate) else {
                    return;
                };
                if let Ok(mut s) = state.lock() {
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

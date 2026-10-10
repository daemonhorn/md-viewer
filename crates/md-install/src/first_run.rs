//! Windows first-launch install prompt.
//!
//! The Windows build is a GUI-subsystem executable (no console), so the first
//! launch from outside the install directory asks with a native `MessageBox`
//! instead of stdin. Unix users install with `md-viewer install` or
//! `install.sh`; nothing here prompts on unix.
//!
//! The decision ([`first_run_decision`]) and the whole flow ([`run`]) are
//! platform neutral and driven through small traits so they are unit tested
//! on any host; only the `MessageBoxW` FFI is `cfg(windows)`.

use crate::{Prompter, Report, Result};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// Environment variable that disables the prompt (CI, tests).
pub const ENV_NO_PROMPT: &str = "MDV_NO_INSTALL_PROMPT";
/// File name of the "don't ask again" marker, kept under
/// `%LOCALAPPDATA%\md-viewer\`.
pub const MARKER_NAME: &str = "no-install-prompt";

/// Facts the decision is based on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstRunState {
    /// The running exe lives inside the install directory.
    pub exe_in_install_dir: bool,
    /// A valid install record (and installed binary) exists.
    pub installed: bool,
    /// The user chose "don't ask again".
    pub dont_ask: bool,
    /// `MDV_NO_INSTALL_PROMPT` is set.
    pub env_disabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    EnvDisabled,
    InInstallDir,
    AlreadyInstalled,
    DontAsk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstRunAction {
    Prompt,
    Skip(SkipReason),
}

/// Pure decision: should the install prompt be shown?
pub fn first_run_decision(s: &FirstRunState) -> FirstRunAction {
    use FirstRunAction::*;
    use SkipReason::*;
    if s.env_disabled {
        Skip(EnvDisabled)
    } else if s.exe_in_install_dir {
        Skip(InInstallDir)
    } else if s.installed {
        Skip(AlreadyInstalled)
    } else if s.dont_ask {
        Skip(DontAsk)
    } else {
        Prompt
    }
}

/// Answer to the three-way install question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Yes: install now.
    Install,
    /// No: not now, ask again next time.
    NotNow,
    /// Cancel: never ask again.
    DontAsk,
}

/// What the caller should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Carry on and open the document from this process.
    Continue,
    /// The installed copy was started with the original arguments; this
    /// process should exit now.
    Relaunched,
}

/// Dialog surface (native message boxes on Windows, fakes in tests).
pub trait FirstRunUi {
    fn ask_install(&mut self) -> Choice;
    /// Informational or error box with an OK button.
    fn notify(&mut self, title: &str, body: &str, is_error: bool);
    /// Prompter for questions asked by the installer (default-app conflict).
    fn prompter(&mut self) -> &mut dyn Prompter;
}

pub const PROMPT_TITLE: &str = "Install md-viewer";
pub const PROMPT_TEXT: &str = "md-viewer is not installed.\n\n\
Install it for your user account (no administrator rights needed) and make it \
the default app for Markdown files?\n\n\
Yes = Install\nNo = Not now (ask again next time)\nCancel = Don't ask again";

/// Run the flow once `first_run_decision` said `Prompt`.
///
/// `install` performs the user-mode install with the given prompter;
/// `relaunch` starts the installed copy. A failure never prevents the
/// document from opening: the caller gets [`Outcome::Continue`].
pub fn run(
    ui: &mut dyn FirstRunUi,
    marker: &Path,
    install: &mut dyn FnMut(&mut dyn Prompter) -> Result<Report>,
    relaunch: &mut dyn FnMut() -> io::Result<()>,
) -> Outcome {
    match ui.ask_install() {
        Choice::NotNow => Outcome::Continue,
        Choice::DontAsk => {
            if let Some(dir) = marker.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(marker, b"md-viewer: do not offer to install\n");
            Outcome::Continue
        }
        Choice::Install => match install(ui.prompter()) {
            Err(e) => {
                ui.notify(
                    "md-viewer installation failed",
                    &format!("{e}\n\nmd-viewer will keep running from its current location."),
                    true,
                );
                Outcome::Continue
            }
            Ok(report) => {
                let body = format!("md-viewer was installed.\n\n{}", report.to_string().trim());
                match relaunch() {
                    Ok(()) => {
                        ui.notify("md-viewer installed", &body, false);
                        Outcome::Relaunched
                    }
                    Err(e) => {
                        ui.notify(
                            "md-viewer installed",
                            &format!("{body}\n\nCould not start the installed copy ({e}); continuing from the current location."),
                            true,
                        );
                        Outcome::Continue
                    }
                }
            }
        },
    }
}

/// Is `exe` located in `dir` (case-insensitive, separators normalised)?
pub fn path_in_dir(exe: &Path, dir: &Path) -> bool {
    let norm = |p: &Path| {
        let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        p.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    };
    let (e, d) = (norm(exe), norm(dir));
    // canonicalize() on Windows yields `\\?\` prefixes on both sides
    e.rsplit_once('\\')
        .map(|(parent, _)| parent == d)
        .unwrap_or(false)
}

/// Offer to install on first launch (Windows only).
///
/// Call at the very top of `main`, before any window or webview is created,
/// when no subcommand was given. `args` are the original arguments (without
/// `argv[0]`) that are forwarded to the installed copy. On non-Windows
/// platforms this always returns [`Outcome::Continue`].
pub fn maybe_prompt_and_install(args: &[OsString]) -> Outcome {
    #[cfg(windows)]
    {
        win::maybe_prompt_and_install(args)
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        Outcome::Continue
    }
}

/// Location of the "don't ask again" marker for a given `%LOCALAPPDATA%`.
pub fn marker_path(local_appdata: &Path) -> PathBuf {
    local_appdata.join(crate::APP).join(MARKER_NAME)
}

/// Strip the CLI-style ` [y/N]` suffix for display in a dialog.
pub fn dialog_question(q: &str) -> String {
    let q = q.trim();
    q.strip_suffix("[y/N]").unwrap_or(q).trim_end().to_string()
}

#[cfg(windows)]
mod win {
    use super::*;
    use crate::windows::{self, WinEnv};
    use crate::{InstallOptions, Prompter};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::process::CommandExt;

    const MB_YESNOCANCEL: u32 = 0x3;
    const MB_YESNO: u32 = 0x4;
    const MB_ICONERROR: u32 = 0x10;
    const MB_ICONQUESTION: u32 = 0x20;
    const MB_ICONINFORMATION: u32 = 0x40;
    const MB_DEFBUTTON2: u32 = 0x100;
    const MB_SETFOREGROUND: u32 = 0x1_0000;
    const MB_TOPMOST: u32 = 0x4_0000;
    const IDCANCEL: i32 = 2;
    const IDYES: i32 = 6;
    const IDNO: i32 = 7;

    #[link(name = "user32")]
    extern "system" {
        // int MessageBoxW(HWND, LPCWSTR text, LPCWSTR caption, UINT type)
        fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, flags: u32) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .filter(|&c| c != 0)
            .chain(Some(0))
            .collect()
    }

    fn message_box(title: &str, text: &str, flags: u32) -> i32 {
        let (t, c) = (wide(text), wide(title));
        // SAFETY: both buffers are NUL-terminated UTF-16 that outlive the
        // synchronous call; a null owner window is allowed.
        unsafe {
            MessageBoxW(
                0,
                t.as_ptr(),
                c.as_ptr(),
                flags | MB_SETFOREGROUND | MB_TOPMOST,
            )
        }
    }

    /// Yes/No dialog implementing the installer's [`Prompter`]; defaults to No.
    pub struct MessageBoxPrompter;
    impl Prompter for MessageBoxPrompter {
        fn confirm(&mut self, question: &str) -> bool {
            message_box(
                "md-viewer",
                &dialog_question(question),
                MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
            ) == IDYES
        }
    }

    struct Ui(MessageBoxPrompter);
    impl FirstRunUi for Ui {
        fn ask_install(&mut self) -> Choice {
            match message_box(PROMPT_TITLE, PROMPT_TEXT, MB_YESNOCANCEL | MB_ICONQUESTION) {
                IDYES => Choice::Install,
                IDNO => Choice::NotNow,
                IDCANCEL => Choice::DontAsk,
                _ => Choice::NotNow,
            }
        }
        fn notify(&mut self, title: &str, body: &str, is_error: bool) {
            let icon = if is_error {
                MB_ICONERROR
            } else {
                MB_ICONINFORMATION
            };
            message_box(title, body, icon);
        }
        fn prompter(&mut self) -> &mut dyn Prompter {
            &mut self.0
        }
    }

    pub fn maybe_prompt_and_install(args: &[OsString]) -> Outcome {
        let Ok(env) = WinEnv::from_env() else {
            return Outcome::Continue;
        };
        let marker = marker_path(&env.local_appdata);
        let state = FirstRunState {
            exe_in_install_dir: path_in_dir(&env.exe, &env.install_dir()),
            installed: windows::is_installed(&env),
            dont_ask: marker.exists(),
            env_disabled: std::env::var_os(ENV_NO_PROMPT).is_some(),
        };
        if first_run_decision(&state) != FirstRunAction::Prompt {
            return Outcome::Continue;
        }
        let bin = env.bin();
        let args = args.to_vec();
        run(
            &mut Ui(MessageBoxPrompter),
            &marker,
            &mut |p| {
                let mut reg = windows::RealRegistry;
                windows::install_with(&env, &mut reg, p, &InstallOptions::default())
            },
            &mut || {
                const DETACHED_PROCESS: u32 = 0x8;
                const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
                std::process::Command::new(&bin)
                    .args(&args)
                    .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
                    .spawn()
                    .map(|_| ())
            },
        )
    }
}

#[cfg(windows)]
pub use win::MessageBoxPrompter;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FixedPrompter;

    fn st(a: bool, b: bool, c: bool, d: bool) -> FirstRunState {
        FirstRunState {
            exe_in_install_dir: a,
            installed: b,
            dont_ask: c,
            env_disabled: d,
        }
    }

    #[test]
    fn decision_table() {
        use FirstRunAction::*;
        use SkipReason::*;
        assert_eq!(first_run_decision(&st(false, false, false, false)), Prompt);
        assert_eq!(
            first_run_decision(&st(true, false, false, false)),
            Skip(InInstallDir)
        );
        assert_eq!(
            first_run_decision(&st(false, true, false, false)),
            Skip(AlreadyInstalled)
        );
        assert_eq!(
            first_run_decision(&st(false, false, true, false)),
            Skip(DontAsk)
        );
        assert_eq!(
            first_run_decision(&st(false, false, false, true)),
            Skip(EnvDisabled)
        );
        assert_eq!(
            first_run_decision(&st(false, false, true, true)),
            Skip(EnvDisabled)
        );
    }

    struct Ui {
        choice: Choice,
        p: FixedPrompter,
        notes: Vec<(String, bool)>,
        asked: u32,
    }
    impl Ui {
        fn new(choice: Choice) -> Self {
            Ui {
                choice,
                p: FixedPrompter(false),
                notes: vec![],
                asked: 0,
            }
        }
    }
    impl FirstRunUi for Ui {
        fn ask_install(&mut self) -> Choice {
            self.asked += 1;
            self.choice
        }
        fn notify(&mut self, title: &str, body: &str, e: bool) {
            self.notes.push((format!("{title}|{body}"), e));
        }
        fn prompter(&mut self) -> &mut dyn Prompter {
            &mut self.p
        }
    }

    fn ok_install(_: &mut dyn Prompter) -> Result<Report> {
        let mut r = Report::default();
        r.act("installed x");
        Ok(r)
    }

    #[test]
    fn no_and_cancel_continue_and_marker() {
        let td = tempfile::tempdir().unwrap();
        let marker = marker_path(td.path());
        let mut called = false;
        let mut inst = |p: &mut dyn Prompter| {
            called = true;
            ok_install(p)
        };
        let mut ui = Ui::new(Choice::NotNow);
        assert_eq!(
            run(&mut ui, &marker, &mut inst, &mut || Ok(())),
            Outcome::Continue
        );
        assert!(!marker.exists());
        let mut ui = Ui::new(Choice::DontAsk);
        assert_eq!(
            run(&mut ui, &marker, &mut inst, &mut || Ok(())),
            Outcome::Continue
        );
        assert!(marker.is_file());
        assert!(!called && ui.notes.is_empty());
    }

    #[test]
    fn yes_installs_then_relaunches() {
        let td = tempfile::tempdir().unwrap();
        let mut ui = Ui::new(Choice::Install);
        let mut launched = 0;
        let o = run(
            &mut ui,
            &marker_path(td.path()),
            &mut ok_install,
            &mut || {
                launched += 1;
                Ok(())
            },
        );
        assert_eq!(o, Outcome::Relaunched);
        assert_eq!(launched, 1);
        assert!(ui.notes[0].0.contains("installed x") && !ui.notes[0].1);
    }

    #[test]
    fn install_failure_continues_with_error() {
        let td = tempfile::tempdir().unwrap();
        let mut ui = Ui::new(Choice::Install);
        let mut launched = false;
        let o = run(
            &mut ui,
            &marker_path(td.path()),
            &mut |_| crate::err("disk full"),
            &mut || {
                launched = true;
                Ok(())
            },
        );
        assert_eq!(o, Outcome::Continue);
        assert!(!launched);
        assert!(ui.notes[0].0.contains("disk full") && ui.notes[0].1);
    }

    #[test]
    fn relaunch_failure_continues() {
        let td = tempfile::tempdir().unwrap();
        let mut ui = Ui::new(Choice::Install);
        let o = run(
            &mut ui,
            &marker_path(td.path()),
            &mut ok_install,
            &mut || Err(io::Error::other("nope")),
        );
        assert_eq!(o, Outcome::Continue);
        assert!(ui.notes[0].1);
    }

    #[test]
    fn installer_questions_reach_the_ui_prompter() {
        let td = tempfile::tempdir().unwrap();
        let mut ui = Ui::new(Choice::Install);
        ui.p = FixedPrompter(true);
        let mut got = None;
        run(
            &mut ui,
            &marker_path(td.path()),
            &mut |p| {
                got = Some(p.confirm("Replace X with md-viewer? [y/N]"));
                ok_install(p)
            },
            &mut || Ok(()),
        );
        assert_eq!(got, Some(true));
    }

    #[test]
    fn helpers() {
        assert_eq!(
            dialog_question("Replace X with md-viewer? [y/N]"),
            "Replace X with md-viewer?"
        );
        assert!(path_in_dir(
            Path::new("C:\\A\\b\\md-viewer.exe"),
            Path::new("c:\\a\\B\\")
        ));
        assert!(!path_in_dir(
            Path::new("C:\\A\\b\\c\\md-viewer.exe"),
            Path::new("C:\\A\\b")
        ));
        assert!(!path_in_dir(
            Path::new("C:\\A\\bb\\md-viewer.exe"),
            Path::new("C:\\A\\b")
        ));
    }
}

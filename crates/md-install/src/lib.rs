//! User-mode installer for md-viewer: copies the binary, registers a desktop
//! entry / registry keys and (optionally) makes md-viewer the default `.md`
//! handler. Never requires root or elevation.
//!
//! All platform logic is expressed against small traits ([`Prompter`],
//! [`unix::MimeDb`], [`windows::Registry`]) so it can be unit tested with
//! fakes on any host. The `unix` and `windows` modules both compile on every
//! platform; only the *real* system backends are cfg-gated.

use std::fmt;
use std::io::{self, BufRead, IsTerminal, Write};

pub mod unix;
pub mod windows;

/// Application name used for file names, ProgIDs and state.
pub const APP: &str = "md-viewer";

#[derive(Debug)]
pub struct Error(pub String);
pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error(e.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error(e.to_string())
    }
}
#[allow(dead_code)]
pub(crate) fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error(msg.into()))
}

/// Options for [`install`].
#[derive(Debug, Clone, Copy, Default)]
pub struct InstallOptions {
    /// Replace an existing different default handler without asking.
    pub yes: bool,
    /// Install files only; never touch the default-application association.
    pub no_assoc: bool,
}

/// Options for [`uninstall`].
#[derive(Debug, Clone, Copy, Default)]
pub struct UninstallOptions {
    /// Do not restore the previously recorded default handler.
    pub no_restore: bool,
}

/// Human readable result of an operation.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// What was done (one line each).
    pub actions: Vec<String>,
    /// Warnings / hints for the user.
    pub notes: Vec<String>,
}

impl Report {
    pub(crate) fn act(&mut self, s: impl Into<String>) {
        self.actions.push(s.into());
    }
    pub(crate) fn note(&mut self, s: impl Into<String>) {
        self.notes.push(s.into());
    }
}
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for a in &self.actions {
            writeln!(f, "  {a}")?;
        }
        for n in &self.notes {
            writeln!(f, "  note: {n}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Missing,
}

#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
}

/// Result of [`doctor`].
#[derive(Debug, Clone, Default)]
pub struct DoctorReport {
    pub checks: Vec<Check>,
}
impl DoctorReport {
    pub(crate) fn add(&mut self, name: &str, status: Status, detail: impl Into<String>) {
        self.checks.push(Check {
            name: name.into(),
            status,
            detail: detail.into(),
        });
    }
    /// True if nothing is `Missing`.
    pub fn healthy(&self) -> bool {
        self.checks.iter().all(|c| c.status != Status::Missing)
    }
    pub fn get(&self, name: &str) -> Option<&Check> {
        self.checks.iter().find(|c| c.name == name)
    }
}
impl fmt::Display for DoctorReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in &self.checks {
            let tag = match c.status {
                Status::Ok => "ok     ",
                Status::Warn => "warn   ",
                Status::Missing => "MISSING",
            };
            writeln!(f, "[{tag}] {}: {}", c.name, c.detail)?;
        }
        Ok(())
    }
}

/// Asks the user yes/no questions.
pub trait Prompter {
    /// Returns true for "yes". Default answer is no.
    fn confirm(&mut self, question: &str) -> bool;
}

/// Prompts on stdin/stderr; answers "no" when stdin is not a terminal.
pub struct StdioPrompter;
impl Prompter for StdioPrompter {
    fn confirm(&mut self, question: &str) -> bool {
        if !io::stdin().is_terminal() {
            eprintln!("{question} (non-interactive: assuming no; use --yes to accept)");
            return false;
        }
        eprint!("{question} ");
        let _ = io::stderr().flush();
        let mut line = String::new();
        if io::stdin().lock().read_line(&mut line).is_err() {
            return false;
        }
        matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }
}

/// Fixed-answer prompter (useful for tests and non-interactive callers).
pub struct FixedPrompter(pub bool);
impl Prompter for FixedPrompter {
    fn confirm(&mut self, _q: &str) -> bool {
        self.0
    }
}

/// What to do about the default-application association.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssocDecision {
    /// `--no-assoc`: leave association alone.
    Disabled,
    /// md-viewer is already the default; nothing to change.
    AlreadyOurs,
    /// Make md-viewer the default. `prior` is the previous (foreign) default.
    Set {
        prior: Option<String>,
        replaced_other: bool,
    },
    /// A different app owns the association and the user said no.
    Declined { current: String },
}

/// Platform-neutral conflict policy shared by the unix and windows planners.
///
/// `current` is the present default handler id (desktop-file name or ProgID),
/// `ours` the ids that denote md-viewer, `label` how to show `current`.
pub fn decide_assoc(
    current: Option<&str>,
    ours: &[&str],
    opts: &InstallOptions,
    prompter: &mut dyn Prompter,
) -> AssocDecision {
    if opts.no_assoc {
        return AssocDecision::Disabled;
    }
    let current = current.map(str::trim).filter(|s| !s.is_empty());
    match current {
        None => AssocDecision::Set {
            prior: None,
            replaced_other: false,
        },
        Some(c) if ours.contains(&c) => AssocDecision::AlreadyOurs,
        Some(c) => {
            if opts.yes || prompter.confirm(&format!("Replace {c} with {APP}? [y/N]")) {
                AssocDecision::Set {
                    prior: Some(c.to_string()),
                    replaced_other: true,
                }
            } else {
                AssocDecision::Declined {
                    current: c.to_string(),
                }
            }
        }
    }
}

/// Install md-viewer for the current user on the running platform.
pub fn install(opts: &InstallOptions, prompter: &mut dyn Prompter) -> Result<Report> {
    #[cfg(unix)]
    {
        let env = unix::UnixEnv::from_env()?;
        let mime = unix::SystemMime::new(&env);
        unix::install_with(&env, &mime, prompter, opts)
    }
    #[cfg(windows)]
    {
        let env = windows::WinEnv::from_env()?;
        let mut reg = windows::RealRegistry;
        windows::install_with(&env, &mut reg, prompter, opts)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (opts, prompter);
        err("unsupported platform")
    }
}

/// Remove everything [`install`] created and restore the prior default.
pub fn uninstall(opts: &UninstallOptions) -> Result<Report> {
    #[cfg(unix)]
    {
        let env = unix::UnixEnv::from_env()?;
        let mime = unix::SystemMime::new(&env);
        unix::uninstall_with(&env, &mime, opts)
    }
    #[cfg(windows)]
    {
        let env = windows::WinEnv::from_env()?;
        let mut reg = windows::RealRegistry;
        windows::uninstall_with(&env, &mut reg, opts)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = opts;
        err("unsupported platform")
    }
}

/// Report installation, association and dependency state.
pub fn doctor() -> Result<DoctorReport> {
    #[cfg(unix)]
    {
        let env = unix::UnixEnv::from_env()?;
        let mime = unix::SystemMime::new(&env);
        Ok(unix::doctor_with(&env, &mime, &unix::SystemProbe))
    }
    #[cfg(windows)]
    {
        let env = windows::WinEnv::from_env()?;
        let reg = windows::RealRegistry;
        Ok(windows::doctor_with(&env, &reg))
    }
    #[cfg(not(any(unix, windows)))]
    {
        err("unsupported platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(cur: Option<&str>, o: InstallOptions, ans: bool) -> AssocDecision {
        decide_assoc(cur, &["md-viewer.desktop"], &o, &mut FixedPrompter(ans))
    }

    #[test]
    fn decisions() {
        let o = InstallOptions::default();
        assert_eq!(
            d(None, o, false),
            AssocDecision::Set {
                prior: None,
                replaced_other: false
            }
        );
        assert_eq!(
            d(Some("  "), o, false),
            AssocDecision::Set {
                prior: None,
                replaced_other: false
            }
        );
        assert_eq!(
            d(Some("md-viewer.desktop"), o, false),
            AssocDecision::AlreadyOurs
        );
        assert_eq!(
            d(Some("x.desktop"), o, true),
            AssocDecision::Set {
                prior: Some("x.desktop".into()),
                replaced_other: true
            }
        );
        assert_eq!(
            d(Some("x.desktop"), o, false),
            AssocDecision::Declined {
                current: "x.desktop".into()
            }
        );
        let yes = InstallOptions { yes: true, ..o };
        assert!(matches!(
            d(Some("x.desktop"), yes, false),
            AssocDecision::Set { .. }
        ));
        let no = InstallOptions {
            no_assoc: true,
            ..o
        };
        assert_eq!(d(None, no, true), AssocDecision::Disabled);
        assert_eq!(d(Some("x.desktop"), no, true), AssocDecision::Disabled);
    }
}

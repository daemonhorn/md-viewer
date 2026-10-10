//! Linux / FreeBSD (XDG) installer. Compiles on all hosts; only
//! [`SystemMime`] / [`SystemProbe`] touch external programs.

use crate::{
    decide_assoc, AssocDecision, DoctorReport, InstallOptions, Prompter, Report, Result,
    Status, UninstallOptions, APP,
};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const DESKTOP_ID: &str = "md-viewer.desktop";
/// MIME types we claim; the first is the canonical one used for conflict checks.
pub const MIMES: [&str; 2] = ["text/markdown", "text/x-markdown"];

/// Resolved directories. Build with [`UnixEnv::from_env`] or [`UnixEnv::new`].
#[derive(Debug, Clone)]
pub struct UnixEnv {
    pub home: PathBuf,
    pub data_home: PathBuf,
    pub config_home: PathBuf,
    pub bin_dir: PathBuf,
    /// The executable to install (normally `current_exe()`).
    pub exe: PathBuf,
    /// Raw `$PATH` for the doctor's on-PATH check.
    pub path_var: String,
}

fn abs_env(name: &str) -> Option<PathBuf> {
    // XDG spec: relative values are invalid and must be ignored.
    std::env::var_os(name).map(PathBuf::from).filter(|p| p.is_absolute())
}

impl UnixEnv {
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or_else(|| crate::Error("$HOME is not set".into()))?;
        let exe = std::env::current_exe()?;
        Ok(Self::new(&home, &exe, |n| abs_env(n)))
    }

    /// `lookup` resolves XDG_DATA_HOME, XDG_CONFIG_HOME, XDG_BIN_HOME.
    pub fn new(home: &Path, exe: &Path, lookup: impl Fn(&str) -> Option<PathBuf>) -> Self {
        UnixEnv {
            home: home.to_path_buf(),
            data_home: lookup("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share")),
            config_home: lookup("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config")),
            bin_dir: lookup("XDG_BIN_HOME").unwrap_or_else(|| home.join(".local/bin")),
            exe: exe.to_path_buf(),
            path_var: std::env::var("PATH").unwrap_or_default(),
        }
    }

    pub fn bin(&self) -> PathBuf {
        self.bin_dir.join(APP)
    }
    pub fn desktop_file(&self) -> PathBuf {
        self.data_home.join("applications").join(DESKTOP_ID)
    }
    pub fn applications_dir(&self) -> PathBuf {
        self.data_home.join("applications")
    }
    pub fn mime_root(&self) -> PathBuf {
        self.data_home.join("mime")
    }
    pub fn mime_xml(&self) -> PathBuf {
        self.mime_root().join("packages").join("md-viewer.xml")
    }
    pub fn icon(&self) -> PathBuf {
        self.data_home.join("icons/hicolor/scalable/apps/md-viewer.svg")
    }
    pub fn state_dir(&self) -> PathBuf {
        self.data_home.join(APP)
    }
    pub fn state_file(&self) -> PathBuf {
        self.state_dir().join("install.json")
    }
    pub fn mimeapps(&self) -> PathBuf {
        self.config_home.join("mimeapps.list")
    }
}

/// Persisted in `install.json` so uninstall can reverse the install.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
    pub version: String,
    pub bin: String,
    /// mime type -> default handler that was replaced (`None` = there was none).
    /// Only contains mime types for which this installer set the default.
    pub prior_defaults: BTreeMap<String, Option<String>>,
}

pub fn load_state(env: &UnixEnv) -> Option<State> {
    serde_json::from_slice(&fs::read(env.state_file()).ok()?).ok()
}

// ---------------------------------------------------------------- MimeDb

/// Access to the per-user default-application database.
pub trait MimeDb {
    /// Is the `xdg-mime` tool present (otherwise a file fallback is used)?
    fn has_xdg_mime(&self) -> bool;
    fn query_default(&self, mime: &str) -> Result<Option<String>>;
    fn set_default(&self, mime: &str, desktop_id: &str) -> Result<()>;
    fn unset_default(&self, mime: &str) -> Result<()>;
    /// Run update-desktop-database / update-mime-database when available;
    /// returns descriptions of what ran.
    fn refresh(&self) -> Vec<String>;
}

/// Real implementation: shells out to `xdg-mime`, else edits `mimeapps.list`.
pub struct SystemMime {
    home: PathBuf,
    data_home: PathBuf,
    config_home: PathBuf,
    apps_dir: PathBuf,
    mime_root: PathBuf,
}

impl SystemMime {
    pub fn new(env: &UnixEnv) -> Self {
        SystemMime {
            home: env.home.clone(),
            data_home: env.data_home.clone(),
            config_home: env.config_home.clone(),
            apps_dir: env.applications_dir(),
            mime_root: env.mime_root(),
        }
    }
    fn mimeapps(&self) -> PathBuf {
        self.config_home.join("mimeapps.list")
    }
    fn cmd(&self, prog: &str) -> Command {
        let mut c = Command::new(prog);
        c.env("HOME", &self.home)
            .env("XDG_DATA_HOME", &self.data_home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .stdin(Stdio::null());
        c
    }
}

impl MimeDb for SystemMime {
    fn has_xdg_mime(&self) -> bool {
        which("xdg-mime", &std::env::var("PATH").unwrap_or_default())
    }
    fn query_default(&self, mime: &str) -> Result<Option<String>> {
        if self.has_xdg_mime() {
            let out = self.cmd("xdg-mime").args(["query", "default", mime]).output()?;
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            return Ok(if s.is_empty() { None } else { Some(s) });
        }
        let text = fs::read_to_string(self.mimeapps()).unwrap_or_default();
        Ok(mimeapps_get(&text, mime))
    }
    fn set_default(&self, mime: &str, desktop_id: &str) -> Result<()> {
        if self.has_xdg_mime() {
            // xdg-mime fails if the config dir does not exist yet
            fs::create_dir_all(&self.config_home)?;
            let st = self.cmd("xdg-mime").args(["default", desktop_id, mime]).status()?;
            if st.success() {
                return Ok(());
            }
            // fall through to direct edit
        }
        edit_mimeapps(&self.mimeapps(), |t| mimeapps_set(t, mime, desktop_id))
    }
    fn unset_default(&self, mime: &str) -> Result<()> {
        // xdg-mime has no "unset"; edit the file directly.
        edit_mimeapps(&self.mimeapps(), |t| mimeapps_unset(t, mime))
    }
    fn refresh(&self) -> Vec<String> {
        let mut done = vec![];
        if which("update-desktop-database", &std::env::var("PATH").unwrap_or_default()) {
            let ok = self.cmd("update-desktop-database").arg(&self.apps_dir).status();
            if ok.map(|s| s.success()).unwrap_or(false) {
                done.push("update-desktop-database".to_string());
            }
        }
        if which("update-mime-database", &std::env::var("PATH").unwrap_or_default()) {
            let ok = self.cmd("update-mime-database").arg(&self.mime_root).status();
            if ok.map(|s| s.success()).unwrap_or(false) {
                done.push("update-mime-database".to_string());
            }
        }
        done
    }
}

fn edit_mimeapps(path: &Path, f: impl FnOnce(&str) -> String) -> Result<()> {
    let old = fs::read_to_string(path).unwrap_or_default();
    let new = f(&old);
    if new != old {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        fs::write(path, new)?;
    }
    Ok(())
}

pub fn which(cmd: &str, path_var: &str) -> bool {
    std::env::split_paths(path_var).any(|d| {
        let p = d.join(cmd);
        p.is_file() && {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::metadata(&p).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
            }
            #[cfg(not(unix))]
            {
                true
            }
        }
    })
}

// ------------------------------------------------- mimeapps.list editing

const DEFAULTS: &str = "[Default Applications]";

/// First handler listed for `mime` in the `[Default Applications]` section.
pub fn mimeapps_get(text: &str, mime: &str) -> Option<String> {
    let mut in_sec = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_sec = l == DEFAULTS;
        } else if in_sec {
            if let Some((k, v)) = l.split_once('=') {
                if k.trim() == mime {
                    return v.split(';').map(str::trim).find(|s| !s.is_empty()).map(String::from);
                }
            }
        }
    }
    None
}

/// Remove `mime` from `[Default Applications]` (other sections untouched).
pub fn mimeapps_unset(text: &str, mime: &str) -> String {
    let mut in_sec = false;
    let mut out = String::new();
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_sec = l == DEFAULTS;
        } else if in_sec && l.split_once('=').map(|(k, _)| k.trim() == mime).unwrap_or(false) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Set `mime=desktop;` in `[Default Applications]`, creating section/line.
pub fn mimeapps_set(text: &str, mime: &str, desktop: &str) -> String {
    let base = mimeapps_unset(text, mime);
    let entry = format!("{mime}={desktop};");
    let mut out = String::new();
    let mut done = false;
    for line in base.lines() {
        out.push_str(line);
        out.push('\n');
        if !done && line.trim() == DEFAULTS {
            out.push_str(&entry);
            out.push('\n');
            done = true;
        }
    }
    if !done {
        if !out.is_empty() && !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str(DEFAULTS);
        out.push('\n');
        out.push_str(&entry);
        out.push('\n');
    }
    out
}

// -------------------------------------------------------- file contents

pub fn desktop_entry(bin: &Path) -> String {
    let exec = bin.to_string_lossy();
    let exec = if exec.contains(' ') { format!("\"{exec}\"") } else { exec.into_owned() };
    format!(
        "[Desktop Entry]\nType=Application\nName=md-viewer\nGenericName=Markdown Viewer\n\
         Comment=Lightweight Markdown viewer\nExec={exec} %f\nIcon=md-viewer\n\
         Terminal=false\nCategories=Utility;Viewer;\nMimeType=text/markdown;text/x-markdown;\n\
         StartupNotify=true\n"
    )
}

pub const MIME_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">
  <mime-type type="text/markdown">
    <comment>Markdown document</comment>
    <glob pattern="*.md"/>
    <glob pattern="*.markdown"/>
    <alias type="text/x-markdown"/>
  </mime-type>
</mime-info>
"#;

pub const ICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="8" fill="#2b5fa8"/><text x="32" y="42" font-size="28" text-anchor="middle" fill="#fff" font-family="sans-serif">M&#8595;</text></svg>
"##;

fn write_file(path: &Path, data: &[u8], exec: bool) -> Result<()> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("tmp-install");
    fs::write(&tmp, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 }))?;
    }
    let _ = exec;
    fs::rename(&tmp, path)?;
    Ok(())
}

// -------------------------------------------------------------- install

pub fn install_with(
    env: &UnixEnv,
    mime: &dyn MimeDb,
    prompter: &mut dyn Prompter,
    opts: &InstallOptions,
) -> Result<Report> {
    let mut r = Report::default();
    let bin = env.bin();

    // Observe the default *before* writing our desktop file: once it exists,
    // xdg-mime falls back to it as an implicit handler.
    let current = mime.query_default(MIMES[0])?;

    // 1. binary (skip self-copy)
    let same = fs::canonicalize(&env.exe).ok() == fs::canonicalize(&bin).ok() && bin.exists();
    if same {
        r.act(format!("binary already at {}", bin.display()));
    } else {
        let data = fs::read(&env.exe)
            .map_err(|e| crate::Error(format!("cannot read {}: {e}", env.exe.display())))?;
        write_file(&bin, &data, true)?;
        r.act(format!("installed {}", bin.display()));
    }

    // 2. desktop entry, mime package, icon
    write_file(&env.desktop_file(), desktop_entry(&bin).as_bytes(), false)?;
    r.act(format!("wrote {}", env.desktop_file().display()));
    write_file(&env.mime_xml(), MIME_XML.as_bytes(), false)?;
    r.act(format!("wrote {}", env.mime_xml().display()));
    write_file(&env.icon(), ICON_SVG.as_bytes(), false)?;
    r.act(format!("wrote placeholder icon {}", env.icon().display()));

    for what in mime.refresh() {
        r.act(format!("ran {what}"));
    }

    // 3. association
    let mut state = load_state(env).unwrap_or_default();
    state.version = env!("CARGO_PKG_VERSION").to_string();
    state.bin = bin.to_string_lossy().into_owned();

    if !mime.has_xdg_mime() {
        r.note("xdg-mime not found (install xdg-utils); editing ~/.config/mimeapps.list directly");
    }
    match decide_assoc(current.as_deref(), &[DESKTOP_ID], opts, prompter) {
        AssocDecision::Disabled => r.act("skipped file association (--no-assoc)"),
        AssocDecision::AlreadyOurs => {
            // make sure the secondary type is covered too, keep recorded priors
            for m in &MIMES[1..] {
                if mime.query_default(m)?.as_deref() != Some(DESKTOP_ID) {
                    state_set(mime, &mut state, m, &mut r)?;
                }
            }
            r.act("md-viewer is already the default for Markdown");
        }
        AssocDecision::Declined { current } => {
            r.act(format!("kept {current} as default Markdown handler"));
            r.note(format!("run `{APP} install --yes` later to switch"));
        }
        AssocDecision::Set { prior, replaced_other } => {
            mime.set_default(MIMES[0], DESKTOP_ID)?;
            state.prior_defaults.insert(MIMES[0].to_string(), prior.clone());
            for m in &MIMES[1..] {
                state_set(mime, &mut state, m, &mut r)?;
            }
            match (replaced_other, prior) {
                (true, Some(p)) => r.act(format!("set default for Markdown (replaced {p})")),
                _ => r.act("set default for Markdown"),
            }
        }
    }

    // 4. state
    write_file(&env.state_file(), &serde_json::to_vec_pretty(&state)?, false)?;
    r.act(format!("recorded {}", env.state_file().display()));

    if !std::env::split_paths(&env.path_var).any(|p| p == env.bin_dir) {
        r.note(format!("{} is not on your PATH", env.bin_dir.display()));
    }
    Ok(r)
}

fn state_set(mime: &dyn MimeDb, st: &mut State, m: &str, _r: &mut Report) -> Result<()> {
    let prior = mime.query_default(m)?.filter(|p| p != DESKTOP_ID);
    mime.set_default(m, DESKTOP_ID)?;
    st.prior_defaults.entry(m.to_string()).or_insert(prior);
    Ok(())
}

// ------------------------------------------------------------ uninstall

pub fn uninstall_with(env: &UnixEnv, mime: &dyn MimeDb, opts: &UninstallOptions) -> Result<Report> {
    let mut r = Report::default();
    let state = load_state(env);
    if state.is_none() && !env.desktop_file().exists() && !env.bin().exists() {
        r.note("md-viewer does not appear to be installed");
        return Ok(r);
    }

    // association first (needs nothing but the DB)
    let mut keys: Vec<String> = MIMES.iter().map(|s| s.to_string()).collect();
    if let Some(s) = &state {
        for k in s.prior_defaults.keys() {
            if !keys.contains(k) {
                keys.push(k.clone());
            }
        }
    }
    for m in &keys {
        if mime.query_default(m)?.as_deref() != Some(DESKTOP_ID) {
            continue; // someone else owns it now; leave it
        }
        let prior = state.as_ref().and_then(|s| s.prior_defaults.get(m)).cloned().flatten();
        match prior {
            Some(p) if !opts.no_restore => {
                mime.set_default(m, &p)?;
                r.act(format!("restored {p} as default for {m}"));
            }
            _ => {
                mime.unset_default(m)?;
                r.act(format!("removed default for {m}"));
            }
        }
    }

    for p in [env.desktop_file(), env.mime_xml(), env.icon(), env.state_file()] {
        if p.exists() {
            fs::remove_file(&p)?;
            r.act(format!("removed {}", p.display()));
        }
    }
    let _ = fs::remove_dir(env.state_dir()); // only if empty
    let bin = env.bin();
    if bin.exists() {
        fs::remove_file(&bin)?;
        r.act(format!("removed {}", bin.display()));
    }
    for what in mime.refresh() {
        r.act(format!("ran {what}"));
    }
    Ok(r)
}

// --------------------------------------------------------------- doctor

pub trait Probe {
    fn has_command(&self, cmd: &str) -> bool;
    fn has_webkit(&self) -> bool;
}

pub struct SystemProbe;
impl Probe for SystemProbe {
    fn has_command(&self, cmd: &str) -> bool {
        which(cmd, &std::env::var("PATH").unwrap_or_default())
    }
    fn has_webkit(&self) -> bool {
        let ok = |c: &mut Command| {
            c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            c.status().map(|s| s.success()).unwrap_or(false)
        };
        if ok(Command::new("pkg-config").args(["--exists", "webkit2gtk-4.1"])) {
            return true;
        }
        // ldconfig is often outside a user's PATH
        for ld in ["ldconfig", "/sbin/ldconfig", "/usr/sbin/ldconfig"] {
            if let Ok(o) = Command::new(ld).arg("-p").stdin(Stdio::null()).output() {
                if String::from_utf8_lossy(&o.stdout).contains("libwebkit2gtk-4.1") {
                    return true;
                }
            }
        }
        // FreeBSD: ldconfig -r
        if let Ok(o) = Command::new("ldconfig").arg("-r").stdin(Stdio::null()).output() {
            if String::from_utf8_lossy(&o.stdout).contains("webkit2gtk-4.1") {
                return true;
            }
        }
        false
    }
}

pub fn doctor_with(env: &UnixEnv, mime: &dyn MimeDb, probe: &dyn Probe) -> DoctorReport {
    let mut d = DoctorReport::default();
    let bin = env.bin();
    if bin.is_file() {
        d.add("binary", Status::Ok, format!("installed at {}", bin.display()));
    } else {
        d.add("binary", Status::Missing, format!("not installed at {}", bin.display()));
    }
    if std::env::split_paths(&env.path_var).any(|p| p == env.bin_dir) {
        d.add("path", Status::Ok, format!("{} is on PATH", env.bin_dir.display()));
    } else {
        d.add("path", Status::Warn, format!("{} is not on PATH", env.bin_dir.display()));
    }
    for (name, p) in [("desktop-entry", env.desktop_file()), ("mime-package", env.mime_xml())] {
        if p.is_file() {
            d.add(name, Status::Ok, p.display().to_string());
        } else {
            d.add(name, Status::Missing, format!("{} not found", p.display()));
        }
    }
    if load_state(env).is_some() {
        d.add("install-record", Status::Ok, env.state_file().display().to_string());
    } else {
        d.add("install-record", Status::Warn, "no install.json (uninstall cannot restore a prior default)");
    }
    if mime.has_xdg_mime() {
        d.add("xdg-mime", Status::Ok, "found");
    } else {
        d.add("xdg-mime", Status::Warn, "not found (install xdg-utils); falling back to editing mimeapps.list");
    }
    for t in ["update-desktop-database", "update-mime-database"] {
        if probe.has_command(t) {
            d.add(t, Status::Ok, "found");
        } else {
            d.add(t, Status::Warn, "not found (optional; desktop may need re-login to notice changes)");
        }
    }
    match mime.query_default(MIMES[0]) {
        Ok(Some(c)) if c == DESKTOP_ID => d.add("association", Status::Ok, "md-viewer is the default for text/markdown"),
        Ok(Some(c)) => d.add("association", Status::Warn, format!("default for text/markdown is {c}")),
        Ok(None) => d.add("association", Status::Warn, "no default handler for text/markdown"),
        Err(e) => d.add("association", Status::Warn, format!("query failed: {e}")),
    }
    if probe.has_webkit() {
        d.add("libwebkit2gtk-4.1", Status::Ok, "found");
    } else {
        d.add(
            "libwebkit2gtk-4.1",
            Status::Missing,
            "not found; install it (Debian/Ubuntu: libwebkit2gtk-4.1-0, FreeBSD: webkit2-gtk4)",
        );
    }
    d
}

// ---------------------------------------------------------------- tests

/// In-memory [`MimeDb`] for tests.
#[derive(Default)]
pub struct FakeMime {
    pub xdg: bool,
    pub defaults: RefCell<BTreeMap<String, String>>,
    pub refreshed: RefCell<u32>,
}
impl FakeMime {
    pub fn with_default(mime: &str, app: &str) -> Self {
        let f = FakeMime { xdg: true, ..Default::default() };
        f.defaults.borrow_mut().insert(mime.into(), app.into());
        f
    }
}
impl MimeDb for FakeMime {
    fn has_xdg_mime(&self) -> bool {
        self.xdg
    }
    fn query_default(&self, m: &str) -> Result<Option<String>> {
        Ok(self.defaults.borrow().get(m).cloned())
    }
    fn set_default(&self, m: &str, d: &str) -> Result<()> {
        self.defaults.borrow_mut().insert(m.into(), d.into());
        Ok(())
    }
    fn unset_default(&self, m: &str) -> Result<()> {
        self.defaults.borrow_mut().remove(m);
        Ok(())
    }
    fn refresh(&self) -> Vec<String> {
        *self.refreshed.borrow_mut() += 1;
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FixedPrompter;

    struct Fixture {
        _td: tempfile::TempDir,
        env: UnixEnv,
    }
    fn fixture() -> Fixture {
        let td = tempfile::tempdir().unwrap();
        let exe = td.path().join("src-exe");
        fs::write(&exe, b"#!/bin/sh\necho hi\n").unwrap();
        let home = td.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let env = UnixEnv::new(&home, &exe, |_| None);
        Fixture { _td: td, env }
    }
    struct P(bool);
    impl Probe for P {
        fn has_command(&self, _: &str) -> bool {
            self.0
        }
        fn has_webkit(&self) -> bool {
            self.0
        }
    }

    #[test]
    fn xdg_vars_honored() {
        let e = UnixEnv::new(Path::new("/h"), Path::new("/x"), |n| match n {
            "XDG_DATA_HOME" => Some("/d".into()),
            "XDG_CONFIG_HOME" => Some("/c".into()),
            _ => None,
        });
        assert_eq!(e.desktop_file(), Path::new("/d/applications/md-viewer.desktop"));
        assert_eq!(e.mimeapps(), Path::new("/c/mimeapps.list"));
        assert_eq!(e.bin(), Path::new("/h/.local/bin/md-viewer"));
    }

    #[test]
    fn fresh_install() {
        let f = fixture();
        let m = FakeMime { xdg: true, ..Default::default() };
        let opts = InstallOptions::default();
        let r = install_with(&f.env, &m, &mut FixedPrompter(false), &opts).unwrap();
        assert!(!r.actions.is_empty());
        assert!(f.env.bin().is_file());
        let de = fs::read_to_string(f.env.desktop_file()).unwrap();
        assert!(de.contains("MimeType=text/markdown;text/x-markdown;"));
        assert!(de.contains(&format!("Exec={} %f", f.env.bin().display())));
        assert!(fs::read_to_string(f.env.mime_xml()).unwrap().contains("*.md"));
        assert!(f.env.icon().is_file());
        assert_eq!(m.query_default("text/markdown").unwrap().as_deref(), Some(DESKTOP_ID));
        assert_eq!(m.query_default("text/x-markdown").unwrap().as_deref(), Some(DESKTOP_ID));
        let st = load_state(&f.env).unwrap();
        assert_eq!(st.prior_defaults["text/markdown"], None);
        assert!(*m.refreshed.borrow() >= 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(f.env.bin()).unwrap().permissions().mode() & 0o777, 0o755);
        }
    }

    #[test]
    fn idempotent_and_keeps_prior() {
        let f = fixture();
        let m = FakeMime::with_default("text/markdown", "typora.desktop");
        let yes = InstallOptions { yes: true, ..Default::default() };
        install_with(&f.env, &m, &mut FixedPrompter(false), &yes).unwrap();
        // second run: no prompt needed, prior must survive
        let r = install_with(&f.env, &m, &mut FixedPrompter(false), &InstallOptions::default()).unwrap();
        assert!(r.actions.iter().any(|a| a.contains("already the default")));
        let st = load_state(&f.env).unwrap();
        assert_eq!(st.prior_defaults["text/markdown"].as_deref(), Some("typora.desktop"));
        assert_eq!(m.query_default("text/markdown").unwrap().as_deref(), Some(DESKTOP_ID));
    }

    #[test]
    fn conflict_declined() {
        let f = fixture();
        let m = FakeMime::with_default("text/markdown", "typora.desktop");
        let r = install_with(&f.env, &m, &mut FixedPrompter(false), &InstallOptions::default()).unwrap();
        assert_eq!(m.query_default("text/markdown").unwrap().as_deref(), Some("typora.desktop"));
        assert!(r.actions.iter().any(|a| a.contains("kept typora.desktop")));
        assert!(f.env.bin().is_file()); // files still installed
        assert!(load_state(&f.env).unwrap().prior_defaults.is_empty());
    }

    #[test]
    fn conflict_accepted_via_prompt() {
        let f = fixture();
        let m = FakeMime::with_default("text/markdown", "typora.desktop");
        install_with(&f.env, &m, &mut FixedPrompter(true), &InstallOptions::default()).unwrap();
        assert_eq!(m.query_default("text/markdown").unwrap().as_deref(), Some(DESKTOP_ID));
    }

    #[test]
    fn conflict_no_assoc() {
        let f = fixture();
        let m = FakeMime::with_default("text/markdown", "typora.desktop");
        let o = InstallOptions { no_assoc: true, yes: true };
        install_with(&f.env, &m, &mut FixedPrompter(true), &o).unwrap();
        assert_eq!(m.query_default("text/markdown").unwrap().as_deref(), Some("typora.desktop"));
        assert_eq!(m.query_default("text/x-markdown").unwrap(), None);
    }

    #[test]
    fn uninstall_restores_prior() {
        let f = fixture();
        let m = FakeMime::with_default("text/markdown", "typora.desktop");
        let yes = InstallOptions { yes: true, ..Default::default() };
        install_with(&f.env, &m, &mut FixedPrompter(false), &yes).unwrap();
        uninstall_with(&f.env, &m, &UninstallOptions::default()).unwrap();
        assert_eq!(m.query_default("text/markdown").unwrap().as_deref(), Some("typora.desktop"));
        assert_eq!(m.query_default("text/x-markdown").unwrap(), None);
        assert!(!f.env.bin().exists());
        assert!(!f.env.desktop_file().exists());
        assert!(!f.env.state_file().exists());
        assert!(!f.env.state_dir().exists());
        // second uninstall is a harmless no-op
        let r = uninstall_with(&f.env, &m, &UninstallOptions::default()).unwrap();
        assert!(r.actions.is_empty());
    }

    #[test]
    fn uninstall_fresh_unsets_and_leaves_foreign_default() {
        let f = fixture();
        let m = FakeMime { xdg: true, ..Default::default() };
        install_with(&f.env, &m, &mut FixedPrompter(false), &InstallOptions::default()).unwrap();
        // user switches to another app afterwards: uninstall must not clobber
        m.set_default("text/markdown", "other.desktop").unwrap();
        uninstall_with(&f.env, &m, &UninstallOptions::default()).unwrap();
        assert_eq!(m.query_default("text/markdown").unwrap().as_deref(), Some("other.desktop"));
        assert_eq!(m.query_default("text/x-markdown").unwrap(), None);
    }

    #[test]
    fn uninstall_no_restore() {
        let f = fixture();
        let m = FakeMime::with_default("text/markdown", "typora.desktop");
        let yes = InstallOptions { yes: true, ..Default::default() };
        install_with(&f.env, &m, &mut FixedPrompter(false), &yes).unwrap();
        uninstall_with(&f.env, &m, &UninstallOptions { no_restore: true }).unwrap();
        assert_eq!(m.query_default("text/markdown").unwrap(), None);
    }

    #[test]
    fn reinstall_over_existing_binary() {
        let f = fixture();
        let m = FakeMime { xdg: true, ..Default::default() };
        let o = InstallOptions::default();
        install_with(&f.env, &m, &mut FixedPrompter(false), &o).unwrap();
        // run "from" the installed binary itself: must not truncate it
        let mut env2 = f.env.clone();
        env2.exe = env2.bin();
        install_with(&env2, &m, &mut FixedPrompter(false), &o).unwrap();
        assert_eq!(fs::read(f.env.bin()).unwrap(), b"#!/bin/sh\necho hi\n");
    }

    #[test]
    fn mimeapps_roundtrip() {
        let t = "[Added Associations]\ntext/markdown=a.desktop;\n\n[Default Applications]\ntext/html=b.desktop;\ntext/markdown=typora.desktop;x.desktop;\n";
        assert_eq!(mimeapps_get(t, "text/markdown").as_deref(), Some("typora.desktop"));
        assert_eq!(mimeapps_get(t, "text/plain"), None);
        let s = mimeapps_set(t, "text/markdown", DESKTOP_ID);
        assert_eq!(mimeapps_get(&s, "text/markdown").as_deref(), Some(DESKTOP_ID));
        assert!(s.contains("[Added Associations]\ntext/markdown=a.desktop;"));
        assert!(s.contains("text/html=b.desktop;"));
        let u = mimeapps_unset(&s, "text/markdown");
        assert_eq!(mimeapps_get(&u, "text/markdown"), None);
        assert!(u.contains("text/markdown=a.desktop;")); // other section kept
        let n = mimeapps_set("", "text/markdown", DESKTOP_ID);
        assert_eq!(n, "[Default Applications]\ntext/markdown=md-viewer.desktop;\n");
    }

    #[test]
    fn doctor_states() {
        let f = fixture();
        let m = FakeMime { xdg: false, ..Default::default() };
        let d = doctor_with(&f.env, &m, &P(false));
        assert!(!d.healthy());
        assert_eq!(d.get("binary").unwrap().status, Status::Missing);
        assert_eq!(d.get("xdg-mime").unwrap().status, Status::Warn);
        assert_eq!(d.get("libwebkit2gtk-4.1").unwrap().status, Status::Missing);
        install_with(&f.env, &m, &mut FixedPrompter(false), &InstallOptions::default()).unwrap();
        let d = doctor_with(&f.env, &m, &P(true));
        assert!(d.healthy(), "{d}");
        assert_eq!(d.get("association").unwrap().status, Status::Ok);
    }
}

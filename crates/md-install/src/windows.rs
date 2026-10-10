//! Windows (HKCU-only) installer. The planner is pure and platform-neutral;
//! only [`RealRegistry`] (cfg(windows), uses `winreg`) touches the registry.

use crate::{
    decide_assoc, AssocDecision, DoctorReport, InstallOptions, Prompter, Report, Result, Status,
    UninstallOptions, APP,
};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const PROG_ID: &str = "md-viewer.md";
pub const APP_KEY: &str = "Applications\\md-viewer.exe";
const CLASSES: &str = "Software\\Classes";
const CAPS: &str = "Software\\md-viewer\\Capabilities";
const REGAPPS: &str = "Software\\RegisteredApplications";
const UNINSTALL: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\md-viewer";
const USERCHOICE: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\.md\\UserChoice";
const ENV_KEY: &str = "Environment";
const WEBVIEW2_KEY: &str =
    "SOFTWARE\\WOW6432Node\\Microsoft\\EdgeUpdate\\Clients\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
const WEBVIEW2_KEY_USER: &str =
    "Software\\Microsoft\\EdgeUpdate\\Clients\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hive {
    Cu,
    Lm,
}

/// Registry access. All writes are HKCU. Keys use `\` separators.
pub trait Registry {
    fn get_string(&self, hive: Hive, key: &str, name: &str) -> Result<Option<String>>;
    /// Set a REG_SZ value in HKCU, creating the key. `name == ""` is (Default).
    fn set_string(&mut self, key: &str, name: &str, value: &str) -> Result<()>;
    /// Set a REG_EXPAND_SZ value in HKCU.
    fn set_expand_string(&mut self, key: &str, name: &str, value: &str) -> Result<()>;
    fn delete_value(&mut self, key: &str, name: &str) -> Result<()>;
    /// Delete a key and all subkeys (no error if absent).
    fn delete_tree(&mut self, key: &str) -> Result<()>;
    /// Open the Windows "Default apps" settings page.
    fn open_default_apps(&mut self) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct WinEnv {
    pub local_appdata: PathBuf,
    pub exe: PathBuf,
}
impl WinEnv {
    pub fn from_env() -> Result<Self> {
        let local_appdata = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| crate::Error("%LOCALAPPDATA% is not set".into()))?;
        Ok(WinEnv { local_appdata, exe: std::env::current_exe()? })
    }
    pub fn install_dir(&self) -> PathBuf {
        self.local_appdata.join("Programs").join(APP)
    }
    pub fn bin(&self) -> PathBuf {
        self.install_dir().join("md-viewer.exe")
    }
    pub fn state_file(&self) -> PathBuf {
        self.install_dir().join("install.json")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
    pub version: String,
    /// True if we wrote HKCU\Software\Classes\.md (Default).
    pub set_default: bool,
    /// Value of Software\Classes\.md (Default) before we set it.
    pub prior_classes_default: Option<String>,
    /// UserChoice ProgId observed when a conflict was found (informational).
    pub prior_user_choice: Option<String>,
    pub path_added: bool,
}

/// One planned registry-level step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Set { key: String, name: String, value: String },
    DeleteTree { key: String },
    DeleteValue { key: String, name: String },
    PathAdd(String),
    PathRemove(String),
    OpenDefaultApps,
}

fn set(key: impl Into<String>, name: &str, value: impl Into<String>) -> Action {
    Action::Set { key: key.into(), name: name.into(), value: value.into() }
}

/// Pure planner: registry actions that register the app (and optionally the
/// `.md` default) for the binary at `exe` (a Windows-style path string).
pub fn plan_install(exe: &str, version: &str, set_default: bool, open_settings: bool) -> Vec<Action> {
    let cmd = format!("\"{exe}\" \"%1\"");
    let dir = exe.rfind(['\\', '/']).map(|i| &exe[..i]).unwrap_or(exe).to_string();
    let c = |s: &str| format!("{CLASSES}\\{s}");
    let mut a = vec![
        set(c(PROG_ID), "", "Markdown Document"),
        set(c(&format!("{PROG_ID}\\DefaultIcon")), "", format!("\"{exe}\",0")),
        set(c(&format!("{PROG_ID}\\shell\\open\\command")), "", cmd.clone()),
        set(c(APP_KEY), "FriendlyAppName", "md-viewer"),
        set(c(&format!("{APP_KEY}\\SupportedTypes")), ".md", ""),
        set(c(&format!("{APP_KEY}\\shell\\open\\command")), "", cmd),
        set(c(".md\\OpenWithProgids"), PROG_ID, ""),
        set(CAPS, "ApplicationName", "md-viewer"),
        set(CAPS, "ApplicationDescription", "Lightweight Markdown viewer"),
        set(format!("{CAPS}\\FileAssociations"), ".md", PROG_ID),
        set(REGAPPS, APP, CAPS),
        set(UNINSTALL, "DisplayName", "md-viewer"),
        set(UNINSTALL, "DisplayVersion", version),
        set(UNINSTALL, "InstallLocation", dir.as_str()),
        set(UNINSTALL, "DisplayIcon", exe),
        set(UNINSTALL, "UninstallString", format!("\"{exe}\" uninstall")),
        Action::PathAdd(dir),
    ];
    if set_default {
        a.push(set(c(".md"), "", PROG_ID));
    }
    if open_settings {
        a.push(Action::OpenDefaultApps);
    }
    a
}

/// Pure planner for uninstall. `restore` is `Some(prior)` when `.md` default
/// should be put back (inner `None` = delete the value).
pub fn plan_uninstall(dir: &str, restore: Option<Option<String>>) -> Vec<Action> {
    let c = |s: &str| format!("{CLASSES}\\{s}");
    let mut a = vec![
        Action::DeleteTree { key: c(PROG_ID) },
        Action::DeleteTree { key: c(APP_KEY) },
        Action::DeleteValue { key: c(".md\\OpenWithProgids"), name: PROG_ID.into() },
        Action::DeleteTree { key: "Software\\md-viewer".into() },
        Action::DeleteValue { key: REGAPPS.into(), name: APP.into() },
        Action::DeleteTree { key: UNINSTALL.into() },
        Action::PathRemove(dir.to_string()),
    ];
    match restore {
        Some(Some(p)) => a.push(set(c(".md"), "", p)),
        Some(None) => a.push(Action::DeleteValue { key: c(".md"), name: "".into() }),
        None => {}
    }
    a
}

/// Add `dir` to a `;`-separated PATH value if absent (case-insensitive).
pub fn path_add(existing: &str, dir: &str) -> String {
    let norm = |s: &str| s.trim_end_matches('\\').to_ascii_lowercase();
    if existing.split(';').any(|p| norm(p) == norm(dir)) {
        return existing.to_string();
    }
    if existing.trim().is_empty() {
        dir.to_string()
    } else {
        format!("{};{dir}", existing.trim_end_matches(';'))
    }
}

pub fn path_remove(existing: &str, dir: &str) -> String {
    let norm = |s: &str| s.trim_end_matches('\\').to_ascii_lowercase();
    existing
        .split(';')
        .filter(|p| !p.is_empty() && norm(p) != norm(dir))
        .collect::<Vec<_>>()
        .join(";")
}

pub fn apply(reg: &mut dyn Registry, actions: &[Action]) -> Result<()> {
    for a in actions {
        match a {
            Action::Set { key, name, value } => reg.set_string(key, name, value)?,
            Action::DeleteTree { key } => reg.delete_tree(key)?,
            Action::DeleteValue { key, name } => reg.delete_value(key, name)?,
            Action::PathAdd(d) => {
                let cur = reg.get_string(Hive::Cu, ENV_KEY, "Path")?.unwrap_or_default();
                let new = path_add(&cur, d);
                if new != cur {
                    reg.set_expand_string(ENV_KEY, "Path", &new)?;
                }
            }
            Action::PathRemove(d) => {
                if let Some(cur) = reg.get_string(Hive::Cu, ENV_KEY, "Path")? {
                    let new = path_remove(&cur, d);
                    if new != cur {
                        reg.set_expand_string(ENV_KEY, "Path", &new)?;
                    }
                }
            }
            Action::OpenDefaultApps => reg.open_default_apps()?,
        }
    }
    Ok(())
}

fn load_state(env: &WinEnv) -> Option<State> {
    serde_json::from_slice(&fs::read(env.state_file()).ok()?).ok()
}

fn user_choice(reg: &dyn Registry) -> Result<Option<String>> {
    reg.get_string(Hive::Cu, USERCHOICE, "ProgId")
}

fn winpath(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

pub fn install_with(
    env: &WinEnv,
    reg: &mut dyn Registry,
    prompter: &mut dyn Prompter,
    opts: &InstallOptions,
) -> Result<Report> {
    let mut r = Report::default();
    let bin = env.bin();
    let same = bin.exists() && fs::canonicalize(&env.exe).ok() == fs::canonicalize(&bin).ok();
    if same {
        r.act(format!("binary already at {}", bin.display()));
    } else {
        fs::create_dir_all(env.install_dir())?;
        let tmp = bin.with_extension("exe.new");
        fs::copy(&env.exe, &tmp)?;
        // a running exe cannot be overwritten but can be renamed away
        if bin.exists() {
            let _ = fs::remove_file(bin.with_extension("exe.old"));
            fs::rename(&bin, bin.with_extension("exe.old"))?;
        }
        fs::rename(&tmp, &bin)?;
        r.act(format!("installed {}", bin.display()));
    }

    let mut state = load_state(env).unwrap_or_default();
    state.version = env!("CARGO_PKG_VERSION").to_string();

    let classes_default = reg.get_string(Hive::Cu, &format!("{CLASSES}\\.md"), "")?;
    let uc = user_choice(reg)?;
    let decision = decide_assoc(uc.as_deref(), &[PROG_ID, APP_KEY], opts, prompter);
    // With no UserChoice, a foreign Classes\.md default is also a conflict.
    let decision = match (&decision, uc.as_deref()) {
        (AssocDecision::Set { prior: None, .. }, None)
            if classes_default.as_deref().map(|c| !c.is_empty() && c != PROG_ID).unwrap_or(false) =>
        {
            decide_assoc(classes_default.as_deref(), &[PROG_ID], opts, prompter)
        }
        _ => decision,
    };

    let (set_default, open_settings) = match &decision {
        AssocDecision::Disabled => {
            r.act("skipped file association (--no-assoc)");
            (false, false)
        }
        AssocDecision::AlreadyOurs => {
            r.act("md-viewer is already the default for .md");
            (false, false)
        }
        AssocDecision::Declined { current } => {
            r.act(format!("kept {current} as default for .md"));
            (false, false)
        }
        AssocDecision::Set { prior, replaced_other } => {
            if !state.set_default {
                state.prior_classes_default = classes_default.clone().filter(|c| c != PROG_ID);
            }
            state.set_default = true;
            if prior.is_some() {
                state.prior_user_choice = prior.clone();
            }
            if *replaced_other && uc.is_some() {
                r.note("Windows protects an existing choice: pick md-viewer for .md in the Settings page that opens");
            }
            r.act("registered md-viewer as the .md handler");
            (true, *replaced_other && uc.is_some())
        }
    };
    state.path_added = true;

    apply(reg, &plan_install(&winpath(&bin), &state.version, set_default, open_settings))?;
    r.act("wrote HKCU registration (ProgID, Applications, Capabilities, Uninstall) and PATH");
    r.note("open new terminals to see the updated PATH");
    fs::write(env.state_file(), serde_json::to_vec_pretty(&state)?)?;
    Ok(r)
}

pub fn uninstall_with(env: &WinEnv, reg: &mut dyn Registry, opts: &UninstallOptions) -> Result<Report> {
    let mut r = Report::default();
    let state = load_state(env);
    let installed = state.is_some() || env.bin().exists();
    if !installed && reg.get_string(Hive::Cu, &format!("{CLASSES}\\{PROG_ID}"), "")?.is_none() {
        r.note("md-viewer does not appear to be installed");
        return Ok(r);
    }
    let mut restore = None;
    if let Some(s) = &state {
        let cur = reg.get_string(Hive::Cu, &format!("{CLASSES}\\.md"), "")?;
        if s.set_default && cur.as_deref() == Some(PROG_ID) {
            restore = Some(if opts.no_restore { None } else { s.prior_classes_default.clone() });
        }
    }
    apply(reg, &plan_uninstall(&winpath(&env.install_dir()), restore))?;
    r.act("removed HKCU registration and PATH entry");

    let _ = fs::remove_file(env.state_file());
    let bin = env.bin();
    if bin.exists() && fs::remove_file(&bin).is_err() {
        // running exe: rename away; remove at next opportunity
        let old = bin.with_extension("exe.old");
        let _ = fs::remove_file(&old);
        let _ = fs::rename(&bin, &old);
        r.note(format!("{} is in use; delete {} manually if it remains", bin.display(), env.install_dir().display()));
    } else {
        r.act(format!("removed {}", bin.display()));
    }
    let _ = fs::remove_file(bin.with_extension("exe.old"));
    let _ = fs::remove_dir(env.install_dir());
    Ok(r)
}

pub fn doctor_with(env: &WinEnv, reg: &dyn Registry) -> DoctorReport {
    let mut d = DoctorReport::default();
    if env.bin().is_file() {
        d.add("binary", Status::Ok, env.bin().display().to_string());
    } else {
        d.add("binary", Status::Missing, format!("not installed at {}", env.bin().display()));
    }
    let g = |k: &str, n: &str| reg.get_string(Hive::Cu, k, n).ok().flatten();
    if g(&format!("{CLASSES}\\{PROG_ID}\\shell\\open\\command"), "").is_some() {
        d.add("progid", Status::Ok, PROG_ID);
    } else {
        d.add("progid", Status::Missing, "ProgID md-viewer.md is not registered");
    }
    if g(REGAPPS, APP).is_some() {
        d.add("capabilities", Status::Ok, "registered in RegisteredApplications");
    } else {
        d.add("capabilities", Status::Warn, "not in RegisteredApplications");
    }
    let dir = winpath(&env.install_dir());
    let on_path = g(ENV_KEY, "Path")
        .map(|p| p.split(';').any(|x| x.trim_end_matches('\\').eq_ignore_ascii_case(&dir)))
        .unwrap_or(false);
    d.add("path", if on_path { Status::Ok } else { Status::Warn }, if on_path { "install dir is on user PATH" } else { "install dir is not on user PATH" });
    match g(USERCHOICE, "ProgId") {
        Some(p) if p == PROG_ID || p == APP_KEY => d.add("association", Status::Ok, "md-viewer is the default for .md"),
        Some(p) => d.add("association", Status::Warn, format!("default for .md is {p}; change it in Settings > Default apps")),
        None => match g(&format!("{CLASSES}\\.md"), "") {
            Some(p) if p == PROG_ID => d.add("association", Status::Ok, "md-viewer is the default for .md"),
            Some(p) => d.add("association", Status::Warn, format!("class default for .md is {p}")),
            None => d.add("association", Status::Warn, "no default handler for .md"),
        },
    }
    let wv = [
        reg.get_string(Hive::Lm, WEBVIEW2_KEY, "pv"),
        reg.get_string(Hive::Cu, WEBVIEW2_KEY_USER, "pv"),
    ]
    .into_iter()
    .filter_map(|x| x.ok().flatten())
    .find(|v| !v.is_empty() && v != "0.0.0.0");
    match wv {
        Some(v) => d.add("WebView2", Status::Ok, format!("runtime {v}")),
        None => d.add("WebView2", Status::Missing, "WebView2 runtime not found; install from https://developer.microsoft.com/microsoft-edge/webview2/"),
    }
    d
}

// ----------------------------------------------------------- real impl

#[cfg(windows)]
pub use real::RealRegistry;

#[cfg(windows)]
mod real {
    use super::*;
    use std::io;
    use winreg::enums::*;
    use winreg::{RegKey, RegValue};

    pub struct RealRegistry;

    fn hive(h: Hive) -> RegKey {
        RegKey::predef(match h {
            Hive::Cu => HKEY_CURRENT_USER,
            Hive::Lm => HKEY_LOCAL_MACHINE,
        })
    }
    fn nf(e: io::Error) -> Result<()> {
        if e.kind() == io::ErrorKind::NotFound { Ok(()) } else { Err(e.into()) }
    }

    impl Registry for RealRegistry {
        fn get_string(&self, h: Hive, key: &str, name: &str) -> Result<Option<String>> {
            match hive(h).open_subkey(key) {
                Ok(k) => match k.get_value::<String, _>(name) {
                    Ok(v) => Ok(Some(v)),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e.into()),
                },
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.into()),
            }
        }
        fn set_string(&mut self, key: &str, name: &str, value: &str) -> Result<()> {
            let (k, _) = hive(Hive::Cu).create_subkey(key)?;
            k.set_value(name, &value.to_string())?;
            Ok(())
        }
        fn set_expand_string(&mut self, key: &str, name: &str, value: &str) -> Result<()> {
            let (k, _) = hive(Hive::Cu).create_subkey(key)?;
            let mut bytes: Vec<u8> = value.encode_utf16().chain(Some(0)).flat_map(|u| u.to_le_bytes()).collect();
            bytes.shrink_to_fit();
            k.set_raw_value(name, &RegValue { bytes: bytes.into(), vtype: REG_EXPAND_SZ })?;
            Ok(())
        }
        fn delete_value(&mut self, key: &str, name: &str) -> Result<()> {
            match hive(Hive::Cu).open_subkey_with_flags(key, KEY_SET_VALUE) {
                Ok(k) => k.delete_value(name).or_else(nf)?,
                Err(e) => nf(e)?,
            }
            Ok(())
        }
        fn delete_tree(&mut self, key: &str) -> Result<()> {
            hive(Hive::Cu).delete_subkey_all(key).or_else(nf)?;
            Ok(())
        }
        fn open_default_apps(&mut self) -> Result<()> {
            std::process::Command::new("cmd").args(["/C", "start", "", "ms-settings:defaultapps"]).status()?;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- tests

/// In-memory registry for tests: (hive, lowercase key) -> name -> value.
#[derive(Default)]
pub struct FakeRegistry {
    pub vals: BTreeMap<(Hive2, String), BTreeMap<String, String>>,
    pub opened_settings: RefCell<u32>,
}
// Hive lacks Ord; mirror it for map keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Hive2 {
    Cu,
    Lm,
}
impl From<Hive> for Hive2 {
    fn from(h: Hive) -> Self {
        match h {
            Hive::Cu => Hive2::Cu,
            Hive::Lm => Hive2::Lm,
        }
    }
}
impl FakeRegistry {
    pub fn put(&mut self, h: Hive, key: &str, name: &str, v: &str) {
        self.vals.entry((h.into(), key.to_ascii_lowercase())).or_default().insert(name.into(), v.into());
    }
    pub fn get(&self, key: &str, name: &str) -> Option<String> {
        self.vals.get(&(Hive2::Cu, key.to_ascii_lowercase()))?.get(name).cloned()
    }
}
impl Registry for FakeRegistry {
    fn get_string(&self, h: Hive, key: &str, name: &str) -> Result<Option<String>> {
        Ok(self.vals.get(&(h.into(), key.to_ascii_lowercase())).and_then(|m| m.get(name)).cloned())
    }
    fn set_string(&mut self, key: &str, name: &str, value: &str) -> Result<()> {
        self.put(Hive::Cu, key, name, value);
        Ok(())
    }
    fn set_expand_string(&mut self, key: &str, name: &str, value: &str) -> Result<()> {
        self.set_string(key, name, value)
    }
    fn delete_value(&mut self, key: &str, name: &str) -> Result<()> {
        if let Some(m) = self.vals.get_mut(&(Hive2::Cu, key.to_ascii_lowercase())) {
            m.remove(name);
        }
        Ok(())
    }
    fn delete_tree(&mut self, key: &str) -> Result<()> {
        let k = key.to_ascii_lowercase();
        let pre = format!("{k}\\");
        self.vals.retain(|(h, kk), _| !(*h == Hive2::Cu && (*kk == k || kk.starts_with(&pre))));
        Ok(())
    }
    fn open_default_apps(&mut self) -> Result<()> {
        *self.opened_settings.borrow_mut() += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FixedPrompter;

    fn fixture() -> (tempfile::TempDir, WinEnv) {
        let td = tempfile::tempdir().unwrap();
        let exe = td.path().join("src.exe");
        fs::write(&exe, b"MZ").unwrap();
        let env = WinEnv { local_appdata: td.path().join("Local"), exe };
        (td, env)
    }
    const CMD: &str = "Software\\Classes\\md-viewer.md\\shell\\open\\command";

    #[test]
    fn path_edit() {
        assert_eq!(path_add("", "C:\\a"), "C:\\a");
        assert_eq!(path_add("C:\\x;", "C:\\a"), "C:\\x;C:\\a");
        assert_eq!(path_add("C:\\x;c:\\A\\", "C:\\a"), "C:\\x;c:\\A\\");
        assert_eq!(path_remove("C:\\x;C:\\a;C:\\y", "c:\\A"), "C:\\x;C:\\y");
        assert_eq!(path_remove("C:\\a", "C:\\a"), "");
    }

    #[test]
    fn planner_contents() {
        let exe = "C:\\L\\Programs\\md-viewer\\md-viewer.exe";
        let a = plan_install(exe, "1.0", true, false);
        assert!(a.contains(&set(CMD, "", format!("\"{exe}\" \"%1\""))));
        assert!(a.contains(&Action::PathAdd("C:\\L\\Programs\\md-viewer".into())));
        assert!(a.contains(&set("Software\\Classes\\.md", "", PROG_ID)));
        assert!(a.contains(&set(REGAPPS, "md-viewer", CAPS)));
        assert!(!a.contains(&Action::OpenDefaultApps));
        // HKCU only: every key is relative (Software\... or Environment)
        let b = plan_install(exe, "1.0", false, true);
        assert!(!b.contains(&set("Software\\Classes\\.md", "", PROG_ID)));
        assert!(b.contains(&Action::OpenDefaultApps));
    }

    #[test]
    fn fresh_install_and_uninstall() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        reg.put(Hive::Cu, ENV_KEY, "Path", "C:\\Tools");
        install_with(&env, &mut reg, &mut FixedPrompter(false), &InstallOptions::default()).unwrap();
        assert!(env.bin().is_file());
        assert!(reg.get(CMD, "").unwrap().contains("md-viewer.exe"));
        assert_eq!(reg.get("Software\\Classes\\.md", "").as_deref(), Some(PROG_ID));
        assert!(reg.get(ENV_KEY, "Path").unwrap().starts_with("C:\\Tools;"));
        assert_eq!(*reg.opened_settings.borrow(), 0);
        assert!(doctor_with(&env, &reg).get("association").unwrap().status == Status::Ok);

        uninstall_with(&env, &mut reg, &UninstallOptions::default()).unwrap();
        assert_eq!(reg.get(CMD, ""), None);
        assert_eq!(reg.get("Software\\Classes\\.md", ""), None);
        assert_eq!(reg.get(REGAPPS, "md-viewer"), None);
        assert_eq!(reg.get(ENV_KEY, "Path").as_deref(), Some("C:\\Tools"));
        assert!(!env.bin().exists());
    }

    #[test]
    fn idempotent() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        let o = InstallOptions::default();
        install_with(&env, &mut reg, &mut FixedPrompter(false), &o).unwrap();
        let p1 = reg.get(ENV_KEY, "Path");
        install_with(&env, &mut reg, &mut FixedPrompter(false), &o).unwrap();
        assert_eq!(reg.get(ENV_KEY, "Path"), p1);
        assert_eq!(load_state(&env).unwrap().prior_classes_default, None);
    }

    #[test]
    fn user_choice_ours() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        reg.put(Hive::Cu, USERCHOICE, "ProgId", PROG_ID);
        let r = install_with(&env, &mut reg, &mut FixedPrompter(false), &InstallOptions::default()).unwrap();
        assert!(r.actions.iter().any(|a| a.contains("already the default")));
        assert_eq!(reg.get("Software\\Classes\\.md", ""), None);
    }

    #[test]
    fn conflict_accepted_opens_settings() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        reg.put(Hive::Cu, USERCHOICE, "ProgId", "Typora.md");
        install_with(&env, &mut reg, &mut FixedPrompter(true), &InstallOptions::default()).unwrap();
        assert_eq!(*reg.opened_settings.borrow(), 1);
        assert_eq!(reg.get(USERCHOICE, "ProgId").as_deref(), Some("Typora.md")); // never touched
        assert_eq!(load_state(&env).unwrap().prior_user_choice.as_deref(), Some("Typora.md"));
    }

    #[test]
    fn conflict_yes_flag() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        reg.put(Hive::Cu, USERCHOICE, "ProgId", "Typora.md");
        let o = InstallOptions { yes: true, ..Default::default() };
        install_with(&env, &mut reg, &mut FixedPrompter(false), &o).unwrap();
        assert_eq!(*reg.opened_settings.borrow(), 1);
    }

    #[test]
    fn conflict_declined_and_no_assoc() {
        for (o, ans) in [(InstallOptions::default(), false), (InstallOptions { no_assoc: true, yes: true }, true)] {
            let (_t, env) = fixture();
            let mut reg = FakeRegistry::default();
            reg.put(Hive::Cu, USERCHOICE, "ProgId", "Typora.md");
            install_with(&env, &mut reg, &mut FixedPrompter(ans), &o).unwrap();
            assert_eq!(*reg.opened_settings.borrow(), 0);
            assert_eq!(reg.get("Software\\Classes\\.md", ""), None);
            assert!(reg.get(CMD, "").is_some()); // app still registered
        }
    }

    #[test]
    fn foreign_classes_default_restored() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        reg.put(Hive::Cu, "Software\\Classes\\.md", "", "Other.md");
        let o = InstallOptions { yes: true, ..Default::default() };
        install_with(&env, &mut reg, &mut FixedPrompter(false), &o).unwrap();
        assert_eq!(reg.get("Software\\Classes\\.md", "").as_deref(), Some(PROG_ID));
        uninstall_with(&env, &mut reg, &UninstallOptions::default()).unwrap();
        assert_eq!(reg.get("Software\\Classes\\.md", "").as_deref(), Some("Other.md"));
    }

    #[test]
    fn doctor_webview() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        assert_eq!(doctor_with(&env, &reg).get("WebView2").unwrap().status, Status::Missing);
        reg.put(Hive::Lm, WEBVIEW2_KEY, "pv", "120.0.1");
        assert_eq!(doctor_with(&env, &reg).get("WebView2").unwrap().status, Status::Ok);
    }
}

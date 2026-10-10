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
/// Extensions md-viewer registers for. `.md` is the primary one; the others
/// are claimed as default only when no one owns them (see `plan_types`).
pub const EXTS: [&str; 5] = [".md", ".markdown", ".mdown", ".mkd", ".mkdn"];
/// MIME types registered under `Software\Classes\MIME\Database\Content Type`.
pub const MIME_TYPES: [&str; 2] = ["text/markdown", "text/x-markdown"];
const CONTENT_TYPE: &str = "text/markdown";
const PERCEIVED_TYPE: &str = "text";
const MIME_DB: &str = "MIME\\Database\\Content Type";
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
    /// Tell running programs (Explorer, new terminals) that the user
    /// environment changed (`WM_SETTINGCHANGE`, "Environment"). Best effort.
    fn broadcast_environment_change(&mut self) -> Result<()> {
        Ok(())
    }
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
        Ok(WinEnv {
            local_appdata,
            exe: std::env::current_exe()?,
        })
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
    /// True only if *we* appended the install dir to the user PATH (it was
    /// not already there); uninstall removes the entry only then.
    pub path_added: bool,
    /// Values we set under `Software\Classes\.md` ("Content Type",
    /// "PerceivedType") -> the value that was there before (`None` = absent).
    #[serde(default)]
    pub prior_values: BTreeMap<String, Option<String>>,
    /// Extensions (besides `.md`) whose class default we set because nobody
    /// owned them.
    #[serde(default)]
    pub claimed_exts: Vec<String>,
    /// MIME types whose `MIME\Database\Content Type` key we created.
    #[serde(default)]
    pub mime_keys: Vec<String>,
}

/// One planned registry-level step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Set {
        key: String,
        name: String,
        value: String,
    },
    DeleteTree {
        key: String,
    },
    DeleteValue {
        key: String,
        name: String,
    },
    PathAdd(String),
    PathRemove(String),
    OpenDefaultApps,
}

fn set(key: impl Into<String>, name: &str, value: impl Into<String>) -> Action {
    Action::Set {
        key: key.into(),
        name: name.into(),
        value: value.into(),
    }
}

/// Pure planner: registry actions that register the app (and optionally the
/// `.md` default) for the binary at `exe` (a Windows-style path string).
pub fn plan_install(
    exe: &str,
    version: &str,
    set_default: bool,
    open_settings: bool,
) -> Vec<Action> {
    let cmd = format!("\"{exe}\" \"%1\"");
    let dir = exe
        .rfind(['\\', '/'])
        .map(|i| &exe[..i])
        .unwrap_or(exe)
        .to_string();
    let c = |s: &str| format!("{CLASSES}\\{s}");
    let mut a = vec![
        set(c(PROG_ID), "", "Markdown Document"),
        set(
            c(&format!("{PROG_ID}\\DefaultIcon")),
            "",
            format!("\"{exe}\",0"),
        ),
        set(
            c(&format!("{PROG_ID}\\shell\\open\\command")),
            "",
            cmd.clone(),
        ),
        set(c(APP_KEY), "FriendlyAppName", "md-viewer"),
        set(
            c(&format!("{APP_KEY}\\DefaultIcon")),
            "",
            format!("\"{exe}\",0"),
        ),
        set(c(&format!("{APP_KEY}\\shell\\open\\command")), "", cmd),
        set(CAPS, "ApplicationName", "md-viewer"),
        set(
            CAPS,
            "ApplicationDescription",
            "Lightweight Markdown viewer",
        ),
        set(REGAPPS, APP, CAPS),
        set(UNINSTALL, "DisplayName", "md-viewer"),
        set(UNINSTALL, "DisplayVersion", version),
        set(UNINSTALL, "InstallLocation", dir.as_str()),
        set(UNINSTALL, "DisplayIcon", exe),
        set(UNINSTALL, "UninstallString", format!("\"{exe}\" uninstall")),
        Action::PathAdd(dir),
    ];
    for ext in EXTS {
        a.push(set(c(&format!("{APP_KEY}\\SupportedTypes")), ext, ""));
        a.push(set(c(&format!("{ext}\\OpenWithProgids")), PROG_ID, ""));
        a.push(set(format!("{CAPS}\\FileAssociations"), ext, PROG_ID));
    }
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
pub fn plan_uninstall(
    dir: &str,
    restore: Option<Option<String>>,
    remove_path: bool,
) -> Vec<Action> {
    let c = |s: &str| format!("{CLASSES}\\{s}");
    let mut a = vec![
        Action::DeleteTree { key: c(PROG_ID) },
        Action::DeleteTree { key: c(APP_KEY) },
        Action::DeleteTree {
            key: "Software\\md-viewer".into(),
        },
        Action::DeleteValue {
            key: REGAPPS.into(),
            name: APP.into(),
        },
        Action::DeleteTree {
            key: UNINSTALL.into(),
        },
    ];
    for ext in EXTS {
        a.push(Action::DeleteValue {
            key: c(&format!("{ext}\\OpenWithProgids")),
            name: PROG_ID.into(),
        });
    }
    if remove_path {
        a.push(Action::PathRemove(dir.to_string()));
    }
    match restore {
        Some(Some(p)) => a.push(set(c(".md"), "", p)),
        Some(None) => a.push(Action::DeleteValue {
            key: c(".md"),
            name: "".into(),
        }),
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

/// Remove every segment equal to `dir`; all other segments (including empty
/// ones) are kept exactly as they were.
pub fn path_remove(existing: &str, dir: &str) -> String {
    let norm = |s: &str| s.trim_end_matches('\\').to_ascii_lowercase();
    existing
        .split(';')
        .filter(|p| norm(p) != norm(dir))
        .collect::<Vec<_>>()
        .join(";")
}

pub fn apply(reg: &mut dyn Registry, actions: &[Action]) -> Result<()> {
    let mut env_changed = false;
    let res = apply_inner(reg, actions, &mut env_changed);
    if env_changed {
        // the registry is already updated; a failed broadcast only delays
        // when other programs notice
        let _ = reg.broadcast_environment_change();
    }
    res
}

fn apply_inner(reg: &mut dyn Registry, actions: &[Action], env_changed: &mut bool) -> Result<()> {
    for a in actions {
        match a {
            Action::Set { key, name, value } => reg.set_string(key, name, value)?,
            Action::DeleteTree { key } => reg.delete_tree(key)?,
            Action::DeleteValue { key, name } => reg.delete_value(key, name)?,
            Action::PathAdd(d) => {
                let cur = reg
                    .get_string(Hive::Cu, ENV_KEY, "Path")?
                    .unwrap_or_default();
                let new = path_add(&cur, d);
                if new != cur {
                    reg.set_expand_string(ENV_KEY, "Path", &new)?;
                    *env_changed = true;
                }
            }
            Action::PathRemove(d) => {
                if let Some(cur) = reg.get_string(Hive::Cu, ENV_KEY, "Path")? {
                    let new = path_remove(&cur, d);
                    if new != cur {
                        reg.set_expand_string(ENV_KEY, "Path", &new)?;
                        *env_changed = true;
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

/// True when an install record and the installed binary both exist.
pub fn is_installed(env: &WinEnv) -> bool {
    load_state(env).is_some() && env.bin().is_file()
}

fn classes(s: &str) -> String {
    format!("{CLASSES}\\{s}")
}

fn user_choice_for(reg: &dyn Registry, ext: &str) -> Result<Option<String>> {
    reg.get_string(
        Hive::Cu,
        &format!(
            "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\{ext}\\UserChoice"
        ),
        "ProgId",
    )
}

/// Content-type / MIME / secondary-extension registration, decided against
/// the live registry. Records what it touches in `state` so uninstall can
/// undo exactly that.
///
/// * `own_md`: md-viewer owns `.md` (so a foreign `Content Type` there may be
///   replaced; otherwise only an absent value is filled in).
/// * `claim_exts`: make md-viewer the class default of `.markdown`, `.mdown`,
///   `.mkd`, `.mkdn` when nobody owns them (no class default, no UserChoice).
fn plan_types(
    reg: &dyn Registry,
    state: &mut State,
    own_md: bool,
    claim_exts: bool,
) -> Result<Vec<Action>> {
    let mut a = vec![];
    let md = classes(".md");
    for (name, val) in [
        ("Content Type", CONTENT_TYPE),
        ("PerceivedType", PERCEIVED_TYPE),
    ] {
        let cur = reg.get_string(Hive::Cu, &md, name)?;
        match cur.as_deref() {
            Some(v) if v == val => {}
            None => {
                state.prior_values.entry(name.into()).or_insert(None);
                a.push(set(md.as_str(), name, val));
            }
            Some(_) if own_md => {
                state.prior_values.entry(name.into()).or_insert(cur.clone());
                a.push(set(md.as_str(), name, val));
            }
            Some(_) => {} // someone else's value and we do not own .md
        }
    }
    for mt in MIME_TYPES {
        let key = classes(&format!("{MIME_DB}\\{mt}"));
        // present (ours already, or foreign): never overwrite
        if reg.get_string(Hive::Cu, &key, "Extension")?.is_none() {
            if !state.mime_keys.iter().any(|m| m == mt) {
                state.mime_keys.push(mt.to_string());
            }
            a.push(set(key, "Extension", ".md"));
        }
    }
    if claim_exts {
        for ext in &EXTS[1..] {
            let cur = reg.get_string(Hive::Cu, &classes(ext), "")?;
            let unowned = cur.as_deref().map(str::is_empty).unwrap_or(true)
                && user_choice_for(reg, ext)?.is_none();
            if unowned {
                if !state.claimed_exts.iter().any(|e| e == ext) {
                    state.claimed_exts.push(ext.to_string());
                }
                a.push(set(classes(ext), "", PROG_ID));
            }
        }
    }
    Ok(a)
}

/// Undo `plan_types` using the install record; only touches values that still
/// hold what we wrote.
fn plan_uninstall_types(
    reg: &dyn Registry,
    state: &State,
    no_restore: bool,
) -> Result<Vec<Action>> {
    let mut a = vec![];
    let md = classes(".md");
    for (name, prior) in &state.prior_values {
        let ours = match name.as_str() {
            "Content Type" => CONTENT_TYPE,
            "PerceivedType" => PERCEIVED_TYPE,
            _ => continue,
        };
        if reg.get_string(Hive::Cu, &md, name)?.as_deref() != Some(ours) {
            continue;
        }
        match prior {
            Some(p) if !no_restore => a.push(set(md.as_str(), name, p.as_str())),
            _ => a.push(Action::DeleteValue {
                key: md.clone(),
                name: name.clone(),
            }),
        }
    }
    for ext in &state.claimed_exts {
        if reg.get_string(Hive::Cu, &classes(ext), "")?.as_deref() == Some(PROG_ID) {
            a.push(Action::DeleteValue {
                key: classes(ext),
                name: "".into(),
            });
        }
    }
    for mt in &state.mime_keys {
        let key = classes(&format!("{MIME_DB}\\{mt}"));
        if reg.get_string(Hive::Cu, &key, "Extension")?.as_deref() == Some(".md") {
            a.push(Action::DeleteTree { key });
        }
    }
    Ok(a)
}

fn user_choice(reg: &dyn Registry) -> Result<Option<String>> {
    reg.get_string(Hive::Cu, USERCHOICE, "ProgId")
}

/// Move an installed (possibly running, hence undeletable) binary out of the
/// way. Tries `<bin>.old` first, clearing a stale one if it can; if that name
/// is stuck (still mapped by a running process) falls back to `.old1`..`.old9`.
/// Returns where the binary went.
pub fn retire_binary(bin: &Path) -> std::io::Result<PathBuf> {
    let mut last = None;
    for n in 0..10 {
        let ext = if n == 0 {
            "exe.old".to_string()
        } else {
            format!("exe.old{n}")
        };
        let cand = bin.with_extension(ext);
        let _ = fs::remove_file(&cand); // tolerate failure: it may be in use
        match fs::rename(bin, &cand) {
            Ok(()) => return Ok(cand),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap())
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
        let retired = if bin.exists() {
            match retire_binary(&bin) {
                Ok(p) => Some(p),
                Err(e) => {
                    let _ = fs::remove_file(&tmp);
                    return Err(e.into());
                }
            }
        } else {
            None
        };
        if let Err(e) = fs::rename(&tmp, &bin) {
            if let Some(old) = &retired {
                let _ = fs::rename(old, &bin); // put the old one back
            }
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
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
            if classes_default
                .as_deref()
                .map(|c| !c.is_empty() && c != PROG_ID)
                .unwrap_or(false) =>
        {
            decide_assoc(classes_default.as_deref(), &[PROG_ID], opts, prompter)
        }
        _ => decision,
    };

    let own_md = matches!(decision, AssocDecision::Set { .. });
    let claim_exts = matches!(
        decision,
        AssocDecision::Set { .. } | AssocDecision::AlreadyOurs
    );
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
        AssocDecision::Set {
            prior,
            replaced_other,
        } => {
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
    // Only claim the PATH entry if it is not already there: a pre-existing
    // user entry must survive uninstall. A previous run's claim is kept.
    let dir = winpath(&env.install_dir());
    let cur_path = reg
        .get_string(Hive::Cu, ENV_KEY, "Path")?
        .unwrap_or_default();
    if path_add(&cur_path, &dir) != cur_path {
        state.path_added = true;
    }

    let extra = plan_types(reg, &mut state, own_md, claim_exts)?;

    // Record intent first so a failure part-way through `apply` still leaves
    // uninstall enough to undo it (uninstall re-checks the live registry).
    fs::write(env.state_file(), serde_json::to_vec_pretty(&state)?)?;
    let mut plan = plan_install(&winpath(&bin), &state.version, set_default, open_settings);
    plan.extend(extra);
    apply(reg, &plan)?;
    r.act("wrote HKCU registration (ProgID, Applications, Capabilities, Uninstall, text/markdown content type, extensions) and PATH");
    r.note("open new terminals to see the updated PATH");
    Ok(r)
}

pub fn uninstall_with(
    env: &WinEnv,
    reg: &mut dyn Registry,
    opts: &UninstallOptions,
) -> Result<Report> {
    let mut r = Report::default();
    let state = load_state(env);
    let installed = state.is_some() || env.bin().exists();
    if !installed
        && reg
            .get_string(Hive::Cu, &format!("{CLASSES}\\{PROG_ID}"), "")?
            .is_none()
    {
        r.note("md-viewer does not appear to be installed");
        return Ok(r);
    }
    let mut restore = None;
    if let Some(s) = &state {
        let cur = reg.get_string(Hive::Cu, &format!("{CLASSES}\\.md"), "")?;
        if s.set_default && cur.as_deref() == Some(PROG_ID) {
            restore = Some(if opts.no_restore {
                None
            } else {
                s.prior_classes_default.clone()
            });
        }
    }
    let remove_path = state.as_ref().is_some_and(|s| s.path_added);
    let mut plan = plan_uninstall(&winpath(&env.install_dir()), restore, remove_path);
    if let Some(s) = &state {
        plan.extend(plan_uninstall_types(reg, s, opts.no_restore)?);
    }
    apply(reg, &plan)?;
    if remove_path {
        r.act("removed HKCU registration and PATH entry");
    } else {
        r.act("removed HKCU registration");
        r.note("left the user PATH untouched (md-viewer did not add its entry)");
    }

    let _ = fs::remove_file(env.state_file());
    let bin = env.bin();
    if bin.exists() && fs::remove_file(&bin).is_err() {
        // running exe: rename away; remove at next opportunity
        let _ = retire_binary(&bin);
        r.note(format!(
            "{} is in use; delete {} manually if it remains",
            bin.display(),
            env.install_dir().display()
        ));
    } else {
        r.act(format!("removed {}", bin.display()));
    }
    let _ = fs::remove_file(bin.with_extension("exe.old"));
    for n in 1..10 {
        let _ = fs::remove_file(bin.with_extension(format!("exe.old{n}")));
    }
    let _ = fs::remove_dir(env.install_dir());
    Ok(r)
}

pub fn doctor_with(env: &WinEnv, reg: &dyn Registry) -> DoctorReport {
    let mut d = DoctorReport::default();
    if env.bin().is_file() {
        d.add("binary", Status::Ok, env.bin().display().to_string());
    } else {
        d.add(
            "binary",
            Status::Missing,
            format!("not installed at {}", env.bin().display()),
        );
    }
    let g = |k: &str, n: &str| reg.get_string(Hive::Cu, k, n).ok().flatten();
    if g(&format!("{CLASSES}\\{PROG_ID}\\shell\\open\\command"), "").is_some() {
        d.add("progid", Status::Ok, PROG_ID);
    } else {
        d.add(
            "progid",
            Status::Missing,
            "ProgID md-viewer.md is not registered",
        );
    }
    if g(REGAPPS, APP).is_some() {
        d.add(
            "capabilities",
            Status::Ok,
            "registered in RegisteredApplications",
        );
    } else {
        d.add(
            "capabilities",
            Status::Warn,
            "not in RegisteredApplications",
        );
    }
    let md_ct = g(&classes(".md"), "Content Type");
    let mime_ok = MIME_TYPES
        .iter()
        .all(|mt| g(&classes(&format!("{MIME_DB}\\{mt}")), "Extension").as_deref() == Some(".md"));
    match (md_ct.as_deref(), mime_ok) {
        (Some(CONTENT_TYPE), true) => d.add(
            "content-type",
            Status::Ok,
            ".md is text/markdown (MIME database entries present)",
        ),
        (Some(CONTENT_TYPE), false) => d.add(
            "content-type",
            Status::Warn,
            ".md is text/markdown but MIME\\Database entries are missing",
        ),
        (other, _) => d.add(
            "content-type",
            Status::Warn,
            format!(".md Content Type is {}", other.unwrap_or("not set")),
        ),
    }
    if g(&classes(&format!("{PROG_ID}\\DefaultIcon")), "").is_some()
        && g(&classes(&format!("{APP_KEY}\\DefaultIcon")), "").is_some()
    {
        d.add(
            "icon",
            Status::Ok,
            "document and application icon registered",
        );
    } else {
        d.add("icon", Status::Warn, "icon not registered (re-run install)");
    }
    let dir = winpath(&env.install_dir());
    let on_path = g(ENV_KEY, "Path")
        .map(|p| {
            p.split(';')
                .any(|x| x.trim_end_matches('\\').eq_ignore_ascii_case(&dir))
        })
        .unwrap_or(false);
    d.add(
        "path",
        if on_path { Status::Ok } else { Status::Warn },
        if on_path {
            "install dir is on user PATH"
        } else {
            "install dir is not on user PATH"
        },
    );
    match g(USERCHOICE, "ProgId") {
        Some(p) if p == PROG_ID || p == APP_KEY => d.add(
            "association",
            Status::Ok,
            "md-viewer is the default for .md",
        ),
        Some(p) => d.add(
            "association",
            Status::Warn,
            format!("default for .md is {p}; change it in Settings > Default apps"),
        ),
        None => match g(&format!("{CLASSES}\\.md"), "") {
            Some(p) if p == PROG_ID => d.add(
                "association",
                Status::Ok,
                "md-viewer is the default for .md",
            ),
            Some(p) => d.add(
                "association",
                Status::Warn,
                format!("class default for .md is {p}"),
            ),
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

    /// Minimal user32 FFI (no extra crates): broadcast `WM_SETTINGCHANGE` with
    /// lParam = "Environment" so Explorer and newly started programs re-read
    /// the user environment. Same call setx/installers use.
    mod ffi {
        use super::Result;

        const HWND_BROADCAST: isize = 0xffff;
        const WM_SETTINGCHANGE: u32 = 0x001A;
        const SMTO_ABORTIFHUNG: u32 = 0x0002;

        #[link(name = "user32")]
        extern "system" {
            // LRESULT SendMessageTimeoutW(HWND, UINT, WPARAM, LPARAM, UINT, UINT, PDWORD_PTR)
            fn SendMessageTimeoutW(
                hwnd: isize,
                msg: u32,
                wparam: usize,
                lparam: isize,
                flags: u32,
                timeout_ms: u32,
                result: *mut usize,
            ) -> isize;
        }

        pub fn broadcast_environment_change() -> Result<()> {
            let wide: Vec<u16> = "Environment\0".encode_utf16().collect();
            let mut res: usize = 0;
            // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives
            // the call; the call is synchronous (bounded by the timeout), and
            // `res` is a valid out pointer.
            let ok = unsafe {
                SendMessageTimeoutW(
                    HWND_BROADCAST,
                    WM_SETTINGCHANGE,
                    0,
                    wide.as_ptr() as isize,
                    SMTO_ABORTIFHUNG,
                    5000,
                    &mut res,
                )
            };
            if ok == 0 {
                // 0 = failure or timeout of every window; not fatal
                return Err(crate::Error(
                    "broadcasting the environment change failed or timed out".into(),
                ));
            }
            Ok(())
        }
    }

    fn hive(h: Hive) -> RegKey {
        RegKey::predef(match h {
            Hive::Cu => HKEY_CURRENT_USER,
            Hive::Lm => HKEY_LOCAL_MACHINE,
        })
    }
    fn nf(e: io::Error) -> Result<()> {
        if e.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(e.into())
        }
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
            let mut bytes: Vec<u8> = value
                .encode_utf16()
                .chain(Some(0))
                .flat_map(|u| u.to_le_bytes())
                .collect();
            bytes.shrink_to_fit();
            k.set_raw_value(
                name,
                &RegValue {
                    bytes: bytes.into(),
                    vtype: REG_EXPAND_SZ,
                },
            )?;
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
        fn broadcast_environment_change(&mut self) -> Result<()> {
            ffi::broadcast_environment_change()
        }
        fn open_default_apps(&mut self) -> Result<()> {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "start", "", "ms-settings:defaultapps"]);
            crate::no_window(&mut c).status()?;
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
    pub broadcasts: u32,
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
        self.vals
            .entry((h.into(), key.to_ascii_lowercase()))
            .or_default()
            .insert(name.into(), v.into());
    }
    pub fn get(&self, key: &str, name: &str) -> Option<String> {
        self.vals
            .get(&(Hive2::Cu, key.to_ascii_lowercase()))?
            .get(name)
            .cloned()
    }
}
impl Registry for FakeRegistry {
    fn get_string(&self, h: Hive, key: &str, name: &str) -> Result<Option<String>> {
        Ok(self
            .vals
            .get(&(h.into(), key.to_ascii_lowercase()))
            .and_then(|m| m.get(name))
            .cloned())
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
        self.vals
            .retain(|(h, kk), _| !(*h == Hive2::Cu && (*kk == k || kk.starts_with(&pre))));
        Ok(())
    }
    fn open_default_apps(&mut self) -> Result<()> {
        *self.opened_settings.borrow_mut() += 1;
        Ok(())
    }
    fn broadcast_environment_change(&mut self) -> Result<()> {
        self.broadcasts += 1;
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
        let env = WinEnv {
            local_appdata: td.path().join("Local"),
            exe,
        };
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
        // unrelated segments, including empty ones, are preserved as-is
        assert_eq!(path_remove("C:\\x;;C:\\a;C:\\y;", "C:\\a"), "C:\\x;;C:\\y;");
        assert_eq!(path_remove("C:\\x;;C:\\y", "C:\\a"), "C:\\x;;C:\\y");
    }

    #[test]
    fn preexisting_path_entry_survives_uninstall() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        let dir = winpath(&env.install_dir());
        let before = format!("C:\\Tools;{dir}\\;;C:\\Z");
        reg.put(Hive::Cu, ENV_KEY, "Path", &before);
        let o = InstallOptions::default();
        install_with(&env, &mut reg, &mut FixedPrompter(false), &o).unwrap();
        assert!(!load_state(&env).unwrap().path_added);
        assert_eq!(reg.get(ENV_KEY, "Path").as_deref(), Some(before.as_str()));
        assert_eq!(reg.broadcasts, 0); // nothing changed, nothing to announce
                                       // reinstall keeps the (false) claim
        install_with(&env, &mut reg, &mut FixedPrompter(false), &o).unwrap();
        assert!(!load_state(&env).unwrap().path_added);
        let r = uninstall_with(&env, &mut reg, &UninstallOptions::default()).unwrap();
        assert_eq!(reg.get(ENV_KEY, "Path").as_deref(), Some(before.as_str()));
        assert!(r.notes.iter().any(|n| n.contains("PATH")));
    }

    #[test]
    fn added_path_entry_removed_and_broadcast() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        reg.put(Hive::Cu, ENV_KEY, "Path", "C:\\Tools;;C:\\Z");
        let o = InstallOptions::default();
        install_with(&env, &mut reg, &mut FixedPrompter(false), &o).unwrap();
        assert!(load_state(&env).unwrap().path_added);
        assert_eq!(reg.broadcasts, 1);
        // second install: already present, claim kept, no new broadcast
        install_with(&env, &mut reg, &mut FixedPrompter(false), &o).unwrap();
        assert!(load_state(&env).unwrap().path_added);
        assert_eq!(reg.broadcasts, 1);
        uninstall_with(&env, &mut reg, &UninstallOptions::default()).unwrap();
        assert_eq!(
            reg.get(ENV_KEY, "Path").as_deref(),
            Some("C:\\Tools;;C:\\Z") // empty segment preserved
        );
        assert_eq!(reg.broadcasts, 2);
    }

    #[test]
    fn uninstall_without_record_leaves_path() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        let dir = winpath(&env.install_dir());
        install_with(
            &env,
            &mut reg,
            &mut FixedPrompter(false),
            &InstallOptions::default(),
        )
        .unwrap();
        fs::remove_file(env.state_file()).unwrap();
        uninstall_with(&env, &mut reg, &UninstallOptions::default()).unwrap();
        assert!(reg.get(ENV_KEY, "Path").unwrap().contains(&dir));
    }

    #[test]
    fn reinstall_survives_stuck_old_binary() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        let o = InstallOptions::default();
        install_with(&env, &mut reg, &mut FixedPrompter(false), &o).unwrap();
        // simulate a held-open .old that cannot be removed or overwritten
        // (a non-empty directory blocks both remove_file and rename)
        let old = env.bin().with_extension("exe.old");
        fs::create_dir_all(old.join("held")).unwrap();
        fs::write(&env.exe, b"MZ-new").unwrap();
        install_with(&env, &mut reg, &mut FixedPrompter(false), &o).unwrap();
        assert_eq!(fs::read(env.bin()).unwrap(), b"MZ-new");
        assert!(env.bin().with_extension("exe.old1").is_file());
    }

    #[test]
    fn retire_binary_prefers_plain_old_and_replaces_stale() {
        let (_t, env) = fixture();
        fs::create_dir_all(env.install_dir()).unwrap();
        let bin = env.bin();
        fs::write(&bin, b"cur").unwrap();
        fs::write(bin.with_extension("exe.old"), b"stale").unwrap();
        let p = retire_binary(&bin).unwrap();
        assert_eq!(p, bin.with_extension("exe.old"));
        assert_eq!(fs::read(p).unwrap(), b"cur");
        assert!(!bin.exists());
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
        install_with(
            &env,
            &mut reg,
            &mut FixedPrompter(false),
            &InstallOptions::default(),
        )
        .unwrap();
        assert!(env.bin().is_file());
        assert!(reg.get(CMD, "").unwrap().contains("md-viewer.exe"));
        assert_eq!(
            reg.get("Software\\Classes\\.md", "").as_deref(),
            Some(PROG_ID)
        );
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
        let r = install_with(
            &env,
            &mut reg,
            &mut FixedPrompter(false),
            &InstallOptions::default(),
        )
        .unwrap();
        assert!(r.actions.iter().any(|a| a.contains("already the default")));
        assert_eq!(reg.get("Software\\Classes\\.md", ""), None);
    }

    #[test]
    fn conflict_accepted_opens_settings() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        reg.put(Hive::Cu, USERCHOICE, "ProgId", "Typora.md");
        install_with(
            &env,
            &mut reg,
            &mut FixedPrompter(true),
            &InstallOptions::default(),
        )
        .unwrap();
        assert_eq!(*reg.opened_settings.borrow(), 1);
        assert_eq!(reg.get(USERCHOICE, "ProgId").as_deref(), Some("Typora.md")); // never touched
        assert_eq!(
            load_state(&env).unwrap().prior_user_choice.as_deref(),
            Some("Typora.md")
        );
    }

    #[test]
    fn conflict_yes_flag() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        reg.put(Hive::Cu, USERCHOICE, "ProgId", "Typora.md");
        let o = InstallOptions {
            yes: true,
            ..Default::default()
        };
        install_with(&env, &mut reg, &mut FixedPrompter(false), &o).unwrap();
        assert_eq!(*reg.opened_settings.borrow(), 1);
    }

    #[test]
    fn conflict_declined_and_no_assoc() {
        for (o, ans) in [
            (InstallOptions::default(), false),
            (
                InstallOptions {
                    no_assoc: true,
                    yes: true,
                },
                true,
            ),
        ] {
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
        let o = InstallOptions {
            yes: true,
            ..Default::default()
        };
        install_with(&env, &mut reg, &mut FixedPrompter(false), &o).unwrap();
        assert_eq!(
            reg.get("Software\\Classes\\.md", "").as_deref(),
            Some(PROG_ID)
        );
        uninstall_with(&env, &mut reg, &UninstallOptions::default()).unwrap();
        assert_eq!(
            reg.get("Software\\Classes\\.md", "").as_deref(),
            Some("Other.md")
        );
    }

    #[test]
    fn doctor_webview() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        assert_eq!(
            doctor_with(&env, &reg).get("WebView2").unwrap().status,
            Status::Missing
        );
        reg.put(Hive::Lm, WEBVIEW2_KEY, "pv", "120.0.1");
        assert_eq!(
            doctor_with(&env, &reg).get("WebView2").unwrap().status,
            Status::Ok
        );
    }

    const MD: &str = "Software\\Classes\\.md";
    const MT: &str = "Software\\Classes\\MIME\\Database\\Content Type\\text/markdown";
    const MTX: &str = "Software\\Classes\\MIME\\Database\\Content Type\\text/x-markdown";

    #[test]
    fn content_type_and_extensions_registered() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        install_with(
            &env,
            &mut reg,
            &mut FixedPrompter(false),
            &InstallOptions::default(),
        )
        .unwrap();
        assert_eq!(
            reg.get(MD, "Content Type").as_deref(),
            Some("text/markdown")
        );
        assert_eq!(reg.get(MD, "PerceivedType").as_deref(), Some("text"));
        assert_eq!(reg.get(MT, "Extension").as_deref(), Some(".md"));
        assert_eq!(reg.get(MTX, "Extension").as_deref(), Some(".md"));
        for ext in EXTS {
            let k = format!("Software\\Classes\\{ext}");
            assert_eq!(
                reg.get(&format!("{k}\\OpenWithProgids"), PROG_ID)
                    .as_deref(),
                Some("")
            );
            assert_eq!(
                reg.get(
                    "Software\\Classes\\Applications\\md-viewer.exe\\SupportedTypes",
                    ext
                )
                .as_deref(),
                Some("")
            );
            assert_eq!(reg.get(&k, "").as_deref(), Some(PROG_ID), "{ext}");
        }
        assert!(reg
            .get(
                "Software\\Classes\\Applications\\md-viewer.exe\\DefaultIcon",
                ""
            )
            .unwrap()
            .ends_with(",0"));
        let d = doctor_with(&env, &reg);
        assert_eq!(d.get("content-type").unwrap().status, Status::Ok);
        assert_eq!(d.get("icon").unwrap().status, Status::Ok);

        // idempotent: a second install changes nothing, priors stay None
        let before = reg.vals.clone();
        install_with(
            &env,
            &mut reg,
            &mut FixedPrompter(false),
            &InstallOptions::default(),
        )
        .unwrap();
        assert_eq!(reg.vals, before);
        let st = load_state(&env).unwrap();
        assert_eq!(st.prior_values["Content Type"], None);

        uninstall_with(&env, &mut reg, &UninstallOptions::default()).unwrap();
        assert_eq!(reg.get(MD, "Content Type"), None);
        assert_eq!(reg.get(MD, "PerceivedType"), None);
        assert_eq!(reg.get(MT, "Extension"), None);
        assert_eq!(reg.get(MTX, "Extension"), None);
        for ext in EXTS {
            let k = format!("Software\\Classes\\{ext}");
            assert_eq!(reg.get(&k, ""), None, "{ext}");
            assert_eq!(reg.get(&format!("{k}\\OpenWithProgids"), PROG_ID), None);
        }
    }

    #[test]
    fn prior_content_type_restored_when_accepted() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        reg.put(Hive::Cu, MD, "", "Other.md");
        reg.put(Hive::Cu, MD, "Content Type", "text/plain");
        reg.put(Hive::Cu, MD, "PerceivedType", "document");
        let yes = InstallOptions {
            yes: true,
            ..Default::default()
        };
        install_with(&env, &mut reg, &mut FixedPrompter(false), &yes).unwrap();
        assert_eq!(
            reg.get(MD, "Content Type").as_deref(),
            Some("text/markdown")
        );
        install_with(&env, &mut reg, &mut FixedPrompter(false), &yes).unwrap(); // keeps prior
        uninstall_with(&env, &mut reg, &UninstallOptions::default()).unwrap();
        assert_eq!(reg.get(MD, "Content Type").as_deref(), Some("text/plain"));
        assert_eq!(reg.get(MD, "PerceivedType").as_deref(), Some("document"));
        assert_eq!(reg.get(MD, "").as_deref(), Some("Other.md"));
    }

    #[test]
    fn declined_conflict_leaves_foreign_types_and_exts_alone() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        reg.put(Hive::Cu, USERCHOICE, "ProgId", "Typora.md");
        reg.put(Hive::Cu, MD, "Content Type", "text/plain");
        install_with(
            &env,
            &mut reg,
            &mut FixedPrompter(false),
            &InstallOptions::default(),
        )
        .unwrap();
        assert_eq!(reg.get(MD, "Content Type").as_deref(), Some("text/plain"));
        assert_eq!(reg.get(MD, "PerceivedType").as_deref(), Some("text")); // was absent: harmless
        for ext in &EXTS[1..] {
            assert_eq!(reg.get(&format!("Software\\Classes\\{ext}"), ""), None);
        }
        // still discoverable via Open with
        assert_eq!(
            reg.get("Software\\Classes\\.markdown\\OpenWithProgids", PROG_ID)
                .as_deref(),
            Some("")
        );
        assert_eq!(reg.get(MT, "Extension").as_deref(), Some(".md"));
        uninstall_with(&env, &mut reg, &UninstallOptions::default()).unwrap();
        assert_eq!(reg.get(MD, "Content Type").as_deref(), Some("text/plain"));
        assert_eq!(reg.get(MD, "PerceivedType"), None);
    }

    #[test]
    fn owned_secondary_exts_and_foreign_mime_keys_untouched() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        reg.put(
            Hive::Cu,
            "Software\\Classes\\.markdown",
            "",
            "Other.markdown",
        );
        let uc =
            "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\.mkd\\UserChoice";
        reg.put(Hive::Cu, uc, "ProgId", "Typora.md");
        reg.put(Hive::Cu, MTX, "Extension", ".mdx");
        install_with(
            &env,
            &mut reg,
            &mut FixedPrompter(false),
            &InstallOptions::default(),
        )
        .unwrap();
        assert_eq!(
            reg.get("Software\\Classes\\.markdown", "").as_deref(),
            Some("Other.markdown")
        );
        assert_eq!(reg.get("Software\\Classes\\.mkd", ""), None);
        assert_eq!(
            reg.get("Software\\Classes\\.mdown", "").as_deref(),
            Some(PROG_ID)
        );
        assert_eq!(reg.get(MTX, "Extension").as_deref(), Some(".mdx"));
        uninstall_with(&env, &mut reg, &UninstallOptions::default()).unwrap();
        assert_eq!(reg.get(MTX, "Extension").as_deref(), Some(".mdx"));
        assert_eq!(
            reg.get("Software\\Classes\\.markdown", "").as_deref(),
            Some("Other.markdown")
        );
        assert_eq!(reg.get("Software\\Classes\\.mdown", ""), None);
    }

    #[test]
    fn no_assoc_claims_no_extensions() {
        let (_t, env) = fixture();
        let mut reg = FakeRegistry::default();
        let o = InstallOptions {
            no_assoc: true,
            ..Default::default()
        };
        install_with(&env, &mut reg, &mut FixedPrompter(false), &o).unwrap();
        assert_eq!(reg.get("Software\\Classes\\.markdown", ""), None);
        assert!(is_installed(&env));
    }
}

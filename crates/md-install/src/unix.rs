//! Linux / FreeBSD (XDG) installer. Compiles on all hosts; only
//! [`SystemMime`] / [`SystemProbe`] touch external programs.

use crate::{
    decide_assoc, AssocDecision, DoctorReport, InstallOptions, Prompter, Report, Result, Status,
    UninstallOptions, APP,
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
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

impl UnixEnv {
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or_else(|| crate::Error("$HOME is not set".into()))?;
        let exe = std::env::current_exe()?;
        Ok(Self::new(&home, &exe, abs_env))
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
        self.data_home
            .join("icons/hicolor/scalable/apps/md-viewer.svg")
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
    /// mime type -> the complete previous `mimeapps.list` value (e.g.
    /// `a.desktop;b.desktop;`), recorded when the file had one, so uninstall
    /// restores the whole list rather than only the first handler.
    #[serde(default)]
    pub prior_lists: BTreeMap<String, String>,
    /// Size and FNV-1a hash of the binary we installed; uninstall only
    /// deletes `bin` while it still matches (absent in older records).
    #[serde(default)]
    pub bin_len: Option<u64>,
    #[serde(default)]
    pub bin_hash: Option<String>,
}

/// 64-bit FNV-1a: a dependency-free content fingerprint (identity check, not
/// a security measure).
pub fn fnv1a_hex(data: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
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
    /// The complete user-level handler list for `mime` as stored (e.g.
    /// `a.desktop;b.desktop;`), if the backend can see it.
    fn query_list(&self, mime: &str) -> Result<Option<String>> {
        self.query_default(mime)
    }
    /// Restore a value previously returned by [`MimeDb::query_list`] verbatim.
    fn set_list(&self, mime: &str, list: &str) -> Result<()> {
        self.set_default(mime, list)
    }
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
            let out = self
                .cmd("xdg-mime")
                .args(["query", "default", mime])
                .output()?;
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            return Ok(if s.is_empty() { None } else { Some(s) });
        }
        let text = fs::read_to_string(self.mimeapps()).unwrap_or_default();
        Ok(mimeapps_get(&text, mime))
    }
    fn query_list(&self, mime: &str) -> Result<Option<String>> {
        let text = fs::read_to_string(self.mimeapps()).unwrap_or_default();
        Ok(mimeapps_get_list(&text, mime))
    }
    fn set_list(&self, mime: &str, list: &str) -> Result<()> {
        edit_mimeapps(&self.mimeapps(), |t| mimeapps_set_list(t, mime, list))
    }
    fn set_default(&self, mime: &str, desktop_id: &str) -> Result<()> {
        if self.has_xdg_mime() {
            // xdg-mime fails if the config dir does not exist yet
            fs::create_dir_all(&self.config_home)?;
            let st = self
                .cmd("xdg-mime")
                .args(["default", desktop_id, mime])
                .status()?;
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
        if which(
            "update-desktop-database",
            &std::env::var("PATH").unwrap_or_default(),
        ) {
            let ok = self
                .cmd("update-desktop-database")
                .arg(&self.apps_dir)
                .status();
            if ok.map(|s| s.success()).unwrap_or(false) {
                done.push("update-desktop-database".to_string());
            }
        }
        if which(
            "update-mime-database",
            &std::env::var("PATH").unwrap_or_default(),
        ) {
            let ok = self
                .cmd("update-mime-database")
                .arg(&self.mime_root)
                .status();
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
                fs::metadata(&p)
                    .map(|m| m.permissions().mode() & 0o111 != 0)
                    .unwrap_or(false)
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

/// Raw value (everything after `=`, trimmed) of `mime` in `[Default
/// Applications]`: the whole `a;b;` list.
pub fn mimeapps_get_list(text: &str, mime: &str) -> Option<String> {
    let mut in_sec = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_sec = l == DEFAULTS;
        } else if in_sec {
            if let Some((k, v)) = l.split_once('=') {
                if k.trim() == mime {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

/// First handler listed for `mime` in the `[Default Applications]` section.
pub fn mimeapps_get(text: &str, mime: &str) -> Option<String> {
    mimeapps_get_list(text, mime)?
        .split(';')
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(String::from)
}

/// Remove `mime` from `[Default Applications]` (other sections untouched).
pub fn mimeapps_unset(text: &str, mime: &str) -> String {
    let mut in_sec = false;
    let mut out = String::new();
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_sec = l == DEFAULTS;
        } else if in_sec
            && l.split_once('=')
                .map(|(k, _)| k.trim() == mime)
                .unwrap_or(false)
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Set `mime=<list>` verbatim in `[Default Applications]`: replaces the
/// existing line in place, else adds it (creating the section if needed).
pub fn mimeapps_set_list(text: &str, mime: &str, list: &str) -> String {
    let entry = format!("{mime}={list}");
    let mut in_sec = false;
    let mut out = String::new();
    let mut done = false;
    let mut have_section = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_sec = l == DEFAULTS;
            have_section |= in_sec;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if in_sec
            && l.split_once('=')
                .map(|(k, _)| k.trim() == mime)
                .unwrap_or(false)
        {
            if !done {
                out.push_str(&entry);
                out.push('\n');
                done = true;
            }
            continue; // also drops duplicate keys
        }
        out.push_str(line);
        out.push('\n');
    }
    if done {
        return out;
    }
    if have_section {
        // insert right after the (first) section header
        let mut res = String::new();
        let mut ins = false;
        for line in out.lines() {
            res.push_str(line);
            res.push('\n');
            if !ins && line.trim() == DEFAULTS {
                res.push_str(&entry);
                res.push('\n');
                ins = true;
            }
        }
        return res;
    }
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
    out.push_str(DEFAULTS);
    out.push('\n');
    out.push_str(&entry);
    out.push('\n');
    out
}

/// Set `mime=desktop;` in `[Default Applications]`, creating section/line.
pub fn mimeapps_set(text: &str, mime: &str, desktop: &str) -> String {
    mimeapps_set_list(text, mime, &format!("{desktop};"))
}

// -------------------------------------------------------- file contents

/// Encode one executable path as the program word of a Desktop Entry `Exec=`
/// value (Desktop Entry spec, "The Exec key").
///
/// Two layers apply, in this order when *reading*: the generic string escapes
/// (`\\` -> `\`, `\n`, ...), then the Exec quoting rules. So when *writing* we
/// first apply the Exec rules (quote when a reserved character is present;
/// inside quotes escape `"`, backtick, `$` and `\` with a backslash; `%` is
/// always `%%`), then double every backslash for the string layer.
pub fn exec_quote(arg: &str) -> String {
    const RESERVED: &str = " \t\n\"'\\><~|&;$*?#()`";
    let quote = arg.is_empty() || arg.chars().any(|c| RESERVED.contains(c));
    let mut s = String::new();
    if quote {
        s.push('"');
    }
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' | '\\' if quote => {
                s.push('\\');
                s.push(c);
            }
            '%' => s.push_str("%%"),
            _ => s.push(c),
        }
    }
    if quote {
        s.push('"');
    }
    // string layer
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

pub fn desktop_entry(bin: &Path) -> String {
    let exec = exec_quote(&bin.to_string_lossy());
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
        fs::set_permissions(
            &tmp,
            fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 }),
        )?;
    }
    let _ = exec;
    fs::rename(&tmp, path)?;
    Ok(())
}

// -------------------------------------------------------------- install

/// One association change decided by the planner, applied after the install
/// record is safely on disk.
struct Pending {
    mime: String,
    /// First handler that will be replaced (`None` = there was none).
    prior: Option<String>,
    /// Complete previous list, when known and its head is `prior`.
    prior_list: Option<String>,
    /// Value as stored before we touch it (for rollback).
    orig_list: Option<String>,
    /// Overwrite an existing record (primary type) vs. keep the first one.
    overwrite: bool,
}

fn first_handler(list: &str) -> Option<&str> {
    list.split(';').map(str::trim).find(|s| !s.is_empty())
}

fn plan_one(mime: &dyn MimeDb, m: &str, overwrite: bool, prior: Option<String>) -> Result<Pending> {
    let prior = if overwrite {
        prior
    } else {
        mime.query_default(m)?.filter(|p| p != DESKTOP_ID)
    };
    let orig_list = mime.query_list(m)?;
    // only trust the stored list if it really starts with `prior`
    let prior_list = orig_list
        .clone()
        .filter(|l| prior.is_some() && first_handler(l) == prior.as_deref());
    Ok(Pending {
        mime: m.to_string(),
        prior,
        prior_list,
        orig_list,
        overwrite,
    })
}

fn rollback(mime: &dyn MimeDb, attempted: &[&Pending]) -> Vec<String> {
    let mut errs = vec![];
    for p in attempted.iter().rev() {
        let cur = mime.query_list(&p.mime).ok().flatten();
        let ours = mime.query_default(&p.mime).ok().flatten().as_deref() == Some(DESKTOP_ID);
        if cur == p.orig_list && !ours {
            continue;
        }
        let res = match &p.orig_list {
            Some(l) => mime.set_list(&p.mime, l),
            None => mime.unset_default(&p.mime),
        };
        if let Err(e) = res {
            errs.push(format!("could not roll back {}: {e}", p.mime));
        }
    }
    errs
}

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

    // 3. decide the association (no side effects yet)
    let old_state_bytes = fs::read(env.state_file()).ok();
    let mut state = load_state(env).unwrap_or_default();
    state.version = env!("CARGO_PKG_VERSION").to_string();
    state.bin = bin.to_string_lossy().into_owned();
    if let Ok(data) = fs::read(&bin) {
        state.bin_len = Some(data.len() as u64);
        state.bin_hash = Some(fnv1a_hex(&data));
    }

    if !mime.has_xdg_mime() {
        r.note("xdg-mime not found (install xdg-utils); editing ~/.config/mimeapps.list directly");
    }
    let mut pending: Vec<Pending> = vec![];
    let mut done_msg: Option<String> = None;
    match decide_assoc(current.as_deref(), &[DESKTOP_ID], opts, prompter) {
        AssocDecision::Disabled => r.act("skipped file association (--no-assoc)"),
        AssocDecision::AlreadyOurs => {
            // make sure the secondary type is covered too, keep recorded priors
            for m in &MIMES[1..] {
                if mime.query_default(m)?.as_deref() != Some(DESKTOP_ID) {
                    pending.push(plan_one(mime, m, false, None)?);
                }
            }
            r.act("md-viewer is already the default for Markdown");
        }
        AssocDecision::Declined { current } => {
            r.act(format!("kept {current} as default Markdown handler"));
            r.note(format!("run `{APP} install --yes` later to switch"));
        }
        AssocDecision::Set {
            prior,
            replaced_other,
        } => {
            pending.push(plan_one(mime, MIMES[0], true, prior.clone())?);
            for m in &MIMES[1..] {
                pending.push(plan_one(mime, m, false, None)?);
            }
            done_msg = Some(match (replaced_other, prior) {
                (true, Some(p)) => format!("set default for Markdown (replaced {p})"),
                _ => "set default for Markdown".to_string(),
            });
        }
    }
    for p in &pending {
        if p.overwrite || !state.prior_defaults.contains_key(&p.mime) {
            state.prior_defaults.insert(p.mime.clone(), p.prior.clone());
            match &p.prior_list {
                Some(l) => state.prior_lists.insert(p.mime.clone(), l.clone()),
                None => state.prior_lists.remove(&p.mime),
            };
        }
    }

    // 4. record first, then change the user's defaults: if recording fails the
    // previous default is untouched; if applying fails we roll both back.
    write_file(
        &env.state_file(),
        &serde_json::to_vec_pretty(&state)?,
        false,
    )?;
    r.act(format!("recorded {}", env.state_file().display()));

    for (i, p) in pending.iter().enumerate() {
        if let Err(e) = mime.set_default(&p.mime, DESKTOP_ID) {
            let attempted: Vec<&Pending> = pending[..=i].iter().collect();
            let mut msg = format!("could not set default for {}: {e}", p.mime);
            for x in rollback(mime, &attempted) {
                msg.push_str("; ");
                msg.push_str(&x);
            }
            match &old_state_bytes {
                Some(b) => {
                    if fs::write(env.state_file(), b).is_err() {
                        msg.push_str("; could not restore install.json");
                    }
                }
                None => {
                    let _ = fs::remove_file(env.state_file());
                }
            }
            return Err(crate::Error(msg));
        }
    }
    if let Some(m) = done_msg {
        r.act(m);
    }

    if !std::env::split_paths(&env.path_var).any(|p| p == env.bin_dir) {
        r.note(format!("{} is not on your PATH", env.bin_dir.display()));
    }
    Ok(r)
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
        let prior = state
            .as_ref()
            .and_then(|s| s.prior_defaults.get(m))
            .cloned()
            .flatten();
        let list = state.as_ref().and_then(|s| s.prior_lists.get(m)).cloned();
        match prior {
            Some(p) if !opts.no_restore => {
                match list {
                    Some(l) => mime.set_list(m, &l)?,
                    None => mime.set_default(m, &p)?,
                }
                r.act(format!("restored {p} as default for {m}"));
            }
            _ => {
                mime.unset_default(m)?;
                r.act(format!("removed default for {m}"));
            }
        }
    }

    for p in [
        env.desktop_file(),
        env.mime_xml(),
        env.icon(),
        env.state_file(),
    ] {
        if p.exists() {
            fs::remove_file(&p)?;
            r.act(format!("removed {}", p.display()));
        }
    }
    let _ = fs::remove_dir(env.state_dir()); // only if empty
    let bin = env.bin();
    if bin.symlink_metadata().is_ok() {
        match bin_is_ours(&bin, state.as_ref()) {
            Ok(()) => {
                fs::remove_file(&bin)?;
                r.act(format!("removed {}", bin.display()));
            }
            Err(why) => r.note(format!("left {} in place: {why}", bin.display())),
        }
    }
    for what in mime.refresh() {
        r.act(format!("ran {what}"));
    }
    Ok(r)
}

/// Is the file at `bin` the one this installer recorded?
fn bin_is_ours(bin: &Path, state: Option<&State>) -> std::result::Result<(), String> {
    let s = state.ok_or("no install record, cannot tell whether it is ours")?;
    if Path::new(&s.bin) != bin {
        return Err(format!("install record points to {}", s.bin));
    }
    if s.bin_len.is_none() && s.bin_hash.is_none() {
        return Ok(()); // older record: path match is all we have
    }
    let data = fs::read(bin).map_err(|e| format!("cannot read it ({e})"))?;
    if s.bin_len.is_some_and(|l| l != data.len() as u64)
        || s.bin_hash.as_deref().is_some_and(|h| h != fnv1a_hex(&data))
    {
        return Err("it was modified or replaced since install".into());
    }
    Ok(())
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
            c.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
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
        if let Ok(o) = Command::new("ldconfig")
            .arg("-r")
            .stdin(Stdio::null())
            .output()
        {
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
        d.add(
            "binary",
            Status::Ok,
            format!("installed at {}", bin.display()),
        );
    } else {
        d.add(
            "binary",
            Status::Missing,
            format!("not installed at {}", bin.display()),
        );
    }
    if std::env::split_paths(&env.path_var).any(|p| p == env.bin_dir) {
        d.add(
            "path",
            Status::Ok,
            format!("{} is on PATH", env.bin_dir.display()),
        );
    } else {
        d.add(
            "path",
            Status::Warn,
            format!("{} is not on PATH", env.bin_dir.display()),
        );
    }
    for (name, p) in [
        ("desktop-entry", env.desktop_file()),
        ("mime-package", env.mime_xml()),
    ] {
        if p.is_file() {
            d.add(name, Status::Ok, p.display().to_string());
        } else {
            d.add(name, Status::Missing, format!("{} not found", p.display()));
        }
    }
    if load_state(env).is_some() {
        d.add(
            "install-record",
            Status::Ok,
            env.state_file().display().to_string(),
        );
    } else {
        d.add(
            "install-record",
            Status::Warn,
            "no install.json (uninstall cannot restore a prior default)",
        );
    }
    if mime.has_xdg_mime() {
        d.add("xdg-mime", Status::Ok, "found");
    } else {
        d.add(
            "xdg-mime",
            Status::Warn,
            "not found (install xdg-utils); falling back to editing mimeapps.list",
        );
    }
    for t in ["update-desktop-database", "update-mime-database"] {
        if probe.has_command(t) {
            d.add(t, Status::Ok, "found");
        } else {
            d.add(
                t,
                Status::Warn,
                "not found (optional; desktop may need re-login to notice changes)",
            );
        }
    }
    match mime.query_default(MIMES[0]) {
        Ok(Some(c)) if c == DESKTOP_ID => d.add(
            "association",
            Status::Ok,
            "md-viewer is the default for text/markdown",
        ),
        Ok(Some(c)) => d.add(
            "association",
            Status::Warn,
            format!("default for text/markdown is {c}"),
        ),
        Ok(None) => d.add(
            "association",
            Status::Warn,
            "no default handler for text/markdown",
        ),
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
    /// mime -> stored handler list (`a.desktop;b.desktop;`)
    pub defaults: RefCell<BTreeMap<String, String>>,
    pub refreshed: RefCell<u32>,
    /// `set_default` fails for these mime types (failure injection).
    pub fail_set: RefCell<Vec<String>>,
}
impl FakeMime {
    pub fn with_default(mime: &str, app: &str) -> Self {
        let f = FakeMime {
            xdg: true,
            ..Default::default()
        };
        f.defaults.borrow_mut().insert(
            mime.into(),
            if app.ends_with(';') {
                app.to_string()
            } else {
                format!("{app};")
            },
        );
        f
    }
}
impl MimeDb for FakeMime {
    fn has_xdg_mime(&self) -> bool {
        self.xdg
    }
    fn query_default(&self, m: &str) -> Result<Option<String>> {
        Ok(self
            .defaults
            .borrow()
            .get(m)
            .and_then(|l| first_handler(l))
            .map(String::from))
    }
    fn query_list(&self, m: &str) -> Result<Option<String>> {
        Ok(self.defaults.borrow().get(m).cloned())
    }
    fn set_list(&self, m: &str, l: &str) -> Result<()> {
        self.defaults.borrow_mut().insert(m.into(), l.into());
        Ok(())
    }
    fn set_default(&self, m: &str, d: &str) -> Result<()> {
        if self.fail_set.borrow().iter().any(|x| x == m) {
            return Err(crate::Error(format!("injected failure for {m}")));
        }
        self.defaults.borrow_mut().insert(m.into(), format!("{d};"));
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
        assert_eq!(
            e.desktop_file(),
            Path::new("/d/applications/md-viewer.desktop")
        );
        assert_eq!(e.mimeapps(), Path::new("/c/mimeapps.list"));
        assert_eq!(e.bin(), Path::new("/h/.local/bin/md-viewer"));
    }

    #[cfg(unix)] // asserts POSIX paths in the Exec line
    #[test]
    fn fresh_install() {
        let f = fixture();
        let m = FakeMime {
            xdg: true,
            ..Default::default()
        };
        let opts = InstallOptions::default();
        let r = install_with(&f.env, &m, &mut FixedPrompter(false), &opts).unwrap();
        assert!(!r.actions.is_empty());
        assert!(f.env.bin().is_file());
        let de = fs::read_to_string(f.env.desktop_file()).unwrap();
        assert!(de.contains("MimeType=text/markdown;text/x-markdown;"));
        assert!(de.contains(&format!("Exec={} %f", f.env.bin().display())));
        assert!(fs::read_to_string(f.env.mime_xml())
            .unwrap()
            .contains("*.md"));
        assert!(f.env.icon().is_file());
        assert_eq!(
            m.query_default("text/markdown").unwrap().as_deref(),
            Some(DESKTOP_ID)
        );
        assert_eq!(
            m.query_default("text/x-markdown").unwrap().as_deref(),
            Some(DESKTOP_ID)
        );
        let st = load_state(&f.env).unwrap();
        assert_eq!(st.prior_defaults["text/markdown"], None);
        assert!(*m.refreshed.borrow() >= 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(f.env.bin()).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
    }

    #[test]
    fn idempotent_and_keeps_prior() {
        let f = fixture();
        let m = FakeMime::with_default("text/markdown", "typora.desktop");
        let yes = InstallOptions {
            yes: true,
            ..Default::default()
        };
        install_with(&f.env, &m, &mut FixedPrompter(false), &yes).unwrap();
        // second run: no prompt needed, prior must survive
        let r = install_with(
            &f.env,
            &m,
            &mut FixedPrompter(false),
            &InstallOptions::default(),
        )
        .unwrap();
        assert!(r.actions.iter().any(|a| a.contains("already the default")));
        let st = load_state(&f.env).unwrap();
        assert_eq!(
            st.prior_defaults["text/markdown"].as_deref(),
            Some("typora.desktop")
        );
        assert_eq!(
            m.query_default("text/markdown").unwrap().as_deref(),
            Some(DESKTOP_ID)
        );
    }

    #[test]
    fn conflict_declined() {
        let f = fixture();
        let m = FakeMime::with_default("text/markdown", "typora.desktop");
        let r = install_with(
            &f.env,
            &m,
            &mut FixedPrompter(false),
            &InstallOptions::default(),
        )
        .unwrap();
        assert_eq!(
            m.query_default("text/markdown").unwrap().as_deref(),
            Some("typora.desktop")
        );
        assert!(r.actions.iter().any(|a| a.contains("kept typora.desktop")));
        assert!(f.env.bin().is_file()); // files still installed
        assert!(load_state(&f.env).unwrap().prior_defaults.is_empty());
    }

    #[test]
    fn conflict_accepted_via_prompt() {
        let f = fixture();
        let m = FakeMime::with_default("text/markdown", "typora.desktop");
        install_with(
            &f.env,
            &m,
            &mut FixedPrompter(true),
            &InstallOptions::default(),
        )
        .unwrap();
        assert_eq!(
            m.query_default("text/markdown").unwrap().as_deref(),
            Some(DESKTOP_ID)
        );
    }

    #[test]
    fn conflict_no_assoc() {
        let f = fixture();
        let m = FakeMime::with_default("text/markdown", "typora.desktop");
        let o = InstallOptions {
            no_assoc: true,
            yes: true,
        };
        install_with(&f.env, &m, &mut FixedPrompter(true), &o).unwrap();
        assert_eq!(
            m.query_default("text/markdown").unwrap().as_deref(),
            Some("typora.desktop")
        );
        assert_eq!(m.query_default("text/x-markdown").unwrap(), None);
    }

    #[test]
    fn uninstall_restores_prior() {
        let f = fixture();
        let m = FakeMime::with_default("text/markdown", "typora.desktop");
        let yes = InstallOptions {
            yes: true,
            ..Default::default()
        };
        install_with(&f.env, &m, &mut FixedPrompter(false), &yes).unwrap();
        uninstall_with(&f.env, &m, &UninstallOptions::default()).unwrap();
        assert_eq!(
            m.query_default("text/markdown").unwrap().as_deref(),
            Some("typora.desktop")
        );
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
        let m = FakeMime {
            xdg: true,
            ..Default::default()
        };
        install_with(
            &f.env,
            &m,
            &mut FixedPrompter(false),
            &InstallOptions::default(),
        )
        .unwrap();
        // user switches to another app afterwards: uninstall must not clobber
        m.set_default("text/markdown", "other.desktop").unwrap();
        uninstall_with(&f.env, &m, &UninstallOptions::default()).unwrap();
        assert_eq!(
            m.query_default("text/markdown").unwrap().as_deref(),
            Some("other.desktop")
        );
        assert_eq!(m.query_default("text/x-markdown").unwrap(), None);
    }

    #[test]
    fn uninstall_no_restore() {
        let f = fixture();
        let m = FakeMime::with_default("text/markdown", "typora.desktop");
        let yes = InstallOptions {
            yes: true,
            ..Default::default()
        };
        install_with(&f.env, &m, &mut FixedPrompter(false), &yes).unwrap();
        uninstall_with(&f.env, &m, &UninstallOptions { no_restore: true }).unwrap();
        assert_eq!(m.query_default("text/markdown").unwrap(), None);
    }

    #[test]
    fn reinstall_over_existing_binary() {
        let f = fixture();
        let m = FakeMime {
            xdg: true,
            ..Default::default()
        };
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
        assert_eq!(
            mimeapps_get(t, "text/markdown").as_deref(),
            Some("typora.desktop")
        );
        assert_eq!(mimeapps_get(t, "text/plain"), None);
        let s = mimeapps_set(t, "text/markdown", DESKTOP_ID);
        assert_eq!(
            mimeapps_get(&s, "text/markdown").as_deref(),
            Some(DESKTOP_ID)
        );
        assert!(s.contains("[Added Associations]\ntext/markdown=a.desktop;"));
        assert!(s.contains("text/html=b.desktop;"));
        let u = mimeapps_unset(&s, "text/markdown");
        assert_eq!(mimeapps_get(&u, "text/markdown"), None);
        assert!(u.contains("text/markdown=a.desktop;")); // other section kept
        let n = mimeapps_set("", "text/markdown", DESKTOP_ID);
        assert_eq!(
            n,
            "[Default Applications]\ntext/markdown=md-viewer.desktop;\n"
        );
    }

    /// Decode an `Exec=` value the way a desktop-entry reader does: string
    /// escapes first, then shell-ish quoting, then `%%`; returns the words.
    fn decode_exec(v: &str) -> Vec<String> {
        let mut un = String::new();
        let mut it = v.chars();
        while let Some(c) = it.next() {
            if c == '\\' {
                match it.next() {
                    Some('\\') => un.push('\\'),
                    Some('n') => un.push('\n'),
                    Some('t') => un.push('\t'),
                    Some('r') => un.push('\r'),
                    Some('s') => un.push(' '),
                    o => panic!("invalid string escape {o:?} in {v:?}"),
                }
            } else {
                un.push(c);
            }
        }
        let mut words = vec![];
        let mut cur = String::new();
        let mut have = false;
        let mut q = false;
        let mut it = un.chars();
        while let Some(c) = it.next() {
            match (c, q) {
                ('"', _) => {
                    q = !q;
                    have = true;
                }
                ('\\', true) => {
                    let n = it.next().expect("dangling backslash");
                    assert!("\"`$\\".contains(n), "bad quote escape {n:?}");
                    cur.push(n);
                }
                (' ', false) => {
                    if have {
                        words.push(std::mem::take(&mut cur));
                        have = false;
                    }
                }
                (c, false) if " \t\n'><~|&;$*?#()`".contains(c) || c == '\\' => {
                    panic!("unquoted reserved char {c:?} in {v:?}")
                }
                (c, _) => {
                    cur.push(c);
                    have = true;
                }
            }
        }
        assert!(!q, "unterminated quote");
        if have {
            words.push(cur);
        }
        words.into_iter().map(|w| w.replace("%%", "%")).collect()
    }

    #[test]
    fn exec_quoting_roundtrips_weird_paths() {
        for path in [
            "/home/u/.local/bin/md-viewer",
            "/home/my user/bin/md-viewer",
            "/home/q\"uote/md-viewer",
            "/home/back`tick/md-viewer",
            "/home/do$llar/$HOME/md-viewer",
            "/home/back\\slash/md-viewer",
            "/home/100%/%f/md-viewer",
            "/home/a'b;c&d(e)/md-viewer",
            "/home/all \"`$\\% mixed/md-viewer",
        ] {
            let de = desktop_entry(Path::new(path));
            let line = de.lines().find(|l| l.starts_with("Exec=")).unwrap();
            let words = decode_exec(&line["Exec=".len()..]);
            assert_eq!(words, vec![path.to_string(), "%f".to_string()], "{line}");
        }
        assert_eq!(exec_quote("/a/b"), "/a/b");
        assert_eq!(exec_quote("/a b"), "\"/a b\"");
        assert_eq!(exec_quote("/a%b"), "/a%%b");
        assert_eq!(exec_quote("/a$b c"), "\"/a\\\\$b c\"");
    }

    #[cfg(unix)] // asserts POSIX paths in the Exec line
    #[test]
    fn install_with_weird_home_writes_valid_exec() {
        let td = tempfile::tempdir().unwrap();
        let exe = td.path().join("src-exe");
        fs::write(&exe, b"x").unwrap();
        let home = td.path().join("we ird\"$`%\\ home");
        fs::create_dir_all(&home).unwrap();
        let env = UnixEnv::new(&home, &exe, |_| None);
        let m = FakeMime {
            xdg: true,
            ..Default::default()
        };
        install_with(
            &env,
            &m,
            &mut FixedPrompter(false),
            &InstallOptions::default(),
        )
        .unwrap();
        let de = fs::read_to_string(env.desktop_file()).unwrap();
        let line = de.lines().find(|l| l.starts_with("Exec=")).unwrap();
        let w = decode_exec(&line["Exec=".len()..]);
        assert_eq!(w[0], env.bin().to_string_lossy());
    }

    #[test]
    fn uninstall_keeps_modified_or_foreign_binary() {
        let f = fixture();
        let m = FakeMime {
            xdg: true,
            ..Default::default()
        };
        let o = InstallOptions::default();
        // user replaced our binary afterwards: keep it, say so
        install_with(&f.env, &m, &mut FixedPrompter(false), &o).unwrap();
        fs::write(f.env.bin(), b"user's own build, different size").unwrap();
        let r = uninstall_with(&f.env, &m, &UninstallOptions::default()).unwrap();
        assert!(f.env.bin().exists());
        assert!(r.notes.iter().any(|n| n.contains("left")), "{r}");
        assert!(!f.env.desktop_file().exists());
        // same size, different content is also caught by the hash
        install_with(&f.env, &m, &mut FixedPrompter(false), &o).unwrap();
        let mut data = fs::read(f.env.bin()).unwrap();
        data[0] ^= 0xff;
        fs::write(f.env.bin(), &data).unwrap();
        uninstall_with(&f.env, &m, &UninstallOptions::default()).unwrap();
        assert!(f.env.bin().exists());
    }

    #[test]
    fn uninstall_without_record_leaves_binary() {
        let f = fixture();
        fs::create_dir_all(f.env.bin_dir.clone()).unwrap();
        fs::write(f.env.bin(), b"someone else's md-viewer").unwrap();
        let m = FakeMime::default();
        let r = uninstall_with(&f.env, &m, &UninstallOptions::default()).unwrap();
        assert!(f.env.bin().exists());
        assert!(
            r.notes.iter().any(|n| n.contains("no install record")),
            "{r}"
        );
    }

    #[test]
    fn uninstall_old_record_without_hash_still_removes() {
        let f = fixture();
        let m = FakeMime::default();
        install_with(
            &f.env,
            &m,
            &mut FixedPrompter(false),
            &InstallOptions::default(),
        )
        .unwrap();
        let mut st = load_state(&f.env).unwrap();
        st.bin_len = None;
        st.bin_hash = None;
        fs::write(f.env.state_file(), serde_json::to_vec(&st).unwrap()).unwrap();
        uninstall_with(&f.env, &m, &UninstallOptions::default()).unwrap();
        assert!(!f.env.bin().exists());
    }

    #[test]
    fn state_write_failure_leaves_default_untouched() {
        let f = fixture();
        let m = FakeMime::with_default("text/markdown", "typora.desktop;vim.desktop;");
        // a directory where install.json must go makes the write fail
        fs::create_dir_all(f.env.state_file()).unwrap();
        let yes = InstallOptions {
            yes: true,
            ..Default::default()
        };
        assert!(install_with(&f.env, &m, &mut FixedPrompter(false), &yes).is_err());
        assert_eq!(
            m.defaults.borrow()["text/markdown"],
            "typora.desktop;vim.desktop;"
        );
        assert_eq!(m.query_default("text/x-markdown").unwrap(), None);
    }

    #[test]
    fn set_failure_rolls_back_defaults_and_state() {
        let f = fixture();
        let m = FakeMime::with_default("text/markdown", "typora.desktop;vim.desktop;");
        m.fail_set.borrow_mut().push("text/x-markdown".into());
        let yes = InstallOptions {
            yes: true,
            ..Default::default()
        };
        let e = install_with(&f.env, &m, &mut FixedPrompter(false), &yes).unwrap_err();
        assert!(e.0.contains("text/x-markdown"), "{e}");
        assert_eq!(
            m.defaults.borrow()["text/markdown"],
            "typora.desktop;vim.desktop;"
        );
        assert_eq!(m.query_default("text/x-markdown").unwrap(), None);
        assert!(!f.env.state_file().exists()); // no stale record
    }

    #[test]
    fn set_failure_restores_previous_state_file() {
        let f = fixture();
        let m = FakeMime::default();
        let o = InstallOptions::default();
        install_with(&f.env, &m, &mut FixedPrompter(false), &o).unwrap();
        let before = fs::read(f.env.state_file()).unwrap();
        // user lost their md-viewer default for the secondary type
        m.unset_default("text/x-markdown").unwrap();
        m.fail_set.borrow_mut().push("text/x-markdown".into());
        assert!(install_with(&f.env, &m, &mut FixedPrompter(false), &o).is_err());
        assert_eq!(fs::read(f.env.state_file()).unwrap(), before);
    }

    #[test]
    fn full_handler_list_restored_exactly() {
        let f = fixture();
        let m = FakeMime::with_default("text/markdown", "a.desktop;b.desktop;");
        let yes = InstallOptions {
            yes: true,
            ..Default::default()
        };
        install_with(&f.env, &m, &mut FixedPrompter(false), &yes).unwrap();
        assert_eq!(m.defaults.borrow()["text/markdown"], "md-viewer.desktop;");
        uninstall_with(&f.env, &m, &UninstallOptions::default()).unwrap();
        assert_eq!(m.defaults.borrow()["text/markdown"], "a.desktop;b.desktop;");
    }

    #[test]
    fn mimeapps_list_preserved_with_system_mime() {
        // exercises the real file-backed list path (no xdg-mime involved)
        let f = fixture();
        let sys = SystemMime::new(&f.env);
        let path = f.env.mimeapps();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let orig = "[Added Associations]\ntext/markdown=z.desktop;\n\n[Default Applications]\ntext/html=h.desktop;\ntext/markdown=a.desktop;b.desktop;\ntext/plain=t.desktop;\n";
        fs::write(&path, orig).unwrap();
        assert_eq!(
            sys.query_list("text/markdown").unwrap().as_deref(),
            Some("a.desktop;b.desktop;")
        );
        let t = fs::read_to_string(&path).unwrap();
        let set = mimeapps_set(&t, "text/markdown", DESKTOP_ID);
        assert!(set.contains("text/markdown=md-viewer.desktop;\ntext/plain"));
        fs::write(&path, &set).unwrap();
        sys.set_list("text/markdown", "a.desktop;b.desktop;")
            .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), orig);
    }

    #[test]
    fn mimeapps_set_list_variants() {
        assert_eq!(
            mimeapps_get_list(
                "[Default Applications]\ntext/markdown = a;b; \n",
                "text/markdown"
            )
            .as_deref(),
            Some("a;b;")
        );
        // section present without the key: insert after header
        let t = "[Default Applications]\ntext/html=h;\n";
        assert_eq!(
            mimeapps_set_list(t, "text/markdown", "a;b;"),
            "[Default Applications]\ntext/markdown=a;b;\ntext/html=h;\n"
        );
        // other section only: new section appended
        let t = "[Added Associations]\nx/y=z;\n";
        assert_eq!(
            mimeapps_set_list(t, "text/markdown", "a;"),
            "[Added Associations]\nx/y=z;\n\n[Default Applications]\ntext/markdown=a;\n"
        );
    }

    #[test]
    fn doctor_states() {
        let f = fixture();
        let m = FakeMime {
            xdg: false,
            ..Default::default()
        };
        let d = doctor_with(&f.env, &m, &P(false));
        assert!(!d.healthy());
        assert_eq!(d.get("binary").unwrap().status, Status::Missing);
        assert_eq!(d.get("xdg-mime").unwrap().status, Status::Warn);
        assert_eq!(d.get("libwebkit2gtk-4.1").unwrap().status, Status::Missing);
        install_with(
            &f.env,
            &m,
            &mut FixedPrompter(false),
            &InstallOptions::default(),
        )
        .unwrap();
        let d = doctor_with(&f.env, &m, &P(true));
        assert!(d.healthy(), "{d}");
        assert_eq!(d.get("association").unwrap().status, Status::Ok);
    }
}

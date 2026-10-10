# Installing md-viewer

md-viewer installs per user. It never needs administrator rights or `sudo`.

```text
md-viewer install [--yes] [--no-assoc]   # copy binary, register, make default
md-viewer uninstall [--no-restore]       # undo everything, restore prior defaults
md-viewer doctor                         # report install / association / icon state
```

`--yes` replaces a different default handler without asking; `--no-assoc`
installs but never touches the default application. Install is idempotent, and
the previous defaults are recorded (`install.json`) so uninstall restores them.

## Windows

Install location: `%LOCALAPPDATA%\Programs\md-viewer\md-viewer.exe` (added to
the user `PATH`). Everything is written under `HKCU`.

### First-launch prompt

The Windows build is a GUI-subsystem program (no console), so it asks with a
native dialog. On a normal launch (no `install`/`uninstall`/`doctor`
subcommand) a dialog appears, before any window is created, when **all** hold:

- the running exe is not inside the install directory,
- there is no valid install record,
- you have not chosen "don't ask again"
  (marker `%LOCALAPPDATA%\md-viewer\no-install-prompt`; delete it to be asked again),
- the environment variable `MDV_NO_INSTALL_PROMPT` is not set (use it in CI/tests).

Buttons: **Yes** installs (a second Yes/No box appears if another program is
the default for `.md`), shows the result, then starts the installed copy with
the same arguments and exits; on failure the error is shown and the document
opens from the current location. **No** = not now, ask again next time.
**Cancel** = never ask again. Either way the document opens right away.

### Registry changes (all HKCU, all reversed by uninstall)

- `Software\Classes\md-viewer.md` ProgID with open command and `DefaultIcon`
  (`"...\md-viewer.exe",0`, the embedded icon);
  `Applications\md-viewer.exe` (open command, `DefaultIcon`, `SupportedTypes`
  for `.md .markdown .mdown .mkd .mkdn`); `Software\md-viewer\Capabilities` +
  `RegisteredApplications`; an Apps & features uninstall entry.
- `OpenWithProgids` for all five extensions, so md-viewer shows in "Open with".
- `.md`: `Content Type = text/markdown` and `PerceivedType = text`, and
  `MIME\Database\Content Type\text/markdown` and `text/x-markdown` with
  `Extension = .md`, so browsers and apps map `.md` to `text/markdown`.
  Prior `.md` values are recorded and restored on uninstall. A foreign
  `Content Type` is only replaced when you accepted md-viewer as the `.md`
  handler; foreign MIME database keys are never overwritten.
- `.markdown .mdown .mkd .mkdn`: md-viewer becomes the class default only if
  nobody owns the extension (no class default and no `UserChoice`).

### Default-app policy

md-viewer never forges or edits the `UserChoice` key (Windows protects it with
a hash). If another program is already the user's choice for `.md`, you are
asked whether to replace it; on Yes the Windows "Default apps" page opens so
you pick md-viewer there. On No (or `--no-assoc`) the app is still installed and
appears in "Open with", and the existing default is untouched.

## Linux and FreeBSD

There is no first-launch prompt; use `md-viewer install` (or `installers/install.sh`).

Files (honouring `XDG_DATA_HOME`/`XDG_CONFIG_HOME`/`XDG_BIN_HOME`):

- `~/.local/bin/md-viewer`
- `~/.local/share/applications/md-viewer.desktop`
  (`MimeType=text/markdown;text/x-markdown;`)
- `~/.local/share/mime/packages/md-viewer.xml`: `text/markdown`, globs `*.md
  *.markdown *.mdown *.mkd *.mkdn`, alias `text/x-markdown`, sub-class of
  `text/plain`; `update-mime-database` is run when available.
- Icons: `~/.local/share/icons/hicolor/{16,24,32,48,64,128,256,512}x…/apps/md-viewer.png`
  and `scalable/apps/md-viewer.svg`; `gtk-update-icon-cache` is run when available.
- `~/.config/mimeapps.list`: default for both MIME types. A different existing
  default for `text/markdown` is only replaced after confirmation (or `--yes`);
  the full previous handler list of each type is restored on uninstall.

`md-viewer doctor` checks the binary, desktop entry, MIME package
("content-type"), icons, association, helper tools and WebKitGTK.

# md-viewer

Fast cross-platform Markdown viewer (Windows / Linux / FreeBSD x64).

## Features
- Pretty-printed (rendered) view by default; press `r` for source, `t` for a table of contents, Ctrl+F to search.
- Printing: Ctrl+P or the Print button opens the OS print dialog (also "save as PDF") with a clean light print layout. See [docs/PRINTING.md](docs/PRINTING.md).
- GitHub/GitLab/Obsidian-flavoured Markdown, sanitized HTML, live reload.
- Mermaid diagrams, embedded by default and rendered offline. See [docs/MERMAID.md](docs/MERMAID.md);
  build with `--no-default-features` for a lean binary without them.

## Icon

The app icon lives in `assets/icons/` (SVG, PNGs, multi-size ICO) and is embedded in the
Windows exe (`crates/md-viewer/build.rs`) and used as the window icon at runtime. See
[assets/icons/README.md](assets/icons/README.md) for how it is generated. The Windows build is a
GUI-subsystem binary (no console window); CI checks this with `.github/scripts/check-pe-gui.ps1`.

## Install

`md-viewer install` registers a per-user desktop entry, MIME types (`text/markdown`,
`.md .markdown .mdown .mkd .mkdn`), icons and the default-app association;
`md-viewer uninstall` reverses it and `md-viewer doctor` checks it. On Windows,
the first normal launch from outside the install directory offers to install via
a native dialog. See [docs/INSTALL.md](docs/INSTALL.md).

## License

BSD 2-Clause. See [LICENSE](LICENSE).

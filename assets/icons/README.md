# md-viewer icons

| File | Purpose |
|---|---|
| `md-viewer.svg` | Scalable icon; `[md]` is vector outlines (no font dependency) |
| `md-viewer-{16,24,32,48,64,128,256,512}.png` | Raster sizes (hicolor theme, installer) |
| `md-viewer.ico` | Windows multi-resolution icon (16-256, PNG-compressed entries); embedded in the exe by `crates/md-viewer/build.rs` |
| `md-viewer-64.rgba` | Raw 64x64 RGBA used for the runtime window/taskbar icon (`include_bytes!` in `main.rs`) |
| `source/markdown-icon.svg` | Original design (uses a `<text>` element, font dependent) |
| `make_icons.py` | Generator for everything above |

16 and 24 px use a simplified variant (no border/shadow, brackets dropped, bolder `md`) because
the full design is illegible at those sizes.

## Regenerating

```sh
pip install fonttools pillow     # (use --target <dir> + PYTHONPATH for a user-space install)
cargo install resvg              # needs `resvg` on PATH
python3 assets/icons/make_icons.py
```

Font: DejaVu Sans Mono Bold (override with `MD_ICON_FONT=/path/to/font.ttf`); see
`THIRD_PARTY_NOTICES.md`. The 64x64 `.rgba` file must be regenerated whenever the icon changes
(the script does it).

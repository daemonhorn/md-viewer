# Printing

## Shortcuts
- **Ctrl+P** or the **Print** button (top right, tooltip "Print (Ctrl+P)") prints the current document.
  Works from the find box too. (Cmd+P on macOS is not bound yet; use the Print button.)
- In raw (source) mode the source view is printed.

## How it works
The page sends a `print` message over wry's IPC channel; the Rust side accepts only that exact
message from its own origin and calls `webview.print()` on the UI thread, which opens the
platform print dialog. To save a PDF, choose "Print to File" / "Save as PDF" / "Microsoft Print to PDF"
in that dialog.

## Print layout (`@media print` in `assets/github.css`)
- Always light (white background, black text), even with a dark system theme or `--theme dark`.
  Mermaid diagrams are rendered a second time with the light theme for print when the screen is dark.
- Toolbar, find bar, contents sidebar and mode badge are hidden; single full-width column, 16 mm margins.
- No page breaks inside code blocks, table rows, images, diagrams, quotes or callouts; none right after headings.
  Table headers repeat on each page; long code lines wrap.
- External links are followed by their URL; in-page anchors, footnote links and the contents list are not.
- Front matter is hidden when printing (it is metadata, and the collapsed `<details>` would not show its content anyway).

## Known limits
- Linux/FreeBSD: WebKitGTK's GTK print dialog; page-size/margin options depend on the installed printers.
- Windows: WebView2's print dialog.
- macOS: WKWebView print behaviour is untested; Cmd+P is not bound.
- Print previews and exact pagination can only be verified on a real display.

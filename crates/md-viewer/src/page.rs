//! Page assembly: pure functions turning rendered Markdown into a full HTML page.

use md_core::Rendered;

pub const CSS: &str = include_str!("../../../assets/github.css");
pub const JS: &str = include_str!("../../../assets/app.js");

#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum Theme {
    Auto,
    Light,
    Dark,
}

impl Theme {
    fn attr(self) -> &'static str {
        match self {
            Theme::Auto => "auto",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }
}

pub fn escape_html(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + s.len() / 8);
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

const FIND_BAR: &str = r#"<div id="find" hidden><input type="text" placeholder="Find (Enter / Shift+Enter)" aria-label="Find"><button class="prev" title="Previous">&uarr;</button><button class="next" title="Next">&darr;</button></div>"#;

fn shell(title: &str, theme: Theme, mode: &str, flags: &str, body: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\" data-theme=\"{theme}\" data-mode=\"{mode}\"{flags}>\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n<title>{title}</title>\n\
         <link rel=\"stylesheet\" href=\"/__mdv/style.css\">\n</head>\n<body>\n{body}\n{FIND_BAR}\n\
         <script src=\"/__mdv/app.js\"></script>\n</body>\n</html>\n",
        theme = theme.attr(),
        title = escape_html(title),
    )
}

/// Rendered (pretty-print) page.
pub fn rendered_page(title: &str, r: &Rendered, theme: Theme) -> String {
    let fm = match &r.front_matter {
        Some(fm) => format!(
            "<details class=\"frontmatter\"><summary>Front matter</summary><pre><code>{}</code></pre></details>\n",
            escape_html(fm)
        ),
        None => String::new(),
    };
    let flags = format!(
        "{}{}",
        if r.needs_mermaid {
            " data-mermaid=\"1\""
        } else {
            ""
        },
        if r.needs_math { " data-math=\"1\"" } else { "" }
    );
    let body = format!(
        "<div id=\"layout\"><nav id=\"toc\" hidden aria-label=\"Contents\"></nav><div id=\"main\"><article class=\"markdown-body\">\n{fm}{}\n</article></div></div>",
        r.html
    );
    shell(title, theme, "rendered", &flags, &body)
}

/// Raw source view.
pub fn raw_page(title: &str, src: &str, theme: Theme) -> String {
    let body = format!(
        "<div id=\"modebadge\">raw (press r)</div><pre class=\"raw\"><code>{}</code></pre>",
        escape_html(src)
    );
    shell(title, theme, "raw", "", &body)
}

/// Landing page shown when no file was given.
pub fn help_page(theme: Theme) -> String {
    let body = "<article class=\"drop\"><h1>md-viewer</h1>\
        <p>Drop a Markdown file onto this window, or run <code>md-viewer FILE.md</code>.</p>\
        <p>Keys: <kbd>r</kbd> raw/rendered &middot; <kbd>t</kbd> contents &middot; <kbd>Ctrl</kbd>+<kbd>F</kbd> find</p></article>";
    shell("md-viewer", theme, "help", "", body)
}

pub fn error_page(theme: Theme, msg: &str) -> String {
    let body = format!(
        "<article><h1>Error</h1><p>{}</p></article>",
        escape_html(msg)
    );
    shell("Error", theme, "help", "", &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_has_frontmatter_details_and_flags() {
        let r = md_core::render("---\ntitle: <x>\n---\n# Hi\n\n```mermaid\ngraph TD;\n```\n");
        let p = rendered_page("t<", &r, Theme::Dark);
        assert!(p.contains("<details class=\"frontmatter\">"));
        assert!(p.contains("title: &lt;x&gt;"));
        assert!(p.contains("data-theme=\"dark\""));
        assert!(p.contains("data-mode=\"rendered\""));
        assert!(p.contains("data-mermaid=\"1\""));
        assert!(!p.contains("data-math"));
        assert!(p.contains("<title>t&lt;</title>"));
    }

    #[test]
    fn raw_escapes() {
        let p = raw_page("a", "<script>alert(1)</script> & \"q\"", Theme::Auto);
        assert!(!p.contains("<script>alert"));
        assert!(p.contains("&lt;script&gt;"));
        assert!(p.contains("data-mode=\"raw\""));
    }

    #[test]
    fn plain_page_has_no_optional_flags() {
        let r = md_core::render("hello");
        let p = rendered_page("t", &r, Theme::Auto);
        assert!(!p.contains("data-mermaid") && !p.contains("data-math"));
    }
}

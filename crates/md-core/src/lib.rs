//! Markdown -> sanitized HTML core for md-viewer.
//!
//! Wide dialect support (GitHub, GitLab, Obsidian-style) via comrak, followed by
//! an ammonia sanitize pass so untrusted documents cannot run script.

use std::collections::HashSet;

use comrak::{markdown_to_html, Options};

/// Result of rendering a document.
pub struct Rendered {
    pub html: String,
    /// Front matter block (without delimiters), if present.
    pub front_matter: Option<String>,
    /// True if the document contains mermaid or math and needs those assets.
    pub needs_mermaid: bool,
    pub needs_math: bool,
}

fn options() -> Options<'static> {
    let mut o = Options::default();
    let e = &mut o.extension;
    e.strikethrough = true;
    e.table = true;
    e.autolink = true;
    e.tasklist = true;
    e.footnotes = true;
    e.description_lists = true;
    e.alerts = true;
    e.multiline_block_quotes = true;
    e.math_dollars = true;
    e.math_code = true;
    e.shortcodes = true;
    e.superscript = true;
    e.subscript = true;
    e.underline = false; // `__x__` stays bold, matching GitHub
    e.wikilinks_title_after_pipe = true;
    e.header_id_prefix = Some(String::new());
    e.front_matter_delimiter = Some("---".to_owned());
    // Raw HTML is passed through, then sanitized below (GitHub allows <details>, <img>, ...).
    o.render.r#unsafe = true;
    o.render.github_pre_lang = true;
    o.render.tasklist_classes = true;
    o
}

fn sanitizer() -> ammonia::Builder<'static> {
    let mut b = ammonia::Builder::default();
    b.add_tags(["details", "summary", "input", "section", "sup", "sub", "kbd", "picture", "source", "span", "div"]);
    b.add_generic_attributes(["id", "class", "align", "dir", "lang", "title"]);
    b.add_tag_attributes("input", ["type", "checked", "disabled"]);
    b.add_tag_attributes("code", ["data-math-style"]);
    b.add_tag_attributes("span", ["data-math-style"]);
    b.add_tag_attributes("img", ["width", "height", "src", "alt"]);
    b.add_tag_attributes("source", ["srcset", "media"]);
    b.add_tag_attributes("details", ["open"]);
    b.add_tag_attributes("th", ["align"]);
    b.add_tag_attributes("td", ["align"]);
    // Only these URL schemes survive; javascript:, data: (except none) etc. are stripped.
    let schemes: HashSet<&str> = ["http", "https", "mailto", "tel"].into_iter().collect();
    b.url_schemes(schemes);
    b.link_rel(Some("noopener noreferrer"));
    b
}

/// Split a leading `---` front matter block off the document. comrak also
/// understands front matter, but we want the text to show it collapsed.
fn split_front_matter(src: &str) -> (Option<String>, &str) {
    let s = src.strip_prefix('\u{feff}').unwrap_or(src);
    let Some(rest) = s.strip_prefix("---\n").or_else(|| s.strip_prefix("---\r\n")) else {
        return (None, s);
    };
    let mut pos = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" || line.trim_end() == "..." {
            let fm = rest[..pos].trim_end().to_owned();
            return (Some(fm), &rest[pos + line.len()..]);
        }
        pos += line.len();
    }
    (None, s)
}

/// Render Markdown source to sanitized HTML.
pub fn render(src: &str) -> Rendered {
    let (front_matter, body) = split_front_matter(src);
    let raw = markdown_to_html(body, &options());
    let html = sanitizer().clean(&raw).to_string();
    Rendered {
        needs_mermaid: html.contains("language-mermaid"),
        needs_math: html.contains("data-math-style"),
        html,
        front_matter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gfm_table_and_tasklist() {
        let r = render("| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n- [ ] todo\n");
        assert!(r.html.contains("<table>"));
        assert!(r.html.contains("checked"));
    }

    #[test]
    fn strips_script_and_js_links() {
        let r = render("<script>alert(1)</script>\n\n[x](javascript:alert(1))\n");
        assert!(!r.html.contains("<script"));
        assert!(!r.html.contains("javascript:"));
    }

    #[test]
    fn front_matter_split() {
        let r = render("---\ntitle: x\n---\n# Hi\n");
        assert_eq!(r.front_matter.as_deref(), Some("title: x"));
        assert!(r.html.contains("<h1"));
        assert!(!r.html.contains("title: x"));
    }

    #[test]
    fn alerts_and_footnotes() {
        let r = render("> [!NOTE]\n> hi\n\nfoo[^1]\n\n[^1]: bar\n");
        assert!(r.html.contains("markdown-alert"));
        assert!(r.html.contains("footnote"));
    }
}

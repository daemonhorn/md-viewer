//! Markdown -> sanitized HTML core for md-viewer.
//!
//! Wide dialect support (GitHub, GitLab, Obsidian-style) via comrak, followed by
//! an ammonia sanitize pass so untrusted documents cannot run script.

use std::collections::HashSet;

use comrak::{format_html, parse_document, Arena, Options};

mod dialect;
pub use dialect::wikilink_href;

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
    e.alerts = false; // callouts (GitHub alerts + Obsidian types) are handled in dialect.rs
    e.highlight = true;
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
    b.add_tags([
        "details", "summary", "input", "section", "sup", "sub", "kbd", "picture", "source", "span",
        "div", "mark", "nav",
    ]);
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
    // `<input>` is only allowed as a (disabled) checkbox, for task lists.
    b.attribute_filter(|element, attr, value| {
        if element == "input" && attr == "type" {
            return value
                .eq_ignore_ascii_case("checkbox")
                .then(|| "checkbox".into());
        }
        Some(value.into())
    });
    b.set_tag_attribute_value("input", "disabled", "");
    b
}

/// Remove any `<input>` element that is not a checkbox (ammonia has already
/// normalized the markup, so a simple scan is sufficient).
fn drop_non_checkbox_inputs(html: String) -> String {
    if !html.contains("<input") {
        return html;
    }
    let mut out = String::with_capacity(html.len());
    let mut rest = html.as_str();
    while let Some(i) = rest.find("<input") {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail.find('>').map_or(tail.len(), |e| e + 1);
        if tail[..end].contains(" type=\"checkbox\"") {
            out.push_str(&tail[..end]);
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// Split a leading `---` front matter block off the document. comrak also
/// understands front matter, but we want the text to show it collapsed.
fn split_front_matter(src: &str) -> (Option<String>, &str) {
    let s = src.strip_prefix('\u{feff}').unwrap_or(src);
    // YAML (`---`) or GitLab TOML (`+++`) front matter.
    let (delim, rest) = if let Some(r) = s
        .strip_prefix("---\n")
        .or_else(|| s.strip_prefix("---\r\n"))
    {
        ("---", r)
    } else if let Some(r) = s
        .strip_prefix("+++\n")
        .or_else(|| s.strip_prefix("+++\r\n"))
    {
        ("+++", r)
    } else {
        return (None, s);
    };
    let mut pos = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == delim || (delim == "---" && line.trim_end() == "...") {
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
    let body = dialect::preprocess(body);
    let opts = options();
    let arena = Arena::new();
    let root = parse_document(&arena, &body, &opts);
    dialect::rewrite_ast(&arena, root);
    let mut raw = String::new();
    format_html(root, &opts, &mut raw).expect("writing to a String cannot fail");
    let raw = dialect::postprocess(raw);
    let html = drop_non_checkbox_inputs(sanitizer().clean(&raw).to_string());
    Rendered {
        // github_pre_lang renders `<pre lang="mermaid">`; also accept the class form.
        needs_mermaid: html.contains("lang=\"mermaid\"") || html.contains("language-mermaid"),
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
    fn mermaid_detected() {
        assert!(render("```mermaid\ngraph TD\n```\n").needs_mermaid);
        assert!(!render("```rust\nfn main(){}\n```\n").needs_mermaid);
    }

    #[test]
    fn alerts_and_footnotes() {
        let r = render("> [!NOTE]\n> hi\n\nfoo[^1]\n\n[^1]: bar\n");
        assert!(r.html.contains("markdown-alert"));
        assert!(r.html.contains("footnote"));
    }

    #[test]
    fn input_restricted_to_checkbox() {
        for src in [
            "<input type=text value=x>go",
            "<input>go",
            "<input type=\"password\" name=p>go",
            "<INPUT TYPE=file>go",
        ] {
            let h = render(src).html;
            assert!(!h.contains("<input"), "{src:?} -> {h}");
        }
        let h = render("<input type=checkbox checked>").html;
        assert!(
            h.contains("type=\"checkbox\"") && h.contains("disabled"),
            "{h}"
        );
        assert!(render("- [x] a\n").html.contains("<input"));
    }

    #[test]
    fn toc_from_gitlab_markers() {
        for marker in ["[[_TOC_]]", "[TOC]"] {
            let h = render(&format!("{marker}\n\n# A\n\n## B\n\n# A\n")).html;
            assert!(h.contains("<nav class=\"toc\">"), "{h}");
            assert!(
                h.contains("<a href=\"#a\"")
                    && h.contains("<a href=\"#b\"")
                    && h.contains("<a href=\"#a-1\""),
                "{h}"
            );
            assert!(h.contains("<ul><li><a href=\"#b\""), "nested: {h}");
            assert!(!h.contains("_TOC_"), "{h}");
        }
        let h = render("```\n[[_TOC_]]\n```\n\n# A\n").html;
        assert!(!h.contains("<nav") && h.contains("[[_TOC_]]"), "{h}");
    }

    #[test]
    fn inapplicable_tasks() {
        let h = render("- [~] skip\n- [-] skip2\n- [ ] todo\n\n```\n- [~] code\n```\n").html;
        assert_eq!(h.matches("task-inapplicable").count(), 6, "{h}");
        assert!(h.contains("[~] code"));
        assert!(
            h.contains("class=\"task-list-item-checkbox task-inapplicable\" disabled"),
            "{h}"
        );
    }

    #[test]
    fn toml_front_matter() {
        let r = render("+++\ntitle = \"x\"\n+++\n# Hi\n");
        assert_eq!(r.front_matter.as_deref(), Some("title = \"x\""));
        assert!(r.html.contains("<h1") && !r.html.contains("title ="));
        assert!(render("+++\nno close\n").front_matter.is_none());
        // delimiters must match
        assert!(render("+++\na\n---\n").front_matter.is_none());
    }

    #[test]
    fn obsidian_callouts() {
        let h =
            render("> [!quote]+ Folded *title*\n> body\n\n> [!faq]-\n> x\n\n> [!note]\n> n\n").html;
        assert!(
            h.contains("markdown-alert markdown-alert-quote") && h.contains(">Folded title<"),
            "{h}"
        );
        assert!(h.contains(">Faq<") && h.contains(">Note<"), "{h}");
        assert!(!h.contains(">+") && !h.contains(">-"), "{h}");
        let n = render("> [!a]\n> > [!b] inner\n> > t\n").html;
        assert!(
            n.contains("markdown-alert-a") && n.contains("markdown-alert-b"),
            "{n}"
        );
        assert!(!render("> [!x] <script>alert(1)</script>\n> y")
            .html
            .contains("&lt;script"));
        assert!(!render("> [!x] <script>alert(1)</script>\n> y")
            .html
            .contains("<script"));
    }

    #[test]
    fn obsidian_highlight_and_comments() {
        let h =
            render("a ==hi== b %%secret%% c\n\n`==code== %%x%%`\n\n%%\nmulti\nline\n%%\nafter\n")
                .html;
        assert!(
            h.contains("<mark>hi</mark>") && !h.contains("secret") && !h.contains("multi"),
            "{h}"
        );
        assert!(h.contains("%%x%%") && h.contains("after"), "{h}");
        // an unterminated `%%` does not swallow the document
        let h = render("50%% off\n\nstill here\n").html;
        assert!(h.contains("still here"), "{h}");
    }

    #[test]
    fn wikilink_href_cases() {
        assert_eq!(wikilink_href("Note"), "Note.md");
        assert_eq!(wikilink_href("My Note"), "My%20Note.md");
        assert_eq!(wikilink_href("a/b.md"), "a/b.md");
        assert_eq!(wikilink_href("img.png"), "img.png");
        assert_eq!(wikilink_href("Note#Some Heading"), "Note.md#some-heading");
        assert_eq!(wikilink_href("#Local"), "#local");
        assert_eq!(wikilink_href("Note|alias"), "Note.md");
        assert_eq!(wikilink_href("v1.2 notes"), "v1.2%20notes.md");
        assert_eq!(wikilink_href("Note#^block"), "Note.md#%5Eblock");
    }

    #[test]
    fn wikilinks_and_embeds_render() {
        let h = render("[[Other Note|alias]] [[N#H]] ![[pic.png]] ![[pic.png|200]] ![[Some Note]]")
            .html;
        assert!(
            h.contains("href=\"Other%20Note.md\"") && h.contains(">alias<"),
            "{h}"
        );
        assert!(h.contains("href=\"N.md#h\""), "{h}");
        assert!(
            h.contains("<img src=\"pic.png\"") && h.contains("width=\"200\""),
            "{h}"
        );
        assert!(h.contains("href=\"Some%20Note.md\""), "{h}");
        assert!(!render("`![[x.png]]`").html.contains("<img"));
    }
}

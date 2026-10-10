//! Markdown -> sanitized HTML core for md-viewer.
//!
//! Wide dialect support (GitHub, GitLab, Obsidian-style) via comrak, followed by
//! an ammonia sanitize pass so untrusted documents cannot run script.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Mutex;

use comrak::adapters::{HeadingAdapter, HeadingMeta};
use comrak::nodes::Sourcepos;
use comrak::options::Plugins;
use comrak::{format_html_with_plugins, parse_document, Anchorizer, Arena, Options};

mod dialect;
pub use dialect::wikilink_href;

/// Inputs larger than this are not processed; a short notice is rendered instead.
/// This bounds worst-case memory and time for pathological documents.
pub const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;

/// Documents whose HTML nests deeper than this are replaced by a notice. The HTML
/// parser used by the sanitizer is quadratic in nesting depth (~2s at 20k levels,
/// ~20s at 60k), and no real document nests this deep.
pub const MAX_NESTING_DEPTH: usize = 4096;

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
    // `<source>`/`srcset` are deliberately not allowed: ammonia does not scheme-filter
    // `srcset`, and `<img src>` inside `<picture>` still works as the fallback.
    b.add_tags([
        "details", "summary", "input", "section", "sup", "sub", "kbd", "picture", "span", "div",
        "mark", "nav",
    ]);
    b.add_generic_attributes(["id", "class", "align", "dir", "lang", "title"]);
    b.add_tag_attributes("input", ["type", "checked", "disabled"]);
    b.add_tag_attributes("code", ["data-math-style"]);
    b.add_tag_attributes("span", ["data-math-style"]);
    b.add_tag_attributes("img", ["width", "height", "src", "alt"]);
    b.add_tag_attributes("details", ["open"]);
    b.add_tag_attributes("th", ["align"]);
    b.add_tag_attributes("td", ["align"]);
    // Only these URL schemes survive; javascript:, data: (except none) etc. are stripped.
    let schemes: HashSet<&str> = ["http", "https", "mailto", "tel"].into_iter().collect();
    b.url_schemes(schemes);
    b.link_rel(Some("noopener noreferrer"));
    b.attribute_filter(|element, attr, value| {
        // `mdv-` ids/names are reserved for the viewer's own UI; documents cannot claim them.
        if matches!(attr, "id" | "name")
            && value.len() >= 4
            && value.as_bytes()[..4].eq_ignore_ascii_case(b"mdv-")
        {
            return None;
        }
        // `<input>` is only allowed as a (disabled) checkbox, for task lists.
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

/// Does this serialized start tag (`<input ...>`, without the leading `<`) carry a
/// `type="checkbox"` attribute?
fn tag_is_checkbox(tag: &str) -> bool {
    let b = tag.as_bytes();
    let mut i = 0;
    // skip the tag name
    while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' && b[i] != b'/' {
        i += 1;
    }
    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        if i >= b.len() || b[i] == b'>' {
            return false;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'=' | b'>' | b'/') {
            i += 1;
        }
        let name = &tag[start..i];
        let mut value = "";
        if b.get(i) == Some(&b'=') {
            i += 1;
            if b.get(i) == Some(&b'"') {
                let vs = i + 1;
                i = vs;
                while i < b.len() && b[i] != b'"' {
                    i += 1;
                }
                value = &tag[vs..i.min(b.len())];
                i += 1;
            } else {
                let vs = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                    i += 1;
                }
                value = &tag[vs..i];
            }
        }
        if name.eq_ignore_ascii_case("type") {
            return value.eq_ignore_ascii_case("checkbox");
        }
    }
}

/// Remove any `<input>` element that is not a checkbox. Operates on ammonia's
/// canonical output with a real (quote-aware) tag scan: a literal `<` in that output
/// only ever starts a tag (text is escaped), and `<input` inside attribute values,
/// text or code is never touched.
fn drop_non_checkbox_inputs(html: String) -> String {
    if !html.contains("<input") {
        return html;
    }
    let b = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut last = 0; // start of the not-yet-copied region
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        // Find the end of this tag, skipping quoted attribute values.
        let mut j = i + 1;
        let mut quoted = false;
        while j < b.len() {
            match b[j] {
                b'"' => quoted = !quoted,
                b'>' if !quoted => break,
                _ => {}
            }
            j += 1;
        }
        let end = (j + 1).min(b.len());
        let tag = &html[i + 1..end];
        let is_input = tag.len() >= 5
            && tag.as_bytes()[..5].eq_ignore_ascii_case(b"input")
            && tag
                .as_bytes()
                .get(5)
                .is_none_or(|c| !c.is_ascii_alphanumeric());
        if is_input && !tag_is_checkbox(tag) {
            out.push_str(&html[last..i]);
            last = end;
        }
        i = end;
    }
    out.push_str(&html[last..]);
    out
}

/// Heading ids with GitHub-compatible de-duplication (`slug`, `slug-1`, `slug-2`, ...).
/// comrak's own anchorizer probes `slug-1`, `slug-2`, ... from scratch for every
/// repeat, which is quadratic for many identical headings; here a per-slug counter
/// resumes where the previous probe stopped (the used set only grows, so the result
/// is identical).
#[derive(Default)]
struct HeadingIds {
    used: HashSet<String>,
    next: HashMap<String, usize>,
    /// id of the heading currently being rendered (between `enter` and `exit`).
    current: Option<String>,
}

impl HeadingIds {
    fn unique(&mut self, slug: String) -> String {
        let id = if self.used.contains(&slug) {
            let n = self.next.entry(slug.clone()).or_insert(0);
            loop {
                *n += 1;
                let cand = format!("{slug}-{n}");
                if !self.used.contains(&cand) {
                    break cand;
                }
            }
        } else {
            slug
        };
        self.used.insert(id.clone());
        id
    }
}

struct Headings(Mutex<HeadingIds>);

/// Approximate maximum element nesting depth of (comrak-generated or raw) HTML.
fn max_nesting_depth(html: &str) -> usize {
    const VOID: &[&str] = &[
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
        "source", "track", "wbr",
    ];
    let b = html.as_bytes();
    let (mut depth, mut max, mut i) = (0usize, 0usize, 0);
    while let Some(p) = html[i..].find('<') {
        i += p + 1;
        let closing = b.get(i) == Some(&b'/');
        let ns = i + usize::from(closing);
        let ne = html[ns..]
            .find(|c: char| !c.is_ascii_alphanumeric())
            .map_or(html.len(), |e| ns + e);
        let name = &html[ns..ne];
        if name.is_empty() {
            continue;
        }
        if closing {
            depth = depth.saturating_sub(1);
        } else if !VOID.iter().any(|v| v.eq_ignore_ascii_case(name)) {
            // `<x/>`-style self-closing tags do not open anything.
            // (bounded look-ahead keeps pathological `<a <a <a ...` input linear)
            let close = html.as_bytes()[ne..]
                .iter()
                .take(512)
                .position(|&c| c == b'>');
            if close.is_none_or(|e| e == 0 || html.as_bytes()[ne + e - 1] != b'/') {
                depth += 1;
                max = max.max(depth);
            }
        }
    }
    max
}

fn notice(msg: String) -> Rendered {
    Rendered {
        html: format!("<p><em>{msg}</em></p>\n"),
        front_matter: None,
        needs_mermaid: false,
        needs_math: false,
    }
}

fn escape_html(out: &mut dyn fmt::Write, s: &str) -> fmt::Result {
    for c in s.chars() {
        match c {
            '"' => out.write_str("&quot;")?,
            '&' => out.write_str("&amp;")?,
            '<' => out.write_str("&lt;")?,
            '>' => out.write_str("&gt;")?,
            '\0' => out.write_char('\u{fffd}')?,
            c => out.write_char(c)?,
        }
    }
    Ok(())
}

impl HeadingAdapter for Headings {
    fn enter(
        &self,
        out: &mut dyn fmt::Write,
        h: &HeadingMeta,
        _sourcepos: Option<Sourcepos>,
    ) -> fmt::Result {
        let mut ids = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let id = ids.unique(Anchorizer::new().anchorize(&h.content));
        write!(out, "<h{} id=\"{id}\">", h.level)?;
        ids.current = Some(id);
        Ok(())
    }

    fn exit(&self, out: &mut dyn fmt::Write, h: &HeadingMeta) -> fmt::Result {
        let id = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current
            .take()
            .unwrap_or_default();
        write!(out, "<a href=\"#{id}\" aria-label=\"Link to heading '")?;
        escape_html(out, &h.content)?;
        out.write_str("'\" data-heading-content=\"")?;
        escape_html(out, &h.content)?;
        writeln!(out, "\" class=\"anchor\"></a></h{}>", h.level)
    }
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
///
/// Documents larger than [`MAX_INPUT_BYTES`] (16 MiB) are not processed: a short
/// notice paragraph is returned instead; likewise for HTML nested deeper than
/// [`MAX_NESTING_DEPTH`]. Rendering time is otherwise linear in the
/// input size (heading ids, `![[embeds]]`, code spans and `%%comments%%` are all
/// handled without quadratic rescans; see the regression tests in `tests/perf.rs`).
pub fn render(src: &str) -> Rendered {
    if src.len() > MAX_INPUT_BYTES {
        return notice(format!(
            "This document is too large to render ({} MiB; the limit is {} MiB).",
            src.len() >> 20,
            MAX_INPUT_BYTES >> 20
        ));
    }
    let (front_matter, body) = split_front_matter(src);
    let body = dialect::preprocess(body);
    let opts = options();
    let arena = Arena::new();
    let root = parse_document(&arena, &body, &opts);
    dialect::rewrite_ast(&arena, root);
    let mut raw = String::new();
    let headings = Headings(Mutex::default());
    let mut plugins = Plugins::default();
    plugins.render.heading_adapter = Some(&headings);
    format_html_with_plugins(root, &opts, &mut raw, &plugins)
        .expect("writing to a String cannot fail");
    if max_nesting_depth(&raw) > MAX_NESTING_DEPTH {
        return notice(format!(
            "This document is nested too deeply to render (limit: {MAX_NESTING_DEPTH} levels)."
        ));
    }
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

    #[test]
    fn heading_ids_dedupe_like_github() {
        let h = render("# A\n\n# A\n\n# A-1\n\n# A\n\n# a 1\n").html;
        for id in ["a", "a-1", "a-1-1", "a-2", "a-1-2"] {
            assert!(h.contains(&format!("<h1 id=\"{id}\"")), "{id}: {h}");
        }
        let mut ids = HeadingIds::default();
        assert_eq!(ids.unique("x".into()), "x");
        assert_eq!(ids.unique("x".into()), "x-1");
    }

    #[test]
    fn input_scan_ignores_attribute_values_text_and_code() {
        // `<input` inside an attribute value, text, or code must survive untouched.
        let h = render("<img src=\"a.png\" alt=\"<input type=text>\">").html;
        assert!(h.contains("alt=\"&lt;input type=text&gt;\""), "{h}");
        // ... and the scanner itself skips a literal `<input` inside a quoted value
        let raw = "<img alt=\"<input type=text>\"><input type=\"text\"><input type=\"checkbox\">";
        assert_eq!(
            drop_non_checkbox_inputs(raw.to_owned()),
            "<img alt=\"<input type=text>\"><input type=\"checkbox\">"
        );
        let h = render("```html\n<input type=text>\n```\n\n`<input>` and &lt;input type=text&gt; and \\<input>\n").html;
        assert_eq!(h.matches("&lt;input").count(), 4, "{h}");
        assert!(h.contains("type=text"), "{h}");
        // a forged ` type=\"checkbox\"` in another attribute does not make a text input pass
        let h = render("<input title=' type=\"checkbox\"' type=text>go").html;
        assert!(!h.contains("<input"), "{h}");
        // a real checkbox next to a text input: only the checkbox survives
        let h = render("<input type=text><input type=checkbox checked>after").html;
        assert_eq!(h.matches("<input").count(), 1, "{h}");
        assert!(
            h.contains("checkbox") && h.ends_with("after</p>\n") || h.contains("after"),
            "{h}"
        );
    }

    #[test]
    fn srcset_and_source_are_dropped() {
        let h = render(
            "<picture><source srcset=\"https://evil/a.png\" media=\"x\"><img src=\"b.png\" srcset=\"javascript:alert(1) 1x\"></picture>",
        )
        .html;
        assert!(
            !h.contains("srcset") && !h.contains("<source") && !h.contains("evil"),
            "{h}"
        );
        assert!(h.contains("<img src=\"b.png\""), "{h}");
    }

    #[test]
    fn reserved_mdv_ids_are_stripped() {
        let h = render(
            "<div id=\"mdv-toolbar\">a</div><span id=\"MDV-x\">b</span><a name=\"mdv-y\">c</a><div id=\"ok\">d</div>\n\n# mdv-find\n",
        )
        .html;
        assert!(
            !h.contains("id=\"mdv-") && !h.contains("id=\"MDV-") && !h.contains("name="),
            "{h}"
        );
        assert!(h.contains("id=\"ok\""), "{h}");
    }

    #[test]
    fn size_and_depth_caps() {
        let big = "a".repeat(MAX_INPUT_BYTES + 1);
        let r = render(&big);
        assert!(r.html.contains("too large") && r.html.len() < 200);
        let deep = format!("{}x\n", "> ".repeat(MAX_NESTING_DEPTH + 10));
        assert!(render(&deep).html.contains("nested too deeply"));
        let ok = format!("{}x\n", "> ".repeat(200));
        assert!(render(&ok).html.contains('x') && !render(&ok).html.contains("too deeply"));
        // void / self-closing tags do not count as nesting
        let flat = "<br>".repeat(MAX_NESTING_DEPTH * 2) + &"<hr/>".repeat(MAX_NESTING_DEPTH * 2);
        assert!(!render(&flat).html.contains("too deeply"));
    }

    #[test]
    fn toc_markers_are_capped() {
        let h = render(&format!("{}# A\n", "[TOC]\n\n".repeat(50))).html;
        assert!(
            h.matches("<nav class=\"toc\">").count() == 8 && !h.contains("mdv-toc"),
            "{h}"
        );
    }
}

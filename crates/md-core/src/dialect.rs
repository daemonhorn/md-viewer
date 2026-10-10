//! Dialect shims layered around comrak: source pre-processing (Obsidian comments and
//! embeds), AST rewrites (callouts, GitLab inapplicable tasks, TOC placeholders,
//! wikilink hrefs) and HTML post-processing (TOC generation, task classes).

use std::cell::RefCell;

use comrak::nodes::{Ast, AstNode, NodeHtmlBlock, NodeTaskItem, NodeValue};
use comrak::{Anchorizer, Arena};

const TOC_PLACEHOLDER: &str = "<mdv-toc-placeholder></mdv-toc-placeholder>";

const KNOWN_EXTS: &[&str] = &[
    "md", "markdown", "mdx", "txt", "png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "avif",
    "ico", "pdf", "mp3", "wav", "ogg", "m4a", "flac", "mp4", "webm", "mov", "mkv", "ogv", "canvas",
    "base", "html", "htm", "csv", "json", "yaml", "yml", "toml", "zip", "docx", "xlsx", "pptx",
];
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "avif"];

fn ext_of(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())
}

fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Convert an Obsidian-style wikilink target (`Note`, `folder/Note#Heading`,
/// `Note|alias`) into a relative href. `.md` is appended when the last path segment
/// has no recognised extension; the path is percent-encoded; a `#heading` fragment is
/// slugified like the heading ids the renderer generates (`#^block` ids are kept).
pub fn wikilink_href(target: &str) -> String {
    let t = target.split('|').next().unwrap_or("").trim();
    let (path, anchor) = match t.split_once('#') {
        Some((p, a)) => (p.trim(), Some(a.trim())),
        None => (t, None),
    };
    let mut out = String::new();
    if !path.is_empty() {
        out.push_str(&pct(path));
        if !ext_of(path).is_some_and(|e| KNOWN_EXTS.contains(&e.as_str())) {
            out.push_str(".md");
        }
    }
    if let Some(a) = anchor {
        if !a.is_empty() {
            out.push('#');
            if a.starts_with('^') {
                out.push_str(&pct(a));
            } else {
                out.push_str(&pct(&Anchorizer::new().anchorize(a)));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Source pre-processing
// ---------------------------------------------------------------------------

fn esc_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn embed_markdown(inner: &str) -> Option<String> {
    let (target, alias) = match inner.split_once('|') {
        Some((t, a)) => (t.trim(), Some(a.trim())),
        None => (inner.trim(), None),
    };
    if target.is_empty() || target.starts_with('#') {
        return None;
    }
    let path = target.split('#').next().unwrap_or(target);
    let stem = path.rsplit('/').next().unwrap_or(path);
    let stem = stem.rsplit_once('.').map_or(stem, |(s, _)| s);
    let is_image = ext_of(path).is_some_and(|e| IMAGE_EXTS.contains(&e.as_str()));
    if is_image {
        let href = pct(path);
        let size = alias.and_then(|a| {
            let (w, h) = a.split_once('x').unwrap_or((a, ""));
            (!w.is_empty()
                && w.bytes().all(|b| b.is_ascii_digit())
                && h.bytes().all(|b| b.is_ascii_digit()))
            .then_some((w, h))
        });
        if let Some((w, h)) = size {
            let hattr = if h.is_empty() {
                String::new()
            } else {
                format!(" height=\"{h}\"")
            };
            return Some(format!(
                "<img src=\"{href}\" alt=\"{}\" width=\"{w}\"{hattr}>",
                esc_attr(stem)
            ));
        }
        let alt = alias.filter(|a| !a.is_empty()).unwrap_or(stem);
        let alt: String = alt
            .chars()
            .filter(|c| !matches!(c, '[' | ']' | '\\' | '\n'))
            .collect();
        Some(format!("![{alt}]({href})"))
    } else {
        let label = alias.filter(|a| !a.is_empty()).unwrap_or(stem);
        let label: String = label
            .chars()
            .filter(|c| !matches!(c, '[' | ']' | '\\' | '\n'))
            .collect();
        Some(format!("[{label}]({})", wikilink_href(target)))
    }
}

fn fence_of(line: &str) -> Option<(char, usize, &str)> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return None;
    }
    let c = t.chars().next()?;
    if c != '`' && c != '~' {
        return None;
    }
    let n = t.chars().take_while(|&x| x == c).count();
    (n >= 3).then(|| (c, n, &t[n..]))
}

/// Process one non-fenced line: strip `%%comments%%` and rewrite `![[embeds]]`,
/// leaving inline code spans untouched.
fn process_line(line: &str, in_comment: &mut bool, comments_ok: bool) -> String {
    let b = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < b.len() {
        if *in_comment {
            match line[i..].find("%%") {
                Some(p) => {
                    *in_comment = false;
                    i += p + 2;
                }
                None => {
                    if line.ends_with('\n') {
                        out.push('\n');
                    }
                    return out;
                }
            }
            continue;
        }
        match b[i] {
            b'\\' => {
                let n = line[i..].chars().take(2).map(char::len_utf8).sum::<usize>();
                out.push_str(&line[i..i + n]);
                i += n;
            }
            b'`' => {
                let n = b[i..].iter().take_while(|&&c| c == b'`').count();
                let ticks = &line[i..i + n];
                let mut j = i + n;
                let mut close = None;
                while j < b.len() {
                    if b[j] == b'`' {
                        let m = b[j..].iter().take_while(|&&c| c == b'`').count();
                        if m == n {
                            close = Some(j + m);
                            break;
                        }
                        j += m;
                    } else {
                        j += 1;
                    }
                }
                match close {
                    Some(e) => {
                        out.push_str(&line[i..e]);
                        i = e;
                    }
                    None => {
                        out.push_str(ticks);
                        i += n;
                    }
                }
            }
            b'%' if comments_ok && b.get(i + 1) == Some(&b'%') => {
                *in_comment = true;
                i += 2;
            }
            b'!' if line[i..].starts_with("![[") => {
                if let Some(end) = line[i + 3..].find("]]") {
                    let inner = &line[i + 3..i + 3 + end];
                    if let Some(rep) = embed_markdown(inner) {
                        out.push_str(&rep);
                        i += 3 + end + 2;
                        continue;
                    }
                }
                out.push('!');
                i += 1;
            }
            _ => {
                let c = line[i..].chars().next().unwrap();
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

fn preprocess_pass(src: &str, comment_limit: usize) -> (String, Option<usize>) {
    let mut out = String::with_capacity(src.len());
    let mut fence: Option<(char, usize)> = None;
    let mut in_comment = false;
    let mut comment_start = None;
    for (idx, line) in src.split_inclusive('\n').enumerate() {
        if !in_comment {
            if let Some((c, n, rest)) = fence_of(line) {
                match fence {
                    None => {
                        fence = Some((c, n));
                        out.push_str(line);
                        continue;
                    }
                    Some((fc, fnn)) if fc == c && n >= fnn && rest.trim().is_empty() => {
                        fence = None;
                        out.push_str(line);
                        continue;
                    }
                    _ => {}
                }
            }
            if fence.is_some() {
                out.push_str(line);
                continue;
            }
        }
        let was = in_comment;
        out.push_str(&process_line(line, &mut in_comment, idx < comment_limit));
        if !was && in_comment {
            comment_start = Some(idx);
        }
    }
    (out, if in_comment { comment_start } else { None })
}

/// Obsidian `%%comment%%` removal and `![[embed]]` rewriting, skipping code.
pub(crate) fn preprocess(src: &str) -> String {
    let (out, unterminated) = preprocess_pass(src, usize::MAX);
    match unterminated {
        // A stray `%%` must not swallow the rest of the document.
        Some(line) => preprocess_pass(src, line).0,
        None => out,
    }
}

// ---------------------------------------------------------------------------
// AST rewrites
// ---------------------------------------------------------------------------

fn new_node<'a>(arena: &'a Arena<'a>, v: NodeValue) -> &'a AstNode<'a> {
    arena.alloc(AstNode::new(RefCell::new(Ast::new(v, (0, 0).into()))))
}

fn html_block<'a>(arena: &'a Arena<'a>, s: String) -> &'a AstNode<'a> {
    new_node(
        arena,
        NodeValue::HtmlBlock(NodeHtmlBlock {
            block_type: 6,
            literal: s,
        }),
    )
}

fn plain_text<'a>(n: &'a AstNode<'a>, out: &mut String) {
    match &n.data.borrow().value {
        NodeValue::Text(t) => out.push_str(t),
        NodeValue::Code(c) => out.push_str(&c.literal),
        NodeValue::SoftBreak | NodeValue::LineBreak => out.push(' '),
        _ => {}
    }
    for c in n.children() {
        plain_text(c, out);
    }
}

fn esc_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Returns true (and rewrites the node) if `bq` is a `> [!type]` callout.
fn rewrite_callout<'a>(arena: &'a Arena<'a>, bq: &'a AstNode<'a>) -> bool {
    let Some(p) = bq.first_child() else {
        return false;
    };
    if !matches!(p.data.borrow().value, NodeValue::Paragraph) {
        return false;
    }
    let Some(first) = p.first_child() else {
        return false;
    };
    let lit = match &first.data.borrow().value {
        NodeValue::Text(t) => t.to_string(),
        _ => return false,
    };
    let Some(after_bang) = lit.strip_prefix("[!") else {
        return false;
    };
    let Some(close) = after_bang.find(']') else {
        return false;
    };
    let kind = &after_bang[..close];
    if kind.is_empty()
        || !kind
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return false;
    }
    let kind = kind.to_ascii_lowercase();
    // Header line = everything up to the first line break.
    let mut header = vec![];
    for c in p.children() {
        if matches!(
            c.data.borrow().value,
            NodeValue::SoftBreak | NodeValue::LineBreak
        ) {
            break;
        }
        header.push(c);
    }
    let mut title = after_bang[close + 1..].to_owned();
    for h in header.iter().skip(1) {
        plain_text(h, &mut title);
    }
    // Fold markers (`+` open / `-` closed) are not part of the title.
    let title = title
        .strip_prefix(['+', '-'])
        .unwrap_or(&title)
        .trim()
        .to_owned();
    let title = if title.is_empty() {
        let mut cs = kind.chars();
        cs.next()
            .map(|c| c.to_ascii_uppercase().to_string() + cs.as_str())
            .unwrap_or_default()
    } else {
        title
    };
    // Drop the header line (and its line break) from the paragraph.
    let brk = header.last().and_then(|l| l.next_sibling()).filter(|n| {
        matches!(
            n.data.borrow().value,
            NodeValue::SoftBreak | NodeValue::LineBreak
        )
    });
    for h in &header {
        h.detach();
    }
    if let Some(b) = brk {
        b.detach();
    }
    if p.first_child().is_none() {
        p.detach();
    }
    let open = html_block(
        arena,
        format!("<div class=\"markdown-alert markdown-alert-{kind}\">\n<p class=\"markdown-alert-title\">{}</p>\n", esc_text(&title)),
    );
    let close = html_block(arena, "</div>\n".to_owned());
    let kids: Vec<_> = bq.children().collect();
    bq.insert_before(open);
    for k in kids {
        bq.insert_before(k);
    }
    bq.insert_before(close);
    bq.detach();
    true
}

fn rewrite_task<'a>(arena: &'a Arena<'a>, item: &'a AstNode<'a>) -> bool {
    let Some(p) = item.first_child() else {
        return false;
    };
    if !matches!(p.data.borrow().value, NodeValue::Paragraph) {
        return false;
    }
    let Some(first) = p.first_child() else {
        return false;
    };
    {
        let mut d = first.data.borrow_mut();
        let NodeValue::Text(t) = &mut d.value else {
            return false;
        };
        let rest = match t.strip_prefix("[~]").or_else(|| t.strip_prefix("[-]")) {
            Some(r) if r.is_empty() || r.starts_with([' ', '\t']) => r.trim_start().to_owned(),
            _ => return false,
        };
        *t = rest.into();
    }
    item.data.borrow_mut().value = NodeValue::TaskItem(NodeTaskItem {
        symbol: None,
        symbol_sourcepos: (0, 0, 0, 0).into(),
    });
    if let Some(list) = item.parent() {
        if let NodeValue::List(l) = &mut list.data.borrow_mut().value {
            l.is_task_list = true;
        }
    }
    p.prepend(new_node(
        arena,
        NodeValue::HtmlInline("<span class=\"task-inapplicable\">".into()),
    ));
    p.append(new_node(arena, NodeValue::HtmlInline("</span>".into())));
    true
}

fn is_toc_paragraph<'a>(p: &'a AstNode<'a>) -> bool {
    let kids: Vec<_> = p.children().collect();
    match kids.as_slice() {
        [t] => match &t.data.borrow().value {
            NodeValue::Text(s) => s.trim().eq_ignore_ascii_case("[toc]"),
            NodeValue::WikiLink(l) => l.url == "_TOC_" || l.url == "_TOC_.md",
            _ => false,
        },
        _ => false,
    }
}

/// Apply all AST-level dialect rewrites to a freshly parsed document.
pub(crate) fn rewrite_ast<'a>(arena: &'a Arena<'a>, root: &'a AstNode<'a>) {
    let nodes: Vec<_> = root.descendants().collect();
    // Innermost first so nested callouts are handled before their parents.
    for n in nodes.iter().rev() {
        enum K {
            Quote,
            Item,
            Para,
            Wiki,
            Other,
        }
        let k = match n.data.borrow().value {
            NodeValue::BlockQuote => K::Quote,
            NodeValue::Item(..) => K::Item,
            NodeValue::Paragraph => K::Para,
            NodeValue::WikiLink(..) => K::Wiki,
            _ => K::Other,
        };
        match k {
            K::Quote => {
                rewrite_callout(arena, n);
            }
            K::Item => {
                rewrite_task(arena, n);
            }
            K::Para => {
                if is_toc_paragraph(n) {
                    n.insert_before(html_block(arena, format!("{TOC_PLACEHOLDER}\n")));
                    n.detach();
                }
            }
            K::Wiki => {
                if let NodeValue::WikiLink(w) = &mut n.data.borrow_mut().value {
                    w.url = wikilink_href(&w.url);
                }
            }
            K::Other => {}
        }
    }
}

// ---------------------------------------------------------------------------
// HTML post-processing
// ---------------------------------------------------------------------------

struct Heading {
    level: u8,
    id: String,
    text: String,
}

fn collect_headings(html: &str) -> Vec<Heading> {
    let mut v = vec![];
    let mut rest = html;
    while let Some(i) = rest.find("<h") {
        rest = &rest[i + 2..];
        let b = rest.as_bytes();
        if b.len() < 6 || !(b'1'..=b'6').contains(&b[0]) || !rest[1..].starts_with(" id=\"") {
            continue;
        }
        let level = b[0] - b'0';
        let after = &rest[6..];
        let Some(q) = after.find('"') else { break };
        let id = &after[..q];
        let end = after.find("</h").unwrap_or(after.len());
        let body = &after[q..end];
        const KEY: &str = "data-heading-content=\"";
        if let Some(k) = body.find(KEY) {
            let t = &body[k + KEY.len()..];
            if let Some(e) = t.find('"') {
                v.push(Heading {
                    level,
                    id: id.to_owned(),
                    text: t[..e].to_owned(),
                });
            }
        }
    }
    v
}

fn build_toc(hs: &[Heading]) -> String {
    if hs.is_empty() {
        return String::new();
    }
    let mut out = String::from("<nav class=\"toc\">");
    let mut stack: Vec<u8> = vec![];
    for h in hs {
        match stack.last() {
            None => {
                out.push_str("<ul>");
                stack.push(h.level);
            }
            Some(&l) if h.level > l => {
                out.push_str("<ul>");
                stack.push(h.level);
            }
            Some(_) => {
                out.push_str("</li>");
                while stack.len() > 1 && h.level < *stack.last().unwrap() {
                    out.push_str("</ul></li>");
                    stack.pop();
                }
            }
        }
        out.push_str(&format!("<li><a href=\"#{}\">{}</a>", h.id, h.text));
    }
    out.push_str("</li>");
    for _ in 1..stack.len() {
        out.push_str("</ul></li>");
    }
    out.push_str("</ul></nav>");
    out
}

pub(crate) fn postprocess(mut html: String) -> String {
    if html.contains(TOC_PLACEHOLDER) {
        let toc = build_toc(&collect_headings(&html));
        html = html
            .replace(&format!("{TOC_PLACEHOLDER}\n"), &format!("{toc}\n"))
            .replace(TOC_PLACEHOLDER, &toc);
    }
    mark_inapplicable(html)
}

/// Add the `task-inapplicable` class to the `<li>` and checkbox that precede our marker span.
fn mark_inapplicable(html: String) -> String {
    const MARK: &str = "<span class=\"task-inapplicable\">";
    const INPUT: &str =
        "<input type=\"checkbox\" class=\"task-list-item-checkbox\" disabled=\"\" /> ";
    const LI: &str = "<li class=\"task-list-item\">";
    if !html.contains(MARK) {
        return html;
    }
    let mut out = String::with_capacity(html.len() + 64);
    let mut rest = html.as_str();
    while let Some(i) = rest.find(MARK) {
        let (before, tail) = rest.split_at(i);
        let mut done = false;
        for p in ["", "<p>", "\n<p>"] {
            let suffix = format!("{LI}{INPUT}{p}");
            if let Some(head) = before.strip_suffix(suffix.as_str()) {
                out.push_str(head);
                out.push_str(&format!(
                    "<li class=\"task-list-item task-inapplicable\"><input type=\"checkbox\" class=\"task-list-item-checkbox task-inapplicable\" disabled=\"\" /> {p}"
                ));
                done = true;
                break;
            }
        }
        if !done {
            out.push_str(before);
        }
        out.push_str(MARK);
        rest = &tail[MARK.len()..];
    }
    out.push_str(rest);
    out
}

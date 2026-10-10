//! CommonMark spec conformance (spec.json 0.31.2) through the full render() pipeline
//! (comrak + ammonia). Known differences live in tests/commonmark_allowlist.txt.
//!
//! Set MDV_DUMP_FAILURES=1 to print failing example numbers (allowlist format).

mod common;

use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Deserialize)]
struct Example {
    markdown: String,
    html: String,
    example: u32,
    section: String,
}

const MIN_PASS_RATE: f64 = 0.95;

/// The spec's expected HTML is un-sanitized. To compare like with like we pass it
/// through an ammonia builder configured like md-core's (keep in sync with
/// `sanitizer()` in src/lib.rs) so entity/quote/attribute serialization and
/// allowed-tag differences cancel out. What remains is parser behavior.
fn expected_through_sanitizer(html: &str) -> String {
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
    b.url_schemes(["http", "https", "mailto", "tel"].into_iter().collect());
    b.link_rel(Some("noopener noreferrer"));
    b.clean(html).to_string()
}

/// Normalize the differences that are expected from sanitization and extensions,
/// plus insignificant whitespace.
fn normalize(s: &str) -> String {
    let mut s = s.replace("\r\n", "\n");
    s = s.replace(" rel=\"noopener noreferrer\"", "");
    s = s.replace("<br>", "<br />").replace("<hr>", "<hr />");
    // Heading ids + self-anchors are an intentional viewer feature (TOC / deep links).
    s = strip_heading_anchors(&s);
    // collapse whitespace between tags and around newlines
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(c);
            prev_ws = false;
        }
    }
    out.replace("> <", "><")
        .replace("> ", ">")
        .replace(" <", "<")
        .trim()
        .to_owned()
}

fn strip_heading_anchors(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("<a href=\"#") {
        let tail = &rest[i..];
        match tail.find("</a>") {
            Some(j) if tail[..j].contains("class=\"anchor\"") => {
                out.push_str(&rest[..i]);
                rest = &tail[j + 4..];
            }
            _ => {
                out.push_str(&rest[..i + 1]);
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    // drop id="..." on headings
    let mut res = String::new();
    let mut r = out.as_str();
    while let Some(i) = r.find("<h") {
        res.push_str(&r[..i]);
        let t = &r[i..];
        let end = t.find('>').unwrap_or(t.len() - 1);
        let tag = &t[..=end];
        let is_h = tag.as_bytes().get(2).is_some_and(|c| c.is_ascii_digit());
        if is_h {
            let mut cleaned = String::new();
            let mut q = tag;
            if let Some(k) = q.find(" id=\"") {
                cleaned.push_str(&q[..k]);
                q = &q[k + 5..];
                let e = q.find('"').unwrap_or(0);
                cleaned.push_str(&q[e + 1..]);
            } else {
                cleaned.push_str(q);
            }
            res.push_str(&cleaned);
        } else {
            res.push_str(tag);
        }
        r = &t[end + 1..];
    }
    res.push_str(r);
    res
}

#[test]
fn commonmark_spec_conformance() {
    let path = common::fixtures_dir().join("commonmark/spec.json");
    let examples: Vec<Example> =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let allow: BTreeSet<u32> = include_str!("commonmark_allowlist.txt")
        .lines()
        .map(|l| l.split('#').next().unwrap().trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.parse().unwrap())
        .collect();

    let mut failed = BTreeSet::new();
    let mut by_section: std::collections::BTreeMap<String, (u32, u32)> = Default::default();
    for ex in &examples {
        let got = normalize(&md_core::render(&ex.markdown).html);
        let ok = got == normalize(&expected_through_sanitizer(&ex.html));
        let e = by_section.entry(ex.section.clone()).or_default();
        e.1 += 1;
        if ok {
            e.0 += 1;
        } else {
            failed.insert(ex.example);
        }
    }
    let total = examples.len();
    let passed = total - failed.len();
    let rate = passed as f64 / total as f64;
    println!(
        "CommonMark conformance: {passed}/{total} = {:.2}%",
        rate * 100.0
    );
    for (s, (p, t)) in &by_section {
        if p != t {
            println!("  {s}: {p}/{t}");
        }
    }
    if std::env::var_os("MDV_DUMP_FAILURES").is_some() {
        for n in &failed {
            let ex = examples.iter().find(|e| e.example == *n).unwrap();
            println!("{n} # {} {:?}", ex.section, ex.markdown);
        }
    }

    let new: Vec<_> = failed.difference(&allow).collect();
    assert!(
        new.is_empty(),
        "unexpected new conformance failures (not allowlisted): {new:?}"
    );
    let stale: Vec<_> = allow.difference(&failed).collect();
    assert!(
        stale.is_empty(),
        "allowlisted examples now pass; remove from allowlist: {stale:?}"
    );
    assert!(
        rate >= MIN_PASS_RATE,
        "pass rate {:.2}% below {:.0}%",
        rate * 100.0,
        MIN_PASS_RATE * 100.0
    );
}

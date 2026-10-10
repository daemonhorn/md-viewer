//! Sanitizer / XSS tests: untrusted Markdown must never yield executable HTML.

use md_core::render;

fn html(s: &str) -> String {
    render(s).html.to_ascii_lowercase()
}

#[test]
fn xss_fixture_has_no_active_content() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/edge/xss.md"
    ))
    .unwrap();
    let h = html(&src);
    for bad in [
        "<script",
        "onerror",
        "onload",
        "onclick",
        "ontoggle",
        "javascript:",
        "data:text",
        "<iframe",
        "<object",
        "<embed",
        "<svg",
        "<style",
        "<form",
        "<button",
        "<meta",
        "<base",
        "<link",
        "<math",
        "http-equiv",
        "position:fixed",
        "style=",
    ] {
        assert!(!h.contains(bad), "found {bad:?} in sanitized output:\n{h}");
    }
}

#[test]
fn dangerous_url_schemes_removed() {
    for src in [
        "[a](javascript:alert(1))",
        "[a](JaVaScRiPt:alert(1))",
        "[a](&#106;avascript:alert(1))",
        "[a](vbscript:x)",
        "[a](data:text/html;base64,AAAA)",
        "![a](javascript:alert(1))",
        "<a href=\"  javascript:alert(1)\">x</a>",
        "<a href=\"java\tscript:alert(1)\">x</a>",
        "<img src=\"data:image/svg+xml,<svg onload=alert(1)>\">",
        "<https://x.com>\n\n[x]: javascript:alert(1)\n\n[x]",
    ] {
        let h = html(src);
        assert!(
            !h.contains("javascript:") && !h.contains("vbscript:") && !h.contains("data:"),
            "{src:?} -> {h}"
        );
    }
}

#[test]
fn event_handlers_and_style_removed() {
    for src in [
        "<img src=x onerror=alert(1)>",
        "<div onmouseover=\"alert(1)\">x</div>",
        "<p style=\"background:url(javascript:alert(1))\">x</p>",
        "<details open ontoggle=alert(1)>x</details>",
        "<input autofocus onfocus=alert(1)>",
    ] {
        let h = html(src);
        assert!(
            !h.contains("onerror")
                && !h.contains("onmouseover")
                && !h.contains("ontoggle")
                && !h.contains("onfocus"),
            "{h}"
        );
        assert!(!h.contains("style="), "{h}");
    }
}

#[test]
fn comments_and_unclosed_tags_are_contained() {
    let h = html(
        "<!-- <script>alert(1)</script> -->\n\n<script\n\n<img src=\"x\" onerror=\"alert(1)\"",
    );
    assert!(!h.contains("<script") && !h.contains("onerror=\""), "{h}");
}

#[test]
fn safe_content_preserved() {
    let h = render("[ok](https://example.com) [m](mailto:a@b.c) ![i](img/a.png) [r](docs/x.md#y)\n\n<details><summary>s</summary>b</details>").html;
    assert!(h.contains("href=\"https://example.com\""));
    assert!(h.contains("mailto:a@b.c"));
    assert!(h.contains("src=\"img/a.png\""));
    assert!(h.contains("href=\"docs/x.md#y\""));
    assert!(h.contains("<details>") && h.contains("<summary>"));
}

#[test]
fn links_get_rel_noopener() {
    let h = render("[a](https://example.com)").html;
    assert!(h.contains("noopener") && h.contains("noreferrer"), "{h}");
    let h = render("<a href=\"https://example.com\" target=\"_blank\">x</a>").html;
    assert!(!h.contains("target="), "{h}");
}

#[test]
fn math_and_mermaid_do_not_enable_script() {
    let h = html("$<script>alert(1)</script>$\n\n```mermaid\n<script>alert(1)</script>\n```\n");
    assert!(!h.contains("<script"), "{h}");
}

#[test]
fn only_checkbox_inputs_survive() {
    for src in [
        "<input type=text>",
        "<input type=password name=p>",
        "<input type=file>",
        "<input type=\"image\" src=x>",
        "<input>",
    ] {
        assert!(!html(src).contains("<input"), "{src:?}");
    }
    let h = html("<input type=checkbox checked>");
    assert!(
        h.contains("type=\"checkbox\"") && h.contains("disabled"),
        "{h}"
    );
}

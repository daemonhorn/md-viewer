//! Regression tests for super-linear render time. Sizes are chosen so that the old
//! quadratic code needed many seconds even in release builds while linear code takes
//! well under a second in debug builds; budgets are generous for slow CI.

use std::time::{Duration, Instant};

use md_core::render;

fn timed(name: &str, src: &str, budget: Duration) -> String {
    let t = Instant::now();
    let html = render(src).html;
    let el = t.elapsed();
    assert!(el < budget, "{name}: {el:?} exceeds {budget:?}");
    html
}

const BUDGET: Duration = Duration::from_secs(5);

#[test]
fn many_duplicate_headings() {
    let n = 20_000;
    let h = timed("dup headings", &"# A\n\n".repeat(n), BUDGET);
    assert!(h.contains("<h1 id=\"a\"") && h.contains("<h1 id=\"a-1\""));
    assert!(h.contains(&format!("<h1 id=\"a-{}\"", n - 1)));
}

#[test]
fn many_duplicate_headings_with_toc() {
    let n = 10_000;
    let h = timed(
        "dup headings + toc",
        &format!("[TOC]\n\n{}", "## B\n\n".repeat(n)),
        BUDGET,
    );
    assert!(
        h.contains("<a href=\"#b-9999\""),
        "toc link for last heading missing"
    );
    assert!(h.contains("<h2 id=\"b-9999\""));
}

#[test]
fn repeated_embed_openers_on_one_line() {
    let n = 100_000;
    timed("![[ x n", &"![[".repeat(n), BUDGET);
    timed(
        "![[# x n ]]",
        &format!("{}]]", "![[#".repeat(n / 2)),
        BUDGET,
    );
    let h = timed("embeds", &"![[a.png]]".repeat(20_000), BUDGET);
    assert_eq!(h.matches("<img").count(), 20_000);
}

#[test]
fn backtick_runs_of_distinct_lengths() {
    let s: String = (1..=1500).map(|i| "`".repeat(i) + " ").collect();
    timed("distinct backtick runs", &s, BUDGET);
    timed("alternating", &"` `` ".repeat(20_000), BUDGET);
}

#[test]
fn comments_and_callouts() {
    timed("%% pairs", &"%%a%% ".repeat(50_000), BUDGET);
    timed("%% lines", &"a %%\n".repeat(20_000), BUDGET);
    timed("callouts", &"> [!note]\n> x\n\n".repeat(10_000), BUDGET);
    timed("tasks", &"- [~] x\n".repeat(20_000), BUDGET);
    timed(
        "tocs",
        &format!("{}# A\n", "[TOC]\n\n".repeat(5_000)),
        BUDGET,
    );
}

#[test]
fn deep_nesting_is_bounded() {
    let t = Instant::now();
    let h = render(&format!("{}x\n", "> ".repeat(10_000))).html;
    assert!(h.contains("nested too deeply"));
    let h = render(&format!(
        "{}x{}",
        "<div>".repeat(100_000),
        "</div>".repeat(100_000)
    ))
    .html;
    assert!(h.contains("nested too deeply"));
    assert!(t.elapsed() < BUDGET);
}

#[test]
fn oversized_input_gets_notice() {
    let t = Instant::now();
    let h = render(&"x".repeat(16 * 1024 * 1024 + 1)).html;
    assert!(h.contains("too large"));
    assert!(t.elapsed() < Duration::from_secs(1));
}

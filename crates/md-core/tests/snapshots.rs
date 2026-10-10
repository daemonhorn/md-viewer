//! Golden snapshot tests: render every fixture .md and compare with the committed .snap.
//! Review changes with `cargo insta review` (or INSTA_UPDATE=always).

mod common;

use std::fs;
use std::path::{Path, PathBuf};

/// Too large to snapshot; covered by property tests below.
const NO_SNAPSHOT: &[&str] = &["edge/huge.md", "edge/deep-nesting.md"];
/// Documentation, not a render fixture.
const SKIP_FILES: &[&str] = &["SOURCES.md"];

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "md") {
            out.push(p);
        }
    }
}

fn fixtures() -> Vec<(String, String)> {
    let root = common::fixtures_dir();
    let mut v = Vec::new();
    collect(&root, &mut v);
    v.sort();
    v.into_iter()
        .map(|p| {
            let rel = p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
            (rel, fs::read_to_string(&p).unwrap())
        })
        .collect()
}

#[test]
fn golden_snapshots() {
    let all = fixtures();
    assert!(all.len() >= 25, "expected the fixture set, found {}", all.len());
    for (rel, src) in all {
        if NO_SNAPSHOT.contains(&rel.as_str()) || SKIP_FILES.contains(&rel.as_str()) {
            continue;
        }
        let r = md_core::render(&src);
        let out = format!(
            "front_matter: {:?}\nneeds_mermaid: {}\nneeds_math: {}\n---\n{}",
            r.front_matter, r.needs_mermaid, r.needs_math, r.html
        );
        let name = rel.trim_end_matches(".md").replace('/', "__");
        insta::with_settings!({ snapshot_path => "snapshots", prepend_module_to_snapshot => false, omit_expression => true }, {
            insta::assert_snapshot!(name, out);
        });
    }
}

#[test]
fn huge_and_deep_inputs_render_without_blowup() {
    let root = common::fixtures_dir().join("edge");
    let huge = fs::read_to_string(root.join("huge.md")).unwrap();
    let t = std::time::Instant::now();
    let r = md_core::render(&huge);
    assert!(r.html.matches("<h2").count() == 2000);
    assert!(t.elapsed().as_secs() < 10, "huge doc too slow: {:?}", t.elapsed());

    let deep = fs::read_to_string(root.join("deep-nesting.md")).unwrap();
    let r = md_core::render(&deep);
    assert!(r.html.contains("deep quote"));
}

#[test]
fn dialect_features() {
    let root = common::fixtures_dir();
    let rd = |p: &str| md_core::render(&fs::read_to_string(root.join(p)).unwrap());
    let a = rd("github/alerts.md");
    assert_eq!(a.html.matches("markdown-alert ").count(), 5);
    assert!(rd("github/mermaid-math.md").needs_mermaid);
    assert!(rd("github/mermaid-math.md").needs_math);
    assert!(rd("github/details.md").html.contains("<details"));
    assert!(rd("github/emoji.md").html.contains('\u{1F389}'));
    assert!(rd("gitlab/multiline-quote.md").html.contains("<blockquote>"));
    assert!(rd("pandoc/deflist.md").html.contains("<dl>"));
    let bom = rd("edge/bom-frontmatter.md");
    assert_eq!(bom.front_matter.as_deref(), Some("title: bom"));
    assert!(bom.html.contains("BOM doc") && !bom.html.contains('\u{feff}'));
    let crlf = rd("edge/crlf.md");
    assert!(crlf.html.contains("<table>") && crlf.html.contains("<li>a</li>"));
}

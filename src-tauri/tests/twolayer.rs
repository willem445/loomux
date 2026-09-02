//! The two-layer shape for agent-authored prose (#1968): a short human layer
//! first, the evidence collapsed in an agent layer below it.
//!
//! What this file can pin is the SHAPE of the instruction, not the prose an
//! agent writes from it — nothing here measures a PR body, and nothing can.
//! What it does pin is that the two templates which dictate a body's shape
//! still TELL an agent to write both layers, and that the collapsible's
//! `<summary>` is one canonical string across every shipped template rather
//! than N paraphrases: a human learns one affordance, a `grep` finds every
//! fold, and the squash-message cut has one line to look for.
//!
//! Integration tests, not unit tests, per CLAUDE.md constraint 4: a test
//! executable linking the full lib needs the comctl32-v6 manifest `build.rs`
//! embeds through `-tests`-scoped link args.
//!
//! **Default-deny, and decided on shapes rather than on names** (CLAUDE.md's
//! source-scanning-guard convention). [`every_fold_in_every_template_uses_the_one_canonical_wording`]
//! reads the templates directory off disk and judges any file that opens a
//! `<details>` or writes a `<summary>`, whatever it is called — a template
//! added tomorrow is covered without being listed here. The two REQUIRED rows
//! are named, and required exactly once each, so a rename fails loudly rather
//! than silently watching nothing.
//!
//! **Residual, stated:** this is a scan over instruction text. It cannot see a
//! body an agent actually posts, and it does not bound prose LENGTH — the rule
//! it enforces is about what sits above the fold, never how much.

use std::fs;
use std::path::PathBuf;

/// The one `<summary>` an agent-authored fold ever carries.
///
/// Pinned here rather than in each template's own words because it is an
/// interface: the orchestrator's squash-message cut, a reader's habit, and any
/// future sweep for folds all key on it.
const SUMMARY: &str = "<summary>Agent context — evidence, receipts, instruments</summary>";

/// The line a squash message is cut at. An HTML comment so it renders as
/// nothing on github.com, and an exact-line match so the cut needs no HTML
/// parsing and leaves a legitimate `<details>` in the human layer alone.
const MARKER: &str = "<!-- agent-layer -->";

/// The templates that dictate the SHAPE of a body an agent posts — a PR body
/// (`worker.md`) and a review (`reviewer.md`). Each is required exactly once:
/// a rename reddens here instead of quietly emptying the population.
const REQUIRE_BOTH_LAYERS: [&str; 2] = ["worker.md", "reviewer.md"];

fn templates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/orchestration/templates")
}

/// Every `*.md` under the templates directory, as (file name, contents).
fn templates() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = fs::read_dir(templates_dir())
        .expect("templates dir must be readable")
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().map(|x| x == "md").unwrap_or(false))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let text = fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {name}: {e}"));
            (name, text)
        })
        .collect();
    out.sort();
    out
}

/// Line endings differ between a Windows checkout (CRLF) and CI's Linux one
/// (LF), and every assertion here is about words. Normalize once.
fn lf(s: &str) -> String {
    s.replace("\r\n", "\n")
}

#[test]
fn the_worker_and_reviewer_templates_name_both_layers_and_open_the_fold() {
    let all = templates();
    // Vacuity control: the scan must have found files at all, and it must have
    // found BOTH required rows. `is_empty()`-style absence assertions below
    // pass just as well over an empty population (CLAUDE.md).
    assert!(all.len() >= 6, "template scan found only {} files — did the directory move?", all.len());

    let mut seen = 0usize;
    for want in REQUIRE_BOTH_LAYERS {
        let hits: Vec<_> = all.iter().filter(|(n, _)| n == want).collect();
        assert_eq!(hits.len(), 1, "{want} must exist exactly once in the templates dir");
        let text = lf(&hits[0].1);
        seen += 1;

        // Counted as whole LINES, which is the rule the template states and the
        // shape the squash cut matches (`sed -n '/^<!-- agent-layer -->$/q;p'`).
        // A substring count would be the wrong instrument in both directions:
        // it reads a legitimate mid-sentence mention inside a code span — which
        // `worker.md` itself makes, stating this very rule — as a second fold.
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        let n = lines.iter().filter(|l| **l == SUMMARY).count();
        assert_eq!(
            n, 1,
            "{want} must carry the canonical agent-layer <summary> on exactly one line, found {n}:\n  {SUMMARY}"
        );
        let m = lines.iter().filter(|l| **l == MARKER).count();
        assert_eq!(
            m, 1,
            "{want} must show the {MARKER} line on exactly one line, found {m} — it is where a squash message is cut"
        );

        // Both layers NAMED, not merely a fold pasted in: an agent that is told
        // to collapse evidence but never told what belongs above the fold has
        // been given half the rule.
        let lower = text.to_lowercase();
        assert!(lower.contains("human layer"), "{want} must name the human layer");
        assert!(lower.contains("agent layer"), "{want} must name the agent layer");

        // The blank line after `</summary>` is load-bearing: without it a table
        // inside the fold renders as literal pipes on github.com (measured
        // through the GFM endpoint, #1968). Pin it where the template SHOWS the
        // shape, so an edit that closes the gap is caught here rather than in a
        // body somebody already posted.
        let at = lines.iter().position(|l| *l == SUMMARY).expect("counted above");
        assert_eq!(
            lines.get(at + 1).copied(),
            Some(""),
            "{want}: the line after the <summary> must be blank — without it a table \
             inside the fold renders as literal pipes on github.com"
        );
    }
    assert_eq!(seen, REQUIRE_BOTH_LAYERS.len());
}

#[test]
fn every_fold_in_every_template_uses_the_one_canonical_wording() {
    let all = templates();
    let mut folds = 0usize;
    for (name, raw) in &all {
        let text = lf(raw);
        // Decided on SHAPE, never on a file's name: the trigger is a LINE that
        // opens a fold, trimmed. That is the shape a real fold has, and it
        // separates one from prose mentioning `<summary>` inside a code span
        // (which `worker.md` legitimately does when it states the exactly-once
        // rule) — matching the bare substring would pull such a file in and
        // demand a fold it never claimed to have. A template added tomorrow is
        // judged without being listed anywhere in this file.
        let opens: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("<details") || l.starts_with("<summary"))
            .collect();
        if opens.is_empty() {
            continue;
        }
        folds += 1;
        assert!(
            text.contains(SUMMARY),
            "{name} opens a fold but does not write the canonical summary:\n  {SUMMARY}"
        );
        assert!(
            text.contains(MARKER),
            "{name} opens a fold without the {MARKER} line a squash cut needs"
        );
        // No paraphrases alongside it: every line that OPENS a fold is one of
        // the two canonical ones, so a second fold cannot arrive in its own
        // words.
        for line in &opens {
            assert!(
                *line == "<details>" || *line == SUMMARY,
                "{name} opens a fold with an unrecognized line: {line:?}"
            );
        }
    }
    // Population control (CLAUDE.md: a positive control proves the mechanism
    // RAN). Counted at the VERIFIED site — these are files whose assertions
    // above actually executed, not files the loop merely visited.
    assert!(
        folds >= REQUIRE_BOTH_LAYERS.len(),
        "only {folds} template(s) carried a fold — the scan saw nothing to judge"
    );
}

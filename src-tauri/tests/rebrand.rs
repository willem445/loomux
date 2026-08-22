//! #1153 phase 3: the emit side of the protocol rename stays single-spelled.
//!
//! The rename's whole rule is **emit one spelling, accept every spelling**. The
//! *accept* half is behavioural and is pinned by behavioural tests — the
//! marker set against the sanitizer (`brand`), a pre-rename transcript against
//! `detect_orch_signature` (`sessions`), a pre-rename queue entry against
//! `is_loomux_notice` (`queue`), a pre-rename launch command against
//! `stripSoloMcpFlags` (`test/panerestore.test.ts`).
//!
//! The *emit* half has no such test, and cannot have one: it is a claim about
//! every emitter that exists, including the one somebody writes next week by
//! copying the line above it. A new `format!("[loomux] …")` would compile, run,
//! and deliver a notice that every reader still accepts — which is exactly why
//! nothing would go red. This scan is what makes that a failure.
//!
//! # What it decides on
//!
//! Default-deny on the **token**, never on a binding's name (the repo's
//! source-scanning-guard convention): a legacy protocol literal on a code line
//! anywhere under the scanned roots is a failure unless its file is on
//! `ALLOWED`, and each row there carries the reason it is allowed and is
//! re-checked so a stale row fails loudly rather than watching nothing.
//!
//! # Stated blind spots
//!
//! - **Comment lines are skipped.** They discuss the legacy spelling by
//!   design — `brand`'s own docs argue at length about why each legacy
//!   constant exists — and a marker in a comment emits nothing.
//! - **A literal split across a concatenation** (`"[loo" "mux]"`) or built at
//!   runtime (`format!("[{}]", legacy)`) is invisible here. Neither appears
//!   today; the point of saying so is that this scan is a net, not a proof.
//! - **Only Rust source and the role templates are scanned.** The frontend
//!   legitimately holds both spellings (`panerestore.ts`'s accepted arrays are
//!   the feature, not a leak), and the test suites hold pre-rename specimens
//!   on purpose; allow-listing all of those would leave the scan enforcing
//!   nothing worth the file it lives in.

use std::path::{Path, PathBuf};

/// Both source roots, manifest-anchored the way `tests/groupid.rs` anchors its
/// own. `brand`, `queue`, `sessions` and `notify` live in the engine crate
/// while `mod.rs`, `mcp.rs` and the role templates live under `src-tauri/src`
/// — a scan over one root would be green forever while enforcing half the
/// tree, which is the failure that scan records for itself. The templates need
/// no root of their own: they sit under the first one and this walk takes
/// `.md` as well as `.rs`.
const ROOTS: [&str; 2] = [
    concat!(env!("CARGO_MANIFEST_DIR"), "/src"),
    concat!(env!("CARGO_MANIFEST_DIR"), "/../crates/loomux-engine/src"),
];

/// Every pre-#1153 protocol literal, with what an accidental emission of it
/// would actually do. These are the strings this app must never WRITE again;
/// that it must still READ them is a different claim, pinned elsewhere.
const LEGACY_LITERALS: [(&str, &str); 3] = [
    (
        "[loomux]",
        "a notice an agent briefed after the rename is not told to recognise",
    ),
    (
        "X-Loomux-Agent",
        "a token header the current server accepts only through its legacy arm",
    ),
    (
        "mcp__loomux",
        "an allowlist entry that denies every tool call a new group's agent makes",
    ),
];

/// Files permitted to spell a legacy literal on a code line, each with the
/// reason. Every row is checked to still contain one: a row that has gone
/// stale is a hole nobody is watching, so it fails here rather than silently
/// widening the allowlist.
const ALLOWED: [(&str, &str); 1] = [(
    "brand.rs",
    "defines the LEGACY_ constants themselves — the one place the old names are \
     spelled on purpose, and the module whose whole job is being that place",
)];

fn rust_and_template_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_and_template_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs" || x == "md") {
            out.push(p);
        }
    }
}

/// A line that only *talks about* a literal. Deliberately crude and stated as
/// such above: a `//`-leading line in Rust, a `#`-leading one nowhere (the
/// templates are prose, and a template naming the legacy marker IS the defect
/// this catches, so nothing is skipped there).
fn is_comment(line: &str, is_rust: bool) -> bool {
    is_rust && line.trim_start().starts_with("//")
}

#[test]
fn no_shipped_code_line_writes_a_pre_rename_protocol_literal() {
    let mut files = Vec::new();
    for root in ROOTS {
        rust_and_template_files(Path::new(root), &mut files);
    }
    assert!(
        files.len() > 20,
        "the scan found only {} files — a root moved and this test is watching nothing",
        files.len()
    );

    let mut offences: Vec<String> = Vec::new();
    let mut allowed_hits = [0usize; ALLOWED.len()];

    for path in &files {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let is_rust = path.extension().is_some_and(|x| x == "rs");
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let allow_idx = ALLOWED.iter().position(|(f, _)| *f == name);

        for (n, line) in text.lines().enumerate() {
            if is_comment(line, is_rust) {
                continue;
            }
            for (literal, consequence) in LEGACY_LITERALS {
                if !line.contains(literal) {
                    continue;
                }
                match allow_idx {
                    Some(i) => allowed_hits[i] += 1,
                    None => offences.push(format!(
                        "{}:{} writes `{literal}` — {consequence}\n    {}",
                        path.display(),
                        n + 1,
                        line.trim()
                    )),
                }
            }
        }
    }

    assert!(
        offences.is_empty(),
        "the rename emits exactly one spelling; these lines write a pre-rename one.\n\
         If a line genuinely must (a new compatibility seam), add its file to `ALLOWED` \
         with the reason — do not delete this test.\n\n{}",
        offences.join("\n")
    );

    for (i, (file, reason)) in ALLOWED.iter().enumerate() {
        assert!(
            allowed_hits[i] > 0,
            "`{file}` is allow-listed here (\"{reason}\") but no longer spells any legacy \
             literal. Either the compatibility seam moved — in which case this row is \
             watching nothing and the new home is unguarded — or it is gone and the row \
             should be. Neither is a thing to leave silent."
        );
    }
}

/// The companion claim, and the one that makes the scan above mean something:
/// the literals it bans are the ones the app still ACCEPTS. A future edit that
/// retired a legacy constant would make its row here a ban on a string nothing
/// reads — harmless, but no longer a compatibility guarantee — so the two are
/// asserted against each other rather than maintained in parallel.
#[test]
fn every_banned_literal_is_one_the_app_still_accepts() {
    use loomux_lib::orchestration::brand;

    let banned: Vec<&str> = LEGACY_LITERALS.iter().map(|(l, _)| *l).collect();
    for l in [brand::LEGACY_NOTICE_MARKER, brand::LEGACY_AGENT_TOKEN_HEADER, brand::LEGACY_MCP_TOOL_PREFIX] {
        assert!(
            banned.contains(&l),
            "`{l}` is still accepted on a reading surface but nothing stops a new emitter \
             from writing it — add it to LEGACY_LITERALS"
        );
    }
    assert_eq!(
        banned.len(),
        3,
        "LEGACY_LITERALS and the accepted set must stay the same size, or one of them is \
         carrying an entry the other has never heard of"
    );
}

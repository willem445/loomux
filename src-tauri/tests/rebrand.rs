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
//! - **Only Rust source and the role templates are scanned**, and within them
//!   only the SHIPPED part. The frontend legitimately holds both spellings
//!   (`panerestore.ts`'s accepted arrays are the feature, not a leak), and
//!   every test suite holds pre-rename specimens on purpose — including the
//!   `#[cfg(test)]` modules that live INSIDE a scanned file, which is why
//!   scanning stops at the first one. Allow-listing all of those instead
//!   would leave the scan enforcing nothing worth the file it lives in.
//! - **Scanning stops at the first `#[cfg(test)]` line and does not resume.**
//!   True for this repo, where the convention is one trailing test module per
//!   file, and stated because it is a real limit rather than a proof: a file
//!   that put production code AFTER a test module would go unscanned from
//!   that point. Nothing does today.

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

/// Where a Rust file stops being shipped code. See the blind-spot note above:
/// a unit test's pre-rename specimen is a specimen, not an emitter, and this
/// scan is about what the app WRITES.
fn shipped_len(text: &str, is_rust: bool) -> usize {
    if !is_rust {
        return text.lines().count();
    }
    text.lines().position(|l| l.trim_start().starts_with("#[cfg(test)]")).unwrap_or(usize::MAX)
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

        let shipped = shipped_len(&text, is_rust);
        for (n, line) in text.lines().enumerate() {
            if n >= shipped {
                break;
            }
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

/// The self-launch refusal shim (#815) is written to disk under the name of the
/// program it blocks, and that name is **not** this phase's to rename: the
/// launcher an agent could actually type is still `loomux` (phase 5 owns the
/// npm package and the installed exe). A shim written under any other name
/// blocks nothing while leaving the real launcher runnable — the guard
/// defeated, with no error anywhere.
///
/// This PR's own bulk audit-actor sweep took that argument, because it is a
/// bare `"loomux"` in the same shape as ~240 actor arguments. `tests/pathseg.rs`
/// caught it — its allowlist row quotes this call site verbatim as the proof
/// that `program` is a literal — and it caught it for a reason that has nothing
/// to do with what the argument MEANS.
///
/// So this pins the meaning: whatever name that call passes, it must be a
/// command the launcher package actually installs. It reddens on the day phase
/// 5 renames the npm bin without renaming the shim, and on any future sweep
/// that rewrites the argument to something no user can type.
///
/// Scanned rather than called because `write_refusal_shim` and `ensure_shims`
/// are both private, and `shim_dir` resolves to a sibling of the registry root
/// — outside a test's own tempdir, i.e. shared — so a filesystem assertion here
/// would be cross-contaminating rather than isolated. The blind spot that
/// leaves is stated plainly: this reads the call, not the write.
#[test]
fn the_self_launch_shim_is_named_after_a_command_the_launcher_installs() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/orchestration/mod.rs"
    ))
    .expect("mod.rs");
    let call = "write_refusal_shim(&dir, ";
    let i = src.find(call).expect(
        "the self-launch refusal shim's call site moved — find it and re-point this test, \
         because what it pins is the one thing that makes the shim work at all",
    );
    let rest = &src[i + call.len()..];
    let arg: String = rest
        .strip_prefix('"')
        .expect("the program argument must stay a literal — `tests/pathseg.rs` allow-lists this call site on exactly that basis")
        .chars()
        .take_while(|c| *c != '"')
        .collect();

    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../npm/package.json"))
        .expect("npm/package.json");
    let pkg: serde_json::Value = serde_json::from_str(&manifest).expect("npm/package.json parses");
    let bins: Vec<&str> = pkg["bin"]
        .as_object()
        .expect("npm/package.json declares a bin map")
        .keys()
        .map(|k| k.as_str())
        .collect();

    assert!(
        bins.contains(&arg.as_str()),
        "the refusal shim is written as `{arg}`, which the launcher package does not install \
         (its bins are {bins:?}). An agent typing the real launcher name would not be blocked, \
         and nothing else in this repo would notice."
    );
}

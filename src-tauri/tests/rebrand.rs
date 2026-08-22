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
//! - **Two accepted spellings are un-bannable, and that is the scan's widest
//!   hole** (rev-967 N2). `brand::LEGACY_AUDIT_ACTOR` and
//!   `brand::LEGACY_MCP_SERVER` are both the bare word `loomux`, which also
//!   names the crate, the launcher binary, the worktree convention and a
//!   thousand comments — banning it as a literal would be noise, not a
//!   guard. So a newly hand-written `, "loomux",` audit-actor argument is
//!   invisible here. That is exactly the shape of the defect this PR caught
//!   in itself: a bulk sweep put `brand::AUDIT_ACTOR` where a launcher
//!   FILENAME belonged, and `tests/pathseg.rs` caught it for an unrelated
//!   reason. Stated so the next reader does not mistake this scan's silence
//!   on the bare word for coverage of it.
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
        "these three are the BANNABLE part of the accepted set — the ones distinctive \
         enough to ban as literals. It is deliberately NOT the whole accepted set (see \
         the module's blind spots), so this pins the three, never an equality with it"
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

/// The token-header axis, which had **no witness at all** before this test.
///
/// Every fixture in the tree carried the current spelling: the config writers
/// emit it, and the suite reaches `resolve_token` directly rather than through
/// the HTTP layer where the header is actually read. So the guarantee that a
/// pre-rename group keeps authenticating rested on a line nothing exercised —
/// a value every fixture happens to share, on the axis the rename made
/// load-bearing.
///
/// It is the most consequential dual-accept in #1153 phase 3. An agent's MCP
/// config is written once, at group create, and lives in that group's dir; a
/// group created before the flag day presents the old header on every call it
/// will ever make. A server reading only the current name fails **every tool
/// call in every live group** the moment the app updates underneath it, and it
/// fails as an auth error, which reads like a bad token rather than an upgrade.
#[test]
fn the_server_takes_a_token_under_either_header_spelling_and_nothing_else() {
    use loomux_lib::orchestration::mcp::is_agent_token_header;

    assert!(is_agent_token_header("X-Orrerix-Agent"), "the spelling every config written from now on carries");
    assert!(is_agent_token_header("X-Loomux-Agent"), "…and the one every live pre-rename group still presents");

    // Case-insensitive on both, because HTTP field names are and nothing
    // guarantees a proxy preserves the casing a generated config wrote.
    assert!(is_agent_token_header("x-orrerix-agent"));
    assert!(is_agent_token_header("X-LOOMUX-AGENT"));

    // The negative control, without which "accept everything" would pass every
    // assertion above: dual-accept widens the accepted SET, never the shape.
    for other in [
        "X-Orrerix",
        "X-Orrerix-Agent-Token",
        "Authorization",
        "X-Someone-Else-Agent",
        "",
    ] {
        assert!(!is_agent_token_header(other), "{other:?} is not an agent token header");
    }
}

/// The accepted MCP identities are written down **twice**, and this is the only
/// thing that keeps the two copies honest.
///
/// `brand` says "write the accepted set down exactly once", and within one
/// language it does. But tab restore reads a recorded command line in
/// TypeScript, across a process boundary no Rust constant reaches, so
/// `panerestore.ts` carries its own `MCP_TOOL_PREFIXES` / `MCP_SERVERS`. That
/// is a real duplication, not a tidy one — so it is asserted rather than
/// asserted-away: whatever Rust accepts, the frontend must accept too.
///
/// The failure this prevents is asymmetric and quiet. Rust dropping a spelling
/// the frontend still strips is harmless; the frontend dropping one Rust still
/// mints leaves a dead `--mcp-config` path in a replayed command, and the pane
/// boots against a file agent exit already deleted.
///
/// Textual, with the limits that implies: it reads the array literals, so a
/// spelling assembled at runtime or moved to another module would read as
/// absent. Neither happens today, and the assertion names the file so a move
/// fails loudly here rather than silently in a user's restored tab.
#[test]
fn the_frontend_accepts_every_mcp_identity_the_backend_still_mints() {
    use loomux_lib::orchestration::brand;

    let ts = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../src/panerestore.ts"
    ))
    .expect("src/panerestore.ts — if this moved, re-point the test; the coupling did not go away");

    for (array, spellings) in [
        ("MCP_TOOL_PREFIXES", [brand::MCP_TOOL_PREFIX, brand::LEGACY_MCP_TOOL_PREFIX]),
        ("MCP_SERVERS", [brand::MCP_SERVER, brand::LEGACY_MCP_SERVER]),
    ] {
        let line = ts
            .lines()
            .find(|l| l.contains(&format!("const {array} =")))
            .unwrap_or_else(|| panic!("`{array}` is gone from panerestore.ts — tab restore's accepted set has moved somewhere this test cannot see"));
        for spelling in spellings {
            assert!(
                line.contains(&format!("\"{spelling}\"")),
                "the backend still accepts `{spelling}` but `{array}` does not list it: {line}\n\
                 A tab recorded under that identity would keep its dead --mcp-config path on restore."
            );
        }
    }
}

/// rev-967 N4. `COPILOT_MCP_TOOL_GRANTS`'s wildcard is a second spelling of
/// the server name, and `brand` argues at length that such a literal may exist
/// only because a test computes and compares it (that is the whole excuse for
/// `MCP_TOOL_PREFIX`). This one had no such test.
///
/// It earns the pin more than the prefix does: this array is the value a
/// REPAIRED persona is granted, so a wildcard naming a server nobody declares
/// is a delegate launched with no orchestration tools — and the repair path is
/// exactly where rev-967 B1 found a capability being widened, so nothing about
/// this array should be taken on trust.
#[test]
fn the_copilot_grant_wildcard_is_derived_from_the_server_name() {
    use loomux_lib::orchestration::{COPILOT_MCP_TOOL_GRANTS, MCP_SERVER};

    assert_eq!(COPILOT_MCP_TOOL_GRANTS[0], format!("{MCP_SERVER}/*"));
    assert_eq!(COPILOT_MCP_TOOL_GRANTS[1], MCP_SERVER);
}

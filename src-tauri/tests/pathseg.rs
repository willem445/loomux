//! `PathSegment` — the one validated-segment mechanism, and the families that
//! now reach a path join only through it (#925; layer 2 of #888 §0, the half
//! #904 left).
//!
//! Integration tests, not unit tests: they link the full lib, which on Windows
//! needs the comctl32-v6 manifest `build.rs` embeds via `-tests`-scoped link
//! args (CLAUDE.md constraint 4).
//!
//! Structured like `groupid.rs`'s suite, deliberately, because the argument is
//! the same one and a reader who knows that file should recognize this one:
//!
//! 1. **Refusal** — every path-shaped id the constructor must reject.
//! 2. **Acceptance** — every id shape loomux and the agent CLIs actually mint. A
//!    validator that rejects a real session id is not hardening, it is an
//!    outage, and this half is what makes the alphabet a claim rather than a
//!    guess.
//! 3. **The consolidation** — that the four validators this replaced now give
//!    one answer, including on the inputs they used to disagree about.

use loomux_lib::orchestration::{PathSegment, SegmentError};

// ─────────────────────────── 1. refusal ───────────────────────────

/// The traversal alphabet, one shape per row. Each is a *distinct* way to name
/// something other than a single child of the directory it is used under, so a
/// regression that re-opens one of them cannot hide behind the others passing.
#[test]
fn parse_refuses_every_path_shaped_segment() {
    let cases: &[(&str, SegmentError)] = &[
        // the classic traversal, and the pieces it is built from
        ("..", SegmentError::IllegalChar('.')),
        ("../escaped", SegmentError::IllegalChar('.')),
        ("../../../../Users/me/.ssh", SegmentError::IllegalChar('.')),
        ("..\\escaped", SegmentError::IllegalChar('.')),
        (".", SegmentError::IllegalChar('.')),
        // a dot anywhere at all, which is what makes `..` unspellable
        ("s.1", SegmentError::IllegalChar('.')),
        // separators, forward and back
        ("a/b", SegmentError::IllegalChar('/')),
        ("a\\b", SegmentError::IllegalChar('\\')),
        ("/absolute", SegmentError::IllegalChar('/')),
        ("\\\\server\\share", SegmentError::IllegalChar('\\')),
        // nothing at all
        ("", SegmentError::Empty),
        // bytes a filesystem or a log line would rather not see
        ("s\0x", SegmentError::IllegalChar('\0')),
        ("s\nx", SegmentError::IllegalChar('\n')),
        ("s x", SegmentError::IllegalChar(' ')),
        (" s", SegmentError::IllegalChar(' ')),
        ("s ", SegmentError::IllegalChar(' ')),
        ("s*", SegmentError::IllegalChar('*')),
        ("s?", SegmentError::IllegalChar('?')),
        // non-ASCII: where homoglyph and normalization confusion would live
        ("sessiön", SegmentError::IllegalChar('ö')),
        ("сессия", SegmentError::IllegalChar('с')),
        // a bare `-foo` is an option to any command line the id reaches
        ("-evil", SegmentError::LeadingDash),
        ("--", SegmentError::LeadingDash),
        // Windows device names: a path that opens a device, not a file
        ("con", SegmentError::ReservedDeviceName),
        ("CON", SegmentError::ReservedDeviceName),
        ("NuL", SegmentError::ReservedDeviceName),
        ("com3", SegmentError::ReservedDeviceName),
        ("LPT9", SegmentError::ReservedDeviceName),
    ];
    for (raw, expected) in cases {
        assert_eq!(
            PathSegment::parse(raw),
            Err(expected.clone()),
            "PathSegment::parse({raw:?}) must be refused with {expected:?}"
        );
    }
}

/// **The shapes the predicate this type replaced let through**, called out
/// separately from the table above because they are the actual regression
/// surface of #925 rather than the generic traversal alphabet.
///
/// `digest::is_safe_session_id` was `!empty && !contains(['/','\\']) && != "."
/// && != ".."`. Every row here satisfied all four of those and reached
/// `Path::join`.
#[test]
fn parse_refuses_the_shapes_the_old_session_id_predicate_admitted() {
    // The sharpest one, and the reason a separator-blocklist is not a
    // substitute for an alphabet. On Windows `"C:"` parses as a `Prefix`
    // component and `Path::join` REPLACES its receiver when the argument
    // carries a prefix — so the join left the session-state root entirely and
    // resolved drive-relative to the process's own cwd. No separator required.
    assert_eq!(PathSegment::parse("C:"), Err(SegmentError::IllegalChar(':')));
    assert_eq!(
        PathSegment::parse("C:/Windows"),
        Err(SegmentError::IllegalChar(':'))
    );
    // NTFS alternate data stream on an otherwise innocuous-looking id.
    assert_eq!(
        PathSegment::parse("sess:stream"),
        Err(SegmentError::IllegalChar(':'))
    );
    // A device name, which names a device rather than a file on Windows.
    assert_eq!(
        PathSegment::parse("CON"),
        Err(SegmentError::ReservedDeviceName)
    );
    // An option to any command line the id is interpolated into.
    assert_eq!(
        PathSegment::parse("-rf"),
        Err(SegmentError::LeadingDash)
    );
    // Unbounded length: the old predicate had no cap at all.
    let huge = "a".repeat(5000);
    assert_eq!(
        PathSegment::parse(&huge),
        Err(SegmentError::TooLong(5000))
    );
    // NUL, which truncates at the syscall.
    assert_eq!(
        PathSegment::parse("sess\0evil"),
        Err(SegmentError::IllegalChar('\0'))
    );
    // Non-ASCII, where normalization/homoglyph confusion lives.
    assert!(PathSegment::parse("sessiоn").is_err(), "Cyrillic 'о' homoglyph");
}

/// The cap, exactly at the boundary in both directions — the half of a length
/// rule that off-by-one errors live in.
#[test]
fn parse_refuses_only_segments_past_the_length_cap() {
    let at_cap = "a".repeat(PathSegment::MAX_LEN);
    assert!(
        PathSegment::parse(&at_cap).is_ok(),
        "a segment of exactly MAX_LEN must be accepted"
    );
    let over = "a".repeat(PathSegment::MAX_LEN + 1);
    assert_eq!(
        PathSegment::parse(&over),
        Err(SegmentError::TooLong(PathSegment::MAX_LEN + 1))
    );
}

/// The refusal must be a refusal, not a repair. If `parse` ever started
/// trimming or stripping, two different strings would name one file — and a
/// lookup and a path join would then disagree about which one they meant.
#[test]
fn parse_never_rewrites_a_segment_into_a_valid_one() {
    for raw in ["  s-1  ", "s-1/", "/s-1", "s-1\n", "-s-1"] {
        assert!(
            PathSegment::parse(raw).is_err(),
            "{raw:?} must be REFUSED, not normalized into a neighbouring valid id"
        );
    }
}

/// Deserialization is a construction site. A state file written by an older
/// build — or edited by hand — must not be able to hand the process an id the
/// constructor would have refused.
#[test]
fn deserialization_goes_through_the_same_gate() {
    assert_eq!(
        serde_json::from_str::<PathSegment>("\"64f4d4f6-5201-4da9-8ed9-e0827ffae7df\"")
            .unwrap()
            .as_str(),
        "64f4d4f6-5201-4da9-8ed9-e0827ffae7df"
    );
    assert!(
        serde_json::from_str::<PathSegment>("\"../escaped\"").is_err(),
        "a traversal id in a persisted file must fail to deserialize"
    );
    assert!(
        serde_json::from_str::<PathSegment>("\"C:\"").is_err(),
        "the drive-prefix shape must not survive a persisted file either"
    );
    // …and the wire shape is unchanged: a bare string, so no persisted file or
    // frontend payload changes format.
    assert_eq!(
        serde_json::to_string(&PathSegment::parse("w-1").unwrap()).unwrap(),
        "\"w-1\""
    );
}

// ────────────────────────── 2. acceptance ──────────────────────────

/// Every id shape this type actually has to carry. Sourced from the code and
/// from the vendors' own formats rather than invented — refusing any of these
/// would be an outage, not hardening, and that is the half a refusal suite
/// cannot tell you about.
#[test]
fn parse_accepts_every_real_shape_the_families_actually_use() {
    let real: &[&str] = &[
        // Claude Code: a hyphenated hex UUID (`new_session_uuid`), 36 chars.
        "64f4d4f6-5201-4da9-8ed9-e0827ffae7df",
        "00000000-0000-4000-8000-000000000000",
        // OpenCode (#722): `ses_` + 12 hex + 14 base62, 30 chars — the `_` and
        // the mixed-case tail are exactly what the pre-#722 hex-only alphabet
        // rejected, so they are the regression this row guards.
        "ses_03bd2d53dffeiBvu9PvuCPjxT7",
        // agent ids, as minted (`w-N`, `rev-N`, the orchestrator, solo panes)
        "orch",
        "w-1",
        "w-711",
        "rev-lead",
        "process",
        // merge-queue batch ids
        "mq-7f3a0000",
        "mq-1",
        // group ids, since `GroupId` shares this checker
        "loomux-68435179",
        "repo-1a2b3c4d",
        "loomux-testbed-cc077f09-2",
        "__solo__",
        // the short shapes this repo's own suite creates
        "g",
        "g1",
        "g-1",
    ];
    for id in real {
        assert!(
            PathSegment::parse(id).is_ok(),
            "{id:?} is an id loomux or an agent CLI really mints — refusing it is an outage, \
             not hardening"
        );
    }
}

// ─────────────────────── 3. the consolidation ───────────────────────

/// **The four validators now give one answer** (#925).
///
/// The point of the consolidation was not tidiness: the four differed, and the
/// weakest of them was the one guarding a live `Path::join`. This pins that they
/// agree — on the inputs that used to separate them, in both directions.
///
/// `GroupId` is checked through its own public constructor rather than through
/// `PathSegment`, because the claim being pinned is that the *shared checker* is
/// what both of them run, not merely that both exist.
#[test]
fn the_group_id_and_segment_validators_cannot_drift_apart() {
    use loomux_lib::orchestration::GroupId;

    let cases: &[&str] = &[
        // accepted by both
        "repo-1a2b3c4d",
        "w-1",
        "ses_03bd2d53dffeiBvu9PvuCPjxT7",
        "64f4d4f6-5201-4da9-8ed9-e0827ffae7df",
        // refused by both — one row per rule, so a rule dropped from one side
        // and not the other is a failure rather than a silent divergence
        "",
        "..",
        "a/b",
        "a\\b",
        "C:",
        "-evil",
        "CON",
        "sessiön",
        "s\0x",
    ];
    for raw in cases {
        assert_eq!(
            PathSegment::parse(raw).is_ok(),
            GroupId::parse(raw).is_ok(),
            "PathSegment and GroupId disagree about {raw:?} — they share one checker, so a \
             disagreement means one of them grew a private rule again"
        );
    }

    // The cap is shared too, boundary included.
    let at_cap = "a".repeat(PathSegment::MAX_LEN);
    let over = "a".repeat(PathSegment::MAX_LEN + 1);
    assert!(PathSegment::parse(&at_cap).is_ok() && GroupId::parse(&at_cap).is_ok());
    assert!(PathSegment::parse(&over).is_err() && GroupId::parse(&over).is_err());
}

// ──────────────────── 4. the filename tripwire ────────────────────

/// **No identifier reaches a FILE NAME without being a validated segment**
/// (#925), the sibling scan to
/// `the_orchestration_root_is_joined_with_a_group_in_exactly_one_place`.
///
/// # Why a second scan rather than a wider first one
///
/// That test is default-deny on two axes: the orchestration-root *receiver*,
/// and the `.as_str()` path-component *shape*. This family crosses neither.
/// `claude_transcript_path` builds `format!("{session}.jsonl")` on one line and
/// joins it on another, and `ledger_path` builds
/// `format!("ledger-{agent_id}.log")` whose receiver is a group dir, not the
/// root. Both were invisible to it — correctly, since it was written for a
/// different question — so the gap needed its own guard rather than a fifth
/// heuristic bolted onto that one.
///
/// # What it flags
///
/// A `format!` template that interpolates a known raw-id binding **and**
/// carries a file-extension literal is a file name being built out of an
/// identifier. That is a finding unless its exact normalized text is on
/// [`SANCTIONED`] with the reason it is safe — which, in every current case, is
/// that the binding is a `PathSegment` at that function's signature and so
/// carries its own proof.
///
/// The extension literal is what keeps this precise: `format!("agent={agent_id}
/// pty={pty_id}")` is a breadcrumb, not a path, and there are dozens of those.
///
/// # Limits, stated rather than implied
///
/// Textual, like its sibling, and it inherits the same honesty requirement.
/// Not caught, and no attempt is made to: an extension held in a `const` or
/// pushed with `PathBuf::push`; a template split across lines; an id renamed to
/// a binding outside [`ID_BINDINGS`]; a file name assembled by `String`
/// concatenation instead of `format!`. What actually holds the property is the
/// **signature** — a function taking `&PathSegment` cannot be handed a raw
/// `&str` at all — and this scan is defence in depth over a *new* site being
/// added with a raw binding, which is the way the guarantee would realistically
/// erode.
#[test]
fn no_raw_identifier_is_interpolated_into_a_file_name() {
    /// Bindings that are raw `&str` identifiers somewhere in this codebase.
    const ID_BINDINGS: &[&str] = &["session_id", "agent_id", "sid", "group_id"];

    /// File-extension literals that mark a `format!` as building a file name.
    const EXTENSIONS: &[&str] = &[".json", ".jsonl", ".log", ".toml", ".yaml", ".md", ".txt"];

    /// Every sanctioned identifier-into-file-name site, with the argument for
    /// why it is safe. Anything else is a finding until it is argued for and
    /// added here — that is what makes this default-deny rather than a
    /// blocklist. Normalized (whitespace collapsed) before comparison.
    const SANCTIONED: &[(&str, &str)] = &[
        // Each of these five sits in a function whose `agent_id` parameter is
        // typed `&PathSegment` (#925), so the binding is already proof and the
        // interpolation cannot be handed a raw string.
        (
            ".join(format!(\"{agent_id}.promptsubmit.jsonl\"))",
            "promptsubmit_marker_path(root, &GroupId, &PathSegment)",
        ),
        (
            "self.group_dir(group).join(format!(\"ledger-{agent_id}.log\"))",
            "OrchRegistry::ledger_path(&GroupId, &PathSegment)",
        ),
        (
            "let path = dir.join(format!(\"{agent_id}-gemini-policy.toml\"));",
            "write_gemini_policy(dir, &PathSegment, _)",
        ),
        (
            "let path = dir.join(format!(\"{agent_id}-hooks.json\"));",
            "write_hook_settings_file(&GroupId, &PathSegment, _)",
        ),
        // These two interpolate a locally-parsed `PathSegment`, not the raw
        // parameter — the binding name itself is the evidence.
        (
            "let path = dir.join(format!(\"{agent_seg}.json\"));",
            "write_mcp_config parses `agent_id` into `agent_seg` at entry",
        ),
        (
            "self.group_dir(&snapshot.group).join(\"configs\").join(format!(\"{agent_seg}.json\")),",
            "the reap path parses into `agent_seg` before removing",
        ),
    ];

    fn normalize(line: &str) -> String {
        line.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    const ROOTS: &[(&str, &str)] = &[
        ("src-tauri", concat!(env!("CARGO_MANIFEST_DIR"), "/src")),
        (
            "loomux-engine",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../crates/loomux-engine/src"),
        ),
    ];

    let mut files: Vec<(&str, std::path::PathBuf)> = Vec::new();
    for (label, root) in ROOTS {
        let mut found = Vec::new();
        collect_rs_files(std::path::Path::new(root), &mut found);
        assert!(
            !found.is_empty(),
            "no `.rs` found under the {label} source root ({root}) — a root that scans nothing \
             is a tripwire that cannot fire"
        );
        files.extend(found.into_iter().map(|p| (*label, p)));
    }

    let mut offenders = Vec::new();
    let mut sanctioned_seen = vec![0usize; SANCTIONED.len()];

    for (label, path) in &files {
        let src = std::fs::read_to_string(path).unwrap();
        let name = format!("{label}/{}", path.file_name().unwrap().to_string_lossy());
        for (i, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            let Some(fmt_at) = trimmed.find("format!(\"") else { continue };
            let template = &trimmed[fmt_at..];
            let builds_a_file_name = EXTENSIONS.iter().any(|e| template.contains(e));
            if !builds_a_file_name {
                continue;
            }
            let interpolates_an_id = ID_BINDINGS
                .iter()
                .any(|b| template.contains(&format!("{{{b}}}")));
            if !interpolates_an_id {
                continue;
            }
            let norm = normalize(trimmed);
            match SANCTIONED.iter().position(|(text, _)| norm.contains(text)) {
                Some(j) => sanctioned_seen[j] += 1,
                None => offenders.push(format!("{name}:{}: {trimmed}", i + 1)),
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "an identifier must not become a FILE NAME unless it is a validated `PathSegment` \
         (#925). Found {} site(s):\n{}\n\nIf one of these is legitimate — because the binding is \
         a `PathSegment` at that signature — add its exact normalized text to `SANCTIONED` with \
         that argument. Writing the argument down is the point of this test.",
        offenders.len(),
        offenders.join("\n")
    );

    // A sanctioned entry that matches nothing means the site was renamed or
    // deleted and this list is now stale — the same self-staleness guard the
    // sibling scan applies to its assembly points. A stale allowlist quietly
    // shrinks what the test covers.
    for (j, (text, whose)) in SANCTIONED.iter().enumerate() {
        assert!(
            sanctioned_seen[j] > 0,
            "`SANCTIONED` entry `{text}` ({whose}) matched nothing — the site moved or was \
             renamed, so this allowlist row is stale. Re-point it or drop it."
        );
    }
}

fn collect_rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

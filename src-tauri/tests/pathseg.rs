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
        // Agent ids, as actually minted: `Role::prefix()` (a `&'static str`)
        // plus a counter, so always `^(orch|w|rev|plan|solo)-[0-9]{1,10}$`.
        "orch-1",
        "w-1",
        "w-711",
        "rev-2",
        "plan-1",
        "solo-1",
        // BLOCK ids, which are a different family and were mislabelled as agent
        // ids here (rev-lead N3). A block id rides in `AgentEntry.block`, never
        // in `.id` — but it does become `<id>.md` in the group dir, so it
        // belongs in this suite on its own merits, under its own name.
        "rev-lead",
        "process",
        "worker-deep",
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
/// # What it flags, and why the trigger names nothing
///
/// **A `format!` whose template both interpolates something and carries a
/// file-extension literal is a file name being built out of a value.** That is
/// the whole trigger. It does not ask what the binding is *called*.
///
/// The first version of this scan did ask, and CLAUDE.md's source-scanning-guard
/// convention says not to: *"a rename steps over it, so it enforces nothing … a
/// name heuristic is a labelled supplement at best"*. That was not theoretical
/// here. This PR renamed `claude_transcript_path`'s parameter `session_id` →
/// `session` as ordinary tidying, and that rename alone moved
/// `format!("{session}.jsonl")` — the declared assembly point for claude
/// transcript paths — out of the name list and clean out of this scan's view.
/// A reviewer found it, not the guard. The name list is gone; the trigger is now
/// the shape.
///
/// Two things keep the shape precise:
///
/// - **The extension is matched inside the `format!` string literal, not
///   anywhere on the line.** `json!({ "reason": "unusable merge_queue.json",
///   "detail": format!("{e:?}") })` has `.json` on the line and builds no path;
///   scoping to the literal drops that shape. (Deliberately not stated as a
///   count: how many such lines exist is a fact about today's tree, and a
///   number here would be wrong on the next commit that adds or removes one.)
/// - **`#[cfg(test)]` regions are skipped**, the same exclusion the sibling scan
///   makes for `tests/`: fixtures build file names from ids constantly, which is
///   their job. Tracked by brace depth from the attribute, so it is the region
///   that is skipped, not a guessed line range.
///
/// # Limits, stated rather than implied
///
/// Textual, like its sibling, and it inherits the same honesty requirement.
/// Not caught, and no attempt is made to: an extension held in a `const`
/// (`{handle}{CLAUDE_AGENT_FILE_EXT}` is real and invisible here); a name built
/// by `String` concatenation or `PathBuf::push` rather than `format!`; a
/// template split across lines; and — because [`format_template`] returns the
/// **first** `format!("` on a line — a second `format!` sharing that line with a
/// first. That last one changes nothing in the tree today (replaying with
/// every-template-per-line finds the same set), which is why it is a stated
/// limit rather than a fix: a scan whose doc enumerates the others and omits
/// this one is implying a completeness it does not have. What actually holds
/// the property is the
/// **signature** — a function taking `&PathSegment` cannot be handed a raw
/// `&str` at all — and this scan is defence in depth over a *new* site being
/// added raw, which is how the guarantee would realistically erode.
///
/// The allowlist's third field is what stops that honesty from being a slogan:
/// each exemption names the signature it depends on, and the scan re-checks it
/// is still there. Otherwise the list would be the weakest part of the guard —
/// every row says "safe, because this is a `PathSegment`", a textual scan cannot
/// see types, and `format!` accepts both, so a signature reverting to `&str`
/// would leave the flagged line byte-identical and every check green.
#[test]
fn no_raw_identifier_is_interpolated_into_a_file_name() {
    /// File-extension literals that mark a `format!` template as building a file
    /// name. Matched inside the template only — see this test's doc.
    const EXTENSIONS: &[&str] = &[
        ".json", ".jsonl", ".log", ".toml", ".yaml", ".md", ".txt", ".cmd", ".ps1", ".sh",
    ];

    /// Every sanctioned identifier-into-file-name site: the line text, the
    /// argument for why it is safe, and — third field — **the text that has to
    /// still be in the same file for that argument to hold**.
    ///
    /// The third field is not bookkeeping. Without it this allowlist would be
    /// pure trust: every entry here says "safe, because the binding is a
    /// `PathSegment` at that signature", and a textual scan cannot see types.
    /// Change `find_claude_session_cwd`'s parameter back to `&str` and the
    /// flagged *line* is character-for-character identical, so the scan would
    /// keep passing while the property it names had gone — and `format!` accepts
    /// both types, so the compiler would not object either. Requiring the
    /// signature to still be present is what makes each row a checked claim
    /// rather than a promise.
    ///
    /// **The proof is file-scoped, not function-scoped**, and that limit is
    /// stated rather than glossed: it asserts the text is somewhere in the same
    /// file, so where two sites share a proof string (the four `agent_id:
    /// &PathSegment` parameters all live in `mod.rs`) reverting *one* of them
    /// would not trip it. It raises the cost of a silent revert; it does not
    /// make one impossible. Binding a proof to its enclosing function would mean
    /// parsing Rust, which is the line this scan deliberately does not cross —
    /// the compiler is what actually holds the type, and this is defence in
    /// depth over the allowlist rotting.
    ///
    /// Anything not listed is a finding until it is argued for and added — that
    /// is what makes this default-deny rather than a blocklist. Normalized
    /// (whitespace collapsed) before comparison.
    const SANCTIONED: &[(&str, &str, &str)] = &[
        // Each of these five sits in a function whose `agent_id` parameter is
        // typed `&PathSegment` (#925), so the binding is already proof and the
        // interpolation cannot be handed a raw string.
        (
            ".join(format!(\"{agent_id}.promptsubmit.jsonl\"))",
            "promptsubmit_marker_path(root, &GroupId, &PathSegment)",
            "agent_id: &PathSegment) -> PathBuf {",
        ),
        (
            "self.group_dir(group).join(format!(\"ledger-{agent_id}.log\"))",
            "OrchRegistry::ledger_path(&GroupId, &PathSegment)",
            "fn ledger_path(&self, group: &GroupId, agent_id: &PathSegment)",
        ),
        (
            "let path = dir.join(format!(\"{agent_id}-gemini-policy.toml\"));",
            "write_gemini_policy(dir, &PathSegment, _)",
            "agent_id: &PathSegment,",
        ),
        (
            "let path = dir.join(format!(\"{agent_id}-hooks.json\"));",
            "write_hook_settings_file(&GroupId, &PathSegment, _)",
            "agent_id: &PathSegment,",
        ),
        // The site this scan itself found (#925): parsed once in
        // `find_session_cwd`, threaded as a type from there.
        (
            "let candidate = project.path().join(format!(\"{session_id}.jsonl\"));",
            "find_claude_session_cwd(root, &PathSegment) — parsed in find_session_cwd",
            "fn find_claude_session_cwd(root: &Path, session_id: &PathSegment)",
        ),
        // The site the NAME-BASED version of this scan could not see, because
        // this PR renamed the parameter `session_id` -> `session` (rev-lead B2).
        // It is the declared assembly point for a claude transcript path, so a
        // guard blind to it was guarding nothing that mattered. The trigger no
        // longer consults names, so it is caught by shape now — this row exists
        // to answer for it, and its proof pins the parameter's type.
        (
            "let name = format!(\"{session}.jsonl\");",
            "claude_transcript_path(root, &PathSegment) — the declared assembly point",
            "fn claude_transcript_path(root: &Path, session: &PathSegment)",
        ),
        // Not an identifier family at all: `write_shim`/`write_refusal_shim`
        // take `program: &str`, and every call site passes a string LITERAL —
        // the proofs below are those call sites, so a future caller threading a
        // caller-supplied name through here would strand the row and fail the
        // stale-row check. Listed because a trigger that does not consult names
        // cannot tell a fixed internal name from an id by looking; that is the
        // honest cost of dropping the name heuristic, and an argued row is the
        // price.
        (
            "let _ = fs::write(dir.join(format!(\"{program}.cmd\")), cmd(&real_fwd, sh_path).as_bytes());",
            "write_shim — `program` is a literal at every call site, not a caller id",
            "self.write_shim(&dir, \"gh\", gh_shim_sh, gh_shim_cmd, sh_path.as_deref(), &shim_paths);",
        ),
        (
            "let _ = fs::write(dir.join(format!(\"{program}.cmd\")), cmd.as_bytes());",
            "write_refusal_shim — `program` is a literal at its call site",
            "self.write_refusal_shim(&dir, \"loomux\", loomux_shim_sh(), loomux_shim_cmd());",
        ),
        // Guarded by the predicate that now delegates to `check_segment`, and
        // guarded in the BUILDER rather than by the caller — the structural
        // form, per that function's own rev-183 note.
        (
            "Some(std::env::temp_dir().join(format!(\"loomux-mq-{}-{kind}.md\", batch_id.trim())))",
            "body_file_path refuses the id itself via valid_id_component",
            "if !valid_id_component(batch_id) {",
        ),
        // A roster agent id, which is minted rather than supplied: `Role::prefix()`
        // is a `&'static str` and the numeric suffix comes from a counter, so
        // `AgentEntry.id` is always `^(orch|w|rev|plan|solo)-[0-9]+$`. These two
        // are reads of a hook marker; the id never leaves the roster.
        (
            "let precompact_marker = hooks_dir.join(format!(\"{}.precompact.json\", a.id));",
            "a.id is a minted roster id, never caller-supplied",
            "let agent_id = format!(\"{}-{seq}\", block.prefix());",
        ),
        (
            "let sessionstart_marker = hooks_dir.join(format!(\"{}.sessionstart-compact.json\", a.id));",
            "a.id is a minted roster id, never caller-supplied",
            "let agent_id = format!(\"{}-{seq}\", block.prefix());",
        ),
        // Not an identifier at all — a formatted timestamp.
        (
            "let path = dir.join(format!(\"crash-{}.log\", stamp(now)));",
            "crash-log name is a timestamp, not an id",
            "format!(\"{y:04}{m:02}{d:02}-{hh:02}{mm:02}{ss:02}\")",
        ),
        // **The known non-member of this family, and the reason it is listed
        // rather than fixed here.** A workflow block id becomes `<id>.md` in the
        // group dir, and it is validated by `workflow::sanitize_id` — a FIFTH
        // predicate, weaker than `check_segment` on exactly the two rules
        // `pathseg.rs` says the alphabet does not give you: it permits a leading
        // `-` and a reserved device name. Converting it is out of #925's scope
        // (block ids come from operator-authored `.loomux/workflow.yml`, not
        // from a caller, so it is not a containment breach) — but the guard
        // should SAY so rather than be silently blind to it, which is the whole
        // of rev-lead's B3. CLAUDE.md constraint 6 names it too.
        (
            "format!(\"{}.md\", self.id)",
            "block ids are validated by workflow::sanitize_id — a separate, weaker predicate, \
             deliberately not converted by #925",
            "let Some(id) = sanitize_id(&rb.id) else {",
        ),
        // These two interpolate a locally-parsed `PathSegment` rather than the
        // raw parameter. The binding name is a hint, not the evidence — the
        // proof field is, and it pins the parse itself still being there.
        (
            "let path = dir.join(format!(\"{agent_seg}.json\"));",
            "write_mcp_config parses `agent_id` into `agent_seg` at entry",
            "let agent_seg = PathSegment::parse(agent_id)",
        ),
        (
            "self.group_dir(&snapshot.group).join(\"configs\").join(format!(\"{agent_seg}.json\")),",
            "the reap path parses into `agent_seg` before removing",
            "if let Ok(agent_seg) = PathSegment::parse(agent_id) {",
        ),
    ];

    fn normalize(line: &str) -> String {
        line.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The `format!` string literal on this line, if any — from `format!("` to
    /// the closing quote, honouring `\"`. Scoping the extension match to THIS
    /// rather than to the whole line is what keeps `json!({ "…merge_queue.json",
    /// "detail": format!("{e:?}") })` from reading as a path build.
    fn format_template(line: &str) -> Option<&str> {
        let start = line.find("format!(\"")? + "format!(\"".len();
        let rest = &line[start..];
        let mut prev_backslash = false;
        for (i, c) in rest.char_indices() {
            match c {
                '\\' => prev_backslash = !prev_backslash,
                '"' if !prev_backslash => return Some(&rest[..i]),
                _ => prev_backslash = false,
            }
        }
        None
    }

    /// Does the template interpolate anything at all? `{{` is an escaped brace,
    /// not a hole.
    fn interpolates(template: &str) -> bool {
        let b: Vec<char> = template.chars().collect();
        let mut i = 0;
        while i < b.len() {
            if b[i] == '{' {
                if b.get(i + 1) == Some(&'{') {
                    i += 2;
                    continue;
                }
                return true;
            }
            i += 1;
        }
        false
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
    // A sanctioned row whose signature-proof is no longer in the file the site
    // lives in: the argument for allowing that line has evaporated.
    let mut unproven: Vec<String> = Vec::new();

    for (label, path) in &files {
        let src = std::fs::read_to_string(path).unwrap();
        let name = format!("{label}/{}", path.file_name().unwrap().to_string_lossy());
        // `#[cfg(test)]` region tracking: `pending` arms on the attribute, the
        // region opens on the next `{` and closes when brace depth returns to
        // zero. Fixtures build file names from ids constantly — that is their
        // job — and the sibling scan excludes `tests/` for the same reason.
        let mut pending_cfg_test = false;
        let mut test_depth: i32 = -1;
        let mut depth: i32 = 0;
        for (i, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            let opens = line.matches('{').count() as i32;
            let closes = line.matches('}').count() as i32;
            let depth_before = depth;
            depth += opens - closes;
            if test_depth >= 0 {
                if depth <= test_depth {
                    test_depth = -1; // left the `#[cfg(test)]` region
                }
                continue;
            }
            if pending_cfg_test && opens > 0 {
                pending_cfg_test = false;
                test_depth = depth_before;
                continue;
            }
            if trimmed.starts_with("#[cfg(test)]") {
                pending_cfg_test = true;
                continue;
            }
            if trimmed.starts_with("//") {
                continue;
            }
            let Some(template) = format_template(trimmed) else { continue };
            // Both halves are required, and each drops a distinct false
            // positive: without the extension every breadcrumb `format!`
            // qualifies, and without the interpolation every literal path does.
            if !EXTENSIONS.iter().any(|e| template.contains(e)) || !interpolates(template) {
                continue;
            }
            let norm = normalize(trimmed);
            match SANCTIONED.iter().position(|(text, _, _)| norm.contains(text)) {
                Some(j) => {
                    sanctioned_seen[j] += 1;
                    // The row's argument, re-checked against the file rather
                    // than taken on trust. `format!` accepts a `&str` and a
                    // `PathSegment` alike, so reverting the signature would
                    // leave this line byte-identical and the compiler silent —
                    // this assertion is the only thing that would notice.
                    let (_, whose, proof) = SANCTIONED[j];
                    if !src.contains(proof) {
                        unproven.push(format!(
                            "{name}:{}: allowlisted as `{whose}`, but its proof `{proof}` is no \
                             longer in this file",
                            i + 1
                        ));
                    }
                }
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

    assert!(
        unproven.is_empty(),
        "an allowlisted site is allowed ONLY because its binding is a validated `PathSegment` at \
         that signature (#925), and that argument no longer holds. Found {} site(s):\n{}\n\nEither \
         restore the signature or remove the row — a row whose proof is gone is an exemption \
         granted for a reason that stopped being true.",
        unproven.len(),
        unproven.join("\n")
    );

    // A sanctioned entry that matches nothing means the site was renamed or
    // deleted and this list is now stale — the same self-staleness guard the
    // sibling scan applies to its assembly points. A stale allowlist quietly
    // shrinks what the test covers.
    for (j, (text, whose, _)) in SANCTIONED.iter().enumerate() {
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

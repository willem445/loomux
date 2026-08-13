//! `GroupId` — the validated group identifier, and the path seams that now
//! refuse an id that isn't one (#904; layer 2 of #888 §0).
//!
//! Integration tests, not unit tests: they link the full lib, which on Windows
//! needs the comctl32-v6 manifest `build.rs` embeds via `-tests`-scoped link
//! args (CLAUDE.md constraint 4).
//!
//! The suite is in three parts, and the middle one is the one that would be
//! easy to leave out:
//!
//! 1. **Refusal** — every path-shaped id the constructor must reject.
//! 2. **Acceptance** — every id shape loomux actually mints or has on disk. A
//!    validator that rejects a real group id is not hardening, it is an outage,
//!    and this half is what makes the alphabet a claim rather than a guess.
//! 3. **The seams** — the raw `root.join(group)` sites, driven end to end.

use loomux_lib::orchestration::{
    generated_agent_handle, group_id_for_repo, promptsubmit_marker_path, GroupId, GroupIdError,
    OrchRegistry, SOLO_GROUP,
};
use loomux_lib::sessions::detect_orch_signature;
use serde_json::json;

// ─────────────────────────── 1. refusal ───────────────────────────

/// The traversal alphabet, one shape per row. Each is a *distinct* way to name
/// something other than a single child of the orchestration root, so a
/// regression that re-opens one of them cannot hide behind the others passing.
#[test]
fn parse_refuses_every_path_shaped_group_id() {
    let cases: &[(&str, GroupIdError)] = &[
        // the classic traversal, and the pieces it is built from
        ("..", GroupIdError::IllegalChar('.')),
        ("../escaped", GroupIdError::IllegalChar('.')),
        ("../../../../Users/me/.ssh", GroupIdError::IllegalChar('.')),
        ("..\\escaped", GroupIdError::IllegalChar('.')),
        (".", GroupIdError::IllegalChar('.')),
        // a dot anywhere at all, which is what makes `..` unspellable
        ("g.1", GroupIdError::IllegalChar('.')),
        // separators, forward and back
        ("a/b", GroupIdError::IllegalChar('/')),
        ("a\\b", GroupIdError::IllegalChar('\\')),
        ("/absolute", GroupIdError::IllegalChar('/')),
        ("\\\\server\\share", GroupIdError::IllegalChar('\\')),
        // absolute-path markers and NTFS alternate data streams
        ("C:", GroupIdError::IllegalChar(':')),
        ("C:/Windows", GroupIdError::IllegalChar(':')),
        ("g:stream", GroupIdError::IllegalChar(':')),
        // nothing at all
        ("", GroupIdError::Empty),
        // bytes a filesystem or a log line would rather not see
        ("g\0x", GroupIdError::IllegalChar('\0')),
        ("g\nx", GroupIdError::IllegalChar('\n')),
        ("g x", GroupIdError::IllegalChar(' ')),
        (" g", GroupIdError::IllegalChar(' ')),
        ("g ", GroupIdError::IllegalChar(' ')),
        ("g*", GroupIdError::IllegalChar('*')),
        ("g?", GroupIdError::IllegalChar('?')),
        // non-ASCII: where homoglyph and normalization confusion would live
        ("grüppe", GroupIdError::IllegalChar('ü')),
        ("группа", GroupIdError::IllegalChar('г')),
        // a bare `-foo` is an option to any command line the id reaches
        ("-evil", GroupIdError::LeadingDash),
        ("--", GroupIdError::LeadingDash),
        // Windows device names: a path that opens a device, not a file
        ("con", GroupIdError::ReservedDeviceName),
        ("CON", GroupIdError::ReservedDeviceName),
        ("NuL", GroupIdError::ReservedDeviceName),
        ("com3", GroupIdError::ReservedDeviceName),
        ("LPT9", GroupIdError::ReservedDeviceName),
    ];
    for (raw, expected) in cases {
        assert_eq!(
            GroupId::parse(raw),
            Err(expected.clone()),
            "GroupId::parse({raw:?}) must be refused with {expected:?}"
        );
    }
}

/// The cap, exactly at the boundary in both directions — the half of a length
/// rule that off-by-one errors live in.
#[test]
fn parse_refuses_only_ids_past_the_length_cap() {
    let at_cap = "a".repeat(GroupId::MAX_LEN);
    assert!(
        GroupId::parse(&at_cap).is_ok(),
        "an id of exactly MAX_LEN must be accepted"
    );

    let over = "a".repeat(GroupId::MAX_LEN + 1);
    assert_eq!(
        GroupId::parse(&over),
        Err(GroupIdError::TooLong(GroupId::MAX_LEN + 1))
    );
}

/// The refusal must be a refusal, not a repair. If `parse` ever started
/// trimming or stripping, two different strings would name one directory — and
/// a membership check and a path join would then disagree about which group
/// they were talking about.
#[test]
fn parse_never_rewrites_an_id_into_a_valid_one() {
    for raw in ["  g-1  ", "g-1/", "/g-1", "g-1\n", "-g-1"] {
        assert!(
            GroupId::parse(raw).is_err(),
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
        serde_json::from_str::<GroupId>("\"repo-1a2b3c4d\"")
            .unwrap()
            .as_str(),
        "repo-1a2b3c4d"
    );
    assert!(
        serde_json::from_str::<GroupId>("\"../escaped\"").is_err(),
        "a traversal id in a persisted file must fail to deserialize"
    );
    // …and the wire shape is unchanged: a bare string, so no persisted file or
    // frontend payload changes format.
    assert_eq!(
        serde_json::to_string(&GroupId::parse("repo-1a2b3c4d").unwrap()).unwrap(),
        "\"repo-1a2b3c4d\""
    );
}

// ────────────────────────── 2. acceptance ──────────────────────────

/// Every id shape loomux mints or has on disk today. Sourced from the code
/// rather than invented: `group_id_for_repo`'s `{slug}-{8hex}`, the `-{n}`
/// concurrency suffix `create_group_ex` appends, the `SOLO_GROUP` constant, the
/// short ids this repo's own suite uses, and two real ids off live groups.
#[test]
fn parse_accepts_every_group_id_shape_the_codebase_actually_uses() {
    let real: &[&str] = &[
        // group_id_for_repo: {slug}-{8hex}
        "loomux-68435179",
        "repo-1a2b3c4d",
        "sempkg-74fe4043",
        // slug keeps '_' and digits, and may be a bare "repo" fallback
        "my_repo-00000000",
        "repo-ffffffff",
        "2024-deadbeef",
        // the -{n} suffix for concurrent groups on one repo
        "loomux-testbed-cc077f09-2",
        "repo-1a2b3c4d-10",
        // the reserved solo pseudo-group
        SOLO_GROUP,
        // the shapes this repo's own test suite creates
        "g",
        "g1",
        "g-1",
        "group-1",
        "no-such-group",
        // historical worktree-derived shapes named in mod.rs's #502 note
        "loomux-tmpAB12cd-1a2b3c4d",
        "loomux-repo-1a2b3c4d",
    ];
    for id in real {
        assert!(
            GroupId::parse(id).is_ok(),
            "{id:?} is a group id loomux really uses — refusing it is an outage, not hardening"
        );
    }
}

/// The minter and the validator must not be able to disagree. This is the
/// no-regression property in its strongest available form: whatever repo path
/// goes in, what comes out is something `GroupId::parse` accepts.
///
/// The inputs are deliberately hostile *repo paths*, because that is the input
/// `group_id_for_repo` actually takes — including the one shape that used to
/// break the property (a repo directory whose name starts with `-`).
#[test]
fn the_minter_can_never_produce_an_id_its_own_validator_rejects() {
    let repos: &[&str] = &[
        "C:/tmp/repo",
        "C:\\Projects\\loomux",
        "/home/me/work/my_project",
        "C:/dev/-weird",
        "C:/dev/---",
        "C:/dev/....",
        "C:/dev/ ",
        "C:/dev/CON",
        "C:/dev/a-very-long-repository-directory-name-well-past-the-slug-cap",
        "C:/dev/ünïcödé",
        "C:/dev/repo with spaces",
        "C:/dev/repo/",
        "",
        "/",
    ];
    for repo in repos {
        let id = group_id_for_repo(repo);
        assert!(
            GroupId::parse(&id).is_ok(),
            "group_id_for_repo({repo:?}) minted {id:?}, which its own validator refuses"
        );
    }
}

// ─────────────────────────── 3. the seams ───────────────────────────

/// `append_audit` is `root.join(group)` then `create_dir_all` + append, so a
/// `..` component wrote a real file OUTSIDE the orchestration root. This is the
/// concrete shape of the risk #888 §0 names once the caller stops being our own
/// in-process webview.
///
/// The positive half is not decoration: without it this test would still pass
/// if `audit` had simply stopped writing anything at all.
#[test]
fn a_traversal_group_id_must_not_write_outside_the_orchestration_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("orchestration");
    let escaped = tmp.path().join("escaped");

    let reg = OrchRegistry::new(root.clone());
    reg.audit("../escaped", "human", "probe", json!({ "probe": "#904" }));

    assert!(
        !escaped.exists(),
        "audit() with group id \"../escaped\" created {} — outside the orchestration root {}",
        escaped.display(),
        root.display()
    );

    reg.audit("repo-1a2b3c4d", "human", "probe", json!({ "probe": "#904" }));
    assert!(
        root.join("repo-1a2b3c4d").join("audit.jsonl").is_file(),
        "a VALID group id must still audit normally — otherwise the refusal above proves nothing"
    );
}

/// The second raw join: the promptsubmit hook marker.
#[test]
fn the_promptsubmit_marker_refuses_a_group_id_that_is_not_a_segment() {
    let root = std::path::Path::new("C:/state");

    assert_eq!(promptsubmit_marker_path(root, "../../elsewhere", "w-1"), None);
    assert_eq!(promptsubmit_marker_path(root, "", "w-1"), None);
    assert_eq!(
        promptsubmit_marker_path(root, "group-1", "w-1"),
        Some(root.join("group-1").join("hooks").join("w-1.promptsubmit.jsonl"))
    );
}

/// The escape that leaves loomux's own root entirely: the generated-agent
/// handle becomes a FILE NAME under `~/.claude/agents` and `~/.copilot/agents`,
/// so a group id carrying a separator writes into the user's CLI configuration
/// rather than merely traversing loomux's state directory.
#[test]
fn the_generated_agent_handle_refuses_an_id_that_would_escape_the_agents_dir() {
    assert_eq!(generated_agent_handle("../../../evil", "worker"), None);
    assert_eq!(generated_agent_handle("a/b", "worker"), None);
    assert_eq!(generated_agent_handle("", "worker"), None);
    assert_eq!(
        generated_agent_handle("g-1", "worker").as_deref(),
        Some("loomux-g-1-worker"),
        "a valid group id must still produce the handle shape end_group's reclaim matches on"
    );
}

/// The one group id in the codebase whose source is **agent-writable**: an
/// agent's own transcript. The scraped id is persisted into the session index
/// and comes back in as `resume_orch_session(group_hint)`, which joins it onto
/// the orchestration root.
///
/// The scrape's `take_while` alphabet already excluded separators, so these are
/// the shapes it let through and `GroupId::parse` does not.
#[test]
fn a_group_id_scraped_from_an_agent_written_transcript_is_validated() {
    let long = "a".repeat(GroupId::MAX_LEN + 1);
    for hostile in ["CON", "-evil", long.as_str()] {
        let text = format!("You are \"w\" (w-2), a worker agent in loomux group {hostile} for X.");
        let (role, gid) = detect_orch_signature(&text).unwrap();
        assert_eq!(role, "worker");
        assert_eq!(
            gid, None,
            "transcript-scraped group id {hostile:?} must read as no-group, not travel on to a path join"
        );
    }

    let ok = "You are \"w\" (w-2), a worker agent in loomux group sempkg-74fe4043 for X.";
    let (role, gid) = detect_orch_signature(ok).unwrap();
    assert_eq!(role, "worker");
    assert_eq!(gid.as_deref(), Some("sempkg-74fe4043"));
}

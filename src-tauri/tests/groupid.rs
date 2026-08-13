//! `GroupId` — the validated group identifier, and the path seams that can no
//! longer be handed an id that is not one (#904; layer 2 of #888 §0).
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
//! 3. **The seams** — the sites that once each joined the root themselves,
//!    now all routed through `group_dir_at`, driven end to end.

use loomux_lib::orchestration::{
    generated_agent_handle, group_id_for_repo, promptsubmit_marker_path, GroupId, GroupIdError,
    OrchRegistry, SOLO_GROUP,
};
use loomux_lib::sessions::detect_orch_signature;
use serde_json::json;

/// This file's one sanctioned `OrchRegistry::new` (#464, pinned by
/// `no_registry_construction_bypasses_the_test_agent_dir_overrides` in
/// `tests/orchestration.rs`). Nothing here spawns an agent, but the guard is
/// deliberately structural rather than case-by-case: a raw registry whose
/// agent-dir overrides are unset writes generated custom-agent files into the
/// developer's REAL `~/.claude/agents` on its first spawn, and "this test
/// doesn't spawn one *yet*" is exactly the reasoning that filled a real agents
/// dir with 1,111 stray files.
///
/// Takes the root rather than minting one, because these tests care about
/// where the root IS: the point of the escape probes is that a path lands
/// somewhere other than under it.
fn registry_at(root: &std::path::Path, scratch: &std::path::Path) -> OrchRegistry {
    let reg = OrchRegistry::new(root.to_path_buf());
    reg.set_claude_agents_dir_override(scratch.join("claude-agents"));
    reg.set_copilot_agents_dir_override(scratch.join("copilot-agents"));
    reg.set_compact_hook_dir_override(scratch.join("compacthook"));
    reg.set_copilot_hooks_dir_override(scratch.join("copilot-hooks"));
    reg
}

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

/// **What slice 2 changed about this section, stated up front.**
///
/// In slice 1 these seams took `&str` and refused at the join, so each had a
/// test passing it `"../escaped"` and asserting the refusal. They now take a
/// [`GroupId`], and those refusal tests are GONE — not weakened, *unspellable*:
/// there is no way to hand a traversal id to any of them any more. The property
/// moved from a runtime assertion to the type, where
/// `parse_refuses_every_path_shaped_group_id` pins it once for all of them.
///
/// What is left here is what a type cannot state on its own: that a **validated**
/// id lands where it should, under the root and nowhere else. That is the
/// containment half, and it is the half a refusal test never covered.

/// Every group-scoped path descends from the one declared root helper, so this
/// is the property that replaces four separate "refuses a traversal" tests: for
/// an id the constructor accepts, every seam produces a path INSIDE the root.
///
/// The positive assertions are not decoration — without them a `group_dir` that
/// had stopped returning anything at all would still pass a containment check.
#[test]
fn every_group_scoped_path_lands_under_the_orchestration_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("orchestration");
    let reg = registry_at(&root, tmp.path());
    let g = GroupId::parse("repo-1a2b3c4d").unwrap();

    // The audit writer, driven for real: the file exists, and it is under the
    // root rather than beside it.
    reg.audit(&g, "human", "probe", json!({ "probe": "#904" }));
    let audit = root.join("repo-1a2b3c4d").join("audit.jsonl");
    assert!(audit.is_file(), "a valid group id must audit normally");

    // The two free-function seams, which take a bare `root` and therefore used
    // to each own a join of their own.
    let marker = promptsubmit_marker_path(&root, &g, "w-1");
    assert_eq!(marker, root.join("repo-1a2b3c4d").join("hooks").join("w-1.promptsubmit.jsonl"));

    let db = reg.opencode_db_path(&g);
    assert!(
        db.starts_with(&root),
        "opencode_db_path escaped the root: {}",
        db.display()
    );
}

/// The handle that becomes a FILE NAME under `~/.claude/agents` and
/// `~/.copilot/agents` — the one interpolation whose blast radius is outside
/// loomux's own root entirely.
///
/// The refusal is now the type's (see this section's opening note). What still
/// needs asserting is that a handle built from a *valid* id carries no path
/// separator of its own: this string is used as a filename, and `end_group`'s
/// reclaim matches on its shape.
#[test]
fn the_generated_agent_handle_is_a_single_filename_component() {
    let handle = generated_agent_handle(&GroupId::parse("g-1").unwrap(), "worker")
        .expect("a valid group id must produce a handle");
    assert_eq!(handle, "loomux-g-1-worker");
    assert!(
        !handle.contains('/') && !handle.contains('\\') && !handle.contains(".."),
        "the handle is used as a FILE NAME under the user's CLI agents dir: {handle:?}"
    );
}

/// **The single-assembly-point claim, as a test rather than as prose** (#904).
///
/// The design note and `groupid.rs`'s module doc both say a group path is built
/// in one place and only there. That sentence was *false* for the whole of
/// slice 1 — `append_audit` and `promptsubmit_marker_path` each had a join of
/// their own — and nothing caught it but a reviewer.
///
/// # Why this is the third version
///
/// v1 was eleven literal needles; rev-440 broke it by naming `.join(g.as_str())`,
/// the exact line this change deleted from `sessions.rs`. v2 parsed arguments
/// structurally but decided "is this a group?" from the **binding's name**
/// (`group`/`gid`/`g.id`), so rev-450 broke it with the same line — `g` is not
/// on that list. Any guard that asks what a variable is *called* can be stepped
/// over by renaming it, and a security tripwire with that property is theatre.
///
/// # So this one does not ask about names at all
///
/// It is **default-deny on two name-independent axes**, and a group path has to
/// cross at least one of them:
///
/// 1. **The receiver.** A group path is `<orchestration root>/<group>`, so every
///    expression that builds a path *from the root* is denied unless it is on
///    [`ROOT_USES`]. That catches `default_root().join(g.as_str())` — rev-450's
///    case — because the offending part is the receiver, whatever the argument
///    is called. It also catches clone-then-`push`, since the `.clone()` is
///    itself a root use.
/// 2. **The `.as_str()` shape.** Turning a typed id into a path component is
///    spelled `x.as_str()`, and `Vec<String>::push(x.as_str())` does not
///    compile — so a `.join(…as_str())` or `.push(…as_str())` is a path build
///    almost by construction, regardless of receiver or name. Exactly one is
///    permitted: `group_dir_at`'s own.
///
/// A third, weaker pass keeps the old name heuristic as a *supplement* for a
/// group joined onto some already-derived directory. It is allowed to be
/// incomplete; the two axes above are near-total — the one shape that crosses
/// neither is a [`ROOT_USES`]-sanctioned root alias feeding a path component
/// built without `.as_str()`, which is exactly what this weaker pass is here to
/// catch.
///
/// Scope: every `.rs` under `src-tauri/src`. Tests are not scanned — they build
/// expected paths from group ids constantly, which is their job.
///
/// The compiler covers what no scan can: `GroupId` has no `AsRef<Path>`, so it
/// cannot reach a `join` as a *value*. That absence is asserted here, since
/// nothing else enforces it.
#[test]
fn the_orchestration_root_is_joined_with_a_group_in_exactly_one_place() {
    /// The ONE permitted group-path assembly, matched on its exact text.
    const PERMITTED: &str = "root.join(group.as_str())";

    /// Every sanctioned expression that builds a path from the orchestration
    /// root. Anything else that touches it is a finding until it is argued for
    /// and added here — that is what makes this default-deny rather than a
    /// blocklist. Normalized (whitespace collapsed) before comparison.
    const ROOT_USES: &[&str] = &[
        // The assembly point itself, and the two free-function seams that hand
        // the root to it rather than joining it themselves.
        "group_dir_at(&self.root, group)",
        "append_audit(&self.root, group, actor, action, detail);",
        // Non-group paths under the root: per-CLI home overrides, the agent
        // sequence counter. Literal or `format!`-built, never a caller's id.
        "Some(self.root.join(format!(\"{}-{sub}\", cli_dir.trim_start_matches('.'))))",
        "return Some(self.root.join(\"copilot-home\"));",
        "self.root.join(\"agent-seq.json\")",
        // Siblings of the root, not children of it.
        "self.root.parent().unwrap_or(&self.root).join(\"ghshim\")",
        "self.root.parent().unwrap_or(&self.root).join(\"compacthook\")",
        // Handing the root itself onward: `state_root()`, and two background
        // tasks that own a clone. None of these append a segment.
        "self.root.clone()",
        "let root = self.root.clone();",
        "let root = reg.root.clone();",
    ];

    fn normalize(line: &str) -> String {
        line.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Does this line build a path *from the orchestration root*?
    fn touches_root(line: &str) -> bool {
        let n = line.replace(' ', "");
        ["self.root", "reg.root", "state_root()", "default_root()"]
            .iter()
            .any(|r| n.contains(r))
            && [".join(", ".push(", ".clone()", ".to_path_buf()", ".parent()"]
                .iter()
                .any(|m| n.contains(m))
    }

    /// Argument text of every `.join(`/`.push(` on this line, to the matching
    /// close paren (nesting-aware, so `format!("{a}-{b}")` is read whole).
    fn call_args(line: &str, openers: &[&str]) -> Vec<String> {
        let mut out = Vec::new();
        for opener in openers {
            let mut from = 0usize;
            while let Some(rel) = line[from..].find(opener) {
                let start = from + rel + opener.len();
                let mut depth = 1i32;
                let mut end = line.len();
                for (i, c) in line[start..].char_indices() {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = start + i;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                out.push(line[start..end].to_string());
                from = start;
            }
        }
        out
    }

    /// Axis 2: a path component made by unwrapping a typed value.
    fn is_as_str_component(arg: &str) -> bool {
        let t = arg.trim();
        // A tuple argument is a `Vec::push`, never a path build.
        !t.starts_with('(') && t.ends_with(".as_str()")
    }

    /// Supplementary heuristic only — see the doc. Literals are never groups.
    fn names_a_group(arg: &str) -> bool {
        let t = arg.trim();
        if t.starts_with('"') && t.ends_with('"') && t.len() >= 2 {
            return false;
        }
        let a = t.replace(['&', '*', ' '], "");
        a.contains("group") || a.contains("gid") || a.contains("g.id") || a.contains("info.id")
    }

    let src_dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    let mut files = Vec::new();
    collect_rs_files(src_dir, &mut files);
    assert!(files.len() > 5, "the source scan found almost nothing — check the path");

    let mut offenders: Vec<String> = Vec::new();
    let mut permitted_seen = 0usize;
    let mut has_asref_path_impl = false;

    for path in &files {
        let src = std::fs::read_to_string(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        for (i, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            // A comment may spell the construct literally — several do, to
            // explain this very rule.
            if trimmed.starts_with("//") {
                continue;
            }
            if trimmed.replace(' ', "").contains("implAsRef<Path>forGroupId") {
                has_asref_path_impl = true;
            }
            if line.contains(PERMITTED) {
                permitted_seen += 1;
                continue;
            }
            let mut flag = |why: &str| {
                offenders.push(format!("{name}:{}: [{why}] {trimmed}", i + 1));
            };
            // Axis 1 — default-deny on the root.
            if touches_root(line) && !ROOT_USES.contains(&normalize(trimmed).as_str()) {
                flag("unsanctioned use of the orchestration root");
            }
            // Axis 2 — the `.as_str()` path-component shape, any receiver.
            // `.push(` belongs to THIS axis only: `Vec<String>::push(x.as_str())`
            // does not compile, so an `.as_str()` push is a path build — but a
            // plain `Vec::push(group.clone())` is not, and feeding `.push(` to
            // the name heuristic below flagged 21 of them.
            if call_args(line, &[".join(", ".push("])
                .iter()
                .any(|a| is_as_str_component(a))
            {
                flag("`.as_str()` path component outside `group_dir_at`");
            } else if call_args(line, &[".join("]).iter().any(|a| names_a_group(a)) {
                flag("argument names a group");
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a group path must be assembled ONLY in `group_dir_at` (#904), and the orchestration \
         root may only be used as `ROOT_USES` sanctions. Found {} site(s):\n{}\n\nIf one of these \
         is legitimate, add its exact normalized text to `ROOT_USES` with a comment saying why \
         — that argument is the point of this test.",
        offenders.len(),
        offenders.join("\n")
    );
    assert_eq!(
        permitted_seen, 1,
        "expected exactly one `{PERMITTED}` — `group_dir_at`'s own join — found {permitted_seen}"
    );
    assert!(
        !has_asref_path_impl,
        "`GroupId` must not implement `AsRef<Path>`: a validated id becomes a path only \
         through `group_dir_at`, and the impl would make every `join` in the tree a second \
         assembly point"
    );
}

/// **Every group-taking command parses at the boundary** (#904, rev-440 N1).
///
/// This is the guard whose absence let `orch_channel_connect` ship unthreaded
/// through a whole slice: the join guard above watches the *sink*, and nothing
/// watched the *source*. The invariant is mechanical, so it should be a scan
/// rather than a habit — a `#[tauri::command]` whose signature takes a
/// group-shaped `String` must call `command_group` (or `GroupId::parse`) in its
/// own body.
///
/// It is the more valuable of the two guards, because the sink is one function
/// and the sources are fifty.
#[test]
fn every_group_taking_command_parses_its_id_at_the_boundary() {
    let path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/orchestration/mod.rs"
    ));
    let src = std::fs::read_to_string(path).unwrap();
    let lines: Vec<&str> = src.lines().collect();

    let mut checked = 0usize;
    let mut unparsed = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        if lines[i].trim() != "#[tauri::command]" {
            i += 1;
            continue;
        }
        // The command runs from here to the first column-0 `}`.
        let start = i;
        let mut end = i + 1;
        while end < lines.len() && lines[end] != "}" {
            end += 1;
        }
        let body: String = lines[start..=end.min(lines.len() - 1)].join("\n");

        // A group-shaped `String` PARAMETER (never a `&GroupId` — that is
        // already proof).
        //
        // Matched by SHAPE, not by a list of four names (rev-450 N12): find each
        // `: String`, walk back over the identifier, and ask whether THAT names a
        // group. Scans the whole body text, not line ends, because most of these
        // signatures are one-liners — a line-anchored version of this found 29 of
        // the 48 and said so.
        let takes_group_string = {
            let b = body.as_bytes();
            let mut found = false;
            let mut from = 0usize;
            while let Some(rel) = body[from..].find(": String") {
                let at = from + rel;
                let mut s = at;
                while s > 0 {
                    let c = b[s - 1] as char;
                    if c.is_ascii_alphanumeric() || c == '_' {
                        s -= 1;
                    } else {
                        break;
                    }
                }
                let name = &body[s..at];
                if name.contains("group") || name == "gid" {
                    found = true;
                    break;
                }
                from = at + 2;
            }
            found
        };

        if takes_group_string {
            checked += 1;
            if !(body.contains("command_group(") || body.contains("GroupId::parse(")) {
                let name = lines[start..=end.min(lines.len() - 1)]
                    .iter()
                    .find(|l| l.contains("pub async fn ") || l.contains("pub fn "))
                    .map(|l| l.trim().to_string())
                    .unwrap_or_else(|| format!("(command at line {})", start + 1));
                unparsed.push(format!("line {}: {name}", start + 1));
            }
        }
        i = end + 1;
    }

    assert!(
        checked >= 45,
        "expected the ~48 group-taking commands, found {checked} — the scan is no longer \
         matching the command shape. A line-anchored version of the parameter match once \
         found 29 of them, and this floor is what said so."
    );
    assert!(
        unparsed.is_empty(),
        "every `#[tauri::command]` taking a group id must parse it at the boundary via \
         `command_group` (#904, CLAUDE.md constraint 6). {} do not:\n{}",
        unparsed.len(),
        unparsed.join("\n")
    );
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

/// A directory under the orchestration root is a name from OUTSIDE this
/// process — anything at all can create one. `session_roles` walks that
/// listing and feeds what it finds straight back into group-scoped reads, so
/// the listing is parsed, not trusted.
///
/// **The first version of this test could not fail** (rev-440 B3): it seeded the
/// squatter with `group.json` alone, and `session_roles` emits a row only per
/// record from `merged_records` — `agents.json` plus the audit log — so it
/// produced nothing with *or* without the parse filter, and would have held
/// just as well for a perfectly valid directory name. It asserted an emptiness
/// that had nothing to do with the guard.
///
/// So the squatter now carries a session-bearing `agents.json`: a row **would**
/// be emitted if the filter were removed. And the valid group beside it, seeded
/// identically, is the positive control that tells "filtered" apart from
/// "this fixture never produces anything" — the failure mode the first version
/// walked straight into.
#[test]
fn a_directory_that_is_not_a_well_formed_group_id_is_not_treated_as_a_group() {
    fn seed(root: &std::path::Path, dir_name: &str, session: &str) {
        let dir = root.join(dir_name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("group.json"),
            format!(r#"{{"group_id":"{dir_name}","repo":"C:/tmp/x"}}"#),
        )
        .unwrap();
        // One roster row with a session id — what `session_roles` turns into a
        // `SessionRole`. Absent this, the fixture proves nothing.
        std::fs::write(
            dir.join("agents.json"),
            format!(
                r#"[{{"id":"w-1","role":"worker","name":"w","session":"{session}",
                     "cwd":"C:/tmp/x","status":"running","updated_ms":1}}]"#
            ),
        )
        .unwrap();
    }

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("orchestration");
    let reg = registry_at(&root, tmp.path());

    seed(&root, "not a group!", "sess-squatter");
    seed(&root, "repo-1a2b3c4d", "sess-valid");

    let roles = reg.session_roles();

    // Positive control: the identically-seeded VALID group IS enumerated. If
    // this fails, the fixture is wrong and the negative assertion below is
    // worthless.
    assert!(
        roles.iter().any(|r| r.session_id == "sess-valid"),
        "the fixture must produce a row for a valid group — otherwise the refusal below is \
         vacuous. Got {} row(s): {:?}",
        roles.len(),
        roles.iter().map(|r| &r.session_id).collect::<Vec<_>>()
    );

    // The guard: the squatter, seeded the same way, is not.
    assert!(
        roles.iter().all(|r| r.session_id != "sess-squatter"),
        "a directory whose name is not a valid group id must not be enumerated as a group"
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

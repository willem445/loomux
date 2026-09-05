//! Codex's rollout store, as the by-id lookup sees it (#2515 C2) — the mirror
//! of `tests/pisessions.rs` for the third file-backed CLI.
//!
//! What each layer decides, and so what is pinned where:
//!
//! 1. `sessions::codex_sessions_root_from` — WHICH directory, given this
//!    process's environment. A port of the vendor's own resolution, so every
//!    branch of it is a claim about someone else's code and gets its own
//!    assertion.
//! 2. `find_session_cwd("codex", …)` — that a codex id is looked up in codex's
//!    store and reads its header `cwd`, including the case the dispatch would
//!    otherwise get silently wrong (falling through to claude's).
//! 3. The filename rule — the thread id from offset 20 of the name's core up to
//!    a `_` or the end, in BOTH representations codex keeps a rollout in.
//!
//! An integration test, not inline `#[cfg(test)]`, per repo constraint 4: it
//! links the full lib, so it needs the comctl32-v6 manifest `build.rs` embeds
//! only for `-tests`-scoped targets.
//!
//! **No codex is ever run** (constraint 3). Every fixture below is written here
//! from the format read off `openai/codex` at tag `rust-v0.153.4`, blob by blob
//! through the GitHub blob API, and quoted in `doc/design/codex.md`:
//! `rollout/src/recorder.rs` (the `sessions/YYYY/MM/DD` path and the
//! `session_meta` first line), `rollout/src/rollout_file_name.rs` (the two
//! filename forms), `rollout/src/compression.rs` (the `.zst` representation)
//! and `utils/home-dir/src/lib.rs` (`CODEX_HOME`).

use loomux_lib::sessions::{
    codex_sessions_root_from, find_session_cwd, set_claude_projects_root_for_test,
    set_codex_sessions_root_for_test,
};
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Real UUIDs from the vendor's own round-trip test
/// (`rollout_file_name_tests.rs`), so the fixtures below are the shapes codex
/// itself asserts on rather than shapes this repo invented.
const THREAD_A: &str = "019ff1a2-b3c4-7d5e-8f60-112233445566";
const ROLLOUT_A: &str = "019ff1a2-b3c4-7d5e-8f60-667788990011";
const THREAD_B: &str = "019ff1b7-0000-7d5e-8f60-aabbccddeeff";

/// One rollout file, at `<root>/<yyyy>/<mm>/<dd>/<name>`, with `body` verbatim —
/// so a test needing an odd shape (no header, a torn line, a `.zst`) writes it
/// rather than asking this helper for a flag.
fn write_raw(root: &Path, date: (&str, &str, &str), name: &str, body: &str) -> PathBuf {
    let dir = root.join(date.0).join(date.1).join(date.2);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    path
}

/// The canonical file name for an ordinary (never-reverted) rollout —
/// `RolloutFileName::render`'s first arm, with the timestamp in its documented
/// `[year]-[month]-[day]T[hour]-[minute]-[second]` form.
fn rollout_name(ts: &str, thread: &str) -> String {
    format!("rollout-{ts}-{thread}.jsonl")
}

/// The `session_meta` first line, in the vendor's own key order and with the
/// fields this side reads (`payload.id`, `payload.cwd`) plus enough of the rest
/// that a reader can compare the fixture against `SessionMeta` directly.
fn header(id: &str, cwd: &str) -> String {
    format!(
        "{{\"timestamp\":\"2026-09-03T14:00:00.000Z\",\"type\":\"session_meta\",\
         \"payload\":{{\"session_id\":\"{id}\",\"id\":\"{id}\",\
         \"timestamp\":\"2026-09-03T14:00:00.000Z\",\"cwd\":\"{cwd}\",\
         \"originator\":\"codex_cli_rs\",\"cli_version\":\"0.153.4\"}}}}\n"
    )
}

/// A well-formed rollout: header plus one user turn, the turn in codex's
/// `response_item` / `ResponseItem::Message` / `ContentItem::InputText` shape.
fn write_session(root: &Path, date: (&str, &str, &str), ts: &str, thread: &str, cwd: &str) -> PathBuf {
    let body = format!(
        "{}{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\
         \"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"hello\"}}]}}}}\n",
        header(thread, cwd)
    );
    write_raw(root, date, &rollout_name(ts, thread), &body)
}

/// A temp root bound to BOTH the codex seam and the claude one. Binding claude
/// too is the point of one test below and harmless to the rest: an unbound
/// claude root would let a codex lookup that wrongly falls through reach the
/// developer's real `~/.claude`, where it would miss for the wrong reason and
/// the assertion would pass while proving nothing.
struct Seam {
    _tmp: tempfile::TempDir,
    codex: PathBuf,
    claude: PathBuf,
}

fn seam() -> Seam {
    let tmp = tempfile::tempdir().unwrap();
    let codex = tmp.path().join("codex-sessions");
    let claude = tmp.path().join("claude-projects");
    fs::create_dir_all(&codex).unwrap();
    fs::create_dir_all(&claude).unwrap();
    set_codex_sessions_root_for_test(Some(codex.clone()));
    set_claude_projects_root_for_test(Some(claude.clone()));
    Seam { _tmp: tmp, codex, claude }
}

impl Drop for Seam {
    fn drop(&mut self) {
        set_codex_sessions_root_for_test(None);
        set_claude_projects_root_for_test(None);
    }
}

// ---------------------------------------------------------------------------
// 1. Which directory — the vendor's resolution, branch by branch
// ---------------------------------------------------------------------------

#[test]
fn codex_home_empty_is_unset_and_a_file_is_not_a_store() {
    let home = PathBuf::from("/home/dev");

    // Default: `~/.codex/sessions`. `CODEX_HOME` names codex's HOME, not its
    // sessions directory, so the `sessions` segment is appended in BOTH the
    // default and the override branch — the difference from pi's
    // `PI_CODING_AGENT_SESSION_DIR`, which names the sessions directory itself.
    assert_eq!(
        codex_sessions_root_from(None, Some(&home)),
        Some(home.join(".codex").join("sessions"))
    );

    // Override: the segment is still appended.
    assert_eq!(
        codex_sessions_root_from(Some("/srv/codex-home"), Some(&home)),
        Some(PathBuf::from("/srv/codex-home").join("sessions")),
        "CODEX_HOME names the home, so `sessions` is appended to it, not replaced by it"
    );

    // EMPTY is unset, per the vendor's
    // `std::env::var("CODEX_HOME").ok().filter(|val| !val.is_empty())`.
    assert_eq!(
        codex_sessions_root_from(Some(""), Some(&home)),
        Some(home.join(".codex").join("sessions")),
        "an empty CODEX_HOME must not shadow the default — the shell exports one routinely"
    );

    // ...and WHITESPACE is NOT unset, which is the deliberate difference from
    // pi's resolver and the reason this test is named for "empty" rather than
    // "blank". The vendor tests `is_empty()`, never `trim()`, so a value of one
    // space is a real path to codex itself; answering `None` here would be this
    // module disagreeing with the tool it is reading after. It fails safely for
    // the same reason either way — see the store test just below.
    assert_eq!(
        codex_sessions_root_from(Some(" "), Some(&home)),
        Some(PathBuf::from(" ").join("sessions")),
        "the vendor filters on is_empty(), not trim() — mirror it rather than improving on it"
    );

    // No home and no variable: there is no store to name, so `None` — never a
    // relative path that would resolve against the app's own cwd.
    assert_eq!(codex_sessions_root_from(None, None), None);
    // ...but a NAMED home still answers with no home directory at all.
    assert_eq!(
        codex_sessions_root_from(Some("/srv/codex-home"), None),
        Some(PathBuf::from("/srv/codex-home").join("sessions"))
    );
}

#[test]
fn a_codex_home_that_is_a_file_yields_nothing_rather_than_an_error_or_a_fallback() {
    // The vendor hard-errors on this ("CODEX_HOME points to {val:?}, but that
    // path is not a directory"). `find_session_cwd` cannot error — it reserves
    // `Err` for a store that exists and cannot be listed — so the requirement
    // is the weaker one that it finds NOTHING.
    //
    // What it must never do is fall back to `~/.codex`: that would read a
    // different store from the one codex would refuse to run against, and hand
    // back a cwd for a session the configured store does not contain.
    let tmp = tempfile::tempdir().unwrap();
    let not_a_dir = tmp.path().join("codex-home-is-a-file");
    fs::write(&not_a_dir, "").unwrap();

    let root = codex_sessions_root_from(Some(not_a_dir.to_str().unwrap()), None).unwrap();
    assert!(!root.exists(), "a file's `sessions` child cannot exist");

    // And the lookup that would use it answers "not found", not `Err`.
    set_codex_sessions_root_for_test(Some(root));
    let out = find_session_cwd("codex", THREAD_A);
    set_codex_sessions_root_for_test(None);
    assert_eq!(out.unwrap(), None);
}

// ---------------------------------------------------------------------------
// 2. The by-id lookup
// ---------------------------------------------------------------------------

#[test]
fn a_codex_session_is_found_by_id_across_date_dirs_and_reads_its_header_cwd() {
    let s = seam();
    write_session(&s.codex, ("2026", "09", "01"), "2026-09-01T09-00-00", THREAD_A, "/home/dev/alpha");
    write_session(&s.codex, ("2026", "09", "02"), "2026-09-02T09-00-00", THREAD_B, "/home/dev/beta");

    // Found in the SECOND date directory, so the walk really descends every
    // year/month/day rather than answering out of the first — and the two live
    // under different day directories, which is the shape a pane booting across
    // local midnight produces.
    assert_eq!(
        find_session_cwd("codex", THREAD_B).unwrap().as_deref(),
        Some("/home/dev/beta")
    );
    assert_eq!(
        find_session_cwd("codex", THREAD_A).unwrap().as_deref(),
        Some("/home/dev/alpha")
    );

    // An id no file carries is `None` — "no such session", distinct from the
    // found-but-cwd-less answer pinned below.
    assert_eq!(find_session_cwd("codex", "019ffdea-0000-7d5e-8f60-000000000000").unwrap(), None);
}

#[test]
fn a_codex_id_never_falls_into_claudes_arm() {
    // THE MISTYPE THIS ARM FIXES, PINNED FROM BOTH SIDES — the same one #2126
    // fixed for pi, one CLI later. `find_session_cwd` dispatches "copilot, pi,
    // else claude", so without a codex arm a codex thread id — a UUID,
    // indistinguishable by SHAPE from a claude session id — is probed for under
    // `~/.claude/projects`. That is not a crash: the probe MISSES, and the miss
    // reads as `Ok(None)`, i.e. "no such session", so a resume silently loses
    // its workspace with nothing red to say so.
    let s = seam();

    // SIDE ONE: the id lives in codex's store and NOT in claude's, and is found.
    // Without the arm this is `Ok(None)`.
    write_session(&s.codex, ("2026", "09", "01"), "2026-09-01T09-00-00", THREAD_A, "/home/dev/alpha");
    assert_eq!(
        find_session_cwd("codex", THREAD_A).unwrap().as_deref(),
        Some("/home/dev/alpha"),
        "a codex id must be looked up in codex's store"
    );

    // SIDE TWO, and the half that makes this discriminate: the SAME id also
    // sits in claude's store, under a DIFFERENT cwd. A codex lookup that fell
    // through would find it there and answer confidently with the wrong
    // directory — so this asserts which store answered, not merely that one did.
    let claude_project = s.claude.join("-home-dev-WRONG");
    fs::create_dir_all(&claude_project).unwrap();
    fs::write(
        claude_project.join(format!("{THREAD_A}.jsonl")),
        format!("{{\"type\":\"user\",\"cwd\":\"/home/dev/WRONG\",\"message\":{{\"role\":\"user\",\"content\":\"x\"}}}}\n"),
    )
    .unwrap();
    assert_eq!(
        find_session_cwd("codex", THREAD_A).unwrap().as_deref(),
        Some("/home/dev/alpha"),
        "the claude store carries the same id under another cwd — the codex arm must still win"
    );
    // The control that proves the claude fixture is real and reachable, so the
    // assertion above is about the DISPATCH rather than about a file that was
    // never readable in the first place.
    assert_eq!(
        find_session_cwd("claude", THREAD_A).unwrap().as_deref(),
        Some("/home/dev/WRONG"),
        "the claude fixture must genuinely resolve, or side two proves nothing"
    );
}

#[test]
fn a_revert_suffixed_rollout_still_matches_its_uuid() {
    // `thread/revert` names the file `rollout-<ts>-<thread>_<rollout>.jsonl` —
    // `RolloutFileName::render`'s second arm, where the two ids differ. The
    // TRAILING half is a second UUID, not a sequence number (the correction this
    // slice's plan carried, verified at the tag), so the match cannot be a
    // fixed-width read and the id must be taken up to the FIRST `_`.
    let s = seam();
    let name = format!("rollout-2026-09-01T09-00-00-{THREAD_A}_{ROLLOUT_A}.jsonl");
    write_raw(
        &s.codex,
        ("2026", "09", "01"),
        &name,
        &format!("{}", header(THREAD_A, "/home/dev/reverted")),
    );

    assert_eq!(
        find_session_cwd("codex", THREAD_A).unwrap().as_deref(),
        Some("/home/dev/reverted"),
        "a reverted thread must still answer for its own thread id"
    );

    // NEGATIVE CONTROL, and the reason the split is `split_once` rather than
    // `rsplit_once`: the ROLLOUT id is not a thread id, so asking for it must
    // find nothing. Without this, a match rule that took the trailing half
    // would pass the assertion above and be wrong.
    assert_eq!(
        find_session_cwd("codex", ROLLOUT_A).unwrap(),
        None,
        "the half after the `_` is the rollout id — `codex resume` names the THREAD"
    );
}

#[test]
fn a_rollout_codex_has_compressed_is_still_found_with_an_unknown_workspace() {
    // THE SEVEN-DAY CLIFF. `compression.rs` runs a fire-and-forget worker that
    // rewrites `<name>.jsonl` to `<name>.jsonl.zst` once `MIN_ROLLOUT_AGE`
    // (`7 * 24 * 60 * 60` seconds) has passed, so a `*.jsonl`-only walk lists
    // the last week of sessions and answers "no such session" for every older
    // one — on a schedule, not by chance.
    //
    // The body is deliberately NOT valid zstd: nothing here decompresses it, and
    // writing real zstd would suggest otherwise. What is pinned is that the
    // session is FOUND (its NAME carries the thread id) with an unknown
    // workspace, which is `Ok(Some(""))` — the same "found, but the file does
    // not say" answer a torn header gets, and a different answer from `None`.
    let s = seam();
    write_raw(
        &s.codex,
        ("2026", "08", "01"),
        &format!("{}.zst", rollout_name("2026-08-01T09-00-00", THREAD_A)),
        "not-really-zstd",
    );

    assert_eq!(
        find_session_cwd("codex", THREAD_A).unwrap().as_deref(),
        Some(""),
        "a compressed rollout is found with an unknown workspace, never reported absent"
    );
    // The discrimination that makes that meaningful: `Some("")` and `None` are
    // different answers, and `resolve_resume_cwd` has a distinct tag for each.
    assert_ne!(find_session_cwd("codex", THREAD_A).unwrap(), None);
    // And an id that really is absent still answers `None` from the same store,
    // so the line above is not "this store answers Some for everything".
    assert_eq!(find_session_cwd("codex", THREAD_B).unwrap(), None);
}

#[test]
fn a_plain_rollout_hides_its_compressed_sibling_rather_than_answering_twice() {
    // Compression publishes by writing the `.zst` and then removing the
    // `.jsonl`, so a window exists in which BOTH are on disk and they are ONE
    // session. The vendor's own `should_skip_compressed_sibling` resolves it the
    // same way: the plain file wins.
    //
    // Here the two carry DIFFERENT answers — the plain one a real cwd, the
    // compressed one unreadable — so the assertion discriminates between the two
    // orderings rather than holding under either.
    let s = seam();
    let date = ("2026", "09", "01");
    let name = rollout_name("2026-09-01T09-00-00", THREAD_A);
    write_raw(&s.codex, date, &format!("{name}.zst"), "not-really-zstd");
    write_raw(&s.codex, date, &name, &header(THREAD_A, "/home/dev/plain-wins"));

    assert_eq!(
        find_session_cwd("codex", THREAD_A).unwrap().as_deref(),
        Some("/home/dev/plain-wins"),
        "the plain sibling is the readable one and must be the one that answers"
    );
}

#[test]
fn the_header_id_wins_over_the_file_name() {
    // The file name proposes and the header disposes. A rollout whose NAME
    // carries one thread id while its `payload.id` names another is not the
    // session the name claims — `codex resume <id>` is resolved against the
    // recorded id — so the lookup must refuse it and keep walking.
    let s = seam();
    let date = ("2026", "09", "01");

    // A file NAMED for THREAD_A whose header says THREAD_B: asking for THREAD_A
    // must not answer out of it.
    write_raw(
        &s.codex,
        date,
        &rollout_name("2026-09-01T09-00-00", THREAD_A),
        &header(THREAD_B, "/home/dev/mislabelled"),
    );
    assert_eq!(
        find_session_cwd("codex", THREAD_A).unwrap(),
        None,
        "the header names another thread — the file name must not be believed over it"
    );

    // POSITIVE CONTROL, so the `None` above is about the header disagreeing and
    // not about the fixture being unreadable: a second file, correctly named,
    // is found in the same store on the same walk.
    write_raw(
        &s.codex,
        date,
        &rollout_name("2026-09-01T10-00-00", THREAD_B),
        &header(THREAD_B, "/home/dev/correct"),
    );
    assert_eq!(
        find_session_cwd("codex", THREAD_B).unwrap().as_deref(),
        Some("/home/dev/correct")
    );
}

#[test]
fn a_torn_header_is_found_with_an_unknown_workspace_not_reported_absent() {
    // A rollout caught mid-write, or one whose first line is not a
    // `session_meta` at all. The session is real — its name carries the thread
    // id — and its workspace is merely unknown, which is `Ok(Some(""))`.
    let s = seam();
    let date = ("2026", "09", "01");

    // Truncated mid-JSON, with no trailing newline: the pathological shape the
    // read cap exists for.
    write_raw(
        &s.codex,
        date,
        &rollout_name("2026-09-01T09-00-00", THREAD_A),
        "{\"timestamp\":\"2026-09-03T14:00:00.000Z\",\"type\":\"session_me",
    );
    assert_eq!(find_session_cwd("codex", THREAD_A).unwrap().as_deref(), Some(""));

    // A file that exists and is EMPTY — created and not yet written through.
    write_raw(&s.codex, date, &rollout_name("2026-09-01T10-00-00", THREAD_B), "");
    assert_eq!(find_session_cwd("codex", THREAD_B).unwrap().as_deref(), Some(""));

    // A header whose `payload` carries no `cwd` at all: found, workspace
    // unknown — the same answer, reached by a third route.
    let no_cwd = "{\"type\":\"session_meta\",\"payload\":{\"id\":\"019ffcccc-0000-7d5e-8f60-000000000000\"}}\n";
    write_raw(
        &s.codex,
        date,
        &rollout_name("2026-09-01T11-00-00", "019ffcccc-0000-7d5e-8f60-000000000000"),
        no_cwd,
    );
    assert_eq!(
        find_session_cwd("codex", "019ffcccc-0000-7d5e-8f60-000000000000").unwrap().as_deref(),
        Some("")
    );
}

#[test]
fn nothing_outside_the_rollout_name_grammar_is_walked() {
    // Default-deny on the file NAME, so a human's own notes, a lock file or an
    // editor's swap file sitting in a date directory cannot become a session —
    // and, more to the point, cannot be OPENED by a browser refresh.
    let s = seam();
    let date = ("2026", "09", "01");

    // A `.jsonl` that is not a rollout's; a rollout name with no `-` at offset
    // 19 (the vendor requires one there); and the vendor's own temp suffix.
    write_raw(&s.codex, date, "notes.jsonl", &header(THREAD_A, "/nope"));
    write_raw(&s.codex, date, &format!("rollout-short-{THREAD_A}.jsonl"), &header(THREAD_A, "/nope"));
    write_raw(
        &s.codex,
        date,
        &format!("{}.tmp", rollout_name("2026-09-01T09-00-00", THREAD_A)),
        &header(THREAD_A, "/nope"),
    );
    assert_eq!(
        find_session_cwd("codex", THREAD_A).unwrap(),
        None,
        "none of those three names is a rollout's, whatever their contents say"
    );

    // POSITIVE CONTROL for the whole test: the store, the walk and the fixture
    // helper all work — a correctly named file in the SAME directory is found.
    // Without this the assertion above would pass just as well against a walk
    // that was broken outright.
    write_raw(&s.codex, date, &rollout_name("2026-09-01T12-00-00", THREAD_A), &header(THREAD_A, "/yes"));
    assert_eq!(find_session_cwd("codex", THREAD_A).unwrap().as_deref(), Some("/yes"));
}

#[test]
fn an_absent_store_is_nothing_found_rather_than_an_error() {
    // codex installed and never run — which is the state of a fresh machine,
    // and was the state of the one this slice was written on: `~/.codex` exists
    // with `config.toml` and `auth.json` and no `sessions/` directory at all.
    let s = seam();
    fs::remove_dir_all(&s.codex).unwrap();
    assert_eq!(find_session_cwd("codex", THREAD_A).unwrap(), None);
}

#[test]
fn an_id_that_is_not_one_path_component_is_refused_before_the_store_is_touched() {
    // `find_session_cwd` parses through `PathSegment` first (#925). An id that
    // is not a single component cannot name a file here, so it is "not found"
    // rather than a lookup — and never a walk out of the sessions root.
    let s = seam();
    write_session(&s.codex, ("2026", "09", "01"), "2026-09-01T09-00-00", THREAD_A, "/home/dev/alpha");
    for bad in ["../escape", "a/b", "..", ""] {
        assert_eq!(find_session_cwd("codex", bad).unwrap(), None, "{bad:?} must be refused");
    }
    // Control: the store really is populated and reachable, so the four `None`s
    // above are about the ids rather than about an empty store.
    assert_eq!(find_session_cwd("codex", THREAD_A).unwrap().as_deref(), Some("/home/dev/alpha"));
}

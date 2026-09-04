//! The Sessions tab's pi source (#2126 P2): the human's own store, read into the
//! same row shape claude's and copilot's files produce — the mirror of
//! `tests/opencodebrowse.rs` for a file-backed CLI.
//!
//! What is pinned here, and why it is a separate file from `pisessions.rs`:
//! that one is about the by-id LOOKUP (which directory, which file, whose
//! store), this one about the ROW — what the browser shows and what its Resume
//! button runs. The two failure modes are independent: a lookup can be right
//! while the row names the wrong CLI, and vice versa.
//!
//! An integration test, not inline `#[cfg(test)]`, per repo constraint 4.
//!
//! **No pi is ever run** (constraint 3). Fixtures are written here from the
//! format recorded in `doc/design/pi.md`, read off
//! `earendil-works/pi@b79e4cc8` (= tag `v0.84.4`),
//! `packages/coding-agent/docs/session-format.md`.

use loomux_lib::sessions::{
    list_sessions_for_test, set_claude_projects_root_for_test,
    set_copilot_session_state_root_for_test, set_launch_intent_path_for_test,
    set_legacy_copilot_posture_path_for_test, set_opencode_store_for_test,
    set_pi_sessions_root_for_test, set_session_index_path_for_test, SessionInfo,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Every store path the scan touches, bound to one tempdir — the same
/// all-or-nothing rule `tests/sessionindex.rs` states: the scan is a single pass
/// over every source, so leaving ANY of them unbound would read the developer's
/// own history and make every row count below a fact about their machine.
struct Seam {
    _tmp: tempfile::TempDir,
    pi: PathBuf,
}

fn seam() -> Seam {
    let tmp = tempfile::tempdir().unwrap();
    let pi = tmp.path().join("pi-sessions");
    let claude = tmp.path().join("claude-projects");
    let copilot = tmp.path().join("copilot-session-state");
    for d in [&pi, &claude, &copilot] {
        fs::create_dir_all(d).unwrap();
    }
    set_pi_sessions_root_for_test(Some(pi.clone()));
    set_claude_projects_root_for_test(Some(claude));
    set_copilot_session_state_root_for_test(Some(copilot));
    set_session_index_path_for_test(Some(tmp.path().join("session-index.json")));
    set_launch_intent_path_for_test(Some(tmp.path().join("launch-intent.json")));
    set_legacy_copilot_posture_path_for_test(Some(tmp.path().join("copilot-posture.json")));
    // Bound to a path nothing creates: this file is about the pi source, so
    // opencode's contribution to every row count must be a deterministic zero.
    set_opencode_store_for_test(Some(tmp.path().join("opencode").join("opencode.db")));
    Seam { _tmp: tmp, pi }
}

impl Drop for Seam {
    fn drop(&mut self) {
        set_pi_sessions_root_for_test(None);
        set_claude_projects_root_for_test(None);
        set_copilot_session_state_root_for_test(None);
        set_session_index_path_for_test(None);
        set_launch_intent_path_for_test(None);
        set_legacy_copilot_posture_path_for_test(None);
        set_opencode_store_for_test(None);
    }
}

/// One pi session file. `lines` is the file's body BELOW the header, verbatim,
/// so a test that needs an odd shape (no header, a truncated tail) writes it
/// rather than asking this helper for a flag.
fn write_raw(root: &Path, project: &str, file: &str, body: &str) -> PathBuf {
    let dir = root.join(project);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(file);
    fs::write(&path, body).unwrap();
    path
}

/// A well-formed session: header line plus one user turn, in the vendor's
/// documented shapes.
fn write_session(root: &Path, project: &str, timestamp: &str, id: &str, cwd: &str, prompt: &str) -> PathBuf {
    let body = format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\
         \"timestamp\":\"2026-09-03T14:00:00.000Z\",\"cwd\":\"{cwd}\"}}\n\
         {{\"type\":\"message\",\"id\":\"m1\",\"parentId\":null,\
         \"timestamp\":\"2026-09-03T14:00:01.000Z\",\
         \"message\":{{\"role\":\"user\",\"content\":\"{prompt}\"}}}}\n"
    );
    write_raw(root, project, &format!("{timestamp}_{id}.jsonl"), &body)
}

/// Stamp an exact mtime so ordering assertions are about the fixture rather than
/// a race against the clock's granularity — the same reason (and the same
/// mechanism) `tests/sessionindex.rs` stamps.
fn stamp(path: &Path, ms: u64) {
    let f = fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_millis(ms)).unwrap();
}

fn pi_rows(rows: &[SessionInfo]) -> Vec<&SessionInfo> {
    rows.iter().filter(|r| r.source == "pi").collect()
}

// ---------------------------------------------------------------------------
// The row
// ---------------------------------------------------------------------------

#[test]
fn a_pi_session_becomes_a_row_that_resumes_pi() {
    let s = seam();
    let f = write_session(
        &s.pi,
        "--home-dev-alpha--",
        "20260901T090000",
        "aaaa-1111",
        "/home/dev/alpha",
        "  fix the login bug\\nand ship it  ",
    );
    stamp(&f, 1_700_000_000_000);

    let (rows, _) = list_sessions_for_test();
    let pi = pi_rows(&rows);
    assert_eq!(pi.len(), 1, "expected exactly one pi row, got {:?}", rows.iter().map(|r| format!("{}:{}", r.source, r.id)).collect::<Vec<_>>());
    let r = pi[0];

    assert_eq!(r.id, "aaaa-1111");
    assert_eq!(r.source, "pi");
    assert_eq!(r.cwd, "/home/dev/alpha", "cwd comes from the header line");
    // Tidied: leading/trailing space gone, and only the FIRST line kept — the
    // browser shows one line per row.
    assert_eq!(r.title, "fix the login bug");
    assert_eq!(r.modified_ms, 1_700_000_000_000);

    // THE ROW'S WHOLE POINT. `--session` continues an existing conversation;
    // `--session-id` would open-or-CREATE, so a file the human had deleted would
    // come back as an empty pane wearing the right id instead of failing
    // honestly. And it must be `pi`, not the `claude --resume` the fallback arm
    // emits for an unknown source — the wrong CLI handed another vendor's id.
    assert_eq!(r.resume_command, "pi --session aaaa-1111");
}

#[test]
fn the_header_id_wins_over_the_one_in_the_file_name() {
    // The file name is a convenience; the header is what pi itself matched
    // `--session <id>` against when it wrote the file. A renamed or hand-copied
    // file must still resume to the conversation it actually contains.
    let s = seam();
    let body = "{\"type\":\"session\",\"version\":3,\"id\":\"true-id\",\"cwd\":\"/home/dev/x\"}\n\
                {\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n";
    let f = write_raw(&s.pi, "--x--", "20260901T090000_renamed-on-disk.jsonl", body);
    stamp(&f, 1_700_000_100_000);

    let (rows, _) = list_sessions_for_test();
    let pi = pi_rows(&rows);
    assert_eq!(pi.len(), 1);
    assert_eq!(pi[0].id, "true-id");
    assert_eq!(pi[0].resume_command, "pi --session true-id");
}

#[test]
fn a_header_only_pi_file_still_lists_as_an_untitled_row() {
    // pi creates the file on the first ASSISTANT response, so a pane prompted
    // once can have a header and no user turn readable yet. Such a session is
    // real and resumable; it must not vanish from the list, and it must not
    // borrow text the human never wrote for its title.
    let s = seam();
    let f = write_raw(
        &s.pi,
        "--y--",
        "20260901T090000_bare.jsonl",
        "{\"type\":\"session\",\"version\":3,\"id\":\"bare\",\"cwd\":\"/home/dev/y\"}\n",
    );
    stamp(&f, 1_700_000_200_000);

    let (rows, _) = list_sessions_for_test();
    let pi = pi_rows(&rows);
    assert_eq!(pi.len(), 1, "a header-only session must still list");
    assert_eq!(pi[0].id, "bare");
    assert_eq!(pi[0].title, "(no prompt)");
    assert_eq!(pi[0].cwd, "/home/dev/y");
}

#[test]
fn a_malformed_trailing_line_is_tolerated_and_a_non_session_file_is_ignored() {
    let s = seam();
    // pi appends as it goes, so the last line can be half-written.
    let ragged = write_raw(
        &s.pi,
        "--z--",
        "20260901T090000_ragged.jsonl",
        "{\"type\":\"session\",\"version\":3,\"id\":\"ragged\",\"cwd\":\"/home/dev/z\"}\n\
         {\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"still here\"}}\n\
         {\"type\":\"message\",\"messa",
    );
    stamp(&ragged, 1_700_000_300_000);
    // Neither of these is a pi session file: no `_` in the stem, and a
    // non-jsonl extension. Both must be skipped rather than listed with a
    // nonsense id.
    write_raw(&s.pi, "--z--", "notasession.jsonl", "{}\n");
    write_raw(&s.pi, "--z--", "20260901T090000_x.txt", "{}\n");

    let (rows, _) = list_sessions_for_test();
    let pi = pi_rows(&rows);
    assert_eq!(pi.len(), 1, "only the real session file may become a row: {:?}", rows.iter().map(|r| format!("{}:{}", r.source, r.id)).collect::<Vec<_>>());
    assert_eq!(pi[0].id, "ragged");
    assert_eq!(pi[0].title, "still here");
}

#[test]
fn a_pi_kickoff_transcript_carries_its_orchestration_identity() {
    // The same scrape the claude scanner does, on pi's own line shape — a pi
    // worker's session must offer "reopen the whole orchestration", not a bare
    // pane. The signature text is the one `detect_orch_signature` matches.
    let s = seam();
    let body = "{\"type\":\"session\",\"version\":3,\"id\":\"orch-1\",\"cwd\":\"/home/dev/repo\"}\n\
                {\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\
                \"text\":\"You are \\\"w 1\\\" (w-2), a worker agent in orrerix group \
                sempkg-74fe4043 for repository X.\"}]}}\n";
    let f = write_raw(&s.pi, "--repo--", "20260901T090000_orch-1.jsonl", body);
    stamp(&f, 1_700_000_400_000);

    let (rows, _) = list_sessions_for_test();
    let pi = pi_rows(&rows);
    assert_eq!(pi.len(), 1);
    assert_eq!(pi[0].orch_role.as_deref(), Some("worker"));
    assert_eq!(pi[0].orch_group.as_deref(), Some("sempkg-74fe4043"));
    // The array-of-blocks content form parsed, too — that is the shape pi uses
    // for anything but a plain string, and the title proves the text was read
    // rather than skipped.
    assert!(pi[0].title.starts_with("You are"), "title was {:?}", pi[0].title);
}

// ---------------------------------------------------------------------------
// The index (#493) applies to pi verbatim, because pi is file-backed
// ---------------------------------------------------------------------------

#[test]
fn a_second_scan_reuses_the_index_for_an_unchanged_pi_file() {
    let s = seam();
    let f = write_session(&s.pi, "--a--", "20260901T090000", "id-1", "/home/dev/a", "hello");
    stamp(&f, 1_700_000_500_000);

    let (cold_rows, cold) = list_sessions_for_test();
    assert_eq!(cold.parsed, 1, "the first scan must actually open the file");
    assert_eq!(cold.reused, 0);
    assert_eq!(pi_rows(&cold_rows).len(), 1);

    let (warm_rows, warm) = list_sessions_for_test();
    assert_eq!(warm.parsed, 0, "an unchanged pi file must not be re-parsed");
    assert_eq!(warm.reused, 1);
    // Same row, served from the index — the cache must not change the answer.
    assert_eq!(pi_rows(&warm_rows).len(), 1);
    assert_eq!(pi_rows(&warm_rows)[0].id, "id-1");
    assert_eq!(pi_rows(&warm_rows)[0].title, "hello");
    assert_eq!(pi_rows(&warm_rows)[0].resume_command, "pi --session id-1");

    // Touch the file: `(mtime, len)` no longer match, so it is re-parsed.
    fs::write(&f, fs::read_to_string(&f).unwrap() + "{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"more\"}}\n").unwrap();
    stamp(&f, 1_700_000_600_000);
    let (_, hot) = list_sessions_for_test();
    assert_eq!(hot.parsed, 1, "a changed pi file must be re-parsed");
    assert_eq!(hot.reused, 0);
}

#[test]
fn pi_rows_merge_with_the_other_sources_by_time_not_by_source() {
    // `LIST_LIMIT` has always meant "the newest N sessions on this machine,
    // whichever CLI wrote them". A fourth source must not change that into N per
    // source, and must not sort itself to one end of the list.
    let s = seam();
    let old = write_session(&s.pi, "--a--", "20260901T090000", "pi-old", "/home/dev/a", "old");
    let new = write_session(&s.pi, "--a--", "20260903T090000", "pi-new", "/home/dev/a", "new");
    stamp(&old, 1_700_000_000_000);
    stamp(&new, 1_700_002_000_000);

    // One claude session between the two in time, written into the claude root
    // this seam already bound.
    let claude_dir = s._tmp.path().join("claude-projects").join("-home-dev-a");
    fs::create_dir_all(&claude_dir).unwrap();
    let cf = claude_dir.join("claude-mid.jsonl");
    fs::write(
        &cf,
        "{\"type\":\"user\",\"cwd\":\"/home/dev/a\",\"message\":{\"content\":\"mid\"}}\n",
    )
    .unwrap();
    stamp(&cf, 1_700_001_000_000);

    let (rows, _) = list_sessions_for_test();
    let order: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        order,
        vec!["pi-new", "claude-mid", "pi-old"],
        "rows must be newest-first across sources, not grouped by source"
    );
    assert_eq!(rows[0].source, "pi");
    assert_eq!(rows[1].source, "claude");
    assert_eq!(rows[2].source, "pi");
}

#[test]
fn an_absent_pi_store_contributes_nothing_and_is_not_an_error() {
    let s = seam();
    // Point the seam at a directory that does not exist — pi has never run here.
    set_pi_sessions_root_for_test(Some(s._tmp.path().join("no-such-pi")));
    let (rows, stats) = list_sessions_for_test();
    assert!(pi_rows(&rows).is_empty());
    // Positive control for the emptiness above: the scan really ran, and really
    // found no FILES at all, rather than the assertion passing because something
    // threw before the pi collector was reached.
    assert_eq!(stats.files_seen, 0);
    assert_eq!(stats.rows, 0);
}
#[test]
fn a_flat_env_override_store_produces_rows_too() {
    // REVIEW ROUND 1, FINDING 1, from the browser's side. Under
    // `PI_CODING_AGENT_SESSION_DIR` pi writes session files FLAT into the named
    // directory — no per-cwd segment (`SOURCE` `core/session-manager.ts:1521`,
    // `:954`; its own `list` reads the dir flat at `:824`). A scanner that
    // walked only the nested layout returned ZERO rows for such a store, with
    // nothing red to say so, because every fixture in this file modelled the
    // default shape.
    let s = seam();
    let f = write_raw(
        &s.pi,
        ".",
        "20260901T090000_flat-1.jsonl",
        "{\"type\":\"session\",\"version\":3,\"id\":\"flat-1\",\"cwd\":\"/srv/work\"}\n\
         {\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"flat store\"}}\n",
    );
    stamp(&f, 1_700_000_700_000);

    let (rows, _) = list_sessions_for_test();
    let pi = pi_rows(&rows);
    assert_eq!(pi.len(), 1, "a flat store must still produce a row");
    assert_eq!(pi[0].id, "flat-1");
    assert_eq!(pi[0].cwd, "/srv/work");
    assert_eq!(pi[0].title, "flat store");
    assert_eq!(pi[0].resume_command, "pi --session flat-1");
}

#[test]
fn both_layouts_list_together_and_neither_hides_the_other() {
    let s = seam();
    let flat = write_raw(
        &s.pi,
        ".",
        "20260901T090000_flat-1.jsonl",
        "{\"type\":\"session\",\"version\":3,\"id\":\"flat-1\",\"cwd\":\"/srv/work\"}\n",
    );
    let nested = write_session(&s.pi, "--home-dev-a--", "20260902T090000", "nested-1", "/home/dev/a", "nested");
    stamp(&flat, 1_700_000_800_000);
    stamp(&nested, 1_700_000_900_000);

    let (rows, _) = list_sessions_for_test();
    let ids: Vec<&str> = pi_rows(&rows).iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["nested-1", "flat-1"], "both layouts list, newest first");

    // And the index (#493) keys on the file path, so it works the same for a
    // flat store — the second scan re-uses both entries rather than re-parsing.
    let (_, warm) = list_sessions_for_test();
    assert_eq!(warm.parsed, 0);
    assert_eq!(warm.reused, 2);
}

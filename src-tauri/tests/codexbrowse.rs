//! The Sessions tab's codex source (#2515 C2): the human's own rollout store,
//! read into the same row shape claude's and pi's files produce — the mirror of
//! `tests/pibrowse.rs` for the third file-backed CLI.
//!
//! What is pinned here, and why it is a separate file from `codexsessions.rs`:
//! that one is about the by-id LOOKUP (which directory, which file, whose
//! store), this one about the ROW — what the browser shows and what its Resume
//! button runs. The two failure modes are independent: a lookup can be right
//! while the row names the wrong CLI, and vice versa.
//!
//! An integration test, not inline `#[cfg(test)]`, per repo constraint 4.
//!
//! **No codex is ever run** (constraint 3). Fixtures are written here from the
//! format read off `openai/codex` at tag `rust-v0.153.4` and quoted in
//! `doc/design/codex.md`.

use loomux_lib::sessions::{
    list_sessions_for_test, set_claude_projects_root_for_test,
    set_codex_sessions_root_for_test, set_copilot_session_state_root_for_test,
    set_launch_intent_path_for_test, set_legacy_copilot_posture_path_for_test,
    set_opencode_store_for_test, set_pi_sessions_root_for_test,
    set_session_index_path_for_test, SessionInfo,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

const THREAD_A: &str = "019ff1a2-b3c4-7d5e-8f60-112233445566";
const ROLLOUT_A: &str = "019ff1a2-b3c4-7d5e-8f60-667788990011";
const THREAD_B: &str = "019ff1b7-0000-7d5e-8f60-aabbccddeeff";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Every store path the scan touches, bound to one tempdir — the same
/// all-or-nothing rule `tests/sessionindex.rs` states: the scan is a single pass
/// over every source, so leaving ANY of them unbound would read the developer's
/// own history and make every row count below a fact about their machine.
struct Seam {
    _tmp: tempfile::TempDir,
    codex: PathBuf,
}

fn seam() -> Seam {
    let tmp = tempfile::tempdir().unwrap();
    let codex = tmp.path().join("codex-sessions");
    let claude = tmp.path().join("claude-projects");
    let copilot = tmp.path().join("copilot-session-state");
    let pi = tmp.path().join("pi-sessions");
    for d in [&codex, &claude, &copilot, &pi] {
        fs::create_dir_all(d).unwrap();
    }
    set_codex_sessions_root_for_test(Some(codex.clone()));
    set_claude_projects_root_for_test(Some(claude));
    set_copilot_session_state_root_for_test(Some(copilot));
    set_pi_sessions_root_for_test(Some(pi));
    set_session_index_path_for_test(Some(tmp.path().join("session-index.json")));
    set_launch_intent_path_for_test(Some(tmp.path().join("launch-intent.json")));
    set_legacy_copilot_posture_path_for_test(Some(tmp.path().join("copilot-posture.json")));
    // Bound to a path nothing creates: this file is about the codex source, so
    // opencode's contribution to every row count must be a deterministic zero.
    set_opencode_store_for_test(Some(tmp.path().join("opencode").join("opencode.db")));
    Seam { _tmp: tmp, codex }
}

impl Drop for Seam {
    fn drop(&mut self) {
        set_codex_sessions_root_for_test(None);
        set_claude_projects_root_for_test(None);
        set_copilot_session_state_root_for_test(None);
        set_pi_sessions_root_for_test(None);
        set_session_index_path_for_test(None);
        set_launch_intent_path_for_test(None);
        set_legacy_copilot_posture_path_for_test(None);
        set_opencode_store_for_test(None);
    }
}

/// One rollout file, verbatim — so a test needing an odd shape (no header, a
/// torn line, a `.zst`) writes it rather than asking this helper for a flag.
fn write_raw(root: &Path, date: (&str, &str, &str), name: &str, body: &str) -> PathBuf {
    let dir = root.join(date.0).join(date.1).join(date.2);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    path
}

fn rollout_name(ts: &str, thread: &str) -> String {
    format!("rollout-{ts}-{thread}.jsonl")
}

/// The `session_meta` first line, in the vendor's own key order.
fn header(id: &str, cwd: &str) -> String {
    format!(
        "{{\"timestamp\":\"2026-09-03T14:00:00.000Z\",\"type\":\"session_meta\",\
         \"payload\":{{\"session_id\":\"{id}\",\"id\":\"{id}\",\
         \"timestamp\":\"2026-09-03T14:00:00.000Z\",\"cwd\":\"{cwd}\",\
         \"originator\":\"codex_cli_rs\",\"cli_version\":\"0.153.4\"}}}}\n"
    )
}

/// A user turn, in codex's `response_item` / `ResponseItem::Message` /
/// `ContentItem::InputText` shape — where the role sits two levels down and the
/// text block's type is `input_text`, NOT the `text` claude and pi share.
fn user_turn(text: &str) -> String {
    format!(
        "{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\
         \"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"{text}\"}}]}}}}\n"
    )
}

fn write_session(
    root: &Path,
    date: (&str, &str, &str),
    ts: &str,
    thread: &str,
    cwd: &str,
    prompt: &str,
) -> PathBuf {
    let body = format!("{}{}", header(thread, cwd), user_turn(prompt));
    write_raw(root, date, &rollout_name(ts, thread), &body)
}

/// Stamp an exact mtime, so ordering assertions are about the fixture rather
/// than a race against the clock's granularity.
fn stamp(path: &Path, ms: u64) {
    let f = fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_millis(ms)).unwrap();
}

fn codex_rows(rows: &[SessionInfo]) -> Vec<&SessionInfo> {
    rows.iter().filter(|r| r.source == "codex").collect()
}

// ---------------------------------------------------------------------------
// The row
// ---------------------------------------------------------------------------

#[test]
fn a_codex_session_becomes_a_row_that_resumes_codex() {
    let s = seam();
    let f = write_session(
        &s.codex,
        ("2026", "09", "01"),
        "2026-09-01T09-00-00",
        THREAD_A,
        "/home/dev/alpha",
        "  fix the login bug\\nand ship it  ",
    );
    stamp(&f, 1_700_000_000_000);

    let (rows, _) = list_sessions_for_test();
    let codex = codex_rows(&rows);
    assert_eq!(
        codex.len(),
        1,
        "expected exactly one codex row, got {:?}",
        rows.iter().map(|r| format!("{}:{}", r.source, r.id)).collect::<Vec<_>>()
    );
    let r = codex[0];

    assert_eq!(r.id, THREAD_A);
    assert_eq!(r.source, "codex");
    assert_eq!(r.cwd, "/home/dev/alpha", "cwd comes from the header line");
    // Tidied: leading/trailing space gone, and only the FIRST line kept — the
    // browser shows one line per row.
    assert_eq!(r.title, "fix the login bug");
    assert_eq!(r.modified_ms, 1_700_000_000_000);

    // THE ROW'S WHOLE POINT, and codex's is shaped unlike every other source's:
    // a SUBCOMMAND, not a flag. It must not be the `claude --resume <id>` the
    // fallback arm emits for an unknown source — the wrong CLI handed another
    // vendor's id — and it must not invent `-C`, `-p` or a posture flag, none of
    // which a session with no recorded orchestration membership is entitled to.
    assert_eq!(r.resume_command, format!("codex resume {THREAD_A}"));
}

#[test]
fn a_codex_title_reads_input_text_blocks_not_the_text_ones_its_siblings_use() {
    // THE ONE PLACE codex's line format diverges from BOTH of the formats the
    // shared `content_text` serves. Its `ContentItem` has no `text` variant at
    // all: a user turn's blocks are `input_text`. A scanner that reused the
    // shared reader would title every codex row `(no prompt)` — a green suite
    // and an empty browser.
    let s = seam();
    let date = ("2026", "09", "01");

    // The claude/pi block shape, on a codex line: not a title, because codex
    // does not write it and this scanner must not invent a reading for it.
    let wrong_shape = format!(
        "{}{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\
         \"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"sibling shape\"}}]}}}}\n",
        header(THREAD_B, "/home/dev/beta")
    );
    write_raw(&s.codex, date, &rollout_name("2026-09-01T08-00-00", THREAD_B), &wrong_shape);

    // codex's real shape, which IS a title.
    write_session(&s.codex, date, "2026-09-01T09-00-00", THREAD_A, "/home/dev/alpha", "real prompt");

    let (rows, _) = list_sessions_for_test();
    let codex = codex_rows(&rows);
    let title_of = |id: &str| {
        codex
            .iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("no codex row for {id}"))
            .title
            .as_str()
    };
    assert_eq!(title_of(THREAD_A), "real prompt");
    assert_eq!(
        title_of(THREAD_B),
        "(no prompt)",
        "a `text` block is not codex's shape and must not be read as one"
    );
}

#[test]
fn the_header_id_wins_over_the_one_in_the_file_name() {
    // The file name is a convenience; the header is what `codex resume <id>` is
    // resolved against. A renamed or hand-copied rollout must still resume to
    // the thread it actually contains.
    let s = seam();
    let f = write_raw(
        &s.codex,
        ("2026", "09", "01"),
        &rollout_name("2026-09-01T09-00-00", THREAD_A),
        &format!("{}{}", header(THREAD_B, "/home/dev/x"), user_turn("hello")),
    );
    stamp(&f, 1_700_000_100_000);

    let (rows, _) = list_sessions_for_test();
    let codex = codex_rows(&rows);
    assert_eq!(codex.len(), 1);
    assert_eq!(codex[0].id, THREAD_B, "the header's id, not the file name's");
    assert_eq!(codex[0].resume_command, format!("codex resume {THREAD_B}"));
}

#[test]
fn a_revert_suffixed_rollout_lists_under_its_thread_id_not_its_rollout_id() {
    // `thread/revert` writes `rollout-<ts>-<thread>_<rollout>.jsonl`. The row
    // must carry the THREAD id, because that is what `codex resume` names — and
    // this fixture's header is deliberately absent, so the id can ONLY have come
    // from the file name, which is the half being tested.
    let s = seam();
    let name = format!("rollout-2026-09-01T09-00-00-{THREAD_A}_{ROLLOUT_A}.jsonl");
    let f = write_raw(&s.codex, ("2026", "09", "01"), &name, "");
    stamp(&f, 1_700_000_200_000);

    let (rows, _) = list_sessions_for_test();
    let codex = codex_rows(&rows);
    assert_eq!(codex.len(), 1);
    assert_eq!(codex[0].id, THREAD_A);
    assert_ne!(codex[0].id, ROLLOUT_A, "the half after the `_` is the rollout id");
    assert_eq!(codex[0].resume_command, format!("codex resume {THREAD_A}"));
}

#[test]
fn a_compressed_rollout_lists_with_an_unknown_workspace_rather_than_vanishing() {
    // THE SEVEN-DAY CLIFF, at the browser rather than at the lookup. Codex's
    // compression worker rewrites a rollout to `.jsonl.zst` once
    // `MIN_ROLLOUT_AGE` (7 days) has passed, so a `*.jsonl`-only walk shows a
    // human only their last week and silently drops the rest.
    //
    // Nothing here decompresses the file — that would be a new `src-tauri`
    // dependency for a header line — so the row's title and cwd are the
    // "found, but the file does not say" answers, and its id comes from the
    // NAME, which a `.zst` still carries.
    let s = seam();
    let f = write_raw(
        &s.codex,
        ("2026", "08", "01"),
        &format!("{}.zst", rollout_name("2026-08-01T09-00-00", THREAD_A)),
        "not-really-zstd",
    );
    stamp(&f, 1_700_000_300_000);

    let (rows, _) = list_sessions_for_test();
    let codex = codex_rows(&rows);
    assert_eq!(codex.len(), 1, "a compressed rollout is a row, not a gap");
    assert_eq!(codex[0].id, THREAD_A);
    assert_eq!(codex[0].cwd, "", "unknown workspace — the header is not readable here");
    assert_eq!(codex[0].title, "(no prompt)");
    // The row is still USEFUL, which is the whole justification for listing it:
    // its Resume button runs a real command.
    assert_eq!(codex[0].resume_command, format!("codex resume {THREAD_A}"));
}

#[test]
fn a_plain_rollout_and_its_compressed_sibling_are_one_row_not_two() {
    // Compression publishes by writing the `.zst` and then removing the
    // `.jsonl`; in the window where both exist they are ONE session. The
    // vendor's `should_skip_compressed_sibling` resolves it the same way.
    //
    // The two carry DIFFERENT content, so this discriminates between the two
    // orderings rather than holding under either: a duplicate row would be
    // two, and the wrong winner would be an untitled one.
    let s = seam();
    let date = ("2026", "09", "01");
    let name = rollout_name("2026-09-01T09-00-00", THREAD_A);
    write_raw(&s.codex, date, &format!("{name}.zst"), "not-really-zstd");
    let plain = write_raw(
        &s.codex,
        date,
        &name,
        &format!("{}{}", header(THREAD_A, "/home/dev/plain"), user_turn("readable")),
    );
    stamp(&plain, 1_700_000_400_000);

    let (rows, _) = list_sessions_for_test();
    let codex = codex_rows(&rows);
    assert_eq!(codex.len(), 1, "one session, one row");
    assert_eq!(codex[0].cwd, "/home/dev/plain");
    assert_eq!(codex[0].title, "readable", "the readable sibling is the one that wins");
}

#[test]
fn rows_from_several_date_dirs_merge_and_sort_by_mtime_with_the_other_sources() {
    // The list is one pass over every source, sorted newest-first by mtime, so a
    // codex row has to interleave with the others rather than clump. Three
    // codex rows across three DAY directories, stamped out of directory order,
    // so a walk that returned them in enumeration order would fail.
    let s = seam();
    let a = write_session(&s.codex, ("2026", "09", "01"), "2026-09-01T09-00-00", THREAD_A, "/a", "first");
    let b = write_session(&s.codex, ("2026", "09", "02"), "2026-09-02T09-00-00", THREAD_B, "/b", "second");
    let c_id = "019ff1c8-0000-7d5e-8f60-cccccccccccc";
    let c = write_session(&s.codex, ("2026", "10", "03"), "2026-10-03T09-00-00", c_id, "/c", "third");
    stamp(&a, 3_000);
    stamp(&b, 1_000);
    stamp(&c, 2_000);

    let (rows, _) = list_sessions_for_test();
    let codex = codex_rows(&rows);
    assert_eq!(codex.len(), 3, "all three date directories are walked");
    assert_eq!(
        codex.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec![THREAD_A, c_id, THREAD_B],
        "newest mtime first, regardless of which day directory the file sits in"
    );
}

#[test]
fn archived_sessions_is_not_listed_and_the_residual_is_pinned() {
    // `archived_sessions/` is a SIBLING subtree of `sessions/`, of the same
    // shape, holding what `codex archive` moved out of the way. A human who
    // archived a session asked for it to stop appearing, so listing it would
    // undo that — the residual `walk_codex_session_files` discloses.
    //
    // This pins the residual ITSELF rather than only the rule: the guard really
    // does miss the archived file, so a later reader finds a test where the
    // disclosure is, instead of a disclosure that has quietly gone false.
    let s = seam();
    let archived = s.codex.parent().unwrap().join("archived_sessions");
    write_session(&archived, ("2026", "09", "01"), "2026-09-01T09-00-00", THREAD_B, "/archived", "old");

    // POSITIVE CONTROL FIRST, and it is what makes the zero below meaningful:
    // an identical file in the LIVE tree does list, so the absence is about the
    // subtree and not about the fixture, the seam or the walk being broken.
    write_session(&s.codex, ("2026", "09", "01"), "2026-09-01T10-00-00", THREAD_A, "/live", "new");

    let (rows, _) = list_sessions_for_test();
    let codex = codex_rows(&rows);
    assert_eq!(codex.len(), 1, "only the live session lists");
    assert_eq!(codex[0].id, THREAD_A);
    assert!(
        !codex.iter().any(|r| r.id == THREAD_B),
        "the archived session must not appear — that is the disclosed residual, pinned"
    );
}

#[test]
fn an_absent_codex_store_contributes_nothing_rather_than_failing_the_scan() {
    // codex installed and never run, which is the state of a fresh machine. The
    // scan must return the other sources' rows, not an error and not an empty
    // list.
    let s = seam();
    fs::remove_dir_all(&s.codex).unwrap();
    let (rows, _) = list_sessions_for_test();
    assert!(codex_rows(&rows).is_empty());
    // POSITIVE CONTROL for the vacuity: `list_sessions_for_test` really did run
    // over a bound store rather than short-circuiting, which an `is_empty()`
    // alone cannot tell you. A claude row in the same pass is the witness.
    let claude_dir = s._tmp.path().join("claude-projects").join("-home-dev-x");
    fs::create_dir_all(&claude_dir).unwrap();
    fs::write(
        claude_dir.join("cl-1.jsonl"),
        "{\"type\":\"user\",\"cwd\":\"/home/dev/x\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
    )
    .unwrap();
    let (rows, _) = list_sessions_for_test();
    assert!(codex_rows(&rows).is_empty(), "still no codex rows");
    assert_eq!(rows.iter().filter(|r| r.source == "claude").count(), 1, "the scan ran");
}

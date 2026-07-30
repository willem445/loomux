//! #493: the startup session scan — bounded by the row limit, cached across
//! launches, and never a source of wrong answers.
//!
//! What this pins, and why it's shaped this way: the bug was 826 session files
//! head-parsed on every scan, 13–17 seconds, for a list that shows at most 300
//! rows. The fix is structural (parse only what the limit will keep; cache each
//! head-parse in `session-index.json` keyed by path and validated by
//! `(mtime, len)`), so the tests assert on the WORK DONE — `ScanStats`'
//! parsed/reused counts — not on elapsed time. A wall-clock assertion would be
//! flaky on CI and, worse, would pass for the wrong reason on a fast disk while
//! a regression re-parsed everything.
//!
//! Integration test, not a unit test, per the repo's constraint 4: these link
//! the lib, so they need the `-tests`-scoped comctl32 manifest `build.rs`
//! embeds.

use loomux_lib::sessions::{
    find_session_cwd, list_sessions_for_test, set_claude_projects_root_for_test,
    set_copilot_session_state_root_for_test, set_launch_intent_path_for_test,
    set_legacy_copilot_posture_path_for_test, set_session_index_path_for_test, LIST_LIMIT,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

/// Every store path the scan touches, bound to one tempdir. Both CLI roots are
/// bound even when a test only fixtures one of them: the scan is a single pass
/// over both, so leaving either unbound would walk the developer's real
/// `~/.claude`/`~/.copilot` history — slow, non-deterministic, and (with 826
/// real sessions) big enough to push a test's own fixtures past `LIST_LIMIT`.
/// The launch-intent pair is bound for the same reason: `resume_command` is
/// derived from it, and an unbound legacy path would read the developer's real
/// `copilot-posture.json`.
struct Seam {
    _tmp: tempfile::TempDir,
    claude: PathBuf,
    copilot: PathBuf,
    index: PathBuf,
}

fn seam() -> Seam {
    let tmp = tempfile::tempdir().unwrap();
    let claude = tmp.path().join("claude-projects");
    let copilot = tmp.path().join("copilot-session-state");
    let index = tmp.path().join("session-index.json");
    fs::create_dir_all(&claude).unwrap();
    fs::create_dir_all(&copilot).unwrap();
    set_claude_projects_root_for_test(Some(claude.clone()));
    set_copilot_session_state_root_for_test(Some(copilot.clone()));
    set_session_index_path_for_test(Some(index.clone()));
    set_launch_intent_path_for_test(Some(tmp.path().join("launch-intent.json")));
    set_legacy_copilot_posture_path_for_test(Some(tmp.path().join("copilot-posture.json")));
    Seam { _tmp: tmp, claude, copilot, index }
}

fn clear_seams() {
    set_claude_projects_root_for_test(None);
    set_copilot_session_state_root_for_test(None);
    set_session_index_path_for_test(None);
    set_launch_intent_path_for_test(None);
    set_legacy_copilot_posture_path_for_test(None);
}

/// Stamp an exact mtime, so "the newest 300" is a fact about the fixture rather
/// than a race against the OS clock's update granularity (many files written in
/// a loop otherwise share one timestamp, making the sort's tie order — and so
/// which rows the limit keeps — arbitrary).
fn set_mtime(path: &Path, ms: u64) {
    let f = fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_millis(ms)).unwrap();
}

/// One claude session: `<root>/<project>/<id>.jsonl`, the shape `scan_claude`'s
/// successor collects and `find_session_cwd`'s claude half resolves by filename.
fn write_claude(root: &Path, id: &str, prompt: &str, mtime_ms: u64) -> PathBuf {
    let proj = root.join("proj");
    fs::create_dir_all(&proj).unwrap();
    let path = proj.join(format!("{id}.jsonl"));
    fs::write(
        &path,
        format!(
            "{{\"type\":\"user\",\"cwd\":\"C:/work/{id}\",\"message\":{{\"content\":{prompt:?}}}}}\n"
        ),
    )
    .unwrap();
    set_mtime(&path, mtime_ms);
    path
}

/// One copilot session directory: `<root>/<dir>/workspace.yaml`, whose `id:`
/// field — not the directory name — is authoritative.
fn write_copilot(root: &Path, dir: &str, id: &str, name: &str, mtime_ms: u64) -> PathBuf {
    let d = root.join(dir);
    fs::create_dir_all(&d).unwrap();
    let ws = d.join("workspace.yaml");
    fs::write(&ws, format!("id: {id}\nname: {name}\ncwd: C:/work/{id}\n")).unwrap();
    set_mtime(&ws, mtime_ms);
    ws
}

/// THE #493 property: the expensive part of the scan is bounded by how many rows
/// the list can show, not by how many sessions the machine has ever recorded.
///
/// The pre-#493 scan head-parsed all 826 (13–17s on the reporting machine) and
/// then threw 526 of those parses away on the same `truncate(300)` this still
/// applies — so this is the "does it scale with history?" question from the
/// issue, answered as a test: 826 files seen, 300 parsed.
#[test]
fn parse_cost_is_bounded_by_the_row_limit_not_by_history() {
    let s = seam();
    // The population the issue measured, with deterministic ascending mtimes so
    // "the newest 300" is exactly ids 526..=825.
    let total = 826u64;
    for i in 0..total {
        write_claude(&s.claude, &format!("sess-{i:04}"), &format!("prompt {i}"), 1_700_000_000_000 + i);
    }

    let (rows, stats) = list_sessions_for_test();

    assert_eq!(stats.files_seen, total as usize, "every session file must still be DISCOVERED (metadata only)");
    assert_eq!(rows.len(), LIST_LIMIT, "the list is still capped at the same row limit");
    assert_eq!(
        stats.parsed, LIST_LIMIT,
        "only the rows the limit keeps may be head-parsed — parsing all {total} and discarding \
         {} of them is the #493 bug",
        total as usize - LIST_LIMIT
    );
    assert_eq!(stats.reused, 0, "cold cache: nothing to reuse yet");

    // …and the rows are the same set, in the same newest-first order, the
    // pre-#493 scan produced: sort by mtime desc, then cut.
    let want: Vec<String> =
        (0..total).rev().take(LIST_LIMIT).map(|i| format!("sess-{i:04}")).collect();
    let got: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
    assert_eq!(got, want, "the limit must keep the NEWEST rows, newest first");

    clear_seams();
}

/// THE structural perf pin: a session file is opened once, ever. A second scan
/// (the next launch, the ↻ button, the reconciler) serves every unchanged file
/// out of the persisted index and opens nothing.
#[test]
fn a_second_scan_opens_no_session_file_at_all() {
    let s = seam();
    for i in 0..40u64 {
        write_claude(&s.claude, &format!("sess-{i:02}"), &format!("prompt {i}"), 1_700_000_000_000 + i);
    }
    write_copilot(&s.copilot, "dir-a", "cop-a", "copilot session a", 1_700_000_000_100);

    let (first, cold) = list_sessions_for_test();
    assert_eq!(cold.parsed, 41, "cold: every listed session is parsed once");
    assert_eq!(cold.reused, 0);

    let (second, warm) = list_sessions_for_test();
    assert_eq!(warm.parsed, 0, "warm: not one session file may be opened again");
    assert_eq!(warm.reused, 41, "…every row must come from the index instead");

    // Identical rows, not merely the same count: a cache that served different
    // content would be worse than no cache.
    let key = |r: &loomux_lib::sessions::SessionInfo| {
        (r.id.clone(), r.source.clone(), r.title.clone(), r.cwd.clone(), r.resume_command.clone(), r.modified_ms)
    };
    assert_eq!(
        first.iter().map(key).collect::<Vec<_>>(),
        second.iter().map(key).collect::<Vec<_>>(),
        "the cached scan must reproduce the parsed scan exactly"
    );

    clear_seams();
}

/// A steady-state launch doesn't even write the index back — the claim
/// `scan_sessions`' "nothing parsed and the same entry count" guard makes.
/// Pinned via the index file's own mtime: a rewrite would move it.
#[test]
fn an_unchanged_scan_does_not_rewrite_the_index() {
    let s = seam();
    write_claude(&s.claude, "sess-a", "hello", 1_700_000_000_000);
    list_sessions_for_test();
    assert!(s.index.is_file(), "the cold scan must have written an index");

    let marker = 1_600_000_000_000u64;
    set_mtime(&s.index, marker);
    let (_, warm) = list_sessions_for_test();
    assert_eq!(warm.parsed, 0);

    let after = fs::metadata(&s.index)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    assert_eq!(after, marker, "an unchanged scan must not rewrite the index");

    clear_seams();
}

/// The index is a cache, not a record: a rewritten transcript is re-parsed (so
/// its row reflects the file, not the memory of it), and a deleted one is simply
/// gone — never resurrected from a stale entry, which is the #440 failure class
/// the issue explicitly warned an index must not introduce.
#[test]
fn a_rewritten_session_is_reparsed_and_a_deleted_one_never_resurrects() {
    let s = seam();
    let changed = write_claude(&s.claude, "sess-changed", "original prompt", 1_700_000_000_003);
    write_claude(&s.claude, "sess-stable", "stable prompt", 1_700_000_000_002);
    let doomed = write_claude(&s.claude, "sess-doomed", "doomed prompt", 1_700_000_000_001);

    let (rows, _) = list_sessions_for_test();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows.iter().find(|r| r.id == "sess-changed").unwrap().title, "original prompt");

    // Rewrite one (new content, new length, new mtime) and delete another.
    fs::write(
        &changed,
        "{\"type\":\"user\",\"cwd\":\"C:/work/sess-changed\",\"message\":{\"content\":\"REWRITTEN prompt, longer than before\"}}\n",
    )
    .unwrap();
    set_mtime(&changed, 1_700_000_000_009);
    fs::remove_file(&doomed).unwrap();

    let (rows, stats) = list_sessions_for_test();
    assert_eq!(stats.parsed, 1, "only the changed file may be re-parsed");
    assert_eq!(stats.reused, 1, "the untouched one still comes from the index");
    assert_eq!(
        rows.iter().find(|r| r.id == "sess-changed").unwrap().title,
        "REWRITTEN prompt, longer than before",
        "a changed transcript must not be answered from the index"
    );
    assert!(
        !rows.iter().any(|r| r.id == "sess-doomed"),
        "a deleted session must never come back out of the index"
    );

    clear_seams();
}

/// #412/#440 semantics, unchanged: the row limit (and now the index behind it)
/// bounds what the LIST shows, never what can be resolved by id. A session too
/// old to be listed is still resolvable — the exact shape of #440's "a session
/// id that existed was reported as unresumable".
#[test]
fn the_row_limit_never_gates_resume_by_id() {
    let s = seam();
    let total = LIST_LIMIT as u64 + 5;
    for i in 0..total {
        write_claude(&s.claude, &format!("sess-{i:04}"), &format!("prompt {i}"), 1_700_000_000_000 + i);
    }
    let oldest = "sess-0000";

    let (rows, _) = list_sessions_for_test();
    assert!(
        !rows.iter().any(|r| r.id == oldest),
        "fixture check: the oldest session must fall outside the listed rows"
    );

    assert_eq!(
        find_session_cwd("claude", oldest).unwrap().as_deref(),
        Some("C:/work/sess-0000"),
        "resume-by-id must resolve a session the list is too short to show (#412/#440)"
    );

    clear_seams();
}

/// Every index failure mode degrades to "parse it again", never to a wrong or
/// missing row: absent (covered by every cold scan above), corrupt, or written
/// by a different version of the entry shape.
#[test]
fn a_corrupt_or_foreign_index_degrades_to_a_full_parse() {
    let s = seam();
    write_claude(&s.claude, "sess-a", "hello a", 1_700_000_000_002);
    write_claude(&s.claude, "sess-b", "hello b", 1_700_000_000_001);

    // Not JSON at all: quarantined by the same fail-safe tabs.json uses.
    fs::write(&s.index, "{not json at all").unwrap();
    let (rows, stats) = list_sessions_for_test();
    assert_eq!(stats.parsed, 2, "a corrupt index must be discarded, not partially trusted");
    assert_eq!(rows.len(), 2);
    assert!(
        s.index.with_extension("corrupt.json").is_file(),
        "the corrupt file must survive for inspection (load_or_quarantine)"
    );

    // Valid JSON, wrong version: same outcome. Entries are shaped exactly like
    // the current ones and would deserialize fine — the version gate is what
    // must reject them, so this can't pass by accident.
    let foreign = format!(
        r#"{{"version":999,"entries":[{{"path":{:?},"modified_ms":1700000000002,"len":9,"id":"sess-a","title":"STALE TITLE","cwd":"C:/nope","orch_role":null,"orch_gid":null}}]}}"#,
        s.claude.join("proj").join("sess-a.jsonl").to_string_lossy()
    );
    fs::write(&s.index, foreign).unwrap();
    let (rows, stats) = list_sessions_for_test();
    assert_eq!(stats.parsed, 2, "an index from another version must be ignored wholesale");
    assert_eq!(
        rows.iter().find(|r| r.id == "sess-a").unwrap().title,
        "hello a",
        "…and certainly never used to answer with its stale content"
    );

    clear_seams();
}

/// Orchestration identity detected in a transcript survives a cached read: the
/// sidebar's ORCH/W/REV chips (and the group-restore routing behind them) must
/// not quietly disappear on the second launch, when the row comes from the index
/// instead of the file.
#[test]
fn cached_rows_keep_their_orchestration_identity() {
    let s = seam();
    let proj = s.claude.join("proj");
    fs::create_dir_all(&proj).unwrap();
    let path = proj.join("orch-sess.jsonl");
    fs::write(
        &path,
        "{\"type\":\"user\",\"cwd\":\"C:/work/orch\",\"message\":{\"content\":\"You are the orchestrator of loomux agent group demo-1234 for the repository C:/work/orch.\"}}\n",
    )
    .unwrap();
    set_mtime(&path, 1_700_000_000_000);

    let (cold, cold_stats) = list_sessions_for_test();
    assert_eq!(cold_stats.parsed, 1);
    let row = cold.iter().find(|r| r.id == "orch-sess").unwrap();
    assert_eq!(row.orch_role.as_deref(), Some("orchestrator"));
    assert_eq!(row.orch_group.as_deref(), Some("demo-1234"));

    let (warm, warm_stats) = list_sessions_for_test();
    assert_eq!(warm_stats.parsed, 0, "second scan must be served from the index");
    let row = warm.iter().find(|r| r.id == "orch-sess").unwrap();
    assert_eq!(row.orch_role.as_deref(), Some("orchestrator"), "role must survive the cache");
    assert_eq!(row.orch_group.as_deref(), Some("demo-1234"), "group must survive the cache");

    clear_seams();
}

/// A synthesized claude store shaped like the one #493 was measured on: many
/// sessions whose HEADS are big, which is where the cost actually is. Measured
/// against the real 955-file store on the reporting machine (`Get-Content
/// -TotalCount 60` over a random 60-file sample): the first 60 lines of a real
/// transcript average 174,599 bytes (median 169,909, min 30,912, max 477,167) —
/// so a scan that head-parses 826 of them reads and JSON-parses ~140 MB, which
/// is what 13–17 seconds buys.
///
/// Deterministic: same file count, same bytes, same ascending mtimes on every
/// run, so a before/after comparison is a comparison of code and not of
/// fixtures.
fn write_big_claude_store(root: &Path, count: u64) {
    let proj = root.join("proj");
    fs::create_dir_all(&proj).unwrap();
    // ~30 KB opening user turn (a real one carries the system reminders and
    // CLAUDE.md injection), then 59 turns of ~2.4 KB — ~172 KB of head, the
    // measured median.
    let big = "x".repeat(30_000);
    let mid = "y".repeat(2_400);
    for i in 0..count {
        let id = format!("sess-{i:04}");
        let mut body = String::with_capacity(180_000);
        body.push_str(&format!(
            "{{\"type\":\"user\",\"cwd\":\"C:/work/{id}\",\"message\":{{\"content\":\"prompt {i} {big}\"}}}}\n"
        ));
        for line in 0..59 {
            body.push_str(&format!(
                "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"turn {line} {mid}\"}}]}}}}\n"
            ));
        }
        let path = proj.join(format!("{id}.jsonl"));
        fs::write(&path, &body).unwrap();
        set_mtime(&path, 1_700_000_000_000 + i);
    }
}

/// MEASUREMENT, not a gate — `#[ignore]`d on purpose. Run it with:
///
/// ```text
/// cargo test --locked --test sessionindex -- --ignored --nocapture measure
/// ```
///
/// The assertions above pin the SHAPE of the work (parsed/reused counts), which
/// is what a test can check without being flaky. This prints the wall clock for
/// a human comparing before/after — the numbers quoted in #493's PR came from
/// here, on the same synthesized population main was measured against.
#[test]
#[ignore]
fn measure_scan_cost_on_a_synthesized_population() {
    let s = seam();
    let count: u64 = std::env::var("LOOMUX_PERF493_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(826);

    let t = std::time::Instant::now();
    write_big_claude_store(&s.claude, count);
    println!("fixture: {count} claude sessions written in {:?}", t.elapsed());

    let t = std::time::Instant::now();
    let (rows, cold) = list_sessions_for_test();
    println!(
        "COLD  {:?}  seen={} rows={} parsed={} reused={}",
        t.elapsed(),
        cold.files_seen,
        rows.len(),
        cold.parsed,
        cold.reused
    );

    let t = std::time::Instant::now();
    let (rows, warm) = list_sessions_for_test();
    println!(
        "WARM  {:?}  seen={} rows={} parsed={} reused={}",
        t.elapsed(),
        warm.files_seen,
        rows.len(),
        warm.parsed,
        warm.reused
    );

    clear_seams();
}

/// Copilot's half of the scan keeps its own rules through the index: the id
/// comes from `workspace.yaml`'s `id:` field and not the directory name, and a
/// directory with no `workspace.yaml` yet (session not written) is skipped
/// rather than listed as an empty row.
#[test]
fn copilot_rows_are_indexed_by_their_recorded_id_and_incomplete_dirs_are_skipped() {
    let s = seam();
    write_copilot(&s.copilot, "dir-name-differs", "cop-real-id", "my copilot session", 1_700_000_000_000);
    fs::create_dir_all(s.copilot.join("not-written-yet")).unwrap();

    let (rows, stats) = list_sessions_for_test();
    assert_eq!(rows.len(), 1, "the workspace.yaml-less directory must not become a row");
    assert_eq!(stats.files_seen, 1, "…and must not even be counted as a session file");
    assert_eq!(rows[0].id, "cop-real-id", "the recorded id wins over the directory name");
    assert_eq!(rows[0].title, "my copilot session");
    assert_eq!(rows[0].resume_command, "copilot --resume=cop-real-id");

    let (rows, warm) = list_sessions_for_test();
    assert_eq!(warm.parsed, 0);
    assert_eq!(rows[0].id, "cop-real-id", "the cached copilot row keeps its recorded id");

    clear_seams();
}

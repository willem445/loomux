//! The Sessions tab's opencode source (#722 slice C2): the human's own store,
//! read into the same row shape claude's and copilot's files produce.
//!
//! What each layer decides, and so what is pinned where:
//!
//! 1. `sessions::opencode_store_from` — WHICH database, given this process's
//!    environment. A port of a vendor function, so every branch of it is a
//!    claim about someone else's code and gets its own assertion.
//! 2. `scan_opencode` (through `list_sessions_for_test`) — which sessions
//!    become rows, what those rows say, and what happens when the store is
//!    absent or shaped differently than the DDL below.
//! 3. The merge — that `LIST_LIMIT` still means "the newest N sessions on this
//!    machine", not N per source.
//!
//! An integration test, not inline `#[cfg(test)]`, per repo constraint 4: it
//! links the full lib, so it needs the comctl32-v6 manifest `build.rs` embeds
//! only for `-tests`-scoped targets.
//!
//! **No opencode is ever run** (constraint 3). Every fixture store is built
//! here from the DDL recorded on issue #722 (slice-V memo §1b), read off
//! `anomalyco/opencode@f67e80c2` (tag `v1.18.11`) — the same DDL
//! `tests/opencodesessions.rs` and `tests/opencodeusage.rs` build from, kept as
//! a literal in each because Rust compiles every integration test as its own
//! crate.

use loomux_lib::sessions::{
    list_sessions_for_test, opencode_store_from, set_claude_projects_root_for_test,
    set_copilot_session_state_root_for_test, set_launch_intent_path_for_test,
    set_legacy_copilot_posture_path_for_test, set_opencode_store_for_test,
    set_session_index_path_for_test, SessionInfo, LIST_LIMIT,
};
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Verbatim from the slice-V memo §1b, trimmed to the columns loomux reads —
/// `title`, `directory`, `parent_id` and `time_updated` are the four this
/// scanner depends on, and they are spelled here exactly as the vendor's DDL
/// spells them.
fn session_ddl() -> &'static str {
    "CREATE TABLE session (
        id text PRIMARY KEY, project_id text NOT NULL, parent_id text,
        slug text NOT NULL, directory text NOT NULL, title text NOT NULL,
        version text NOT NULL, share_url text, permission text,
        time_created integer NOT NULL, time_updated integer NOT NULL,
        time_compacting integer, time_archived integer, workspace_id text,
        path text, agent text, model text,
        cost real DEFAULT 0 NOT NULL,
        tokens_input integer DEFAULT 0 NOT NULL,
        tokens_output integer DEFAULT 0 NOT NULL,
        tokens_reasoning integer DEFAULT 0 NOT NULL,
        tokens_cache_read integer DEFAULT 0 NOT NULL,
        tokens_cache_write integer DEFAULT 0 NOT NULL,
        metadata text)"
}

/// One `session` row's worth of what these tests vary. `directory` carries
/// forward slashes because that is how opencode writes the column
/// (`LOCAL-OBSERVED`: `C:/Projects/loomux`) — the row must surface the store's
/// own string, not a normalized one, since that string is what the CLI itself
/// recorded for the session.
struct Row<'a> {
    id: &'a str,
    parent: Option<&'a str>,
    title: &'a str,
    directory: &'a str,
    updated: i64,
}

impl<'a> Row<'a> {
    fn new(id: &'a str, title: &'a str, updated: i64) -> Self {
        Row { id, parent: None, title, directory: "C:/Projects/loomux", updated }
    }
    fn child_of(mut self, parent: &'a str) -> Self {
        self.parent = Some(parent);
        self
    }
    fn in_dir(mut self, dir: &'a str) -> Self {
        self.directory = dir;
        self
    }
}

fn insert(conn: &Connection, r: &Row) {
    conn.execute(
        "INSERT INTO session (id, project_id, parent_id, slug, directory, title, version,
                              time_created, time_updated)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            r.id,
            "f9dd9fcdf18a51fa9de041f787210d1ce5e0d1e7",
            r.parent,
            "loomux",
            r.directory,
            r.title,
            "1.18.11",
            r.updated,
            r.updated,
        ],
    )
    .unwrap();
}

/// A store at `path` holding `rows`, in WAL — the journal mode opencode opens
/// its database in (`SOURCE`, `database/database.ts`).
fn store(path: &Path, rows: &[Row]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0)).unwrap();
    conn.execute_batch(session_ddl()).unwrap();
    for r in rows {
        insert(&conn, r);
    }
}

/// Every store the scan touches, bound to one scratch directory — ALL of them,
/// including the two this file never fixtures. The scan is a single pass over
/// every source, so an unbound root walks the developer's real `~/.claude`,
/// `~/.copilot` or opencode history, which would make these row counts a fact
/// about that machine rather than about the code.
struct Seam {
    dir: PathBuf,
}

impl Seam {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir()
            .join(format!("opencode-browse-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let claude = dir.join("claude-projects");
        let copilot = dir.join("copilot-session-state");
        fs::create_dir_all(&claude).unwrap();
        fs::create_dir_all(&copilot).unwrap();
        set_claude_projects_root_for_test(Some(claude));
        set_copilot_session_state_root_for_test(Some(copilot));
        set_session_index_path_for_test(Some(dir.join("session-index.json")));
        set_launch_intent_path_for_test(Some(dir.join("launch-intent.json")));
        set_legacy_copilot_posture_path_for_test(Some(dir.join("copilot-posture.json")));
        // The store this file is about — bound even by the tests that never
        // create the file, since "no store" is itself a case under test.
        set_opencode_store_for_test(Some(dir.join("opencode").join("opencode.db")));
        Seam { dir }
    }
    fn db(&self) -> PathBuf {
        self.dir.join("opencode").join("opencode.db")
    }
    fn claude_root(&self) -> PathBuf {
        self.dir.join("claude-projects")
    }
}

impl Drop for Seam {
    fn drop(&mut self) {
        set_claude_projects_root_for_test(None);
        set_copilot_session_state_root_for_test(None);
        set_session_index_path_for_test(None);
        set_launch_intent_path_for_test(None);
        set_legacy_copilot_posture_path_for_test(None);
        set_opencode_store_for_test(None);
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// One claude session file, for the tests that need a second source to merge
/// against: `<root>/<project>/<id>.jsonl`, timestamped exactly so the merge
/// order is a fact about the fixture and not a race with the OS clock.
fn write_claude(root: &Path, id: &str, prompt: &str, mtime_ms: u64) {
    let proj = root.join("proj");
    fs::create_dir_all(&proj).unwrap();
    let path = proj.join(format!("{id}.jsonl"));
    fs::write(
        &path,
        format!("{{\"type\":\"user\",\"cwd\":\"C:/work\",\"message\":{{\"content\":{prompt:?}}}}}\n"),
    )
    .unwrap();
    let f = fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_millis(mtime_ms)).unwrap();
}

fn opencode_rows(rows: &[SessionInfo]) -> Vec<&SessionInfo> {
    rows.iter().filter(|r| r.source == "opencode").collect()
}

// ---------------------------------------------------------------------------
// 1. Which database
// ---------------------------------------------------------------------------

/// The default resolution, and the one that decides whether the tab shows
/// anything at all on an ordinary machine: `<home>/.local/share/opencode/
/// opencode.db` — including on Windows, where the vendor's `xdgData` has no
/// platform special-case (`SOURCE`, `global.ts`; `LOCAL-OBSERVED` at
/// `%USERPROFILE%\.local\share\opencode`).
#[test]
fn the_store_defaults_to_the_xdg_data_path_under_home() {
    let home = PathBuf::from("C:/Users/Someone");
    assert_eq!(
        opencode_store_from(None, None, Some(&home)),
        Some(home.join(".local").join("share").join("opencode").join("opencode.db"))
    );
}

/// `XDG_DATA_HOME` moves the whole data directory, home or no home — the
/// vendor reads it first and so must this.
#[test]
fn xdg_data_home_relocates_the_store() {
    let xdg = PathBuf::from("D:/xdg");
    let home = PathBuf::from("C:/Users/Someone");
    assert_eq!(
        opencode_store_from(None, Some(&xdg), Some(&home)),
        Some(xdg.join("opencode").join("opencode.db")),
        "XDG_DATA_HOME must win over the home fallback, not be ignored"
    );
    // Empty is not a value: an exported-but-blank variable falls back rather
    // than resolving the store to a relative `opencode/opencode.db`.
    assert_eq!(
        opencode_store_from(None, Some(Path::new("")), Some(&home)),
        Some(home.join(".local").join("share").join("opencode").join("opencode.db"))
    );
}

/// `OPENCODE_DB` overrides both — absolute (or `:memory:`) taken as-is, a bare
/// name resolved under the data directory. This is the branch that keeps the
/// scanner honest for a human who sets the variable globally: loomux would
/// otherwise list the sessions of a store their own `opencode` never opens.
#[test]
fn the_opencode_db_variable_wins_exactly_as_the_vendor_resolves_it() {
    let home = PathBuf::from("C:/Users/Someone");
    assert_eq!(
        opencode_store_from(Some("D:/elsewhere/mine.db"), None, Some(&home)),
        Some(PathBuf::from("D:/elsewhere/mine.db")),
        "an absolute OPENCODE_DB is used verbatim"
    );
    assert_eq!(
        opencode_store_from(Some("mine.db"), None, Some(&home)),
        Some(home.join(".local").join("share").join("opencode").join("mine.db")),
        "a relative OPENCODE_DB resolves under the data directory, not the cwd"
    );
    assert_eq!(
        opencode_store_from(Some(":memory:"), None, Some(&home)),
        Some(PathBuf::from(":memory:")),
        ":memory: is a literal the vendor passes through — never joined onto a path"
    );
}

/// No home and no `XDG_DATA_HOME` is "there is no store", not a relative path
/// resolved against whatever directory loomux happens to be running in.
#[test]
fn without_a_home_directory_there_is_no_store_to_read() {
    assert_eq!(opencode_store_from(None, None, None), None);
    assert_eq!(opencode_store_from(Some("mine.db"), None, None), None);
}

// ---------------------------------------------------------------------------
// 2. Which sessions become rows, and what they say
// ---------------------------------------------------------------------------

/// The row itself: id, title, the store's own directory string, the store's
/// own timestamp — and a resume command that runs OPENCODE.
///
/// That last assertion is the reason this slice's two halves ship together:
/// before the opencode arm existed, `build_resume_command`'s fallback answered
/// for it and produced `claude --resume ses_…` — the wrong CLI, handed an id
/// out of another vendor's store.
#[test]
fn a_root_session_becomes_a_row_that_resumes_opencode() {
    let s = Seam::new("row");
    store(
        &s.db(),
        &[Row::new("ses_03bd2d53dffeiBvu9PvuCPjxT7", "wire up the sessions tab", 1_785_703_307_950)],
    );

    let (rows, stats) = list_sessions_for_test();
    let rows = opencode_rows(&rows);

    assert_eq!(rows.len(), 1, "the store's one root session must be listed");
    assert_eq!(stats.opencode, 1, "…and counted as an opencode read, not a file parse");
    assert_eq!(rows[0].id, "ses_03bd2d53dffeiBvu9PvuCPjxT7");
    assert_eq!(rows[0].title, "wire up the sessions tab");
    assert_eq!(rows[0].cwd, "C:/Projects/loomux", "the store's own directory string, verbatim");
    assert_eq!(rows[0].modified_ms, 1_785_703_307_950);
    assert_eq!(
        rows[0].resume_command, "opencode --session ses_03bd2d53dffeiBvu9PvuCPjxT7",
        "an opencode row must resume opencode: `--session`, and never claude's `--resume`"
    );
    assert_eq!(stats.parsed, 0, "no session FILE exists to head-parse for this row");
}

/// A subagent session is a row in the same table, and adopting one would offer
/// the human a conversation they never had — the parent's `@explore` detour —
/// as if it were their session. Same exclusion `identify_session` makes, for
/// the same reason.
#[test]
fn a_subagent_session_is_never_offered_as_a_session_to_reopen() {
    let s = Seam::new("subagent");
    store(
        &s.db(),
        &[
            Row::new("ses_parent0000aaaaaaaaaaaaaa", "the pane's own session", 1_785_703_300_000),
            Row::new("ses_child00000bbbbbbbbbbbbb", "explore (@explore subagent)", 1_785_703_400_000)
                .child_of("ses_parent0000aaaaaaaaaaaaaa"),
        ],
    );

    let (rows, _) = list_sessions_for_test();
    let ids: Vec<&str> = opencode_rows(&rows).iter().map(|r| r.id.as_str()).collect();

    assert_eq!(
        ids,
        vec!["ses_parent0000aaaaaaaaaaaaaa"],
        "only root sessions are listed — and note the subagent is the NEWER row, so a rule \
         that merely took the latest would have kept the wrong one"
    );
}

/// Ordered by the store's timestamp, never by id. OpenCode may mint ids
/// `descending` (a bitwise-inverted timestamp, `id.ts#L62`), so id order is not
/// time order — and these ids are chosen so that NEITHER direction of an id
/// sort reproduces the expected list: by time it is b, a, c; by id ascending
/// a, b, c; descending c, b, a. A scanner that sorted on the id string would
/// therefore be caught whichever way round it did it.
#[test]
fn rows_are_ordered_by_time_not_by_id_string() {
    let s = Seam::new("order");
    store(
        &s.db(),
        &[
            Row::new("ses_cccccccccccc000000000003", "oldest", 1_785_703_300_000),
            Row::new("ses_aaaaaaaaaaaa000000000001", "middle", 1_785_703_400_000),
            Row::new("ses_bbbbbbbbbbbb000000000002", "newest", 1_785_703_500_000),
        ],
    );

    let (rows, _) = list_sessions_for_test();
    let titles: Vec<&str> = opencode_rows(&rows).iter().map(|r| r.title.as_str()).collect();

    assert_eq!(
        titles,
        vec!["newest", "middle", "oldest"],
        "newest first, by time_updated — the ids sort the other way on purpose"
    );
}

/// A session titles itself from its first turn, so a store can hold a root
/// session with an empty title. It gets a name rather than a blank row, the
/// same way an untitled copilot session does — and a very long one is cut at
/// the same width every other source's title is.
#[test]
fn an_untitled_session_is_named_and_a_long_title_is_cut() {
    let s = Seam::new("titles");
    let long = "x".repeat(200);
    store(
        &s.db(),
        &[
            Row::new("ses_untitled00000000000000a", "", 1_785_703_500_000),
            Row::new("ses_verylongtitle00000000b", &long, 1_785_703_400_000),
        ],
    );

    let (rows, _) = list_sessions_for_test();
    let rows = opencode_rows(&rows);

    assert_eq!(rows[0].title, "OpenCode session", "an empty title must not render as a blank row");
    assert_eq!(
        rows[1].title.chars().count(),
        91,
        "a long title is cut to the same 90 characters + ellipsis as every other source's"
    );
    assert!(rows[1].title.ends_with('…'));
}

/// Two directories, both listed: unlike identification (which asks "is this
/// session THIS pane's"), browsing asks "what has this human got", and the
/// answer is not scoped to one folder.
#[test]
fn sessions_from_every_directory_are_listed() {
    let s = Seam::new("dirs");
    store(
        &s.db(),
        &[
            Row::new("ses_here000000000000000001", "in the repo", 1_785_703_500_000),
            Row::new("ses_there00000000000000002", "somewhere else", 1_785_703_400_000)
                .in_dir("D:/other/project"),
        ],
    );

    let (rows, _) = list_sessions_for_test();
    let cwds: Vec<&str> = opencode_rows(&rows).iter().map(|r| r.cwd.as_str()).collect();

    assert_eq!(cwds, vec!["C:/Projects/loomux", "D:/other/project"]);
}

// ---------------------------------------------------------------------------
// 3. Degrading, never failing
// ---------------------------------------------------------------------------

/// The ordinary state of a machine where opencode has never run: no store, no
/// rows, and every other source still listed. A scan that failed here would
/// empty the whole sidebar for the vast majority of users.
#[test]
fn no_store_at_all_is_no_rows_and_no_effect_on_the_other_sources() {
    let s = Seam::new("absent");
    write_claude(&s.claude_root(), "11111111-2222-3333-4444-555555555555", "hello", 1_700_000_000_000);
    assert!(!s.db().exists(), "the fixture must not create the store for this case");

    let (rows, stats) = list_sessions_for_test();

    assert_eq!(stats.opencode, 0);
    assert!(opencode_rows(&rows).is_empty());
    assert_eq!(rows.len(), 1, "the claude session is still listed");
    assert_eq!(rows[0].source, "claude");
}

/// The vendor's schema carries no compatibility promise, so a store shaped
/// differently than the DDL above is a degrade — the same posture the usage
/// reader takes. A wrong-shaped `session` table must not take the sidebar's
/// other sources down with it.
#[test]
fn a_drifted_schema_degrades_to_no_rows_rather_than_failing_the_scan() {
    let s = Seam::new("drift");
    fs::create_dir_all(s.db().parent().unwrap()).unwrap();
    let conn = Connection::open(s.db()).unwrap();
    // A `session` table that is not THIS `session` table: no title, no
    // directory, no parent_id — exactly what a vendor rename would leave.
    conn.execute_batch("CREATE TABLE session (id text PRIMARY KEY, whatever text)").unwrap();
    conn.execute("INSERT INTO session (id, whatever) VALUES ('ses_x', 'y')", []).unwrap();
    drop(conn);
    write_claude(&s.claude_root(), "11111111-2222-3333-4444-555555555555", "hello", 1_700_000_000_000);

    let (rows, stats) = list_sessions_for_test();

    assert_eq!(stats.opencode, 0, "a drifted store yields nothing…");
    assert_eq!(rows.len(), 1, "…and the scan still answers with what it could read");
    assert_eq!(rows[0].source, "claude");
}

// ---------------------------------------------------------------------------
// 4. The merge
// ---------------------------------------------------------------------------

/// `LIST_LIMIT` is a property of the LIST, not of each source: the rows come
/// back interleaved by time, so a source's newer session outranks another
/// source's older one. Appending opencode's rows after the file-backed ones
/// would show a two-year-old opencode session above a claude session from this
/// morning.
#[test]
fn rows_from_every_source_interleave_by_time() {
    let s = Seam::new("merge");
    write_claude(&s.claude_root(), "aaaaaaaa-0000-0000-0000-000000000001", "old claude", 1_700_000_001_000);
    write_claude(&s.claude_root(), "aaaaaaaa-0000-0000-0000-000000000002", "new claude", 1_700_000_003_000);
    store(
        &s.db(),
        &[
            Row::new("ses_middle0000000000000001", "middle opencode", 1_700_000_002_000),
            Row::new("ses_newest0000000000000002", "newest opencode", 1_700_000_004_000),
        ],
    );

    let (rows, _) = list_sessions_for_test();
    let order: Vec<(&str, u64)> =
        rows.iter().map(|r| (r.source.as_str(), r.modified_ms)).collect();

    assert_eq!(
        order,
        vec![
            ("opencode", 1_700_000_004_000),
            ("claude", 1_700_000_003_000),
            ("opencode", 1_700_000_002_000),
            ("claude", 1_700_000_001_000),
        ],
        "one list, newest first, whichever CLI wrote the row"
    );
}

/// The limit still bounds the LIST, and the query still bounds the read: a
/// store with more sessions than the list can show never materializes them
/// all, and never pushes the list past its cap.
#[test]
fn the_row_limit_bounds_the_merged_list_and_the_query_that_feeds_it() {
    let s = Seam::new("limit");
    let over = LIST_LIMIT + 25;
    // The ids and titles are owned up front so the borrowing `Row`s built from
    // them outlive the slice handed to `store`.
    let ids: Vec<String> = (0..over).map(|i| format!("ses_{i:026}")).collect();
    let titles: Vec<String> = (0..over).map(|i| format!("session {i}")).collect();
    let built: Vec<Row> = (0..over)
        .map(|i| Row::new(&ids[i], &titles[i], 1_700_000_000_000 + i as i64))
        .collect();
    store(&s.db(), &built);
    // One claude file NEWER than every opencode session: the list's newest row
    // must still be able to come from another source once opencode alone could
    // fill it.
    write_claude(&s.claude_root(), "aaaaaaaa-0000-0000-0000-000000000009", "newest of all", 1_800_000_000_000);

    let (listed, stats) = list_sessions_for_test();

    assert_eq!(stats.opencode, LIST_LIMIT, "the LIMIT is pushed into the query, not applied after");
    assert_eq!(listed.len(), LIST_LIMIT, "the merged list is still capped");
    assert_eq!(listed[0].source, "claude", "…and the cap never crowds out a newer row from elsewhere");
}

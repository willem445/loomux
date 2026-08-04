//! OpenCode session identification (#722 slice C): which session a pane owns.
//!
//! Three layers, each pinned where it decides something:
//!
//! 1. `opencodedb::identify_session` — the candidate rule and the refusal.
//! 2. `OrchRegistry::capture_session_baseline` / `search_for_session` — the
//!    wiring that turns that rule into a pane's session id, including the
//!    claim exclusion that only exists because a group's store is shared.
//! 3. `sanitize_session` and the resume-cwd router, reached through the public
//!    paths that were broken for opencode before this slice.
//!
//! An integration test, not inline `#[cfg(test)]`, per repo constraint #4 — a
//! unit-test binary linking the full lib misses the comctl32-v6 manifest
//! `build.rs` only embeds for integration-test targets.
//!
//! **No opencode is ever run** (constraint 3). Every fixture store is built
//! here from the DDL recorded on issue #722 (slice-V memo §1b), read off
//! `anomalyco/opencode@f67e80c2` (tag `v1.18.11`) — the same DDL
//! `tests/opencodeusage.rs` builds from, kept as a literal in each file
//! because Rust compiles every integration test as its own crate.

use loomux_lib::opencodedb::{self, Identified, Unavailable};
use loomux_lib::orchestration::{
    self, workflow, Guardrails, OrchRegistry, Role, SessionBaseline, SessionSearch,
};
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

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

/// One `session` row's worth of the fields these tests vary. `directory` is
/// spelled with forward slashes because that is how opencode writes the column
/// (`LOCAL-OBSERVED`: `C:/Projects/loomux`) — a pane's cwd arrives with
/// backslashes, and the gap between the two is a thing under test, not an
/// accident of the fixture.
struct Row<'a> {
    id: &'a str,
    parent: Option<&'a str>,
    directory: &'a str,
    created: i64,
}

impl<'a> Row<'a> {
    fn new(id: &'a str) -> Self {
        Row { id, parent: None, directory: "C:/Projects/loomux", created: 1_785_703_307_950 }
    }
}

fn insert(conn: &Connection, r: &Row) {
    conn.execute(
        "INSERT INTO session (id, project_id, parent_id, slug, directory, title, version,
                              time_created, time_updated)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            r.id,
            // sha1("git-remote:github.com/willem445/loomux") — the real project
            // id for this repo. Every loomux worktree shares it, which is the
            // whole reason identification cannot key on the project.
            "f9dd9fcdf18a51fa9de041f787210d1ce5e0d1e7",
            r.parent,
            "loomux",
            r.directory,
            "a title",
            "1.18.11",
            r.created,
            r.created,
        ],
    )
    .unwrap();
}

/// A store at `path` holding `rows`, in WAL — the journal mode opencode opens
/// its database in (`SOURCE`, `database/database.ts`).
fn store(path: &Path, rows: &[Row]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0)).unwrap();
    conn.execute_batch(session_ddl()).unwrap();
    for r in rows {
        insert(&conn, r);
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir()
            .join(format!("opencode-sessions-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    fn db(&self) -> PathBuf {
        self.0.join("opencode.db")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Real ids from the slice-V memo, and same-shape siblings: `ses_` + 12 hex +
/// 14 base62 (`SOURCE`, `id.ts`).
const OLD: &str = "ses_03bd2d53dffeiBvu9PvuCPjxT7";
const NEW: &str = "ses_1508a391dffext5Xb0UUF2UDjk";
const OTHER: &str = "ses_15089ff54ffeQ2mSoRBFxdH2mS";
const SUB: &str = "ses_1508b00120ffZZmSoRBFxdH2mS";

/// The pane's cwd as loomux holds it: Windows-native separators, which is NOT
/// how the column is spelled.
const CWD: &str = r"C:\Projects\loomux";

fn ids(v: &[&str]) -> HashSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn none() -> HashSet<String> {
    HashSet::new()
}

// ---------------------------------------------------------------------------
// 1. The candidate rule
// ---------------------------------------------------------------------------

#[test]
fn the_session_that_appeared_since_the_spawn_is_the_panes_own() {
    let s = Scratch::new("baseline");
    store(&s.db(), &[Row::new(OLD), Row::new(NEW)]);

    // `OLD` was in the store when this pane was spawned; `NEW` was not. Both
    // sit in the same directory, on the same project — the baseline is the
    // only thing that separates them.
    let got = opencodedb::identify_session(&s.db(), CWD, &ids(&[OLD]), &none()).unwrap();
    assert_eq!(got, Identified::One(NEW.to_string()));
}

#[test]
fn the_column_is_matched_across_the_separator_and_case_difference() {
    let s = Scratch::new("norm");
    store(&s.db(), &[Row::new(NEW)]);

    // opencode writes `C:/Projects/loomux`; loomux holds `C:\Projects\loomux`.
    // A raw string compare matches nothing on Windows, so a pane would never
    // identify at all — the failure this normalization exists to prevent.
    assert_eq!(
        opencodedb::identify_session(&s.db(), CWD, &none(), &none()).unwrap(),
        Identified::One(NEW.to_string()),
        "a backslash cwd must match the forward-slash column"
    );
    assert_eq!(
        opencodedb::identify_session(&s.db(), r"c:\projects\LOOMUX\", &none(), &none()).unwrap(),
        Identified::One(NEW.to_string()),
        "case and a trailing separator must not decide a Windows path comparison"
    );
}

#[test]
fn a_pane_in_another_worktree_is_not_offered_this_ones_session() {
    let s = Scratch::new("dir");
    store(
        &s.db(),
        &[Row { directory: "C:/Projects/loomux-worktrees/agent/rev-163", ..Row::new(NEW) }],
    );

    // Every loomux worktree hashes to ONE project id, so the project cannot
    // separate panes and `directory` is the only thing that does. A pane in
    // the repo root must not be handed a worktree's session.
    assert_eq!(
        opencodedb::identify_session(&s.db(), CWD, &none(), &none()).unwrap(),
        Identified::None
    );
}

#[test]
fn a_subagent_session_is_never_adopted_as_the_panes_own() {
    let s = Scratch::new("subagent");
    // Both rows are new since the spawn, both in the pane's directory. The
    // subagent is a `session` row like any other and differs ONLY by
    // `parent_id`.
    store(&s.db(), &[Row::new(NEW), Row { parent: Some(NEW), ..Row::new(SUB) }]);

    // Not `Contested` either: a subagent is not an ambiguous candidate, it is
    // not a candidate. Binding a pane to its own subagent would make every
    // later read — usage, resume, digest — answer about the wrong
    // conversation, while looking perfectly healthy.
    assert_eq!(
        opencodedb::identify_session(&s.db(), CWD, &none(), &none()).unwrap(),
        Identified::One(NEW.to_string())
    );
}

#[test]
fn a_session_another_pane_already_took_is_not_a_candidate() {
    let s = Scratch::new("claimed");
    // Two panes in the SAME directory — the orchestrator and a reviewer both
    // run in the repo root — each with a session that appeared after this
    // pane's baseline was taken.
    store(&s.db(), &[Row::new(NEW), Row::new(OTHER)]);

    // Without the claim exclusion this is a contest and NEITHER pane
    // identifies; with it, the pane whose sibling already bound `OTHER`
    // resolves cleanly.
    assert_eq!(
        opencodedb::identify_session(&s.db(), CWD, &none(), &ids(&[OTHER])).unwrap(),
        Identified::One(NEW.to_string())
    );
}

#[test]
fn two_unclaimed_candidates_refuse_rather_than_bind_the_wrong_conversation() {
    let s = Scratch::new("contested");
    store(&s.db(), &[Row::new(NEW), Row { created: 1_785_703_400_000, ..Row::new(OTHER) }]);

    // Newest-wins would answer `OTHER` here, confidently and possibly wrongly.
    // The refusal is the point: a wrong bind reports one agent's spend as
    // another's and resumes a human into someone else's conversation, with
    // nothing to see. `doc/design/session-id-learning.md`'s ambiguity policy.
    assert_eq!(
        opencodedb::identify_session(&s.db(), CWD, &none(), &none()).unwrap(),
        Identified::Contested(2),
        "two candidates must refuse, and say how many were in contention"
    );
}

#[test]
fn a_pane_with_no_recorded_directory_never_widens_the_search() {
    let s = Scratch::new("nocwd");
    store(&s.db(), &[Row::new(NEW)]);

    // "I don't know where this pane is" must not become "match anything" —
    // an empty key would otherwise be a wildcard against every row.
    assert_eq!(
        opencodedb::identify_session(&s.db(), "", &none(), &none()).unwrap(),
        Identified::None
    );
}

#[test]
fn the_baseline_is_every_id_in_the_store_including_subagents() {
    let s = Scratch::new("ids");
    store(&s.db(), &[Row::new(OLD), Row { parent: Some(OLD), ..Row::new(SUB) }]);

    assert_eq!(
        opencodedb::session_ids(&s.db()).unwrap(),
        ids(&[OLD, SUB]),
        "a baseline is what was already here; a row missing from it must mean 'new'"
    );
}

#[test]
fn an_absent_store_is_an_empty_baseline_not_a_failure() {
    let s = Scratch::new("absent");
    // The ordinary state of the first opencode pane in a group: `OPENCODE_DB`
    // names a file opencode has not created yet.
    assert_eq!(opencodedb::session_ids(&s.db()), Err(Unavailable::Absent));
    assert_eq!(opencodedb::identify_session(&s.db(), CWD, &none(), &none()), Err(Unavailable::Absent));
}

#[test]
fn a_drifted_schema_degrades_instead_of_panicking() {
    let s = Scratch::new("drift");
    std::fs::create_dir_all(s.db().parent().unwrap()).unwrap();
    let conn = Connection::open(s.db()).unwrap();
    // A real database, a `session` table — without the columns this reads.
    // The vendor promises nothing about this schema, so drift is a when.
    conn.execute_batch("CREATE TABLE session (id text PRIMARY KEY)").unwrap();
    drop(conn);

    match opencodedb::identify_session(&s.db(), CWD, &none(), &none()) {
        Err(Unavailable::Query(_)) => {}
        other => panic!("schema drift must degrade to Unavailable::Query, got {other:?}"),
    }
}

#[test]
fn a_sessions_recorded_directory_is_readable_for_resume() {
    let s = Scratch::new("dirread");
    store(&s.db(), &[Row { directory: "C:/Projects/loomux-worktrees/feat/x", ..Row::new(NEW) }]);

    assert_eq!(
        opencodedb::session_directory(&s.db(), NEW).unwrap().as_deref(),
        Some("C:/Projects/loomux-worktrees/feat/x")
    );
    assert_eq!(
        opencodedb::session_directory(&s.db(), OTHER).unwrap(),
        None,
        "a readable store with no such session is None, not an error"
    );
}

// ---------------------------------------------------------------------------
// 2. The wiring
// ---------------------------------------------------------------------------

fn rails(cli: &str) -> Guardrails {
    Guardrails {
        max_agents: 4,
        agent_cli: cli.into(),
        blocks: workflow::default_roster(&[
            (Role::Orchestrator, "", ""),
            (Role::Worker, "", ""),
            (Role::Reviewer, "", ""),
            (Role::Planner, "", ""),
        ]),
        auto_ops: false,
        idle_kill_minutes: 0,
        max_spawns_per_hour: 0,
        watchdog_stall_minutes: 0,
        ..Guardrails::default()
    }
}

fn test_registry() -> (OrchRegistry, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let reg = OrchRegistry::new(dir.path().to_path_buf());
    reg.set_port(45996);
    reg.set_claude_projects_dir(dir.path().join("claude-projects"));
    reg.set_claude_agents_dir_override(dir.path().join("claude-agents"));
    reg.set_copilot_agents_dir_override(dir.path().join("copilot-agents"));
    reg.set_compact_hook_dir_override(dir.path().join("compacthook"));
    reg.set_copilot_hooks_dir_override(dir.path().join("copilot-hooks"));
    (reg, dir)
}

#[test]
fn only_the_clis_that_mint_their_own_id_get_a_baseline() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/opencode-repo", rails("opencode")).unwrap();

    // Claude is handed its id up front (`--session-id`), so there is nothing
    // to learn and no watcher to run; gemini has no store loomux reads.
    // Watching either would be a thread that can only ever time out.
    assert!(reg.capture_session_baseline("claude", &g.id).is_none());
    assert!(reg.capture_session_baseline("gemini", &g.id).is_none());
    assert!(matches!(
        reg.capture_session_baseline("opencode", &g.id),
        Some(SessionBaseline::OpenCode { .. })
    ));
}

#[test]
fn a_store_that_cannot_be_snapshotted_refuses_to_watch_at_all() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/opencode-repo", rails("opencode")).unwrap();

    // Not a missing store (that is an empty baseline, above) — a file that is
    // there and unreadable as a database. Treating THAT as "the store held
    // nothing" would make every session already in it a candidate for this
    // pane, which is how one pane ends up bound to another's conversation.
    let db = reg.opencode_db_path(&g.id);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    std::fs::write(&db, b"this is not a database").unwrap();

    assert!(
        reg.capture_session_baseline("opencode", &g.id).is_none(),
        "an unreadable baseline must refuse to watch, never degrade into an empty one"
    );
}

#[test]
fn a_panes_search_excludes_the_sessions_its_group_siblings_hold() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/opencode-repo", rails("opencode")).unwrap();
    // Two panes with NO worktree, so both run in the group's repo — the
    // orchestrator/reviewer shape that makes a shared store ambiguous.
    let a = reg.spawn_agent(&g.id, Role::Worker, "a", "t", false, None).unwrap();
    let b = reg.spawn_agent(&g.id, Role::Reviewer, "b", "t", false, None).unwrap();

    let cwd = a.cwd.clone();
    let dir = cwd.replace('\\', "/");
    store(
        &reg.opencode_db_path(&g.id),
        &[Row { directory: &dir, ..Row::new(NEW) }, Row { directory: &dir, ..Row::new(OTHER) }],
    );
    let baseline = SessionBaseline::OpenCode { ids: none() };

    // Nothing bound yet: two candidates in one directory, so neither pane may
    // pick — and the count travels so the timeout can say why.
    assert_eq!(
        reg.search_for_session(&g.id, &a.id, &cwd, &baseline),
        SessionSearch::Contested(2)
    );

    // Once `b` has taken one, `a`'s answer is unambiguous. This is the claim
    // exclusion doing the work no baseline could: both sessions appeared after
    // both baselines were taken.
    reg.associate_session(&g.id, &b.id, OTHER);
    assert_eq!(
        reg.search_for_session(&g.id, &a.id, &cwd, &baseline),
        SessionSearch::Found(NEW.to_string())
    );
}

#[test]
fn a_learned_session_reaches_the_roster_and_the_usage_key() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/opencode-repo", rails("opencode")).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "t", false, None).unwrap();
    assert!(w.session_id.is_none(), "a fresh opencode pane starts with no id");

    reg.associate_session(&g.id, &w.id, NEW);

    let bound = reg.agent(&w.id).expect("agent");
    assert_eq!(bound.session_id.as_deref(), Some(NEW));
    // The point of learning it: #812's usage arm keys on this id, and keyed
    // usage is what stops an opencode agent reading as having spent nothing.
    assert_eq!(reg.compute_usage_snapshot(&bound, "opencode").key, NEW);
}

#[test]
fn an_absent_store_leaves_the_watcher_waiting_rather_than_calling_it_unreadable() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/opencode-repo", rails("opencode")).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "t", false, None).unwrap();

    // The first seconds of every opencode pane's life. `Waiting` and not
    // `Unreadable`, because the difference is what the timeout audit says —
    // "the CLI never wrote a session" versus "your store is broken".
    assert_eq!(
        reg.search_for_session(&g.id, &w.id, &w.cwd, &SessionBaseline::OpenCode { ids: none() }),
        SessionSearch::Waiting
    );
}

// ---------------------------------------------------------------------------
// 3. What an opencode pane could not do at all
// ---------------------------------------------------------------------------

#[test]
fn an_opencode_session_id_is_accepted_as_a_resume_id() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/opencode-repo", rails("opencode")).unwrap();
    let dir = tempfile::tempdir().unwrap();

    // `ses_` + 12 hex + 14 base62. The old validator took hex digits and `-`
    // only — exactly a claude UUID — so this failed as "invalid resume session
    // id" with nothing wrong with the id.
    let w = reg
        .spawn_agent_ex(
            &g.id,
            Role::Worker,
            None,
            "w",
            "follow-up",
            false,
            None,
            None,
            Some(NEW.to_string()),
            Some(dir.path().to_string_lossy().to_string()),
            None,
        )
        .expect("an opencode session id must be a resumable id");
    assert_eq!(
        w.session_id.as_deref(),
        Some(NEW),
        "the id must survive validation intact — a mangled one would resume the wrong session"
    );
}

#[test]
fn a_session_id_that_could_escape_a_path_or_a_command_line_is_still_refused() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/opencode-repo", rails("opencode")).unwrap();
    let dir = tempfile::tempdir().unwrap();

    // The widening admitted two characters, not "letters and symbols": a
    // session id still reaches a `Path::join` (`read_session_transcript_events`)
    // and a shell command line, so every separator, dot, quote, space and
    // metacharacter must still bounce.
    for bad in [
        "../../etc/passwd",
        r"..\..\windows",
        "ses_ok/../evil",
        "ses ok",
        "ses_ok;rm -rf /",
        "ses_ok$(id)",
        "ses_ok\"quoted\"",
        ".",
        "..",
    ] {
        let r = reg.spawn_agent_ex(
            &g.id,
            Role::Worker,
            None,
            "w",
            "t",
            false,
            None,
            None,
            Some(bad.to_string()),
            Some(dir.path().to_string_lossy().to_string()),
            None,
        );
        assert!(r.is_err(), "{bad:?} must be refused as a resume session id, got {r:?}");
    }
}

#[test]
fn an_opencode_resume_reads_its_own_store_and_not_claudes() {
    let s = Scratch::new("router");
    store(&s.db(), &[Row { directory: "C:/Projects/loomux-worktrees/feat/x", ..Row::new(NEW) }]);

    // Before this slice, `find_session_cwd`'s `_` arm sent every CLI it does
    // not name at ~/.claude/projects — so an opencode resume searched claude's
    // store, found nothing, and hard-failed with "not found in the opencode
    // session history on this machine". An opencode group could not be
    // reopened at all.
    assert_eq!(
        orchestration::session_cwd_in_store("opencode", NEW, Some(&s.db())).unwrap().as_deref(),
        Some("C:/Projects/loomux-worktrees/feat/x"),
    );
    assert_eq!(
        orchestration::session_cwd_in_store("opencode", OTHER, Some(&s.db())).unwrap(),
        None,
        "a session this store has never held is not found — not an error to escalate"
    );

    // A store that was never created is "no such session" too, not a failure
    // the caller should be told to go investigate.
    let empty = Scratch::new("router-absent");
    assert_eq!(
        orchestration::session_cwd_in_store("opencode", NEW, Some(&empty.db())).unwrap(),
        None
    );
}

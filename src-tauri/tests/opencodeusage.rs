//! OpenCode usage/cost readback (#722 slice B): the read-only SQLite reader
//! (`opencodedb`), the mapping onto loomux's four token buckets
//! (`usage::opencode_session_usage`), and the `compute_usage_snapshot` arm
//! that decides an opencode agent's usage comes from that store.
//!
//! An integration test, not inline `#[cfg(test)]`, per repo constraint #4 — a
//! unit-test binary linking the full lib misses the comctl32-v6 manifest
//! `build.rs` only embeds for integration-test targets.
//!
//! **No opencode is ever run** (constraint 3). Every fixture store below is
//! built here from the DDL recorded on issue #722 (slice-V memo §1b), read off
//! `anomalyco/opencode@f67e80c2` (tag `v1.18.11`) and cross-checked against a
//! real store on the maintainer's machine. `session_ddl()` is that DDL: if
//! OpenCode ever changes the schema, these tests keep passing against a shape
//! that no longer exists — which is exactly why the production reader treats
//! drift as a degrade rather than trusting the columns to be there.

use loomux_lib::opencodedb::{self, SessionTotals, Unavailable};
use loomux_lib::orchestration::{workflow, Guardrails, OrchRegistry, Role};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// OpenCode's `session` table, verbatim from the recorded DDL (columns loomux
/// never reads included on purpose — a fixture narrowed to the six columns
/// under test would stop being evidence that the query works against the real
/// table).
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

/// One `session` row's worth of the fields these tests vary.
struct Row<'a> {
    id: &'a str,
    parent: Option<&'a str>,
    directory: &'a str,
    model: Option<&'a str>,
    cost: f64,
    input: i64,
    output: i64,
    reasoning: i64,
    cache_read: i64,
    cache_write: i64,
}

impl<'a> Row<'a> {
    fn new(id: &'a str) -> Self {
        Row {
            id,
            parent: None,
            directory: "C:/Projects/loomux",
            model: Some("deepseek-v4-flash-free"),
            cost: 0.0,
            input: 0,
            output: 0,
            reasoning: 0,
            cache_read: 0,
            cache_write: 0,
        }
    }
}

fn insert(conn: &Connection, r: &Row) {
    conn.execute(
        "INSERT INTO session (id, project_id, parent_id, slug, directory, title, version,
                              time_created, time_updated, model, cost,
                              tokens_input, tokens_output, tokens_reasoning,
                              tokens_cache_read, tokens_cache_write)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        rusqlite::params![
            r.id,
            // sha1("git-remote:" + this repo's pre-#1153-rename GitHub remote)
            // — the real project id for this repo, verified in the slice-V
            // memo. Every loomux worktree shares it; `directory` is what
            // separates panes. Frozen at the pre-rename value rather than
            // recomputed for the new slug: constraint 3 bans running opencode
            // live to re-verify it, and the id only needs to be consistent
            // across worktree rows here, not equal to today's real hash.
            "f9dd9fcdf18a51fa9de041f787210d1ce5e0d1e7",
            r.parent,
            "loomux",
            r.directory,
            "a title",
            "1.18.11",
            1_785_703_307_950i64,
            1_785_703_341_088i64,
            r.model,
            r.cost,
            r.input,
            r.output,
            r.reasoning,
            r.cache_read,
            r.cache_write,
        ],
    )
    .unwrap();
}

/// A store at `path` holding `rows`. WAL, because that is the journal mode
/// opencode opens its database in (`SOURCE`, `database/database.ts`).
fn store(path: &Path, rows: &[Row]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    // `PRAGMA journal_mode` answers with a row, so it is a query, not an
    // `execute`.
    conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0)).unwrap();
    conn.execute_batch(session_ddl()).unwrap();
    for r in rows {
        insert(&conn, r);
    }
}

/// A disposable directory for one test's store, cleaned up on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir()
            .join(format!("opencode-usage-{tag}-{}", std::process::id()));
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

const SES: &str = "ses_03bd2d53dffeiBvu9PvuCPjxT7";
const SUB: &str = "ses_15089ff54ffeQ2mSoRBFxdH2mS";
const DEEP: &str = "ses_1508a391dffext5Xb0UUF2UDjk";

// ---------------------------------------------------------------------------
// The reader
// ---------------------------------------------------------------------------

#[test]
fn a_session_row_yields_its_cost_and_all_five_token_counters() {
    let s = Scratch::new("five");
    // The token split of a real assistant turn recorded in the slice-V memo,
    // with a non-zero cost and cache-write added so every counter is
    // distinguishable from every other — a reader that transposed two columns
    // could not pass this.
    store(
        &s.db(),
        &[Row {
            cost: 0.4275,
            input: 15_260,
            output: 1_115,
            reasoning: 1_193,
            cache_read: 63_104,
            cache_write: 512,
            ..Row::new(SES)
        }],
    );

    let got = opencodedb::session_usage(&s.db(), SES).unwrap().expect("session row");
    assert_eq!(
        got,
        SessionTotals {
            cost_usd: 0.4275,
            input: 15_260,
            output: 1_115,
            reasoning: 1_193,
            cache_read: 63_104,
            cache_write: 512,
            model: Some("deepseek-v4-flash-free".into()),
        }
    );
}

#[test]
fn a_panes_usage_includes_the_subagent_sessions_it_spawned() {
    let s = Scratch::new("subagents");
    store(
        &s.db(),
        &[
            Row { input: 100, output: 10, ..Row::new(SES) },
            // A subagent of the pane's session, and a subagent of THAT — the
            // recursion has to reach both, not just one level.
            Row { parent: Some(SES), input: 200, output: 20, ..Row::new(SUB) },
            Row { parent: Some(SUB), input: 400, output: 40, ..Row::new(DEEP) },
            // A different pane's session in the same store, on the same
            // project. Its spend is not this pane's.
            Row {
                directory: "C:/Projects/loomux-worktrees/agent/rev-163",
                input: 9_000,
                output: 900,
                ..Row::new("ses_0000000000000000000000000a")
            },
        ],
    );

    let got = opencodedb::session_usage(&s.db(), SES).unwrap().expect("session row");
    assert_eq!(got.input, 700, "root + subagent + sub-subagent input: {got:?}");
    assert_eq!(got.output, 70, "root + subagent + sub-subagent output: {got:?}");

    // …and asked about the subagent alone, only the subtree below it.
    let sub = opencodedb::session_usage(&s.db(), SUB).unwrap().expect("subagent row");
    assert_eq!(sub.input, 600, "subagent + its own child, never its parent: {sub:?}");
}

#[test]
fn an_unknown_session_is_no_usage_rather_than_a_zero_row() {
    let s = Scratch::new("unknown");
    store(&s.db(), &[Row { input: 5, ..Row::new(SES) }]);

    // Ok(None), NOT Ok(Some(all zeroes)) — the caller has to be able to tell
    // "this pane has spent nothing yet" from "there is no such session", or a
    // mistyped id would read as a free agent.
    assert_eq!(opencodedb::session_usage(&s.db(), "ses_nope").unwrap(), None);
}

#[test]
fn an_absent_store_is_a_zero_usage_agent_not_a_failure() {
    let s = Scratch::new("absent");
    // Nothing written: the normal state of a group whose opencode panes have
    // never booted.
    assert_eq!(opencodedb::session_usage(&s.db(), SES), Err(Unavailable::Absent));
}

#[test]
fn a_schema_missing_the_token_columns_degrades_instead_of_panicking() {
    let s = Scratch::new("drift");
    std::fs::create_dir_all(s.db().parent().unwrap()).unwrap();
    let conn = Connection::open(s.db()).unwrap();
    // A plausible future (or past) opencode: `session` still exists, still has
    // `id` and `model`, but the counters have moved elsewhere.
    conn.execute_batch("CREATE TABLE session (id text PRIMARY KEY, parent_id text, model text)")
        .unwrap();
    conn.execute("INSERT INTO session (id, model) VALUES (?1, 'm')", [SES]).unwrap();
    drop(conn);

    match opencodedb::session_usage(&s.db(), SES) {
        Err(Unavailable::Query(e)) => {
            // Same trap as rev-298's F3, at a second site: `e` is SQLite's
            // diagnosis followed by an echo of the failing statement, and the
            // echo names every column the query mentions — so the old
            // `contains("tokens_input")` disjunct matched the echo rather than
            // the report, and would have held even against an empty diagnosis.
            // Pin the diagnosis itself, which is everything before the echo.
            let diagnosis = e.split(" in ").next().unwrap_or_default();
            assert_eq!(
                diagnosis, "no such column: s.cost",
                "the degrade has to carry which column is missing, not merely \
                 quote the query back: {e}"
            );
        }
        other => panic!("schema drift must degrade to Unavailable::Query, got {other:?}"),
    }
}

#[test]
fn a_file_that_is_not_a_database_degrades_and_is_never_mistaken_for_an_absent_one() {
    let s = Scratch::new("garbage");
    std::fs::create_dir_all(s.db().parent().unwrap()).unwrap();
    std::fs::write(s.db(), b"this is not a SQLite database, it is a sentence").unwrap();

    let err = opencodedb::session_usage(&s.db(), SES).expect_err("must not be read as usage");
    assert_ne!(
        err,
        Unavailable::Absent,
        "a corrupt store and a missing one are different problems; conflating them would \
         report the corrupt one as the ordinary never-booted case"
    );
}

#[test]
fn a_writer_mid_transaction_neither_blocks_the_reader_nor_hides_committed_spend() {
    let s = Scratch::new("wal");
    store(&s.db(), &[Row { input: 100, ..Row::new(SES) }]);

    // A live opencode, holding a write transaction open the way it does
    // between turns.
    let writer = Connection::open(s.db()).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    writer.execute("UPDATE session SET tokens_input = 999 WHERE id = ?1", [SES]).unwrap();

    // The read completes — it does not wait for the writer, and it does not
    // see the uncommitted number.
    let during = opencodedb::session_usage(&s.db(), SES).unwrap().expect("row");
    assert_eq!(during.input, 100, "a reader must not see an uncommitted write");

    writer.execute_batch("COMMIT").unwrap();

    // …and once committed, the reader sees it. This is the assertion that
    // fails if the primary open is ever switched to `immutable=1`: an
    // immutable connection ignores the WAL, so it would still read 100 —
    // silently under-reporting every live agent's spend.
    let after = opencodedb::session_usage(&s.db(), SES).unwrap().expect("row");
    assert_eq!(after.input, 999, "a committed write must be visible through the WAL");
}

#[test]
fn the_immutable_fallback_uri_is_one_this_platforms_sqlite_actually_opens() {
    let s = Scratch::new("immutable");
    store(&s.db(), &[Row { input: 77, ..Row::new(SES) }]);
    // Checkpoint and drop the WAL, which is the state the fallback exists for
    // (a store no live writer is holding).
    {
        let c = Connection::open(s.db()).unwrap();
        c.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get::<_, String>(0)).unwrap();
    }

    let conn = opencodedb::open_immutable(&s.db())
        .expect("the immutable fallback must open a plain store on this platform");
    let got = opencodedb::session_usage_on(&conn, SES).unwrap().expect("row");
    assert_eq!(got.input, 77);
}

#[test]
fn the_immutable_uri_encodes_the_characters_sqlites_parser_would_otherwise_eat() {
    // Forward slashes (SQLite's URI parser does not treat `\` as a separator),
    // and the three characters that would truncate or re-decode the path.
    let uri = opencodedb::immutable_uri(Path::new(r"C:\Users\a b\g#1\q?x\100%\opencode.db"));
    assert_eq!(uri, "file:C:/Users/a b/g%231/q%3fx/100%25/opencode.db?immutable=1");
}

#[test]
fn a_nonsense_cost_or_counter_cannot_poison_the_groups_totals() {
    let s = Scratch::new("nonsense");
    store(
        &s.db(),
        &[Row { cost: f64::INFINITY, input: -5, output: 3, ..Row::new(SES) }],
    );

    let got = opencodedb::session_usage(&s.db(), SES).unwrap().expect("row");
    assert_eq!(got.cost_usd, 0.0, "a non-finite cost is dropped, not summed into a group total");
    assert_eq!(got.input, 0, "a negative counter clamps to zero rather than wrapping to 2^64-5");
    assert_eq!(got.output, 3, "…and the counters beside it are untouched");
}

// ---------------------------------------------------------------------------
// The mapping onto loomux's four buckets
// ---------------------------------------------------------------------------

#[test]
fn reasoning_tokens_are_folded_into_output_and_cache_write_into_cache_creation() {
    let s = Scratch::new("mapping");
    store(
        &s.db(),
        &[Row {
            cost: 1.25,
            input: 15_260,
            output: 1_115,
            reasoning: 1_193,
            cache_read: 63_104,
            cache_write: 512,
            ..Row::new(SES)
        }],
    );

    let u = loomux_lib::usage::opencode_session_usage(&s.db(), SES).unwrap().expect("row");
    assert_eq!(u.tokens.input_tokens, 15_260);
    assert_eq!(
        u.tokens.output_tokens,
        1_115 + 1_193,
        "reasoning tokens must land in `output`; dropping them here would have \
         under-reported this session by more than its entire output"
    );
    assert_eq!(u.tokens.cache_creation_tokens, 512, "opencode's cache_write is a cache creation");
    assert_eq!(u.tokens.cache_read_tokens, 63_104);
    // Every counted token reaches exactly one bucket: OpenCode's own
    // `tokens.total` is the sum of its five, so loomux's total must equal it.
    assert_eq!(u.tokens.total(), 15_260 + 1_115 + 1_193 + 63_104 + 512);
    assert_eq!(u.cost_usd, Some(1.25), "opencode's own dollars pass through unpriced");
    assert_eq!(u.model.as_deref(), Some("deepseek-v4-flash-free"));
}

#[test]
fn a_free_models_zero_cost_is_reported_as_zero_not_as_unknown() {
    let s = Scratch::new("free");
    store(&s.db(), &[Row { cost: 0.0, input: 15_260, output: 1_115, ..Row::new(SES) }]);

    let u = loomux_lib::usage::opencode_session_usage(&s.db(), SES).unwrap().expect("row");
    assert_eq!(
        u.cost_usd,
        Some(0.0),
        "a free model genuinely costs $0.00 — reporting `None` would read as \
         'no cost figure available' and hide a real, known answer"
    );
}

// ---------------------------------------------------------------------------
// The wiring: which source an opencode agent's usage comes from
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
    reg.set_port(45997);
    // The claude arm of `compute_usage_snapshot` reads a transcript root, and
    // `the_opencode_arm_never_fires_for_another_cli` drives it deliberately —
    // point it at this disposable tree so no test here ever reads the real
    // `~/.claude/projects`.
    reg.set_claude_projects_dir(dir.path().join("claude-projects"));
    reg.set_claude_agents_dir_override(dir.path().join("claude-agents"));
    reg.set_copilot_agents_dir_override(dir.path().join("copilot-agents"));
    reg.set_compact_hook_dir_override(dir.path().join("compacthook"));
    reg.set_copilot_hooks_dir_override(dir.path().join("copilot-hooks"));
    (reg, dir)
}

#[test]
fn an_opencode_agents_usage_comes_from_the_group_store_and_is_reported_not_estimated() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/opencode-repo", rails("opencode")).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "task", false, None).unwrap();

    // Session identification is slice C's; until it lands nothing sets this,
    // so the test supplies the id the way that slice will.
    let mut entry = w.clone();
    entry.session_id = Some(SES.to_string());

    // The store goes exactly where the spawn points `OPENCODE_DB` — the same
    // function, so this cannot pass against a path the pane never writes.
    store(
        &reg.opencode_db_path(&g.id),
        &[Row {
            cost: 0.4275,
            input: 15_260,
            output: 1_115,
            reasoning: 1_193,
            cache_read: 63_104,
            cache_write: 512,
            ..Row::new(SES)
        }],
    );

    let snap = reg.compute_usage_snapshot(&entry, "opencode");
    assert_eq!(snap.source, "session-db", "usage must come from the store, not the statusline");
    assert_eq!(snap.input_tokens, 15_260);
    assert_eq!(snap.output_tokens, 1_115 + 1_193);
    assert_eq!(snap.cache_creation_tokens, 512);
    assert_eq!(snap.cache_read_tokens, 63_104);
    assert_eq!(snap.cost_usd, Some(0.4275));
    assert!(
        !snap.estimated,
        "opencode priced this itself — labelling it `estimated` would blend a reported \
         figure into a total the UI calls a price-table guess"
    );
    assert_eq!(snap.model.as_deref(), Some("deepseek-v4-flash-free"));
}

#[test]
fn an_opencode_agent_whose_session_is_not_identified_yet_reports_no_usage() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/opencode-repo", rails("opencode")).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "task", false, None).unwrap();
    // A store with real spend in it — but nothing ties this agent to that
    // session, and guessing (newest row, only row, …) would attribute another
    // pane's spend to this one.
    store(&reg.opencode_db_path(&g.id), &[Row { input: 15_260, ..Row::new(SES) }]);

    assert!(w.session_id.is_none(), "opencode mints its own session id after boot");
    let snap = reg.compute_usage_snapshot(&w, "opencode");
    assert_eq!(snap.source, "none");
    assert_eq!(snap.input_tokens, 0);
}

#[test]
fn a_missing_store_leaves_an_opencode_agent_at_zero_rather_than_wedging() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/opencode-repo", rails("opencode")).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "task", false, None).unwrap();
    let mut entry = w.clone();
    entry.session_id = Some(SES.to_string());
    // No store written at all.

    let snap = reg.compute_usage_snapshot(&entry, "opencode");
    assert_eq!(snap.source, "none");
    assert_eq!(snap.cost_usd, None);
}

#[test]
fn the_opencode_arm_never_fires_for_another_cli() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/opencode-repo", rails("claude")).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "task", false, None).unwrap();
    let mut entry = w.clone();
    entry.session_id = Some(SES.to_string());
    // An opencode store sitting in the group dir — a mixed-CLI group is the
    // normal case, so a claude agent must not be charged out of it just
    // because the ids happen to line up.
    store(&reg.opencode_db_path(&g.id), &[Row { input: 15_260, ..Row::new(SES) }]);

    let snap = reg.compute_usage_snapshot(&entry, "claude");
    assert_ne!(snap.source, "session-db");
    assert_eq!(snap.input_tokens, 0);
}

// ---------------------------------------------------------------------------
// Diagnosability: one line per degrade episode (rev-298 F2)
// ---------------------------------------------------------------------------

/// Audit entries a group recorded under `action`.
fn audit_entries(reg: &OrchRegistry, group: &str, action: &str) -> Vec<serde_json::Value> {
    std::fs::read_to_string(reg.state_root().join(group).join("audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|e| e["action"] == action)
        .collect()
}

/// Write a `session` table that has an `id` but none of the token columns —
/// the shape a schema drift would leave behind.
fn drifted_store(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("CREATE TABLE session (id text PRIMARY KEY, parent_id text, model text)")
        .unwrap();
    conn.execute("INSERT INTO session (id, model) VALUES (?1, 'm')", [SES]).unwrap();
}

#[test]
fn a_store_that_cannot_be_read_is_diagnosed_once_not_once_per_poll() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/opencode-repo", rails("opencode")).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "task", false, None).unwrap();
    let mut entry = w.clone();
    entry.session_id = Some(SES.to_string());
    drifted_store(&reg.opencode_db_path(&g.id));

    // The group view polls `group_usage` every couple of seconds; ten ticks is
    // a twenty-second glance at a broken store.
    for _ in 0..10 {
        let snap = reg.compute_usage_snapshot(&entry, "opencode");
        assert_eq!(snap.source, "none", "a drifted store still degrades to no usage");
    }

    let lines = audit_entries(&reg, &g.id, "opencode-usage-degraded");
    assert_eq!(
        lines.len(),
        1,
        "one line per episode, not per poll — an unlatched audit floods the log \
         exactly when something is wrong with it: {lines:?}"
    );
    assert_eq!(lines[0]["detail"]["kind"], "unreadable");

    // SQLite's message is `no such column: <col> in <the whole statement> at
    // offset N` — a DIAGNOSIS followed by an echo of the failing SQL. The echo
    // names every column the query mentions, so a `contains(…)` against the
    // whole string is satisfied by the echo alone and would still pass if the
    // diagnosis were empty (rev-298 F3: this assertion used to look for
    // `tokens_input`, which appears ONLY in the echo — the column SQLite
    // actually reports missing here is `s.cost`, the rollup's first reference
    // to a column the drifted table lacks).
    //
    // So pin the diagnosis, which is everything before the echo. Matching it
    // exactly is safe precisely because SQLite is `bundled`: the message text
    // comes from a C library whose version this repo pins itself, not from
    // whatever the operator has installed.
    let detail = lines[0]["detail"]["detail"].as_str().unwrap_or_default();
    let diagnosis = detail.split(" in ").next().unwrap_or_default();
    assert_eq!(
        diagnosis, "opencode database schema drift: no such column: s.cost",
        "the line has to carry WHICH column the store is missing; an assertion \
         that matches the echoed SQL instead would pass on any error at all — \
         full detail: {detail:?}"
    );
}

#[test]
fn an_absent_store_is_never_audited_at_all() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/opencode-repo", rails("opencode")).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "task", false, None).unwrap();
    let mut entry = w.clone();
    entry.session_id = Some(SES.to_string());
    // No store: an opencode pane that has not booted yet, which is ordinary.

    for _ in 0..3 {
        reg.compute_usage_snapshot(&entry, "opencode");
    }
    assert!(
        audit_entries(&reg, &g.id, "opencode-usage-degraded").is_empty(),
        "a group whose panes have not booted has no incident to report; auditing \
         it would put a line in every group that ever spawns an opencode pane"
    );
}

#[test]
fn a_degrade_that_recurs_after_a_recovery_is_diagnosed_again() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/opencode-repo", rails("opencode")).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "task", false, None).unwrap();
    let mut entry = w.clone();
    entry.session_id = Some(SES.to_string());
    let db = reg.opencode_db_path(&g.id);

    drifted_store(&db);
    reg.compute_usage_snapshot(&entry, "opencode");
    reg.compute_usage_snapshot(&entry, "opencode");

    // The operator upgrades opencode and the store is whole again.
    std::fs::remove_file(&db).unwrap();
    store(&db, &[Row { input: 15_260, ..Row::new(SES) }]);
    let ok = reg.compute_usage_snapshot(&entry, "opencode");
    assert_eq!(ok.source, "session-db", "the recovered store must actually read");

    // …and then drifts again. A latch that never clears would swallow this,
    // leaving the second incident invisible for the life of the process.
    std::fs::remove_file(&db).unwrap();
    drifted_store(&db);
    reg.compute_usage_snapshot(&entry, "opencode");

    assert_eq!(
        audit_entries(&reg, &g.id, "opencode-usage-degraded").len(),
        2,
        "a successful read ends the episode, so a genuine recurrence is a new incident"
    );
}

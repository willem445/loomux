//! OpenCode transcript/digest readback (#722 slice B2): the `message`/`part`
//! read (`opencodedb::session_transcript`), the normalizer that turns those
//! rows into digest events (`digest::parse_opencode_transcript_events`), and
//! the `read_session_transcript_events` arm that wires an opencode agent's
//! `session_digest` to its group's store.
//!
//! An integration test, not inline `#[cfg(test)]`, per repo constraint #4 — a
//! unit-test binary linking the full lib misses the comctl32-v6 manifest
//! `build.rs` only embeds for integration-test targets. The pure normalizer's
//! own fixtures live inline in `digest.rs` beside the claude/copilot ones;
//! what needs a real store and a real registry lives here.
//!
//! **No opencode is ever run** (constraint 3). Every fixture store below is
//! built here from the DDL recorded on issue #722 (slice-V memo §1b) and from
//! `packages/core/src/session/sql.ts` at `anomalyco/opencode@f67e80c2` (tag
//! `v1.18.11`); the JSON in `message.data`/`part.data` is the shape
//! `packages/schema/src/v1/session.ts` defines at that same pin.

use loomux_lib::orchestration::digest::FrictionSignature;
use loomux_lib::orchestration::{workflow, DigestLookup, Guardrails, OrchRegistry, Role};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;

/// A real opencode session id (`ses_` + 12 hex + 14 base62, slice-V §1c).
const SES: &str = "ses_03bd2d53dffeiBvu9PvuCPjxT7";

// ---------------------------------------------------------------------------
// Fixture store
// ---------------------------------------------------------------------------

/// The three tables a transcript read touches, verbatim from the recorded
/// schema — `session` included even though `session_transcript` never joins
/// it, because a store without it is not a shape opencode ever writes and a
/// fixture that drifts from the real store stops being evidence.
fn ddl() -> [&'static str; 3] {
    [
        "CREATE TABLE session (
            id text PRIMARY KEY, project_id text NOT NULL, parent_id text,
            slug text NOT NULL, directory text NOT NULL, title text NOT NULL,
            version text NOT NULL, time_created integer NOT NULL,
            time_updated integer NOT NULL, agent text, model text,
            cost real DEFAULT 0 NOT NULL,
            tokens_input integer DEFAULT 0 NOT NULL,
            tokens_output integer DEFAULT 0 NOT NULL,
            tokens_reasoning integer DEFAULT 0 NOT NULL,
            tokens_cache_read integer DEFAULT 0 NOT NULL,
            tokens_cache_write integer DEFAULT 0 NOT NULL)",
        "CREATE TABLE message (
            id text PRIMARY KEY, session_id text NOT NULL,
            time_created integer NOT NULL, time_updated integer NOT NULL,
            data text NOT NULL)",
        "CREATE TABLE part (
            id text PRIMARY KEY, message_id text NOT NULL, session_id text NOT NULL,
            time_created integer NOT NULL, time_updated integer NOT NULL,
            data text NOT NULL)",
    ]
}

/// One message and its parts, in the order they were written.
struct Msg<'a> {
    id: &'a str,
    role: &'a str,
    time: i64,
    parts: Vec<Value>,
}

fn store(db: &Path, session: &str, messages: &[Msg]) {
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = Connection::open(db).unwrap();
    for stmt in ddl() {
        conn.execute(stmt, []).unwrap();
    }
    conn.execute(
        "INSERT INTO session (id, project_id, parent_id, slug, directory, title, version,
                              time_created, time_updated, agent, model)
         VALUES (?1, 'f9dd9fcd', NULL, 'slug', 'C:/Projects/loomux', 'a session', '1.18.11',
                 1, 2, 'build', 'deepseek-v4-flash-free')",
        [session],
    )
    .unwrap();
    for m in messages {
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            rusqlite::params![
                m.id,
                session,
                m.time,
                json!({ "role": m.role, "agent": "build", "time": { "created": m.time } }).to_string()
            ],
        )
        .unwrap();
        for (i, p) in m.parts.iter().enumerate() {
            conn.execute(
                "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
                rusqlite::params![format!("prt_{}_{i:03}", m.id), m.id, session, m.time, p.to_string()],
            )
            .unwrap();
        }
    }
}

fn text(t: &str) -> Value {
    json!({ "type": "text", "text": t })
}

fn bash(call: &str, command: &str, output: &str) -> Value {
    json!({ "type": "tool", "callID": call, "tool": "bash",
            "state": { "status": "completed", "input": { "command": command },
                       "output": output, "title": command, "metadata": {},
                       "time": { "start": 1, "end": 2 } } })
}

fn bash_error(call: &str, command: &str, error: &str) -> Value {
    json!({ "type": "tool", "callID": call, "tool": "bash",
            "state": { "status": "error", "input": { "command": command },
                       "error": error, "time": { "start": 1, "end": 2 } } })
}

/// A session that hit one wall and recovered: a failed command, then the
/// substituted one that worked.
fn one_wall_session() -> Vec<Msg<'static>> {
    vec![
        Msg { id: "msg_001", role: "user", time: 1_785_703_300_000, parts: vec![text("fix the flaky test")] },
        Msg {
            id: "msg_002",
            role: "assistant",
            time: 1_785_703_301_000,
            parts: vec![text("running the suite"), bash_error("c1", "npm test", "npm: command not found")],
        },
        Msg {
            id: "msg_003",
            role: "assistant",
            time: 1_785_703_302_000,
            parts: vec![bash("c2", "pnpm test", "1 passing"), text("green")],
        },
    ]
}

// ---------------------------------------------------------------------------
// The reader
// ---------------------------------------------------------------------------

#[test]
fn the_transcript_read_returns_every_part_in_the_stores_own_order() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("opencode.db");
    store(&db, SES, &one_wall_session());

    let rows = loomux_lib::opencodedb::session_transcript(&db, SES).unwrap();
    assert_eq!(rows.len(), 5, "one text + two parts + two parts: {rows:?}");
    assert_eq!(
        rows.iter().map(|r| r.message_id.as_str()).collect::<Vec<_>>(),
        vec!["msg_001", "msg_002", "msg_002", "msg_003", "msg_003"],
        "messages by time_created, parts by id within a message"
    );
    assert_eq!(rows[0].time_created_ms, 1_785_703_300_000, "ms since the epoch, from the message row");
}

#[test]
fn another_sessions_messages_are_never_read_into_this_ones_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("opencode.db");
    store(&db, SES, &one_wall_session());
    // A second pane's session in the SAME group store — the ordinary case,
    // since `OPENCODE_DB` is per group and every pane in it writes here.
    let conn = Connection::open(&db).unwrap();
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data)
         VALUES ('msg_900', 'ses_other', 1, 1, ?1)",
        [json!({ "role": "user" }).to_string()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
         VALUES ('prt_900', 'msg_900', 'ses_other', 1, 1, ?1)",
        [text("another agent's prompt").to_string()],
    )
    .unwrap();
    drop(conn);

    let rows = loomux_lib::opencodedb::session_transcript(&db, SES).unwrap();
    assert_eq!(rows.len(), 5, "{rows:?}");
    assert!(
        !rows.iter().any(|r| r.part_json.contains("another agent's prompt")),
        "a group store holds every pane's session — the read must be scoped to one"
    );
}

#[test]
fn a_missing_store_is_a_named_degrade_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let err = loomux_lib::opencodedb::session_transcript(&dir.path().join("nope.db"), SES).unwrap_err();
    assert_eq!(err, loomux_lib::opencodedb::Unavailable::Absent);
}

#[test]
fn a_drifted_schema_degrades_rather_than_reading_the_wrong_columns() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("opencode.db");
    let conn = Connection::open(&db).unwrap();
    // A real database, but not this one: opencode's own migration spec
    // (`specs/storage/remove-opencode-db.md` at the pin) is live work, so the
    // tables moving under loomux is a real future, not a hypothetical.
    conn.execute("CREATE TABLE something_else (id text)", []).unwrap();
    drop(conn);
    assert!(matches!(
        loomux_lib::opencodedb::session_transcript(&db, SES),
        Err(loomux_lib::opencodedb::Unavailable::Query(_))
    ));
}

// ---------------------------------------------------------------------------
// The wiring: an opencode agent's session_digest
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
    reg.set_port(45998);
    // Nothing here may read the real `~/.claude` / `~/.copilot` trees.
    reg.set_claude_projects_dir(dir.path().join("claude-projects"));
    reg.set_claude_agents_dir_override(dir.path().join("claude-agents"));
    reg.set_copilot_agents_dir_override(dir.path().join("copilot-agents"));
    reg.set_compact_hook_dir_override(dir.path().join("compacthook"));
    reg.set_copilot_hooks_dir_override(dir.path().join("copilot-hooks"));
    (reg, dir)
}

#[test]
fn an_opencode_workers_digest_comes_from_its_groups_store() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/opencode-repo", rails("opencode")).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "fix the flaky test", false, None).unwrap();
    // The real binding path — slice C's association, not a hand-set field.
    assert!(reg.associate_session(&g.id, &w.id, SES), "the session must bind");

    // The store goes exactly where the spawn points `OPENCODE_DB`: the same
    // function, so this cannot pass against a path a pane never writes.
    store(&reg.opencode_db_path(&g.id), SES, &one_wall_session());

    // Read cold, after the worker that did the work is reaped (#324).
    reg.mark_dead(&w.id, Some(0));
    let digest = reg.session_digest(&g.id, DigestLookup::Agent(w.id.clone())).unwrap();

    assert_eq!(
        digest.initial_prompt.as_deref(),
        Some("fix the flaky test"),
        "the prompt is a user TEXT PART — opencode messages carry no text of their own"
    );
    assert!(
        digest.windows.iter().any(|win| win.signature == FrictionSignature::ToolError),
        "the failed command must reach the extractor: {digest:?}"
    );
    assert!(
        digest.windows.iter().any(|win| win.signature == FrictionSignature::NearDuplicateCommand),
        "npm->pnpm is the same wall for opencode as for claude: {digest:?}"
    );
}

/// The regression this slice exists to remove: before it, every opencode
/// agent's digest was the `does not support agent CLI "opencode"` error.
#[test]
fn an_opencode_digest_is_no_longer_refused_for_the_cli_itself() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/opencode-repo", rails("opencode")).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "task", false, None).unwrap();
    reg.associate_session(&g.id, &w.id, SES);
    store(&reg.opencode_db_path(&g.id), SES, &one_wall_session());

    let err = reg.session_digest(&g.id, DigestLookup::Agent(w.id.clone())).err();
    assert!(err.is_none(), "expected a digest, got {err:?}");
}

/// A store that isn't there is reported, not swallowed. An empty digest would
/// read as "this worker hit no friction" — a claim about the session, made
/// from evidence that was never read.
#[test]
fn a_digest_with_no_store_to_read_says_so_instead_of_reporting_no_friction() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/opencode-repo", rails("opencode")).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "task", false, None).unwrap();
    reg.associate_session(&g.id, &w.id, SES);

    let err = reg.session_digest(&g.id, DigestLookup::Agent(w.id.clone())).unwrap_err();
    assert!(err.contains(SES), "the error must name the session it failed on: {err}");
    assert!(err.contains("no opencode database"), "…and why it failed: {err}");
}

/// Each group has its own `OPENCODE_DB`, so a digest must read the store of
/// the group the agent belongs to. Reading some other group's would attribute
/// one team's session to another's — the same class of error slice C's
/// contested-match refusal exists to prevent.
#[test]
fn a_digest_reads_the_agents_own_groups_store_not_another_groups() {
    let (reg, _d) = test_registry();
    let a = reg.create_group("C:/tmp/repo-a", rails("opencode")).unwrap();
    let b = reg.create_group("C:/tmp/repo-b", rails("opencode")).unwrap();
    let w = reg.spawn_agent(&a.id, Role::Worker, "w", "task", false, None).unwrap();
    reg.associate_session(&a.id, &w.id, SES);

    // Only group B has a store, and it holds a session with the SAME id — the
    // shape that would let a group-blind read succeed against the wrong file.
    store(&reg.opencode_db_path(&b.id), SES, &one_wall_session());

    let err = reg.session_digest(&a.id, DigestLookup::Agent(w.id.clone())).unwrap_err();
    assert!(err.contains("no opencode database"), "group A has no store of its own: {err}");
}

//! Read-only reads of OpenCode's SQLite session store (#722).
//!
//! OpenCode keeps sessions, messages and parts in **one SQLite database**, not
//! the per-project JSON tree its troubleshooting page still describes — that
//! layout survives only as migration code (`SOURCE`, `storage/storage.ts`).
//! loomux points each group's panes at their own file through `OPENCODE_DB`
//! (see `orchestration::OPENCODE_DB_ENV` and `doc/design/opencode.md`), so the
//! store this module reads is one loomux created and one only that group's
//! agents write.
//!
//! # The schema this module depends on
//!
//! Verified against `anomalyco/opencode@f67e80c2` (tag `v1.18.11`) and a live
//! store on the maintainer's machine; recorded in full on issue #722 (slice-V
//! memo, §1b). The columns read here, verbatim from that DDL:
//!
//! ```sql
//! CREATE TABLE `session` (
//!   `id` text PRIMARY KEY, `project_id` text NOT NULL, `parent_id` text,
//!   `directory` text NOT NULL, … `model` text,
//!   `cost` real DEFAULT 0 NOT NULL,
//!   `tokens_input` integer DEFAULT 0 NOT NULL,
//!   `tokens_output` integer DEFAULT 0 NOT NULL,
//!   `tokens_reasoning` integer DEFAULT 0 NOT NULL,
//!   `tokens_cache_read` integer DEFAULT 0 NOT NULL,
//!   `tokens_cache_write` integer DEFAULT 0 NOT NULL, … )
//! ```
//!
//! That schema is a vendor's internal detail with no compatibility promise, so
//! every failure mode here is **degraded, never fatal**: a store that is
//! absent, unopenable, or shaped differently than the above yields
//! [`Unavailable`] rather than an error, and what that means is the caller's
//! call — the polled usage meter reports zero and moves on, while a digest,
//! whose entire product is the transcript it came to read, surfaces the reason
//! instead of returning an empty one (see `session_transcript`). Nothing in
//! this module panics, retries in a loop, or blocks for longer than
//! [`BUSY_TIMEOUT_MS`] — it runs on the polled `group_usage` path, where a
//! wedge would freeze a UI tick.
//!
//! The variant is not decoration: degrading is not the same as being
//! undiagnosable, and `OrchRegistry::note_opencode_db_degrade` turns these into
//! one audit line per episode so a drifted schema and a never-booted pane stop
//! looking alike. Which is why [`Unavailable::Query`] carries its message and
//! [`Unavailable::Absent`] is a distinct arm rather than one more error string.
//!
//! # Read-only, and why not `immutable`
//!
//! The primary open is `SQLITE_OPEN_READ_ONLY`: SQLite refuses every write on
//! such a connection, so loomux cannot corrupt or lock out a live opencode no
//! matter what this code does. It is deliberately *not* `immutable=1` in the
//! normal case — an immutable connection skips locking and reads the main
//! database file alone, which on a WAL store means silently missing everything
//! committed but not yet checkpointed. For a live agent that is most of the
//! session, and a usage meter that under-reports spend without saying so is
//! worse than one that reports nothing. A read-only connection consults the
//! WAL and sees the last committed state.
//!
//! `immutable=1` survives only as a **fallback** ([`open_immutable`]), for the
//! one case where the plain read-only open cannot work: an opencode that died
//! without checkpointing leaves a dirty `-wal` behind, and rebuilding the
//! `-shm` index that would need is a write. There, a possibly-stale number
//! beats no number, and the staleness is bounded by whatever that dead process
//! failed to checkpoint.

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

/// How long a read may wait on a lock before giving up. Short on purpose: in
/// WAL mode a reader is not blocked by a writer at all, so this only bites for
/// a store in some other journal mode with a writer mid-transaction — and this
/// runs on a polled path, where the right answer to contention is "no number
/// this tick", not "hold the tick".
pub const BUSY_TIMEOUT_MS: u64 = 250;

/// Why a read produced no usage. Each arm is a *degrade*, never an error the
/// caller should surface as a failure — see the module docs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// No database file at all: the normal state of a group whose opencode
    /// panes have never booted.
    Absent,
    /// The file exists but could not be opened read-only (nor immutably).
    Open(String),
    /// The database opened but the query failed — the schema drifted from the
    /// one recorded above, or the file is not the store loomux expects.
    Query(String),
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unavailable::Absent => write!(f, "no opencode database"),
            Unavailable::Open(e) => write!(f, "opencode database unopenable: {e}"),
            Unavailable::Query(e) => write!(f, "opencode database schema drift: {e}"),
        }
    }
}

/// One pane's usage, exactly as OpenCode counts it: dollars it computed
/// itself, and its **five** token counters. Deliberately faithful to the
/// vendor's shape rather than folded into loomux's four-bucket
/// [`crate::usage::TokenUsage`] here — the fold is a lossy mapping decision
/// that belongs at the boundary (`usage::opencode_session_usage`), and slice C
/// reuses this reader for identification without wanting it.
///
/// `total` is not a field: OpenCode's own `tokens.total` is exactly the sum of
/// the five (`LOCAL-OBSERVED` on a real message record — 15260 + 1115 + 1193 +
/// 63104 + 0 = 80672), so storing it separately could only ever disagree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionTotals {
    /// Dollars, as OpenCode computed them from its own provider price table.
    /// A **reported** figure, not a loomux estimate — and legitimately `0.0`
    /// on a free model, which is different from "unknown".
    pub cost_usd: f64,
    pub input: u64,
    pub output: u64,
    /// Counted *separately* from `output` by OpenCode, not inside it.
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// The model id the pane's own (root) session ran on, for display. The
    /// rollup below may span subagent sessions on other models; this names the
    /// one the pane is, which is the question the UI is asking.
    pub model: Option<String>,
}

/// A pane's usage is its session **plus every session descended from it**.
///
/// Subagent sessions are rows of their own with `parent_id` pointing at their
/// spawner (`LOCAL-OBSERVED`: a `ses_1508…` row with `agent = "explore"` and a
/// title ending `(@explore subagent)`), and their tokens are spend the pane
/// caused. Charging only the root row would under-report exactly the agents
/// that fan out the most — the direction this repo's price-table note already
/// commits to never erring in.
///
/// `UNION` (not `UNION ALL`) makes the recursion terminate even if the
/// `parent_id` edges ever contained a cycle: a row already in `tree` is not
/// re-added.
const ROLLUP_SQL: &str = "\
WITH RECURSIVE tree(id) AS (
    SELECT id FROM session WHERE id = ?1
    UNION
    SELECT s.id FROM session s JOIN tree t ON s.parent_id = t.id
)
SELECT COALESCE(SUM(s.cost), 0.0),
       COALESCE(SUM(s.tokens_input), 0),
       COALESCE(SUM(s.tokens_output), 0),
       COALESCE(SUM(s.tokens_reasoning), 0),
       COALESCE(SUM(s.tokens_cache_read), 0),
       COALESCE(SUM(s.tokens_cache_write), 0)
  FROM session s WHERE s.id IN (SELECT id FROM tree)";

/// `file:` URI for `db` with `immutable=1`, for [`open_immutable`].
///
/// Windows paths go in with forward slashes (SQLite's URI parser does not
/// treat `\` as a separator), and the three characters its parser gives
/// meaning to — `?` starts the query, `#` starts the fragment, `%` starts an
/// escape — are percent-encoded so a directory name containing one cannot
/// truncate the path or decode into a different one. Pure, so the encoding is
/// assertable without a database.
#[doc(hidden)] // pub for integration tests
pub fn immutable_uri(db: &Path) -> String {
    let mut s = String::from("file:");
    for ch in db.display().to_string().chars() {
        match ch {
            '\\' => s.push('/'),
            '?' => s.push_str("%3f"),
            '#' => s.push_str("%23"),
            '%' => s.push_str("%25"),
            c => s.push(c),
        }
    }
    s.push_str("?immutable=1");
    s
}

/// Open `db` read-only, falling back to an immutable open when that fails
/// (see the module docs for why that order and not the other one).
pub fn open_readonly(db: &Path) -> Result<Connection, Unavailable> {
    // Checked before asking SQLite so "the agent never ran" is its own arm
    // rather than an `unable to open database file` string a caller would have
    // to pattern-match. This is the common case, not an error case.
    if !db.is_file() {
        return Err(Unavailable::Absent);
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = match Connection::open_with_flags(db, flags) {
        Ok(c) => c,
        Err(primary) => open_immutable(db)
            .map_err(|second| Unavailable::Open(format!("{primary}; immutable retry: {second}")))?,
    };
    conn.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))
        .map_err(|e| Unavailable::Open(e.to_string()))?;
    Ok(conn)
}

/// The immutable fallback, separately callable so a test can prove the URI
/// this platform's SQLite actually accepts rather than asserting the string.
#[doc(hidden)] // pub for integration tests
pub fn open_immutable(db: &Path) -> Result<Connection, rusqlite::Error> {
    Connection::open_with_flags(
        immutable_uri(db),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )
}

/// Total usage for `session_id` and its descendants, from the store at `db`.
///
/// `Ok(None)` means the database is readable and simply has no such session —
/// the state between a pane spawning and its first turn landing. `Err` is a
/// degrade, described by [`Unavailable`].
pub fn session_usage(db: &Path, session_id: &str) -> Result<Option<SessionTotals>, Unavailable> {
    session_usage_on(&open_readonly(db)?, session_id)
}

/// [`session_usage`] against an already-open connection, so a caller reading
/// more than one thing from a store pays for one open. Slice C's session
/// identification is that caller.
pub fn session_usage_on(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<SessionTotals>, Unavailable> {
    // The root row first: it answers both "does this session exist at all"
    // (which the rollup's COALESCE'd SUM cannot — an empty set sums to zero,
    // indistinguishable from a real zero-token session) and "what model is
    // this pane on".
    let model: Option<Option<String>> = conn
        .query_row("SELECT model FROM session WHERE id = ?1", [session_id], |r| {
            r.get::<_, Option<String>>(0)
        })
        .optional()
        .map_err(drift)?;
    let Some(model) = model else { return Ok(None) };

    let (cost, input, output, reasoning, cache_read, cache_write) = conn
        .query_row(ROLLUP_SQL, [session_id], |r| {
            Ok((
                r.get::<_, f64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })
        .map_err(drift)?;

    Ok(Some(SessionTotals {
        cost_usd: sane_cost(cost),
        input: sane_count(input),
        output: sane_count(output),
        reasoning: sane_count(reasoning),
        cache_read: sane_count(cache_read),
        cache_write: sane_count(cache_write),
        model,
    }))
}

/// Any `rusqlite` failure *after* a successful open is schema drift as far as
/// this module is concerned: the file opened, so it is a database; it just is
/// not shaped the way the DDL above says. Shared by every query here so one
/// classification covers them all.
fn drift(e: rusqlite::Error) -> Unavailable {
    Unavailable::Query(e.to_string())
}

// ── Session identification (#722 slice C) ──────────────────────────────────
//
// Which session a pane owns cannot be asked of this store by project: the
// project id is `sha1("git-remote:" + host/path)` (`SOURCE`, `project.ts`), so
// every worktree of one repo — which is every agent in a loomux group —
// collides on a single project row. What separates them is the `directory`
// column, plus knowing which rows are NEW.
//
// So identification is the copilot pattern, one store down: snapshot the ids
// that exist before the pane is spawned, then poll for one that appeared
// since, in this pane's directory, that no other pane has already taken.

/// Every session id the store already holds — the snapshot taken *before* a
/// pane is spawned, so the session that pane later creates can be told apart
/// from the ones that were already there.
///
/// Includes subagent rows deliberately, even though [`identify_session_on`]
/// would never adopt one: a baseline is "what was already here", and making it
/// selective would only create a class of row whose absence from the baseline
/// means nothing.
///
/// A store that does not exist yet is [`Unavailable::Absent`] and that is the
/// *ordinary* case, not a failure: the first opencode pane in a group is
/// spawned against a file opencode has not created yet. Callers treat it as an
/// empty baseline.
pub fn session_ids(db: &Path) -> Result<HashSet<String>, Unavailable> {
    session_ids_on(&open_readonly(db)?)
}

/// [`session_ids`] against an already-open connection.
pub fn session_ids_on(conn: &Connection) -> Result<HashSet<String>, Unavailable> {
    let mut stmt = conn.prepare("SELECT id FROM session").map_err(drift)?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(drift)?;
    let mut out = HashSet::new();
    for id in rows {
        out.insert(id.map_err(drift)?);
    }
    Ok(out)
}

/// What one identification attempt found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identified {
    /// No candidate yet — this pane's opencode has not written its session row.
    /// The caller polls; this is the normal answer for the first seconds of a
    /// pane's life.
    None,
    /// Exactly one candidate. This pane's session.
    One(String),
    /// More than one unclaimed candidate matches this directory, carrying how
    /// many. Deliberately not a pick — see [`identify_session_on`].
    Contested(usize),
}

/// The session a just-spawned pane in `directory` created, if it can be named
/// without guessing.
///
/// A candidate is a row that is all four of:
///
/// - **`parent_id IS NULL`** — subagent sessions are rows of their own
///   (`LOCAL-OBSERVED`: a row with `agent = "explore"` and a title ending
///   `(@explore subagent)`), and binding a pane to its own subagent would make
///   every later read — usage, resume, digest — answer about the wrong
///   conversation.
/// - **in `directory`**, compared with [`crate::sessions::norm_path`], the same
///   rule copilot's session store is compared with. opencode writes this column
///   with forward slashes (`LOCAL-OBSERVED`: `C:/Projects/loomux`) while a
///   pane's cwd arrives with backslashes, so a raw string compare would match
///   nothing on Windows.
/// - **absent from `baseline`** — it appeared after this pane was spawned.
/// - **absent from `claimed`** — no other pane in this group has taken it.
///
/// **The result does not depend on the order rows come back in**, which is why
/// there is no `ORDER BY` here: with more than one candidate this refuses, so
/// no ordering could ever break a tie, and a newest-first sort would be a rule
/// nothing reads — the kind that looks load-bearing to the next person and
/// quietly justifies a wrong pick if the refusal is ever softened.
///
/// **Two or more candidates refuse rather than pick**, which is where this
/// departs from `newest_new_copilot_session`'s newest-wins. The difference is
/// not taste, it is the store: copilot's is the machine's, and two panes racing
/// in one directory is the exotic case; a group's opencode store is written by
/// *every pane in that group*, and the orchestrator, the reviewer and any
/// worker without its own worktree all sit in the repo root — so a contested
/// match is the ordinary case here, not the corner. `doc/design/session-id-
/// learning.md`'s ambiguity policy already settled which way to fail: a refused
/// match costs a pane that stays unidentified (degrading exactly as an
/// opencode pane does today), while a wrong one silently reports one agent's
/// spend as another's and resumes a human into a conversation that is not the
/// one they asked for.
///
/// Refusal is usually self-healing rather than terminal: the caller keeps
/// polling, and the moment the *other* pane's watcher claims its session, the
/// count falls to one and this pane identifies normally. Panes in one group
/// are spawned one at a time, seconds apart, so the ordinary case is that the
/// earlier pane's session is already in the later pane's baseline and no
/// contest arises at all.
///
/// **The residual, stated rather than assumed away:** if two panes in the SAME
/// directory are spawned close enough together that neither's session had
/// appeared when the other's baseline was taken, both see two candidates and
/// both refuse — nothing breaks the tie, and neither identifies. That is the
/// deliberate cost of the policy: both panes stay at the status quo an
/// opencode pane is in today (no usage attribution, no resume), and the
/// watcher's timeout audits *contested* rather than a silent nothing, so the
/// state is diagnosable instead of mysterious. The alternative — picking one —
/// buys identification for one pane by giving the other pane's conversation
/// away, undetectably.
pub fn identify_session(
    db: &Path,
    directory: &str,
    baseline: &HashSet<String>,
    claimed: &HashSet<String>,
) -> Result<Identified, Unavailable> {
    identify_session_on(&open_readonly(db)?, directory, baseline, claimed)
}

/// [`identify_session`] against an already-open connection.
pub fn identify_session_on(
    conn: &Connection,
    directory: &str,
    baseline: &HashSet<String>,
    claimed: &HashSet<String>,
) -> Result<Identified, Unavailable> {
    let want = crate::sessions::norm_path(directory);
    // An empty cwd would otherwise match every session whose directory is also
    // empty — "I don't know where this pane is" must never widen the search.
    if want.is_empty() {
        return Ok(Identified::None);
    }
    let mut stmt = conn
        .prepare("SELECT id, directory FROM session WHERE parent_id IS NULL")
        .map_err(drift)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(drift)?;
    let mut hits: Vec<String> = Vec::new();
    for row in rows {
        let (id, dir) = row.map_err(drift)?;
        if baseline.contains(&id) || claimed.contains(&id) {
            continue;
        }
        if crate::sessions::norm_path(&dir) != want {
            continue;
        }
        hits.push(id);
    }
    Ok(match hits.len() {
        0 => Identified::None,
        1 => Identified::One(hits.remove(0)),
        n => Identified::Contested(n),
    })
}

/// The directory a session recorded for itself, for resolving where to resume
/// it — the opencode analogue of reading a claude transcript's `cwd` or a
/// copilot `workspace.yaml`'s. `Ok(None)`: the store is readable and has no
/// such session.
pub fn session_directory(db: &Path, session_id: &str) -> Result<Option<String>, Unavailable> {
    session_directory_on(&open_readonly(db)?, session_id)
}

/// [`session_directory`] against an already-open connection.
pub fn session_directory_on(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<String>, Unavailable> {
    conn.query_row(
        "SELECT directory FROM session WHERE id = ?1",
        [session_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(drift)
}

// ── Transcript readback (#722 slice B2) ───────────────────────────────────
//
// The digest reads a finished session's conversation, and OpenCode keeps it in
// two more tables of the same store. Verified at the same pin as the `session`
// DDL above (`anomalyco/opencode@f67e80c2`, tag `v1.18.11`), from
// `packages/core/src/session/sql.ts`:
//
// ```sql
// CREATE TABLE `message` ( `id` text PRIMARY KEY, `session_id` text NOT NULL,
//   `time_created` integer NOT NULL, `time_updated` integer NOT NULL, `data` text NOT NULL )
// CREATE TABLE `part` ( `id` text PRIMARY KEY, `message_id` text NOT NULL,
//   `session_id` text NOT NULL, `time_created` integer NOT NULL,
//   `time_updated` integer NOT NULL, `data` text NOT NULL )
// ```
//
// `data` is JSON on both: `message.data` is `Omit<SessionV1.Info, "id" |
// "sessionID">` and `part.data` is `Omit<SessionV1.Part, "id" | "sessionID" |
// "messageID">` (same file, `V1MessageData`/`V1PartData`). This module hands
// those documents over verbatim — what they MEAN is
// `digest::parse_opencode_transcript_events`'s job, so the two per-CLI
// concerns stay where their claude/copilot counterparts already are: reading
// bytes here, normalizing them there.

/// One `part` row plus the message it hangs off — the unit
/// [`digest::parse_opencode_transcript_events`](crate::orchestration::digest::parse_opencode_transcript_events)
/// normalizes.
///
/// `message_json` repeats across every part of one message, which is why
/// `message_id` rides along: the normalizer parses a message document once and
/// reuses it until that id changes, rather than re-parsing it per part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptRow {
    pub message_id: String,
    /// The MESSAGE's `time_created`, ms since the epoch (`Timestamps` defaults
    /// to `Date.now()`, `packages/core/src/database/schema.sql.ts`). Parts
    /// carry their own optional times inside `part_json`; this is the one that
    /// is always present and is what the rows are ordered by.
    pub time_created_ms: i64,
    pub message_json: String,
    pub part_json: String,
}

/// Every part of every message in `session_id`, in the order OpenCode itself
/// reads them.
///
/// **The `ORDER BY` is the vendor's own index order**, not a guess:
/// `message_session_time_created_id_idx` is `(session_id, time_created, id)`
/// and `part_message_id_id_idx` is `(message_id, id)` (`session/sql.ts`).
/// Ordering by id *string* alone would be wrong at the session level — session
/// ids may be minted with a bitwise-inverted timestamp (`id.ts`, recorded in
/// `doc/design/opencode.md`) — but message and part ids are minted `ascending`
/// (`schema/src/v1/session.ts`, `MessageID.ascending`/`PartID.ascending`), so
/// they are a sound tiebreak within a session and within a message.
///
/// An empty vec means the store is readable and this session has no messages —
/// a pane that never took a turn. `Err` is a degrade, described by
/// [`Unavailable`], exactly as everywhere else in this module.
pub fn session_transcript(db: &Path, session_id: &str) -> Result<Vec<TranscriptRow>, Unavailable> {
    session_transcript_on(&open_readonly(db)?, session_id)
}

/// [`session_transcript`] against an already-open connection.
pub fn session_transcript_on(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<TranscriptRow>, Unavailable> {
    // PROBE (#722 slice B2): no transcript reader, exactly as before this slice.
    #[allow(unreachable_code, unused_variables)]
    return Ok(Vec::new());
    let mut stmt = conn
        .prepare(
            "SELECT m.id, m.time_created, m.data, p.data \
               FROM message m JOIN part p ON p.message_id = m.id \
              WHERE m.session_id = ?1 \
              ORDER BY m.time_created, m.id, p.id",
        )
        .map_err(drift)?;
    let rows = stmt
        .query_map([session_id], |r| {
            Ok(TranscriptRow {
                message_id: r.get(0)?,
                time_created_ms: r.get(1)?,
                message_json: r.get(2)?,
                part_json: r.get(3)?,
            })
        })
        .map_err(drift)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(drift)?);
    }
    Ok(out)
}

/// A cost is only worth reporting if it is finite and non-negative. SQLite
/// stores whatever a writer put in a `real` column, and one NaN would not stay
/// local: `group_usage` sums every agent's cost into a group total, and NaN
/// poisons that sum for the whole group (and serializes to `null`, so the UI
/// would read "no cost figure at all"). Clamped here, at the boundary.
fn sane_cost(c: f64) -> f64 {
    if c.is_finite() && c > 0.0 {
        c
    } else {
        0.0
    }
}

/// Same posture for the counters: a negative token count is not a number this
/// codebase has a use for, and `as u64` on one would wrap to something
/// astronomical.
fn sane_count(n: i64) -> u64 {
    n.max(0) as u64
}

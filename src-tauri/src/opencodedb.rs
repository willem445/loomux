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
//! [`Unavailable`], and the caller reports zero usage and moves on. Nothing in
//! this module panics, retries in a loop, or blocks for longer than
//! [`BUSY_TIMEOUT_MS`] — it runs on the polled `group_usage` path, where a
//! wedge would freeze a UI tick.
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
    let drift = |e: rusqlite::Error| Unavailable::Query(e.to_string());
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

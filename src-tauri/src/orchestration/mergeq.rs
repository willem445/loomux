//! The bisecting merge queue — **pure core** (#581 slice C).
//!
//! Design note: `doc/design/merge-queue.md`. Section references below (§4, §6,
//! §8, §9, §11.3, §11.4) point into it; that note is the spec this file
//! implements and the argument for every choice made here.
//!
//! # What lives here, and what deliberately does not
//!
//! Everything in this module is a **pure function over values**. There is no
//! I/O, no `git`, no `gh`, no clock and no filesystem: the queue's decisions —
//! which entries go into a batch, which half a bisect tests next, whether the
//! merge gate still holds — are the part that has to be deterministic and
//! test-pinned, so they are separated from the part that talks to the world.
//! The driver that pushes the scratch ref, opens the draft PR, observes the
//! checks and lands the batch is slice D; it consumes this module and supplies
//! every external fact as an argument.
//!
//! **No code path in this file writes anything anywhere.** Not a ref, not a
//! file, not a PR. [`scratch_branch`] builds a *name*; nothing pushes it.
//!
//! # The two invariants this file exists to hold
//!
//! 1. **Eight states and no ninth** (§4). "Paused" is not a state — it is an
//!    eligibility predicate computed live from [`QueueEntry::blocked_reason`],
//!    because a persisted staleness flag is a claim that rots the moment the
//!    world moves. Every transition is enumerated in [`transition`] with the
//!    section that asks for it, and anything not enumerated is **refused**.
//! 2. **The queue is strictly additive to the merge gate** (§6). It never
//!    grants what the gate would not. [`recheck_gate`] therefore *calls* the
//!    shim's own parsers (`workflow::evaluate_merge_gate` and friends) rather
//!    than re-deriving the decision — a third implementation of the gate
//!    decision is a defect, not an optimization.

use super::workflow::{
    condition_supported, evaluate_merge_gate, parse_gate_file, BlockId, Gate, GateOutcome, ReviewVerdict,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;

/// Schema version of `merge_queue.json` (§11.3). Recorded in the file so a
/// future breaking change is *detected* rather than mis-parsed — see
/// [`MergeQueueState::version_supported`].
pub const MERGE_QUEUE_VERSION: u32 = 1;

/// Hard cap on queued entries (§10), so `merge_queue.json` stays bounded and an
/// enqueue storm cannot grow it without limit. Enqueue past the cap is refused
/// with a stated reason (`queue-full`, §11.1) — the refusal itself is the
/// driver's (slice D), this constant is the number it refuses against.
pub const MAX_ENTRIES: usize = 64;

// ── entry states (§4) ───────────────────────────────────────────────────────

/// The state of one queued PR.
///
/// Serialized kebab-case into `merge_queue.json`, matching the §11.3 example
/// (`"state": "ci-wait"`).
///
/// **There is no `Unknown` variant, and that is the point.** Unknown *fields*
/// in the state file are tolerated and preserved ([`MergeQueueState::extra`]);
/// an unknown *state* is not, because a state loomux cannot interpret is not
/// one it may act on, and a catch-all bucket would be the ninth state §4
/// forbids. A file naming a state this build does not know fails to parse, and
/// §4's reconcile requires that to surface **loudly** rather than be dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryState {
    /// Enqueued and eligible in principle; not yet in a batch.
    Queued,
    /// Selected into the batch currently being constructed.
    Batching,
    /// The scratch is pushed, the draft PR is open, checks are being observed.
    CiWait,
    /// Checks are green and the gate is being re-verified at the moment of
    /// submit (§6 — the #532 rule).
    Landing,
    /// The batch went red and this entry is inside the search (§9).
    Bisecting,
    /// Terminal: landed on the target.
    Landed,
    /// Terminal: kicked back to its owner (conflict, culprit, gate refusal).
    KickedBack,
    /// Terminal: cancelled out of the queue.
    Cancelled,
}

impl EntryState {
    /// The wire/audit spelling — the same string serde writes.
    pub fn as_str(self) -> &'static str {
        match self {
            EntryState::Queued => "queued",
            EntryState::Batching => "batching",
            EntryState::CiWait => "ci-wait",
            EntryState::Landing => "landing",
            EntryState::Bisecting => "bisecting",
            EntryState::Landed => "landed",
            EntryState::KickedBack => "kicked-back",
            EntryState::Cancelled => "cancelled",
        }
    }

    /// Parse a state word. `None` for anything unrecognized — never coerced,
    /// the same "reject, never guess" posture `workflow::Verdict::parse` takes.
    pub fn parse(s: &str) -> Option<EntryState> {
        match s.trim() {
            "queued" => Some(EntryState::Queued),
            "batching" => Some(EntryState::Batching),
            "ci-wait" => Some(EntryState::CiWait),
            "landing" => Some(EntryState::Landing),
            "bisecting" => Some(EntryState::Bisecting),
            "landed" => Some(EntryState::Landed),
            "kicked-back" => Some(EntryState::KickedBack),
            "cancelled" => Some(EntryState::Cancelled),
            _ => None,
        }
    }

    /// `landed` / `kicked-back` / `cancelled` (§4). A terminal state has no
    /// outgoing transition **at all** — a kicked-back PR that gets fixed comes
    /// back through a fresh `queue_merge`, as a new entry, so its enqueue
    /// re-runs every §7 refusal against the world as it is then.
    pub fn is_terminal(self) -> bool {
        matches!(self, EntryState::Landed | EntryState::KickedBack | EntryState::Cancelled)
    }

    /// Whether this entry is somewhere inside an in-flight batch. Used by
    /// [`plan_batch`] to hold the single-in-flight discipline (§4) even when
    /// the batch record itself is missing — an inconsistent file must not be
    /// the reason a second batch is dispatched.
    pub fn in_flight(self) -> bool {
        matches!(
            self,
            EntryState::Batching | EntryState::CiWait | EntryState::Landing | EntryState::Bisecting
        )
    }
}

/// A transition the state machine refuses. Carries both ends so the audit
/// event and the notice can say what was actually attempted (§11.5 — an audit
/// action must name what happened).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidTransition {
    pub from: EntryState,
    pub to: EntryState,
}

impl fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} -> {} is not a legal merge-queue transition",
            self.from.as_str(),
            self.to.as_str()
        )
    }
}

/// **The state machine.** Every legal edge of §4's diagram plus every one
/// §10's failure table names, each with the section that asks for it. Anything
/// else is refused — including a self-transition, which is not a transition:
/// refreshing [`QueueEntry::blocked_reason`] leaves an entry `queued`, and §4
/// is explicit that "paused" is a live predicate rather than a state change.
///
/// The point of enumerating rather than defaulting: the queue is a gate, and a
/// gate that quietly accepts a transition nobody designed is a gate whose
/// behavior is whatever the caller happened to ask for.
pub fn transition(from: EntryState, to: EntryState) -> Result<EntryState, InvalidTransition> {
    use EntryState::*;
    let ok = match (from, to) {
        // Selected into the batch being constructed (§4).
        (Queued, Batching) => true,
        // Construction succeeded: scratch pushed, draft PR open (§4/§5).
        (Batching, CiWait) => true,
        // The speculative merge conflicted — kicked back before any CI is
        // spent (§8 "conflicts cost no CI", §10).
        (Batching, KickedBack) => true,
        // The batch aborted before anything was observed: push auth failure,
        // the gate disappeared mid-flight, the target moved (§6, §10). Entries
        // return to `queued` — they did nothing wrong.
        (Batching, Queued) => true,
        // Checks went green; the gate is re-verified at submit (§6).
        (CiWait, Landing) => true,
        // Checks went red with k > 1: into the search (§9).
        (CiWait, Bisecting) => true,
        // Checks went red with k = 1: that PR is the culprit, no further CI
        // (§9). (Whether it is instead an infrastructure/flake case — red at
        // k = 1 while the PR's own checks are green — is an *observation* the
        // driver makes; both outcomes leave the entry here.)
        (CiWait, KickedBack) => true,
        // The batch became unverifiable (`checks_timeout_minutes` backstop) or
        // was abandoned because a sibling cancelled (§5, §10). Nothing landed,
        // so the entries requeue.
        (CiWait, Queued) => true,
        // The tested SHA became the target head (§8).
        (Landing, Landed) => true,
        // The gate re-check refused at the moment of submit (§6, §10).
        (Landing, KickedBack) => true,
        // The fast-forward push failed because the target moved under the
        // batch (§10). Never `--force`; the batch aborts and requeues.
        (Landing, Queued) => true,
        // The search attributed this entry (§9).
        (Bisecting, KickedBack) => true,
        // The search exonerated this entry: survivors are auto-requeued, at the
        // front, in their original order (§9).
        (Bisecting, Queued) => true,
        // A cancel reaches any non-terminal entry (§4, §10). Terminal states
        // are excluded by the guard below, not by an arm here.
        (_, Cancelled) if !from.is_terminal() => true,
        _ => false,
    };
    if ok {
        Ok(to)
    } else {
        Err(InvalidTransition { from, to })
    }
}

// ── `merge_queue.json` (§11.3) ──────────────────────────────────────────────
//
// Forward compatibility here is the OPPOSITE of `.loomux/workflow.yml`'s, and
// §11.2 spells out why: `workflow.yml` is human-authored **policy**, so a key
// this build does not understand is a human believing a policy is in force that
// is not — it fails the parse loudly. `merge_queue.json` is machine-authored
// **state**, and an older build must be able to read it and rewrite it without
// destroying what a newer one wrote. Policy fails loud; state degrades
// gracefully. Different documents, different jobs.
//
// "Tolerated" is not enough on its own: serde ignores unknown fields by
// default, and an ignored field is *lost* on the next write. So every type here
// carries a flattened `extra` map, which makes the round trip preserving rather
// than merely non-fatal. That is the property `merge_queue_state_round_trip_
// preserves_unknown_fields` pins.

/// The whole of `merge_queue.json` (§11.3): one target, the entries, and the
/// single in-flight batch if there is one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MergeQueueState {
    /// Schema version (§11.3). **Required** — a state file with no version is
    /// malformed, not a v1 file, and §4's reconcile fails such an entry loudly
    /// rather than guessing at it.
    pub version: u32,
    /// The single target branch this queue lands on (§4, one target per group
    /// in v1). Established by the first successful enqueue from that PR's live
    /// base, and released when the queue drains — a property of the work in the
    /// queue, never a configured setting.
    #[serde(default)]
    pub target: String,
    /// Queue order. Survivors of a bisect are requeued **at the front**, in
    /// their original order (§9), so this vector's order is meaningful.
    #[serde(default)]
    pub entries: Vec<QueueEntry>,
    /// The one in-flight batch, if any (§4 — one per target).
    #[serde(default)]
    pub batch: Option<BatchRecord>,
    /// Fields written by a newer build, preserved verbatim across a read/write
    /// cycle. See the module comment above this type.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for MergeQueueState {
    fn default() -> Self {
        MergeQueueState {
            version: MERGE_QUEUE_VERSION,
            target: String::new(),
            entries: Vec::new(),
            batch: None,
            extra: BTreeMap::new(),
        }
    }
}

impl MergeQueueState {
    /// Whether this build understands the file's schema.
    ///
    /// The conservative half of forward compatibility: unknown *fields* are
    /// preserved, but a file whose whole schema moved is one this build must
    /// **not act on** — the fields it recognizes may no longer mean what it
    /// thinks. The driver's contract (slice D) is to refuse to operate and to
    /// leave the file alone, which is the only way "an older build does not
    /// destroy what a newer one wrote" survives a version bump that changes
    /// meanings rather than adding keys.
    pub fn version_supported(&self) -> bool {
        self.version == MERGE_QUEUE_VERSION
    }

    /// The entry for a PR, if it is in the queue at all.
    pub fn entry(&self, pr: u64) -> Option<&QueueEntry> {
        self.entries.iter().find(|e| e.pr == pr)
    }
}

/// One PR in the queue (§11.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueueEntry {
    pub pr: u64,
    /// The PR head this entry was last resolved against. Refreshed live at
    /// every batch build — §7 is explicit that no decision may key on stored
    /// data, so this is a record of what was seen, never a gate input.
    #[serde(default)]
    pub head: String,
    /// Private on purpose: [`advance`](QueueEntry::advance) is the only
    /// sanctioned way an entry's state changes, so the enumeration in
    /// [`transition`] cannot be bypassed by a caller outside this module
    /// assigning a state directly. Deserialization is the one other writer, and
    /// that is resuming a persisted state rather than transitioning to one.
    state: EntryState,
    /// Why this `queued` entry is not batchable **right now** — a rebase moved
    /// its head, so its verdicts died (§4). Refreshed at every batch build and
    /// cleared the instant a re-review covers the new head; never a persisted
    /// claim about the world.
    #[serde(default)]
    pub blocked_reason: Option<String>,
    #[serde(default)]
    pub enqueued_ms: u64,
    /// The id of the batch this entry is in, while it is in one.
    #[serde(default)]
    pub batch: Option<String>,
    /// Preserved unknown fields — see the section comment above.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl QueueEntry {
    /// A freshly enqueued entry: `queued`, unblocked, in no batch.
    pub fn new(pr: u64, head: &str, enqueued_ms: u64) -> QueueEntry {
        QueueEntry {
            pr,
            head: head.to_string(),
            state: EntryState::Queued,
            blocked_reason: None,
            enqueued_ms,
            batch: None,
            extra: BTreeMap::new(),
        }
    }

    pub fn state(&self) -> EntryState {
        self.state
    }

    /// Move this entry to `to`, or refuse. The **only** mutation path for
    /// [`QueueEntry::state`]; see that field's comment.
    pub fn advance(&mut self, to: EntryState) -> Result<(), InvalidTransition> {
        self.state = transition(self.state, to)?;
        Ok(())
    }

    /// Eligible to be picked into a batch: `queued`, and not blocked (§4). The
    /// live predicate that replaces a ninth state.
    pub fn batchable(&self) -> bool {
        self.state == EntryState::Queued && self.blocked_reason.is_none()
    }
}

/// The one in-flight batch (§11.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BatchRecord {
    /// Opaque and unordered (§11.4) — see [`new_batch_id`]. **Nothing may read
    /// "which batch came first" out of this string.**
    pub id: String,
    pub prs: Vec<u64>,
    /// The SHA CI is judging — and, on green, the exact object that lands. The
    /// Bors invariant (§8).
    #[serde(default)]
    pub scratch_sha: String,
    #[serde(default)]
    pub draft_pr: Option<u64>,
    /// The batch's own state, drawn from the same eight (§11.3's example is
    /// `"state": "ci-wait"`). Deliberately not a second enum: the batch is
    /// wherever its entries are, and a parallel vocabulary would be one more
    /// thing that can disagree.
    state: EntryState,
    #[serde(default)]
    pub started_ms: u64,
    /// Preserved unknown fields — see the section comment above.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl BatchRecord {
    /// A batch being constructed: `batching`, no scratch yet, no draft PR yet.
    pub fn new(id: &str, prs: Vec<u64>, started_ms: u64) -> BatchRecord {
        BatchRecord {
            id: id.to_string(),
            prs,
            scratch_sha: String::new(),
            draft_pr: None,
            state: EntryState::Batching,
            started_ms,
            extra: BTreeMap::new(),
        }
    }

    pub fn state(&self) -> EntryState {
        self.state
    }

    /// Same enumeration, same refusal as an entry's — see
    /// [`QueueEntry::advance`].
    pub fn advance(&mut self, to: EntryState) -> Result<(), InvalidTransition> {
        self.state = transition(self.state, to)?;
        Ok(())
    }
}

// ── batch ids and the scratch-ref namespace (§11.4) ─────────────────────────

/// A batch id from std's OS-seeded `RandomState` — **no getrandom-based
/// crates** (CLAUDE.md constraint 2; see the notes in `Cargo.toml`). Same
/// idiom as the registry's agent tokens, with the clock passed in so this stays
/// a pure function of its arguments.
///
/// **Opaque and unordered** (§11.4): not a sequence, not monotonic, carrying no
/// "which batch came first" meaning that any code may read. Non-reuse across a
/// restart is guaranteed by §4's remote check — refuse to mint onto a scratch
/// ref that already exists, and push create-only so the check has no TOCTOU
/// window — **not** by advancing a counter. §4 argues why: a counter's
/// guarantee is scoped to loomux's own record, while the object at risk (a
/// leaked scratch ref) lives on the remote.
///
/// Wider than §11.3's illustrative `mq-7f3a`: 32 bits rather than 16, because
/// the remote collision check is bounded at 3 attempts and it is cheaper to
/// make a collision rare than to spend that bound on one.
pub fn new_batch_id(now_ms: u64) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::hash::RandomState::new().build_hasher();
    h.write_u64(now_ms);
    format!("mq-{:08x}", h.finish() as u32)
}

/// The branch name of a batch's scratch ref: `loomux/mq/<group>-<batch-id>`
/// (§11.4). `refs/heads/` + this is the full ref.
///
/// **Reserved namespace.** Nothing else in loomux writes under `loomux/`, and
/// the queue writes nowhere else — which is what lets §10's cleanup delete by
/// exact name and never by pattern sweep.
///
/// Both components are sanitized here, at the one place the name is built,
/// because a group id is agent-visible and a batch id is generated: a component
/// that is not `[A-Za-z0-9_-]` would be a ref name loomux did not intend, and
/// `..`, a leading `-`, or a `/` in either component is exactly how a
/// "namespace-scoped" write stops being scoped. Rejected, never rewritten —
/// [`None`] means "do not build a ref for this", not "here is a different one".
pub fn scratch_branch(group: &str, batch_id: &str) -> Option<String> {
    fn clean(s: &str) -> Option<&str> {
        let s = s.trim();
        let ok = !s.is_empty()
            && s.len() <= 64
            && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && !s.starts_with('-');
        ok.then_some(s)
    }
    Some(format!("loomux/mq/{}-{}", clean(group)?, clean(batch_id)?))
}

// ── the batch planner (§4, §8) ──────────────────────────────────────────────

/// What the driver should do about dispatching a batch right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchPlan {
    /// A batch is already in flight. Bors discipline: **one in-flight batch per
    /// target** (§4) — entries pile up naturally while it runs, and there is no
    /// timed accumulation window.
    InFlight,
    /// Nothing to dispatch: the queue is empty, or every queued entry is
    /// blocked (§4's live eligibility predicate).
    Idle,
    /// Build a batch from these PRs, **in queue order** — the order is part of
    /// the contract, because §8 merges them onto the scratch in it and §9
    /// requeues survivors preserving it.
    Build(Vec<u64>),
}

/// Pick the next batch, or say why there isn't one.
///
/// Two independent reasons to hold: an explicit batch record, and any entry
/// sitting in an in-flight state. The second is not redundant — a file whose
/// batch record was lost while its entries still say `ci-wait` is exactly the
/// crash case §4 reconciles, and an inconsistent file must not be the reason a
/// **second** batch races the first. The fast-forward invariant (§8) is only
/// trivial to reason about while one batch can be racing the target head.
pub fn plan_batch(state: &MergeQueueState, max_batch: u32) -> BatchPlan {
    if state.batch.is_some() || state.entries.iter().any(|e| e.state().in_flight()) {
        return BatchPlan::InFlight;
    }
    let prs: Vec<u64> =
        state.entries.iter().filter(|e| e.batchable()).take(max_batch as usize).map(|e| e.pr).collect();
    if prs.is_empty() {
        BatchPlan::Idle
    } else {
        BatchPlan::Build(prs)
    }
}

// ── the bisect splitter (§9) ────────────────────────────────────────────────

/// The next step of the search on a red batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BisectStep {
    /// k = 1 — that PR is the culprit, and **no further CI is spent** (§9).
    ///
    /// Two honest limits the driver carries from here, both §9's: bisect finds
    /// *a* culprit, not necessarily *the* culprit (a genuine pairwise
    /// interaction attributes to whichever entry the search isolates), and a
    /// batch still red at k = 1 while that PR's **own** checks are green is an
    /// infrastructure/flake case to surface, not to loop on. Both need an
    /// observation this module does not have, so both are the driver's call.
    Culprit(u64),
    /// k > 1 — run `test` and recurse into whichever half reproduces red: into
    /// `test` if it is red, into `rest` if it is green. ceil(log2 k) runs; at
    /// the default `max_batch: 3`, at most 2.
    Split { test: Vec<u64>, rest: Vec<u64> },
    /// An empty set. There is nothing to attribute, and the search must not
    /// invent something — the driver aborts rather than blaming a PR.
    Nothing,
}

/// Halve a red batch (§9). Larger half first, so k = 3 splits 2/1 and the whole
/// search still terminates in at most 2 runs; depth is bounded by `max_batch`,
/// so it always terminates.
pub fn bisect_step(prs: &[u64]) -> BisectStep {
    match prs.len() {
        0 => BisectStep::Nothing,
        1 => BisectStep::Culprit(prs[0]),
        n => {
            let mid = (n + 1) / 2;
            BisectStep::Split { test: prs[..mid].to_vec(), rest: prs[mid..].to_vec() }
        }
    }
}

// ── the gate re-check (§6) ──────────────────────────────────────────────────
//
// The landing push is a merge the `gh` shim never sees: the shim gates the `gh`
// binary inside an agent pane via PATH, the backend is not an agent pane, and
// the landing verb is a `git push` rather than `gh pr merge` in any case. So the
// backend re-enforces the SAME gate itself — by calling the same parsers the
// shim's decision mirrors, never by reimplementing the decision. A third
// implementation of the gate decision is a defect, not an optimization.

/// What the driver found where the group's `merge_gate` file should be.
///
/// The three cases are genuinely different and §6 refuses two of them for
/// different reasons, so they are not collapsed into `Option<Gate>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateSpec {
    /// No `merge_gate` file: the repo declares no gate covering this target.
    ///
    /// `evaluate_merge_gate` with no gate returns *allowed*, which is right for
    /// the shim (an ungated repo merges as it always did) and **wrong** for the
    /// queue: it would mean the backend pushing approved-by-nobody PRs onto a
    /// branch under its own authority, which is the one thing §3's new
    /// authority is not for.
    Absent,
    /// A `merge_gate` file `parse_gate_file` could not read — a poison line, a
    /// truncation, a hand edit. The shim refuses **every** merge on exactly
    /// these, and so does the queue.
    Malformed,
    Declared(Gate),
}

impl GateSpec {
    /// Read the group's `merge_gate` file contents (`None` = the file is not
    /// there) into a spec, reusing `workflow::parse_gate_file` — the same
    /// reader the registry uses, so the two cannot disagree about what a gate
    /// file says.
    pub fn read(text: Option<&str>) -> GateSpec {
        match text {
            None => GateSpec::Absent,
            Some(t) => match parse_gate_file(t) {
                Some(g) => GateSpec::Declared(g),
                None => GateSpec::Malformed,
            },
        }
    }
}

/// The live facts about **one sub-PR** that the gate's `also:` clauses need and
/// this module cannot resolve itself. The driver fills them in from the real
/// `gh`, at the moment of the check.
///
/// `ci_green` is **that sub-PR's own checks** — §6: the batch's checks are an
/// additional signal, never a substitute for the per-PR one. Passing the
/// batch's verdict here would turn the queue into a way to land a PR whose own
/// CI was never green, which is the bypass §6 forbids.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrObservation {
    /// sha256 of `workflow::canonical_body` of the PR's body **now**. `None`
    /// when it could not be read — which refuses, never waves through.
    pub body_digest: Option<String>,
    /// Whether the PR's own checks are all-green. `None` when that could not be
    /// determined — again a refusal, mirroring the shim's `ci-not-green` arm,
    /// which treats failing, still-running and no-checks-reported alike.
    pub ci_green: Option<bool>,
}

/// Why an `also:` clause refuses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConditionRefusal {
    /// `ci-green` is declared and the PR's own checks are not all-green.
    CiNotGreen,
    /// `ci-green` is declared and the checks could not be read at all.
    CiUnknown,
    /// `body-unchanged` is declared and these reviewers' **live passes** were
    /// recorded against a different PR body (#565).
    BodyChanged { reviewers: Vec<BlockId> },
    /// `body-unchanged` is declared and the current body could not be digested
    /// — the shim's `unresolved-body`/`no-sha256` arms. Unknown is never
    /// "unbound, therefore fine".
    BodyUnknown,
    /// A clause this build cannot check. **Fails closed**: a condition loomux
    /// silently ignored would make a stricter-looking workflow file a weaker
    /// one, the worst failure a gate can have.
    Unsupported(String),
}

/// The outcome of re-checking the merge gate for one sub-PR (§6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateRecheck {
    /// Every requirement met, for the head it was checked against.
    Ok,
    /// No gate covers this target — a **refusal**, not a pass ([`GateSpec::Absent`]).
    NotConfigured,
    /// The gate file is on disk and unreadable ([`GateSpec::Malformed`]).
    Malformed,
    /// The reviewer half refuses, with the shim-shaped reason.
    Reviewers(GateOutcome),
    /// An `also:` clause refuses, naming the clause.
    Condition { condition: String, refusal: ConditionRefusal },
}

impl GateRecheck {
    pub fn passed(&self) -> bool {
        matches!(self, GateRecheck::Ok)
    }

    /// The §11.1 refusal code an MCP caller sees. Deliberately drawn from that
    /// closed vocabulary and no wider: [`GateRecheck::Malformed`] reports
    /// `gate-not-met` (which is true — an unreadable gate can never be met)
    /// while the variant keeps the distinction for the audit event and the
    /// notice, where §11.5 requires the text to name what actually happened.
    pub fn refusal_code(&self) -> Option<&'static str> {
        match self {
            GateRecheck::Ok => None,
            GateRecheck::NotConfigured => Some("gate-not-configured"),
            _ => Some("gate-not-met"),
        }
    }
}

/// **Re-verify the merge gate for one sub-PR.** Called at batch build and again
/// at landing (§6 — the #532 rule: re-verify every write gate at the moment of
/// submit). Between those two points there is a full CI cycle, tens of minutes
/// in which a reviewer can record a `fail`, a PR can be rebased, or a body can
/// change; verifying only at build would land a decision that was true half an
/// hour ago.
///
/// Three properties carried over from the shim verbatim, because dropping any
/// one would make the queue a weaker gate than the shim it sits beside:
///
/// - **A stale pass is not a pass.** A verdict whose recorded head is not the
///   PR's current head does not count — that is `ReviewVerdict::reviewed`, and
///   `evaluate_merge_gate` already applies it. `head: None` refuses outright
///   (`UnknownRevision`).
/// - **One `fail` beats any number of passes.** Blockers are checked before any
///   counting, whatever the threshold — also already in `evaluate_merge_gate`.
/// - **The #565 body-digest asymmetry.** Only *live passes* are digest-checked;
///   a `fail`/`escalate` whose body moved afterwards is the fix loop working as
///   intended, and re-staling it would ping-pong forever.
///
/// `verdicts` is what the group dir holds under `verdicts/pr-<N>/`, read with
/// `workflow::parse_verdict_file`; `head` is the PR's live head.
pub fn recheck_gate(
    spec: &GateSpec,
    verdicts: &BTreeMap<BlockId, ReviewVerdict>,
    head: Option<&str>,
    observed: &PrObservation,
) -> GateRecheck {
    let gate = match spec {
        GateSpec::Absent => return GateRecheck::NotConfigured,
        GateSpec::Malformed => return GateRecheck::Malformed,
        GateSpec::Declared(g) => g,
    };
    // The reviewer half, from the one definition. Note this is where "a stale
    // pass is not a pass" and "one fail beats any count of passes" live — they
    // are not re-derived here.
    let outcome = evaluate_merge_gate(gate, verdicts, head);
    if !outcome.satisfied() {
        return GateRecheck::Reviewers(outcome);
    }
    for c in &gate.also {
        let refusal = if !condition_supported(c) {
            // Checked FIRST: an unknown clause must fail closed even if it
            // happens to be spelled like one of the arms below.
            Some(ConditionRefusal::Unsupported(c.clone()))
        } else if c.as_str() == "ci-green" {
            match observed.ci_green {
                Some(true) => None,
                Some(false) => Some(ConditionRefusal::CiNotGreen),
                None => Some(ConditionRefusal::CiUnknown),
            }
        } else if c.as_str() == "body-unchanged" {
            body_unchanged(gate, verdicts, head, observed.body_digest.as_deref())
        } else {
            // `condition_supported` said yes and no arm handled it: a condition
            // was added to KNOWN_CONDITIONS without teaching the queue to check
            // it. Fail closed, exactly as the shim does for an unknown clause.
            Some(ConditionRefusal::Unsupported(c.clone()))
        };
        if let Some(refusal) = refusal {
            return GateRecheck::Condition { condition: c.clone(), refusal };
        }
    }
    GateRecheck::Ok
}

/// The `body-unchanged` clause (#565), mirroring the shim's loop: for every
/// reviewer the gate names, a verdict that is a **live pass** (the word `pass`,
/// recorded against the head that would merge) must carry the digest of the
/// body as it stands now. A pass the threshold does not need is still checked —
/// "this reviewer approved a different commit message" is true either way.
fn body_unchanged(
    gate: &Gate,
    verdicts: &BTreeMap<BlockId, ReviewVerdict>,
    head: Option<&str>,
    now: Option<&str>,
) -> Option<ConditionRefusal> {
    let (Some(head), Some(now)) = (head, now.filter(|d| !d.is_empty())) else {
        return Some(ConditionRefusal::BodyUnknown);
    };
    let mut bad: Vec<BlockId> = Vec::new();
    for r in &gate.reviewers {
        let Some(v) = verdicts.get(r) else { continue };
        if v.verdict.is_blocking() || !v.reviewed(head) {
            continue;
        }
        // An absent digest (a verdict recorded before #565, or one whose body
        // gh could not read) reads as EMPTY, which equals no digest and so
        // refuses.
        if v.body_digest != now {
            bad.push(r.clone());
        }
    }
    (!bad.is_empty()).then_some(ConditionRefusal::BodyChanged { reviewers: bad })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::workflow::{body_digest, GateRequire, Verdict};

    // ── the state machine (§4) ──────────────────────────────────────────────

    /// Every state the design note names, and no ninth. The list is spelled out
    /// rather than derived so that ADDING a variant is a visible test change.
    const ALL: [EntryState; 8] = [
        EntryState::Queued,
        EntryState::Batching,
        EntryState::CiWait,
        EntryState::Landing,
        EntryState::Bisecting,
        EntryState::Landed,
        EntryState::KickedBack,
        EntryState::Cancelled,
    ];

    /// Exactly the edges §4's diagram and §10's failure table name. Anything
    /// not here must be refused — `every_transition_outside_the_design_note_is_
    /// refused` walks the whole 8x8 product against this list.
    const LEGAL: [(EntryState, EntryState); 17] = [
        (EntryState::Queued, EntryState::Batching),
        (EntryState::Queued, EntryState::Cancelled),
        (EntryState::Batching, EntryState::CiWait),
        (EntryState::Batching, EntryState::KickedBack),
        (EntryState::Batching, EntryState::Queued),
        (EntryState::Batching, EntryState::Cancelled),
        (EntryState::CiWait, EntryState::Landing),
        (EntryState::CiWait, EntryState::Bisecting),
        (EntryState::CiWait, EntryState::KickedBack),
        (EntryState::CiWait, EntryState::Queued),
        (EntryState::CiWait, EntryState::Cancelled),
        (EntryState::Landing, EntryState::Landed),
        (EntryState::Landing, EntryState::KickedBack),
        (EntryState::Landing, EntryState::Queued),
        (EntryState::Landing, EntryState::Cancelled),
        (EntryState::Bisecting, EntryState::KickedBack),
        (EntryState::Bisecting, EntryState::Queued),
    ];

    #[test]
    fn the_design_notes_legal_transitions_are_all_accepted() {
        for (from, to) in LEGAL {
            assert_eq!(
                transition(from, to),
                Ok(to),
                "{} -> {} is in the design note and must be legal",
                from.as_str(),
                to.as_str()
            );
        }
        // Bisecting -> Cancelled is legal too, via the any-non-terminal arm.
        assert_eq!(
            transition(EntryState::Bisecting, EntryState::Cancelled),
            Ok(EntryState::Cancelled)
        );
    }

    #[test]
    fn every_transition_outside_the_design_note_is_refused() {
        for from in ALL {
            for to in ALL {
                let legal = LEGAL.contains(&(from, to))
                    || (to == EntryState::Cancelled && !from.is_terminal());
                let got = transition(from, to);
                if legal {
                    assert_eq!(got, Ok(to), "{} -> {}", from.as_str(), to.as_str());
                } else {
                    assert_eq!(
                        got,
                        Err(InvalidTransition { from, to }),
                        "{} -> {} is not in the design note and must be refused",
                        from.as_str(),
                        to.as_str()
                    );
                }
            }
        }
    }

    /// The three specific refusals worth naming on their own, because each is a
    /// thing a plausible driver would try: resurrecting a terminal entry,
    /// skipping the CI wait, and treating a no-op as a transition.
    #[test]
    fn terminal_states_never_move_and_ci_wait_is_not_skippable() {
        for from in [EntryState::Landed, EntryState::KickedBack, EntryState::Cancelled] {
            for to in ALL {
                assert!(
                    transition(from, to).is_err(),
                    "{} is terminal but accepted {}",
                    from.as_str(),
                    to.as_str()
                );
            }
        }
        // Landing without ever observing the batch's checks would land an
        // object CI never judged — the Bors invariant inverted (§8).
        assert!(transition(EntryState::Batching, EntryState::Landing).is_err());
        assert!(transition(EntryState::Queued, EntryState::Landed).is_err());
        // A blocked_reason refresh leaves the entry queued; it is not a
        // transition (§4 — "paused" is a live predicate, not a state).
        assert!(transition(EntryState::Queued, EntryState::Queued).is_err());
    }

    #[test]
    fn advance_is_the_only_state_mutation_and_refuses_the_same_way() {
        let mut e = QueueEntry::new(612, "abc", 0);
        assert_eq!(e.state(), EntryState::Queued);
        assert!(e.advance(EntryState::Batching).is_ok());
        assert_eq!(e.state(), EntryState::Batching);
        // Refused, and the entry does NOT move — a rejected transition must not
        // leave the entry somewhere in between.
        assert_eq!(
            e.advance(EntryState::Landed),
            Err(InvalidTransition { from: EntryState::Batching, to: EntryState::Landed })
        );
        assert_eq!(e.state(), EntryState::Batching);
    }

    #[test]
    fn state_words_round_trip_through_the_wire_spelling() {
        for s in ALL {
            assert_eq!(EntryState::parse(s.as_str()), Some(s));
        }
        assert_eq!(EntryState::as_str(EntryState::CiWait), "ci-wait");
        assert_eq!(EntryState::as_str(EntryState::KickedBack), "kicked-back");
        // Never coerced, never defaulted to something batchable.
        assert_eq!(EntryState::parse("paused"), None);
        assert_eq!(EntryState::parse("QUEUED"), None);
    }

    // ── the batch planner (§4, §8) ──────────────────────────────────────────

    fn queued(prs: &[u64]) -> MergeQueueState {
        MergeQueueState {
            entries: prs.iter().map(|p| QueueEntry::new(*p, "head", 0)).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn the_planner_takes_queue_order_up_to_max_batch() {
        let s = queued(&[612, 613, 614, 615]);
        assert_eq!(plan_batch(&s, 3), BatchPlan::Build(vec![612, 613, 614]));
        assert_eq!(plan_batch(&s, 1), BatchPlan::Build(vec![612]));
        assert_eq!(plan_batch(&s, 9), BatchPlan::Build(vec![612, 613, 614, 615]));
    }

    #[test]
    fn the_planner_holds_while_a_batch_is_in_flight() {
        let mut s = queued(&[612, 613]);
        s.batch = Some(BatchRecord::new("mq-0000dead", vec![612], 0));
        assert_eq!(plan_batch(&s, 3), BatchPlan::InFlight);

        // …and holds on the entries alone, even when the batch record is gone —
        // the crash case §4 reconciles must not dispatch a second batch.
        let mut s = queued(&[612, 613]);
        s.entries[0].advance(EntryState::Batching).unwrap();
        s.entries[0].advance(EntryState::CiWait).unwrap();
        assert!(s.batch.is_none());
        assert_eq!(plan_batch(&s, 3), BatchPlan::InFlight);
    }

    #[test]
    fn a_blocked_entry_is_skipped_but_stays_queued() {
        let mut s = queued(&[612, 613, 614]);
        s.entries[1].blocked_reason = Some("rebased past its verdicts".into());
        assert_eq!(plan_batch(&s, 3), BatchPlan::Build(vec![612, 614]));
        // §4: it is an eligibility predicate, NOT a ninth state.
        assert_eq!(s.entries[1].state(), EntryState::Queued);

        // Every entry blocked → nothing to dispatch, and specifically not an
        // empty batch.
        for e in s.entries.iter_mut() {
            e.blocked_reason = Some("stale".into());
        }
        assert_eq!(plan_batch(&s, 3), BatchPlan::Idle);
        assert_eq!(plan_batch(&MergeQueueState::default(), 3), BatchPlan::Idle);
    }

    // ── the bisect splitter (§9) ────────────────────────────────────────────

    #[test]
    fn bisect_shapes_for_k_of_one_two_and_three() {
        assert_eq!(bisect_step(&[]), BisectStep::Nothing);
        // k = 1: attributed with no further CI.
        assert_eq!(bisect_step(&[612]), BisectStep::Culprit(612));
        assert_eq!(
            bisect_step(&[612, 613]),
            BisectStep::Split { test: vec![612], rest: vec![613] }
        );
        // k = 3 splits 2/1 — larger half first.
        assert_eq!(
            bisect_step(&[612, 613, 614]),
            BisectStep::Split { test: vec![612, 613], rest: vec![614] }
        );
    }

    #[test]
    fn the_search_terminates_within_ceil_log2_k_runs() {
        // Walk the whole search for every k up to the queue cap, with every
        // possible culprit, counting the CI runs a driver would spend. §9
        // promises ceil(log2 k) and "bisect depth is bounded by max_batch, so
        // the search always terminates" — an unbounded splitter would hang here
        // rather than fail an assertion, which is why this drives the real loop.
        for k in 1..=8usize {
            let batch: Vec<u64> = (0..k as u64).collect();
            for culprit in &batch {
                let mut set = batch.clone();
                let mut runs = 0;
                loop {
                    match bisect_step(&set) {
                        BisectStep::Culprit(pr) => {
                            assert_eq!(pr, *culprit, "k={k} isolated the wrong entry");
                            break;
                        }
                        BisectStep::Nothing => panic!("k={k} lost the culprit"),
                        BisectStep::Split { test, rest } => {
                            assert!(!test.is_empty() && !rest.is_empty(), "a half was empty");
                            runs += 1;
                            // The half that reproduces red is the one holding
                            // the culprit.
                            set = if test.contains(culprit) { test } else { rest };
                        }
                    }
                }
                let bound = (usize::BITS - (k - 1).leading_zeros()) as usize; // ceil(log2 k)
                assert!(runs <= bound, "k={k} culprit={culprit} took {runs} runs, bound {bound}");
            }
        }
    }

    // ── `merge_queue.json` forward compatibility (§11.2/§11.3) ──────────────

    #[test]
    fn merge_queue_state_round_trip_preserves_unknown_fields() {
        // A file a NEWER build wrote: fields this build has never heard of, at
        // the file, the entry and the batch level.
        let text = r#"{
            "version": 1,
            "target": "feat/integration-batch-2",
            "entries": [
                { "pr": 612, "head": "abc", "state": "queued", "blocked_reason": null,
                  "enqueued_ms": 7, "batch": null, "priority": "high" }
            ],
            "batch": { "id": "mq-7f3a", "prs": [612], "scratch_sha": "def",
                       "draft_pr": 640, "state": "ci-wait", "started_ms": 9,
                       "bisect_depth": 2 },
            "second_target": "feat/other"
        }"#;
        let s: MergeQueueState = serde_json::from_str(text).expect("a newer file must still read");
        assert_eq!(s.target, "feat/integration-batch-2");
        assert_eq!(s.entries[0].state(), EntryState::Queued);
        assert_eq!(s.batch.as_ref().unwrap().state(), EntryState::CiWait);

        // Read is not enough: an ignored field is LOST on the next write, and
        // §11.2's promise is that an older build can read AND REWRITE without
        // destroying what a newer one wrote.
        let back: Value = serde_json::to_value(&s).unwrap();
        assert_eq!(back["second_target"], "feat/other");
        assert_eq!(back["entries"][0]["priority"], "high");
        assert_eq!(back["batch"]["bisect_depth"], 2);
        // …and the known fields are still where the schema says.
        assert_eq!(back["entries"][0]["pr"], 612);
        assert_eq!(back["batch"]["state"], "ci-wait");
    }

    #[test]
    fn an_unreadable_state_never_degrades_into_a_batchable_one() {
        // An unknown STATE is not an unknown field: there is no ninth state to
        // put it in, and a state loomux cannot interpret is not one it may act
        // on. Hard parse error → §4's reconcile fails the file loudly.
        let bad = r#"{"version":1,"entries":[{"pr":1,"state":"paused"}]}"#;
        assert!(serde_json::from_str::<MergeQueueState>(bad).is_err());
        // A file with no version at all is malformed, not "probably v1".
        let bad = r#"{"entries":[]}"#;
        assert!(serde_json::from_str::<MergeQueueState>(bad).is_err());

        // A file from a FUTURE schema parses structurally (so the driver can
        // leave it alone intact) but reports itself unsupported.
        let future: MergeQueueState = serde_json::from_str(r#"{"version":2,"entries":[]}"#).unwrap();
        assert!(!future.version_supported());
        assert!(MergeQueueState::default().version_supported());
    }

    // ── ids and the scratch-ref namespace (§11.4) ───────────────────────────

    #[test]
    fn batch_ids_are_namespaced_and_do_not_repeat_within_a_millisecond() {
        let a = new_batch_id(1_700_000_000_000);
        assert!(a.starts_with("mq-"), "{a}");
        assert_eq!(a.len(), "mq-".len() + 8);
        // Same clock reading, different ids: the entropy is RandomState's, not
        // the clock's, so a burst of mints inside one millisecond does not
        // collide (constraint 2 — no getrandom crates).
        let ids: std::collections::BTreeSet<String> =
            (0..64).map(|_| new_batch_id(1_700_000_000_000)).collect();
        assert!(ids.len() > 60, "only {} distinct ids from 64 mints", ids.len());
    }

    #[test]
    fn the_scratch_ref_cannot_be_talked_out_of_its_namespace() {
        assert_eq!(scratch_branch("g1", "mq-7f3a").as_deref(), Some("loomux/mq/g1-mq-7f3a"));
        // Every one of these would be a write outside `loomux/mq/*` — refused,
        // never rewritten into a different name.
        for (g, b) in [
            ("../..", "mq-1"),
            ("g/../..", "mq-1"),
            ("g1", "../../main"),
            ("g 1", "mq-1"),
            ("", "mq-1"),
            ("g1", ""),
            ("-g1", "mq-1"),
            ("g1", "mq-1\nrefs/heads/main"),
        ] {
            assert_eq!(scratch_branch(g, b), None, "group {g:?} batch {b:?} must not build a ref");
        }
    }

    // ── the gate re-check (§6) ──────────────────────────────────────────────

    fn gate(reviewers: &[&str], also: &[&str]) -> Gate {
        Gate {
            require: GateRequire::AllPass,
            reviewers: reviewers.iter().map(|r| r.to_string()).collect(),
            also: also.iter().map(|c| c.to_string()).collect(),
        }
    }

    fn verdict(block: &str, v: Verdict, head: &str, body: &str) -> (BlockId, ReviewVerdict) {
        (
            block.to_string(),
            ReviewVerdict {
                pr: 612,
                block: block.to_string(),
                agent_id: "rev-1".into(),
                verdict: v,
                head: head.into(),
                body_digest: body.to_string(),
                summary: String::new(),
                ts_ms: 0,
            },
        )
    }

    fn observed(ci: Option<bool>, body: Option<&str>) -> PrObservation {
        PrObservation { body_digest: body.map(|b| b.to_string()), ci_green: ci }
    }

    #[test]
    fn a_stale_pass_is_not_a_pass() {
        let g = GateSpec::Declared(gate(&["rev-a"], &[]));
        let v: BTreeMap<_, _> = [verdict("rev-a", Verdict::Pass, "OLD", "")].into();
        // Passed an earlier revision: the branch moved under the reviewer, so
        // what they approved is not what would land.
        match recheck_gate(&g, &v, Some("NEW"), &observed(None, None)) {
            GateRecheck::Reviewers(GateOutcome::Short { passes, stale, .. }) => {
                assert_eq!(passes, 0);
                assert_eq!(stale, vec!["rev-a".to_string()]);
            }
            other => panic!("a stale pass must not satisfy the gate: {other:?}"),
        }
        // The same verdict against the head that would land does pass.
        let v: BTreeMap<_, _> = [verdict("rev-a", Verdict::Pass, "NEW", "")].into();
        assert!(recheck_gate(&g, &v, Some("NEW"), &observed(None, None)).passed());
        // No resolvable head refuses outright — unknown is never "fine".
        assert_eq!(
            recheck_gate(&g, &v, None, &observed(None, None)),
            GateRecheck::Reviewers(GateOutcome::UnknownRevision)
        );
    }

    #[test]
    fn one_fail_beats_any_count_of_passes() {
        let g = GateSpec::Declared(Gate {
            require: GateRequire::Threshold(2),
            reviewers: vec!["rev-a".into(), "rev-b".into(), "rev-c".into()],
            also: vec![],
        });
        let v: BTreeMap<_, _> = [
            verdict("rev-a", Verdict::Pass, "NEW", ""),
            verdict("rev-b", Verdict::Pass, "NEW", ""),
            verdict("rev-c", Verdict::Fail, "NEW", ""),
        ]
        .into();
        // Two live passes meet the threshold on their own — and are still
        // refused, because blockers are checked before any counting.
        assert_eq!(
            recheck_gate(&g, &v, Some("NEW"), &observed(None, None)),
            GateRecheck::Reviewers(GateOutcome::Blocked { blocking: vec!["rev-c".into()] })
        );
        // `escalate` refuses identically: a gate must never be satisfiable by a
        // reviewer that declined to decide.
        let v: BTreeMap<_, _> = [
            verdict("rev-a", Verdict::Pass, "NEW", ""),
            verdict("rev-b", Verdict::Pass, "NEW", ""),
            verdict("rev-c", Verdict::Escalate, "NEW", ""),
        ]
        .into();
        assert!(!recheck_gate(&g, &v, Some("NEW"), &observed(None, None)).passed());
    }

    #[test]
    fn no_gate_configured_refuses_rather_than_passes() {
        let v = BTreeMap::new();
        // §6: `evaluate_merge_gate` with no gate returns ALLOWED, which is
        // right for the shim and wrong for the queue — the backend must not
        // push approved-by-nobody PRs under its own authority.
        let r = recheck_gate(&GateSpec::Absent, &v, Some("NEW"), &observed(None, None));
        assert_eq!(r, GateRecheck::NotConfigured);
        assert_eq!(r.refusal_code(), Some("gate-not-configured"));
        // A gate file that exists but cannot be read refuses too — the shim
        // refuses every merge on exactly these.
        assert_eq!(GateSpec::read(None), GateSpec::Absent);
        assert_eq!(GateSpec::read(Some("wat unparseable\n")), GateSpec::Malformed);
        let r = recheck_gate(&GateSpec::Malformed, &v, Some("NEW"), &observed(None, None));
        assert_eq!(r, GateRecheck::Malformed);
        // …reported inside §11.1's closed refusal vocabulary.
        assert_eq!(r.refusal_code(), Some("gate-not-met"));
        // And a real gate file reads back through the shim's own parser.
        assert_eq!(
            GateSpec::read(Some("require all-pass\nreviewer rev-a\nalso ci-green\n")),
            GateSpec::Declared(gate(&["rev-a"], &["ci-green"]))
        );
    }

    #[test]
    fn an_also_clause_the_queue_cannot_check_fails_closed() {
        let v: BTreeMap<_, _> = [verdict("rev-a", Verdict::Pass, "NEW", "")].into();
        // ci-green is the sub-PR's OWN checks (§6) — the batch's green is never
        // a substitute for it.
        let g = GateSpec::Declared(gate(&["rev-a"], &["ci-green"]));
        assert!(recheck_gate(&g, &v, Some("NEW"), &observed(Some(true), None)).passed());
        assert_eq!(
            recheck_gate(&g, &v, Some("NEW"), &observed(Some(false), None)),
            GateRecheck::Condition {
                condition: "ci-green".into(),
                refusal: ConditionRefusal::CiNotGreen
            }
        );
        // Not determinable is a refusal, not a pass.
        assert_eq!(
            recheck_gate(&g, &v, Some("NEW"), &observed(None, None)),
            GateRecheck::Condition {
                condition: "ci-green".into(),
                refusal: ConditionRefusal::CiUnknown
            }
        );
        // A condition no build knows fails closed — silently ignoring it would
        // turn a stricter-looking workflow file into a weaker one.
        let g = GateSpec::Declared(gate(&["rev-a"], &["no-live-agents-on-pr"]));
        assert_eq!(
            recheck_gate(&g, &v, Some("NEW"), &observed(Some(true), Some("d"))),
            GateRecheck::Condition {
                condition: "no-live-agents-on-pr".into(),
                refusal: ConditionRefusal::Unsupported("no-live-agents-on-pr".into())
            }
        );
    }

    #[test]
    fn body_unchanged_checks_live_passes_only() {
        let now = body_digest("the reviewed body");
        let then = body_digest("a body nobody reviewed");
        let g = GateSpec::Declared(gate(&["rev-a"], &["body-unchanged"]));

        // A live pass over the body as it stands: allowed.
        let v: BTreeMap<_, _> = [verdict("rev-a", Verdict::Pass, "NEW", &now)].into();
        assert!(recheck_gate(&g, &v, Some("NEW"), &observed(None, Some(&now))).passed());

        // The body moved after that pass: refused, naming the reviewer who must
        // re-read it (#565 — on a squash-merging repo the body becomes the
        // permanent commit message).
        let v: BTreeMap<_, _> = [verdict("rev-a", Verdict::Pass, "NEW", &then)].into();
        assert_eq!(
            recheck_gate(&g, &v, Some("NEW"), &observed(None, Some(&now))),
            GateRecheck::Condition {
                condition: "body-unchanged".into(),
                refusal: ConditionRefusal::BodyChanged { reviewers: vec!["rev-a".into()] }
            }
        );
        // A pass carrying NO digest (recorded before #565) refuses the same
        // way: unknown is never "unbound, therefore fine".
        let v: BTreeMap<_, _> = [verdict("rev-a", Verdict::Pass, "NEW", "")].into();
        assert!(!recheck_gate(&g, &v, Some("NEW"), &observed(None, Some(&now))).passed());
        // …and so does a body the driver could not read now.
        let v: BTreeMap<_, _> = [verdict("rev-a", Verdict::Pass, "NEW", &now)].into();
        assert_eq!(
            recheck_gate(&g, &v, Some("NEW"), &observed(None, None)),
            GateRecheck::Condition {
                condition: "body-unchanged".into(),
                refusal: ConditionRefusal::BodyUnknown
            }
        );
    }

    #[test]
    fn the_body_digest_asymmetry_is_preserved() {
        // #565's asymmetry: only PASSES are digest-checked. A fail/escalate
        // whose body moved afterwards is the fix loop working as intended, and
        // re-staling it would ping-pong forever — body finding → worker fixes
        // the body → verdict auto-stales → re-review → repeat.
        let now = body_digest("the body as it stands");
        let then = body_digest("the body that was failed");
        let g = GateSpec::Declared(Gate {
            require: GateRequire::Threshold(1),
            reviewers: vec!["rev-a".into(), "rev-b".into()],
            also: vec!["body-unchanged".into()],
        });
        let v: BTreeMap<_, _> = [
            verdict("rev-a", Verdict::Pass, "NEW", &now),
            // rev-b failed an older body. That refuses the merge on the
            // reviewer half (blockers beat approvals) — but it must refuse as
            // `Blocked`, NOT as a body-changed finding against rev-b.
            verdict("rev-b", Verdict::Fail, "NEW", &then),
        ]
        .into();
        assert_eq!(
            recheck_gate(&g, &v, Some("NEW"), &observed(None, Some(&now))),
            GateRecheck::Reviewers(GateOutcome::Blocked { blocking: vec!["rev-b".into()] })
        );

        // With the blocker cleared to a pass over the CURRENT body, the gate
        // opens — proving the previous refusal was the blocker and not a
        // digest complaint about a stale-bodied verdict.
        let v: BTreeMap<_, _> = [
            verdict("rev-a", Verdict::Pass, "NEW", &now),
            verdict("rev-b", Verdict::Pass, "NEW", &now),
        ]
        .into();
        assert!(recheck_gate(&g, &v, Some("NEW"), &observed(None, Some(&now))).passed());
    }
}

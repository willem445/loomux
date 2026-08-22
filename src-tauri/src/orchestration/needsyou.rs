//! The NEEDS-YOU item registry (#1151 slice A) — the durable record behind the
//! panel's demo and feedback rows.
//!
//! # Why an entity at all, when the board already had the statuses
//!
//! Before this, the panel's DEMOS tier was a **pure projection** of `tasks.json`
//! rows whose status was demo-gated: the panel entry *was* the task. That gives
//! the thing a human is being asked to look at no identity of its own — no "who
//! raised this and when", no close-out that is not also a board move, and no way
//! for an agent to ask for a look at something that is not a board row at all.
//! Resolving such a row could only mean moving the task, which is a different
//! decision from "I have seen this".
//!
//! So a needs-you item is a first-class record: it **owns who asked, when, why,
//! and open/resolved**, and it *links* a task rather than copying one.
//! `demo_path`, `pr`, `assignee` and the proceed/feedback affordances are joined
//! live from `tasks.json` at render — snapshotting them onto the item would be
//! the drift machine the panel's own header already warns about. The item is the
//! lifecycle record; the task keeps owning the facts.
//!
//! # Why this is not `humanq`, and not `AttentionItem`
//!
//! *A note on how [`super::humanq`]'s answer-source type is referred to below.*
//! It is named indirectly, never spelled, throughout this file — on purpose.
//! `the_mcp_surface_has_no_path_to_the_answer_entry_point` asserts that exactly
//! two source files under `src/` contain that identifier at all, so that a new
//! file naming it is a new answering surface and has to be argued for. Spelling
//! it here in prose would cost that guard an allowlist row it cannot verify —
//! "prose only" is not something a text scan can check, and the row would still
//! be sitting there on the day the prose became code. A doc link is not worth
//! weakening a security guard, so this file describes the type instead.
//!
//! **Not `humanq`.** `questions.json` is a shipped public contract with a
//! purpose-built trust boundary (a closed answer-source enum, two boundary tests),
//! and *answering* a question — which releases the work that was waiting on it —
//! is a different power from *resolving* an item, which acknowledges that a
//! human has looked. Folding the two stores together would widen the one surface
//! #946 spent three layers keeping narrow. The panel unions two registries; the
//! registries stay apart.
//!
//! **Not "attention".** `AttentionItem` is already taken by the pane-chip scan
//! (`mod.rs`, with a frontend mirror in `src/attention.ts`) — unrelated
//! machinery. This module keeps its own vocabulary behind the `needsyou::` path
//! (`needsyou::Item`, `needsyou::Kind`), following the precedent [`super::humanq`]
//! set for exactly this reason: pick a distinct word rather than overload one.
//!
//! # The resolve boundary
//!
//! An item is settled three ways, and they are deliberately not one operation:
//!
//! 1. **The human resolves it** — [`ResolveSource`] is a closed enum supplied by
//!    the entry point (`orch_needs_you_resolve` hard-codes
//!    [`ResolveSource::Webview`]), never a caller-supplied string, so "resolve as
//!    the human" has no spelling. Same shape and same reason as
//!    [`super::humanq`]'s own answer-source enum.
//! 2. **The raiser withdraws it** — an item overtaken by events should not need
//!    a human click. Settles as `withdrawn:<agent>`, which is visibly not a
//!    human's acknowledgement.
//! 3. **The board resolves it** — a task leaving the demo-gated statuses
//!    auto-resolves its demo item as `board:<new-status>`. The board moved, so
//!    the ask is moot.
//!
//! Taking your own ask back, and the board moving on, are both weaker than a
//! human saying "seen" — `resolved_by` is what keeps them distinguishable
//! forever, which is why every settle records one and none of them deletes a row.
//!
//! # Clearing is not deleting
//!
//! "Clear completed" is a per-group **watermark** (the `needs-you-cleared`
//! marker file, the `set_notify` marker precedent), not a row mutation and not a
//! delete: the panel hides settled rows stamped at or before it, and the rows on
//! disk are untouched. That is literally "clears the UI, persists on disk", it
//! survives a restart, and it can never touch an OPEN row.

use serde::{Deserialize, Serialize};

use super::notify::sanitize_gh_text;

/// The per-group file, beside `questions.json` / `tasks.json` in the group dir.
pub const NEEDS_YOU_FILE: &str = "needs-you.json";

/// The per-group clear-completed watermark marker (a decimal ms timestamp).
/// A marker file rather than a key inside `needs-you.json`, because the thing
/// being recorded is a fact about the *view*, and writing it into the record
/// would mean every "hide what I have seen" click rewrites the file holding
/// items a human has not seen yet.
pub const CLEARED_MARKER: &str = "needs-you-cleared";

/// The per-group marker that says the one-shot upgrade migration has run.
///
/// **Its presence is the whole of the once-ever guarantee**, and that guarantee
/// is load-bearing rather than tidy: the migration synthesizes demo items for a
/// board that predates the registry, so a second run has nothing to recover and
/// everything to break — it would re-raise a row the human had already resolved,
/// under a new id, on a task still sitting in its demo status. Written even when
/// the migration added nothing, because "already considered" and "found nothing
/// to do" are the same answer for every run after the first.
pub const MIGRATED_MARKER: &str = "needs-you-migrated";

/// Longest item body. [`super::humanq::QUESTION_TEXT_MAX`]'s budget, for the
/// same reason: generous, because a self-contained ask is the point, but bounded
/// because this text is rendered into a panel row and delivered into a pane.
pub const ITEM_TEXT_MAX: usize = 2000;

/// Longest close-out note a human may attach when resolving an item.
pub const RESOLUTION_TEXT_MAX: usize = 2000;

/// A board task's title is not written for this file, so it is cut to something
/// a panel row can hold before it becomes an auto-raised item's text. Cutting
/// rather than refusing, deliberately, and only here: [`validate_raise`] refuses
/// an over-long *ask* because the asker can see the refusal and rewrite it,
/// while the board hook has no author to tell — a refusal there would silently
/// lose the demo item instead of shortening a title.
pub const DEMO_TITLE_MAX: usize = 200;

/// Most items that may be OPEN in one group at once.
///
/// Not a rate limit — a backstop on the file, [`super::humanq::PENDING_MAX`]'s
/// argument applied here: reaching it means things are being queued for a human
/// faster than a human could ever work through them, which is worth refusing
/// loudly rather than absorbing silently.
pub const OPEN_MAX: usize = 32;

/// Resolved rows kept in the file. Older ones are dropped on the next write; the
/// audit log keeps all of them regardless, so this caps the hot read, not the
/// history.
pub const RESOLVED_RETAINED: usize = 20;

/// Resolved rows the MCP-side list projection returns alongside the open ones.
/// The omitted count travels with the response, so a filtered list is never
/// mistaken for the whole one.
pub const LIST_RESOLVED_CAP: usize = 10;

/// Total cap on the composed `[orrerix] n-N resolved …` notice, matching
/// [`super::humanq::ANSWER_NOTICE_CAP`]: this one also carries a human's own
/// words rather than a status line's payload.
pub const RESOLVE_NOTICE_CAP: usize = 2400;

/// How much of a raiser-supplied task ref is quoted into a notice. Short,
/// because a real one is `t-12`; bounded, because nothing validates the string
/// an ask attaches (see [`resolve_notice`]).
pub const NOTICE_TASK_MAX: usize = 64;

/// **How loudly this item wants the human's attention — the same vocabulary
/// questions use**, re-exported rather than re-declared.
///
/// The panel unions questions and items into ONE list and sorts it
/// urgency-first, so two identical enums would be two spellings of one word that
/// the sort then has to reconcile. Sharing the type is what makes
/// "urgency-pinned, then newest-first" a single comparison instead of a mapping
/// table.
pub use super::humanq::Urgency;

/// What kind of look the human is being asked for. A **closed** set: an item is
/// either "go try this" or "tell us what you think", and every surface that
/// renders one branches on exactly these two.
///
/// `question` is deliberately NOT a kind — questions live in [`super::humanq`]
/// behind their own trust boundary (see this module's header).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Something is built and parked for the human to go run and judge.
    Demo,
    /// An opinion is wanted — on a direction, a shape, a trade-off.
    Feedback,
}

impl Kind {
    /// Parse a tool argument. Unrecognized is an ERROR, never a defaulted kind,
    /// with [`Urgency::parse`]'s posture and for its reason: a raiser that wrote
    /// `"demos"` meant a demo, and filing it as feedback silently changes what
    /// the human is being asked to do.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "demo" => Ok(Kind::Demo),
            "feedback" => Ok(Kind::Feedback),
            other => Err(format!("unknown kind {other:?} — use \"demo\" or \"feedback\"")),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Demo => "demo",
            Kind::Feedback => "feedback",
        }
    }
}

/// Where an item is in its life. Two states, not three: withdrawal and a board
/// move are both *resolutions* with a different `resolved_by`, rather than
/// statuses of their own.
///
/// That asymmetry with [`humanq::Status`](super::humanq::Status) — which does
/// carry a separate `Withdrawn` — is deliberate. A question's terminal states
/// differ in whether the human's DECISION was ever obtained, which every reader
/// of a settled question needs. An item's do not: nobody decided anything, the
/// row is simply closed, and the provenance of the close lives in `resolved_by`
/// where a reader who cares can read it exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Open,
    Resolved,
}

impl Status {
    pub fn is_resolved(self) -> bool {
        matches!(self, Status::Resolved)
    }
    pub fn label(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Resolved => "resolved",
        }
    }
}

/// One thing waiting on the human, and how it was closed once it was.
///
/// Every field past the required core carries `#[serde(default)]` so a file
/// written by an older build still loads, following `Task`'s and
/// [`humanq::Question`](super::humanq::Question)'s convention.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// `n-1`, `n-2`, … — minted per group from the file's own high-water mark,
    /// exactly as task and question ids are (no getrandom — CLAUDE.md
    /// constraint 2). Legible rather than opaque, and never a capability:
    /// holding one grants nothing, because the only surfaces that act on one are
    /// trusted.
    pub id: String,
    pub kind: Kind,
    /// The agent that raised it, or `board` when the demo-gate hook did.
    /// Recorded rather than inferred, so the audit answers "who asked".
    pub raiser: String,
    pub text: String,
    /// The board row this is about. **Required for `demo`** (a demo with no task
    /// is a demo nobody can open) and optional for `feedback` (an opinion can be
    /// wanted before any row exists) — see [`validate_raise`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Serialized unconditionally, like `status` and unlike `task`: the
    /// `skip_serializing_if` in this struct marks the fields that can be
    /// genuinely ABSENT, while this one always has a value that decides where
    /// the row sorts. An open item is a record a human may read straight out of
    /// the file, so it says what it means rather than making the reader know the
    /// defaults.
    #[serde(default)]
    pub urgency: Urgency,
    pub status: Status,
    #[serde(default)]
    pub created_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_ms: Option<u64>,
    /// Who closed it: [`ResolveSource::tag`] (`webview`), `board:<new-status>`,
    /// or `withdrawn:<agent>`. See this module's resolve-boundary section for
    /// why all three are kept distinguishable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    /// The human's optional close-out note, verbatim as the trusted surface
    /// supplied it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
}

impl Item {
    /// Whether this row is a demo item for `task`, whatever state it is in.
    pub fn is_demo_for(&self, task: &str) -> bool {
        self.kind == Kind::Demo && self.task.as_deref() == Some(task)
    }

    /// Whether this row is the OPEN demo item for `task` — the ordinary dedupe
    /// key, in one place so the hook and an explicit raise cannot disagree about
    /// what "already raised" means.
    pub fn is_open_demo_for(&self, task: &str) -> bool {
        self.status == Status::Open && self.is_demo_for(task)
    }
}

/// **Which existing rows count as "this task has already been raised".**
///
/// The two scopes are genuinely different, and conflating them was a real
/// defect rather than a hypothetical one (rev-lead round 1, blocking 1): the
/// migration below ran on every read, deduped on [`Dedupe::OpenEpisode`], and so
/// minted a fresh demo item one refresh after the human resolved the previous
/// one — the row came back under a new id, and again on every subsequent
/// resolve. A human's close-out has to stick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dedupe {
    /// One OPEN demo row per task — what an ordinary raise wants. A settled row
    /// is a *closed episode*: a task that leaves the gate and comes back has
    /// genuinely been parked twice, and the second parking is a second ask that
    /// deserves its own row and its own timestamps.
    OpenEpisode,
    /// ANY demo row for this task, settled or not — what the one-shot migration
    /// wants. A settled row is proof the registry has already seen this task, so
    /// there is nothing to migrate, and re-raising would undo a close-out rather
    /// than recover a lost one. Never use this for a raise: it would make a
    /// re-parked task silently invisible for ever after its first episode.
    EverRaised,
}

impl Dedupe {
    fn matches(self, item: &Item, task: &str) -> bool {
        match self {
            Dedupe::OpenEpisode => item.is_open_demo_for(task),
            Dedupe::EverRaised => item.is_demo_for(task),
        }
    }
}

/// **Which trusted surface a resolve came from — a closed set, never a
/// caller-supplied string.**
///
/// [`super::humanq`]'s answer-source enum's shape, for the same
/// reason: each variant is an entry point loomux itself controls, there is no
/// variant for an agent, and adding one would defeat the feature rather than
/// extend it. The board's auto-resolve and an agent's withdraw are deliberately
/// NOT variants here — they are weaker settles that write their own tags, and
/// giving them a `ResolveSource` would let them be mistaken for a human's
/// acknowledgement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveSource {
    /// The human clicking in the app's own webview, via the
    /// `orch_needs_you_resolve` Tauri command.
    Webview,
}

impl ResolveSource {
    /// The stable string recorded on the item and in the audit log. `String`
    /// rather than `&'static str` for the question registry's equivalent tag's
    /// reason: a future surface carries an identity, and a signature that has to
    /// widen later is a signature every call site re-touches.
    pub fn tag(&self) -> String {
        match self {
            ResolveSource::Webview => "webview".to_string(),
        }
    }
}

/// What a raise was asked to register, after argument parsing and before
/// validation — split from the registry method so [`validate_raise`] is a pure
/// function of its input.
#[derive(Clone, Debug)]
pub struct RaiseRequest {
    pub kind: Kind,
    pub text: String,
    pub task: Option<String>,
    pub urgency: Urgency,
}

impl RaiseRequest {
    /// The board hook's request: a demo item for a task that just parked.
    pub fn demo_for(task_id: &str, title: &str, status: &str) -> Self {
        RaiseRequest {
            kind: Kind::Demo,
            text: demo_text(title, status),
            task: Some(task_id.to_string()),
            urgency: Urgency::Normal,
        }
    }
}

/// The text an auto-raised demo item carries. Pure, so the wording is pinned by
/// a test rather than by reading the hook.
pub fn demo_text(title: &str, status: &str) -> String {
    let title = title.trim();
    let cut: String = if title.chars().count() > DEMO_TITLE_MAX {
        title.chars().take(DEMO_TITLE_MAX).collect::<String>() + "…"
    } else {
        title.to_string()
    };
    format!("{cut} — parked in {status} for your look")
}

/// Normalize and bounds-check a raise, or say exactly what is wrong with it.
///
/// **Rejects rather than truncates**, [`super::humanq::validate_ask`]'s posture:
/// an ask silently cut at its cap is an ask whose actual point may have been the
/// part that was dropped, and the raiser has no way to see that happened.
///
/// # What this does NOT check: that `task` names a real board row
///
/// Stated rather than left to be discovered (rev-lead round 1, non-blocking 2).
/// Being pure is not the reason — the reason is that it is not a defect for
/// slice A's callers and *is* one for slice B's. Every raiser today is the board
/// hook, which supplies the id of a row it has just written, so the check would
/// have nothing to catch. Once `request_attention` ships, an agent naming a task
/// that does not exist — or one that will never move again — pins a permanently
/// open row on the human's queue: nothing auto-resolves it, because the hook
/// only ever fires on a real row's transition, and the only bounds left are
/// [`OPEN_MAX`] and a human or the raiser clearing it by hand.
///
/// **So the existence check belongs at that entry point, not here**, where the
/// board is in reach and the refusal can name the id. `raise_needs_you` is
/// deliberately not made board-aware for it: a registry method that reads
/// `tasks.json` to validate would take the board's lock from inside the items
/// lock, which is the nesting `needs_you_lock`'s doc rules out in the other
/// direction.
pub fn validate_raise(req: RaiseRequest) -> Result<RaiseRequest, String> {
    let text = req.text.trim().to_string();
    if text.is_empty() {
        return Err(
            "text required: an item with no body is nothing for a human to act on".into()
        );
    }
    if text.chars().count() > ITEM_TEXT_MAX {
        return Err(format!(
            "item text is {} characters, max {ITEM_TEXT_MAX} — say what to look at and what you \
             want back; the detail belongs on the issue or PR you cite",
            text.chars().count()
        ));
    }
    let task = req.task.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    // The one asymmetry between the kinds, and it is the whole of it: a demo is
    // "go run this", which needs a row to open, while feedback can legitimately
    // precede any board row at all.
    if req.kind == Kind::Demo && task.is_none() {
        return Err(
            "a demo item needs a task: the panel opens the board row to show what to run, so a \
             demo with nothing linked is a demo nobody can reach"
                .into(),
        );
    }
    Ok(RaiseRequest { kind: req.kind, text, task, urgency: req.urgency })
}

/// Bounds-check a close-out note from a trusted surface.
///
/// Trusted is not unbounded — the resolve dialog is a text area, and a paste of
/// a whole file would otherwise become a pane delivery.
pub fn validate_resolution(note: &str) -> Result<String, String> {
    let note = note.trim().to_string();
    if note.is_empty() {
        return Err("a resolution note needs text — resolve without one to close it silently".into());
    }
    if note.chars().count() > RESOLUTION_TEXT_MAX {
        return Err(format!(
            "resolution note is {} characters, max {RESOLUTION_TEXT_MAX}",
            note.chars().count()
        ));
    }
    Ok(note)
}

/// The outcome of a raise: the row the caller should quote back, and **whether
/// it is new**.
///
/// `fresh: false` means the raise deduped onto a row that already existed — the
/// caller must not audit a second open or write the file again, and a caller
/// with an actual author to answer (slice B's `request_attention`) must say so,
/// because a deduped raise **keeps the existing row's text and discards the new
/// ask's** (rev-lead round 1, non-blocking 3). Returning that bit rather than a
/// bare [`Item`] is what stops "I asked for a look at the empty state" from
/// silently becoming the board's generic "— parked in prototype for your look".
#[derive(Clone, Debug, PartialEq)]
pub struct Raised {
    pub item: Item,
    pub fresh: bool,
}

/// Admit a raise into an already-loaded item list: validate, dedupe, mint,
/// append.
///
/// **Pure, and the single place "already raised" is decided.** The board hook,
/// the one-shot migration and an explicit agent raise all come through here,
/// which is what makes "one open demo item per task, whoever asked" true by
/// construction rather than by three call sites agreeing — with the one
/// difference between them named in the [`Dedupe`] argument rather than left
/// implicit. `now_ms` is a parameter for the same reason: it keeps this a
/// function of its inputs, so a test can pin ordering without a clock.
pub fn admit(
    items: &mut Vec<Item>,
    raiser: &str,
    req: RaiseRequest,
    now_ms: u64,
    dedupe: Dedupe,
) -> Result<Raised, String> {
    let req = validate_raise(req)?;
    // Dedupe BEFORE the cap: a duplicate raise must stay idempotent even on a
    // full board, or the hook that re-raises on every transition would start
    // failing exactly when the queue is worst.
    if req.kind == Kind::Demo {
        if let Some(task) = req.task.as_deref() {
            if let Some(existing) = items.iter().find(|i| dedupe.matches(i, task)) {
                return Ok(Raised { item: existing.clone(), fresh: false });
            }
        }
    }
    let open = items.iter().filter(|i| !i.status.is_resolved()).count();
    if open >= OPEN_MAX {
        return Err(format!(
            "{open} items are already waiting on the human for this group (max {OPEN_MAX}) — \
             nobody works through a queue that size. Resolve or withdraw the ones overtaken by \
             events before raising another"
        ));
    }
    let item = Item {
        id: next_id(items),
        kind: req.kind,
        raiser: raiser.to_string(),
        text: req.text,
        task: req.task,
        urgency: req.urgency,
        status: Status::Open,
        created_ms: now_ms,
        resolved_ms: None,
        resolved_by: None,
        resolution: None,
    };
    items.push(item.clone());
    Ok(Raised { item, fresh: true })
}

/// The next id for this group: `n-{highest + 1}`, read off the file rather than
/// a counter, exactly as `q-N`/`t-N` are minted. Ids are never reused: resolved
/// rows are retained, and once dropped only ever from below the high-water mark
/// that produced them.
pub fn next_id(existing: &[Item]) -> String {
    let max: u32 = existing
        .iter()
        .filter_map(|i| i.id.strip_prefix("n-").and_then(|n| n.parse().ok()))
        .max()
        .unwrap_or(0);
    format!("n-{}", max + 1)
}

/// The notice delivered into the orchestrator's pane when a human resolves an
/// item **with a note**. A note-less resolve delivers nothing: it is the human
/// tidying their own queue, and a pane notice per tidy is noise.
///
/// **Both `note` and `task` are untrusted text entering an `[orrerix]` line.** The
/// note was typed by a human rather than an agent, but the pane cannot tell
/// those apart and a newline in it would forge a second line reading as its own
/// legitimate notice. The task ref is worse: nothing validates the string an ask
/// attached to its item, so it is raiser-controlled. Both go through
/// `sanitize_gh_text`. Only the id is loomux-built, and it is emitted FIRST so
/// the cap trims the note's tail rather than swallowing the attribution.
pub fn resolve_notice(id: &str, task: Option<&str>, note: &str) -> String {
    let body = sanitize_gh_text(note, RESOLUTION_TEXT_MAX);
    let about = match task {
        Some(t) => format!(" ({})", sanitize_gh_text(t, NOTICE_TASK_MAX)),
        None => String::new(),
    };
    let text = format!("[orrerix] the human resolved needs-you item {id}{about}: {body}");
    text.chars().filter(|c| !c.is_control()).take(RESOLVE_NOTICE_CAP).collect()
}

/// Drop the longest-RAISED resolved rows past `keep`, preserving every open one.
///
/// **Raise order, not resolve order**, and the distinction is real: the vector
/// is append-ordered by when an item was raised and nothing re-orders it on
/// settle, so a forward scan evicts by age-since-raising. That is the useful
/// order anyway (the oldest ask is the least likely to still be worth reading)
/// and it is not the same as evicting the longest-resolved row.
///
/// **Open rows are never pruned at any count** — an item the human has not
/// looked at is the one thing this file exists to not lose. [`OPEN_MAX`] is what
/// bounds those, by refusing new raises rather than by deleting old ones.
pub fn prune(items: &mut Vec<Item>, keep: usize) {
    let resolved = items.iter().filter(|i| i.status.is_resolved()).count();
    if resolved <= keep {
        return;
    }
    let mut to_drop = resolved - keep;
    items.retain(|i| {
        if to_drop > 0 && i.status.is_resolved() {
            to_drop -= 1;
            false
        } else {
            true
        }
    });
}

/// **What an AGENT is shown of an item — an explicitly enumerated projection,
/// never the stored struct.**
///
/// The field list here is a decision, and it is written down as one so that
/// adding a field to [`Item`] can never *by itself* put that field on an agent
/// surface. That failure class is not hypothetical — whole-struct serialization
/// onto an agent surface is exactly what went wrong on #1160 — and the cost of
/// pre-empting it is this struct.
///
/// **What is shown, and why.** Everything an agent needs to decide what to do
/// next: the id to quote, what was asked, which row it is about, how loud it is,
/// and — once settled — *how* it was settled. `resolved_by` is included
/// deliberately: an orchestrator must be able to tell "the human looked" from
/// "the board moved on" from "I withdrew this myself", which is the whole reason
/// those three tags stay distinguishable.
///
/// **What is withheld, and why.** `resolution` — the human's verbatim close-out
/// note. It is *not* a secret: `resolve_notice` delivers it, sanitized, into the
/// orchestrator's own pane, which is the surface it was written for. But
/// [`project_list`] feeds a **shared** read that every delegate may call, and a
/// note the human typed to their orchestrator is not thereby addressed to every
/// worker in the fleet. The narrower answer is the one that can be widened later
/// without a migration; the wider one cannot be narrowed without breaking a
/// contract. `had_resolution` carries the one bit an agent actually needs — that
/// a note exists at all, so it can ask rather than invent.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AgentItem {
    pub id: String,
    pub kind: Kind,
    pub raiser: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    pub urgency: Urgency,
    pub status: Status,
    pub created_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    /// Whether a human close-out note exists — not the note itself. See above.
    pub had_resolution: bool,
}

impl From<&Item> for AgentItem {
    /// Field-by-field on purpose: `..` spread or a derive would re-open exactly
    /// the hole this type exists to close.
    fn from(i: &Item) -> Self {
        AgentItem {
            id: i.id.clone(),
            kind: i.kind,
            raiser: i.raiser.clone(),
            text: i.text.clone(),
            task: i.task.clone(),
            urgency: i.urgency,
            status: i.status,
            created_ms: i.created_ms,
            resolved_ms: i.resolved_ms,
            resolved_by: i.resolved_by.clone(),
            had_resolution: i.resolution.is_some(),
        }
    }
}

/// What an agent-facing list returns: every open item (oldest first — the order
/// they were raised in), then the newest resolved rows up to `cap`, plus how
/// many resolved rows were left off.
///
/// Open rows are never omitted and never counted in the omitted total: a caller
/// reading this to decide what is still outstanding must see all of it.
///
/// Newest-first presentation is the PANEL's job, not this projection's — the
/// panel unions these with questions and sorts the union. Sorting here would put
/// a second, weaker ordering in the way of that one.
/// Returns [`AgentItem`]s, never [`Item`]s — see that type for why the
/// projection is the return value rather than something the caller is trusted to
/// remember to apply.
pub fn project_list(items: &[Item], cap: usize) -> (Vec<AgentItem>, usize) {
    let mut open: Vec<AgentItem> =
        items.iter().filter(|i| !i.status.is_resolved()).map(AgentItem::from).collect();
    let resolved: Vec<AgentItem> =
        items.iter().filter(|i| i.status.is_resolved()).map(AgentItem::from).collect();
    let omitted = resolved.len().saturating_sub(cap);
    open.extend(resolved.into_iter().skip(omitted));
    (open, omitted)
}

/// What the webview's read returns: the whole file plus the clear-completed
/// watermark, in one round trip so the panel cannot render rows against a
/// watermark it fetched separately (and possibly a moment earlier).
///
/// Uncapped, for `orch_questions_list`'s reason: retention already bounds the
/// file ([`OPEN_MAX`] open, [`RESOLVED_RETAINED`] resolved), so "everything" is
/// a bounded answer by construction — and a cap whose size the caller cannot see
/// is the silent truncation the rest of this feature refuses.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct View {
    pub items: Vec<Item>,
    /// Settled rows stamped at or before this are hidden by the panel. `0` means
    /// nothing has ever been cleared.
    pub cleared_ms: u64,
}

/// Parse the `needs-you-cleared` marker's body.
///
/// **Unparseable reads as 0, and that is a deliberate fail direction** — the
/// opposite of the items file's loud read, because the two failures are not
/// symmetric. A misread items file that came back empty would let the next write
/// destroy open items; a misread watermark that comes back 0 shows the human a
/// settled row they had already cleared. One loses a record, the other costs a
/// click. The watermark therefore fails toward showing MORE, never toward hiding
/// something the human has not seen.
pub fn parse_cleared(body: &str) -> u64 {
    body.trim().parse::<u64>().unwrap_or(0)
}

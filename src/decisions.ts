// Pure NEEDS-YOU helpers (#1091 slice C, reworked by #1151 slice C), kept
// DOM/Tauri-free so they can be unit-tested (`decisionsview.ts` wires the DOM +
// IPC and imports these). See test/decisions.test.ts.
//
// The panel is ONE human-attention surface over TWO registries, and it owns
// neither of them:
//
//   QUESTIONS — pending `ask_human` rows from `questions.json`, read through
//   the existing `orch_questions_list` command and answered through the
//   existing `orch_question_answer` one. The registry is the record; this is
//   the trusted surface that settles a row (doc/design/human-questions.md).
//
//   NEEDS-YOU ITEMS — first-class rows from `needs-you.json` (#1151 slice A,
//   doc/design/needs-you-items.md), read with `orch_needs_you_list` and closed
//   out with `orch_needs_you_resolve`. An item OWNS who asked, when, what for
//   and open/resolved; everything about the board row it names — title, status,
//   demo path, PR, assignee, whether Proceed applies — is JOINED LIVE from
//   `tasks.json` at render time (`linkTask`) and never snapshotted, because a
//   second record about board state is the drift machine.
//
// **The demo tier used to be a pure projection of `tasks.json`** — every
// demo-gated row became a card, and that card WAS the task. That is the model
// #1151 replaced: it gave a panel entry no identity, no timestamps and no
// close-out, so "I have looked" and "I have decided" were the same gesture. The
// backend's transition hook now keeps one open demo item per parked task, so
// this module joins rather than projects.
//
// Nothing in this module reads or writes anything — it is a projection plus a
// selection state machine, which is exactly the part worth pinning with tests.

// Explicit `.ts`, like `agenticons.ts`/`channel.ts`: this module is imported
// directly by `node --test`, which resolves real files rather than Vite's
// extensionless specifiers.
import { canApprove, canProceed } from "./taskboard.ts";

// ---------- the question wire shape ----------

/** One question's life stage, as `humanq::Status` serializes it. Both terminal
 *  states are settled; only `pending` is answerable. */
export type QuestionStatus = "pending" | "answered" | "withdrawn";

/** How many of a question's options the human may pick (`humanq::Select`). */
export type SelectMode = "single" | "multi";

/** How loudly the ask wants attention (`humanq::Urgency`). */
export type Urgency = "normal" | "high";

/** One option exactly as it arrives on the wire: `humanq::OptionSpec` is an
 *  UNTAGGED serde enum, so a description-less option is a bare string and one
 *  that carries reasoning is an object. Both shapes are live on disk at once —
 *  a Q1-era `questions.json` has only strings — so the panel must handle
 *  either, which is what `normalizeOptions` is for. */
export type WireOption = string | { label: string; description?: string };

/** An option after normalization: always an object, `description` present only
 *  when there is real text. The backend already normalizes an empty
 *  description back to the bare-string form on write, so the two agree — this
 *  is the reader's half of that same rule, not a second opinion about it. */
export interface DecisionOption {
  label: string;
  description?: string;
}

/** A row of `questions.json` as `orch_questions_list` returns it.
 *
 *  Every optional field here is optional for a REASON the backend states, not
 *  as defensive typing: `options`/`task`/`answer`/`settled_by`/`settled_ms`
 *  carry `skip_serializing_if`, so their keys are genuinely ABSENT rather than
 *  null. `select`/`allow_free_text`/`urgency`/`created_ms` are serialized
 *  unconditionally by the current backend, but are typed optional here so a
 *  file written by an older build — which is exactly what the additive schema
 *  promises will still load — cannot crash the panel. */
export interface OrchQuestion {
  id: string;
  asker: string;
  text: string;
  options?: WireOption[];
  select?: SelectMode;
  allow_free_text?: boolean;
  task?: string | null;
  urgency?: Urgency;
  status: QuestionStatus;
  created_ms?: number;
  answer?: string | null;
  settled_by?: string | null;
  settled_ms?: number | null;
}

/** The board row this panel reads. Structural, in `taskboard.ts`'s idiom: the
 *  fields the panel actually renders, so a test can build one without
 *  assembling a whole `OrchTask`. */
export interface DemoTask {
  id: string;
  title: string;
  status: string;
  assignee?: string | null;
  pr?: string | null;
  /** Where a demo of this row lives (#1091 slice B). Absent means "no path
   *  recorded", NEVER "there is no demo" — the panel says so rather than
   *  guessing a path from an assignee's cwd. */
  demo_path?: string | null;
}

// ---------- caps mirrored from the backend ----------

/** Mirror of `humanq::ANSWER_TEXT_MAX`. The backend REJECTS an over-cap
 *  answer rather than truncating it, so the panel must not let the human
 *  compose one and discover that at submit time — `canSubmit` enforces the
 *  same bound on the COMPOSED string, which is what actually travels. */
export const ANSWER_MAX = 2000;

/** How many settled rows the panel keeps in its faded tail — over BOTH
 *  registries at once, since #1151 unified the tail. Mirrors
 *  `humanq::LIST_SETTLED_CAP` and `needsyou::LIST_RESOLVED_CAP`, which are the
 *  same caps the MCP projections apply, so the human and the orchestrator see
 *  the same depth of history. Both registries retain more than this
 *  (`SETTLED_RETAINED` / `RESOLVED_RETAINED`); this is a display cap, not a
 *  deletion. */
export const SETTLED_SHOWN = 10;

// ---------- decisions: projection ----------

/** True for a question that can still be answered. */
export function isPending(q: OrchQuestion): boolean {
  return q.status === "pending";
}

// A question-only `projectQuestions` used to live here, splitting the file into
// pending (oldest-first) and a settled tail. `projectPanel` replaced it: the
// panel is one list over two registries now, and a second projection with its
// own opposite sort would be exactly the drift this module's header warns
// about. Questions still keep their own registry — only the RENDER is unified.

/** Normalize a question's options to the object form, dropping an option whose
 *  label is blank (nothing to put on a button) and a description that is only
 *  whitespace. Absent `options` is an empty list — the key is omitted, not
 *  `[]`, on a question that has none, so nothing may index it blind. */
export function normalizeOptions(options: readonly WireOption[] | undefined): DecisionOption[] {
  const out: DecisionOption[] = [];
  for (const o of options ?? []) {
    const label = (typeof o === "string" ? o : o.label ?? "").trim();
    if (!label) continue;
    const description = typeof o === "string" ? "" : (o.description ?? "").trim();
    out.push(description ? { label, description } : { label });
  }
  return out;
}

/** Whether this question's answer surface offers a free-text box.
 *
 *  Two rules, and the second is the one worth writing down: free text is the
 *  human's escape from an agent's list, so it is allowed unless the ask
 *  explicitly opted out (`allow_free_text: false` — absent means allowed, which
 *  is also the right reading of a Q1-era row whose only answer surface WAS free
 *  text). And a question with no options can never deny it, because denying it
 *  would leave nothing to answer at all — the backend refuses to store that
 *  combination, and this agrees with it rather than trusting the file. */
export function freeTextAllowed(q: OrchQuestion): boolean {
  if (normalizeOptions(q.options).length === 0) return true;
  return q.allow_free_text !== false;
}

/** `single` unless the ask said otherwise. An unrecognized value reads as
 *  `single`: the backend rejects an unknown `select` at ask time, so anything
 *  else here is a file this build does not understand, and offering ONE choice
 *  when the truth might be several is the reading that cannot silently invent
 *  a multi-select the asker never requested. */
export function selectMode(q: OrchQuestion): SelectMode {
  return q.select === "multi" ? "multi" : "single";
}

/** The board task a question is holding up, normalized: a blank or absent
 *  `task` is `null`. This is what the panel's card-to-board link routes on
 *  (`Question.task` — "what lets the orchestrator un-block exactly one task"),
 *  and it is already in the schema, so the cross-link needs no new field. */
export function citedTask(q: OrchQuestion): string | null {
  const t = (q.task ?? "").trim();
  return t || null;
}

/** Whether the ask asked to be shouted about. Drives presentation only — an
 *  urgent row is not acted on differently, it is just harder to miss, and it
 *  sorts above the quiet ones (`compareOpen`).
 *
 *  **Structural in `urgency`, so ONE spelling serves both registries.** A
 *  question and a needs-you item carry the same `humanq::Urgency`, imported
 *  rather than cloned backend-side for exactly this reason (see
 *  needs-you-items.md); two readers of it here would be two spellings of one
 *  word that the union sort then has to reconcile. */
export function isUrgent(row: { urgency?: Urgency }): boolean {
  return row.urgency === "high";
}

// ---------- the needs-you item wire shape ----------

/** What an item is asking for (`needsyou::Kind`) — a closed set.
 *
 *  `question` is deliberately not one: questions stay in their own registry
 *  behind their own trust boundary, and the panel unions the two. */
export type ItemKind = "demo" | "feedback";

/** Where an item is in its life (`needsyou::Status`). TWO states, not three:
 *  a withdrawal and a board move are resolutions with a different
 *  `resolved_by`, not statuses of their own. */
export type ItemStatus = "open" | "resolved";

/** One row of `needs-you.json` as `orch_needs_you_list` returns it.
 *
 *  Optional exactly where the backend's `skip_serializing_if` makes a key
 *  genuinely ABSENT (`task`, `resolved_*`, `resolution`), plus the additive
 *  ones (`urgency`, `created_ms`) which the current build always writes but an
 *  older file may not carry — the same reading `OrchQuestion` takes of the
 *  registry beside it. */
export interface NeedsYouItem {
  id: string;
  kind: ItemKind;
  /** The agent that raised it, or `board` when the demo-gate hook did. */
  raiser: string;
  text: string;
  /** The board row this is ABOUT — required for a demo, optional for feedback.
   *  Never a snapshot of that row: see `linkTask`. */
  task?: string | null;
  urgency?: Urgency;
  status: ItemStatus;
  created_ms?: number;
  resolved_ms?: number | null;
  /** `webview` \| `board:<new-status>` \| `withdrawn:<agent>` — the three ways
   *  a row settles, kept distinguishable on purpose. */
  resolved_by?: string | null;
  /** The human's optional close-out note. */
  resolution?: string | null;
}

/** Exactly what `orch_needs_you_list` returns: the rows AND the
 *  clear-completed watermark, in one round trip.
 *
 *  One call rather than two is the backend's own contract and it matters here:
 *  the panel hides settled rows stamped at or before the watermark, so two
 *  reads would let it render this second's rows against last second's stamp and
 *  flash back a row the human had just cleared. */
export interface NeedsYouView {
  items: NeedsYouItem[];
  /** Settled rows stamped at or before this are hidden. `0` means nothing has
   *  ever been cleared — and `0` is NOT a watermark that hides a row stamped
   *  `0`, see `isCleared`. */
  cleared_ms: number;
}

/** What the panel renders before its first read resolves, and what a failed
 *  read leaves behind. */
export const EMPTY_VIEW: NeedsYouView = { items: [], cleared_ms: 0 };

/** Mirror of `needsyou::RESOLUTION_TEXT_MAX`. The backend REJECTS an over-cap
 *  note rather than truncating it (`validate_resolution`), so the panel stops
 *  the human before the click rather than after it — `ANSWER_MAX`'s rule, for
 *  the same reason. */
export const RESOLUTION_MAX = 2000;

/** True for an item still waiting on the human. */
export function isOpenItem(i: NeedsYouItem): boolean {
  return i.status === "open";
}

/** The board row an item names, normalized: blank or absent is `null`. */
export function itemTask(i: NeedsYouItem): string | null {
  const t = (i.task ?? "").trim();
  return t || null;
}

/** What a close-out note actually sends, or `null` for a note-less resolve.
 *
 *  **A blank box must become `null`, never `""`.** `validate_resolution`
 *  REFUSES an empty note ("resolve without one to close it silently"), so
 *  sending the empty string would turn the ordinary tidy — the common case,
 *  and the one that deliberately delivers no pane notice — into an error the
 *  human did nothing to earn. */
export function resolveNote(text: string): string | null {
  return text.trim() || null;
}

/** Why a close-out note cannot be sent as typed, or `null` when it can.
 *
 *  Only one reason exists: an empty box is not a block, it is a note-less
 *  resolve (`resolveNote`). The cap counts CHARACTERS, not UTF-16 units,
 *  because `validate_resolution` counts `chars()` — a naive `.length` would
 *  refuse an all-astral note the backend would have taken. */
export function resolveBlock(text: string): "too-long" | null {
  const note = resolveNote(text);
  if (note && [...note].length > RESOLUTION_MAX) return "too-long";
  return null;
}

// ---------- the live task join ----------

/** Which backend verb this row's **Feedback** gesture must use.
 *
 *  The demo tier spans two statuses and the backend accepts a DIFFERENT verb
 *  for each, so one button cannot mean one call:
 *
 *  - `"merge-gate"` — `orch_request_changes`, whose `ensure_at_merge_gate`
 *    admits only `MERGE_GATE_STATUSES` (`pr`/`human-testing`). It records the
 *    findings, delivers a typed notice, and the caller then reopens the row as
 *    working so the panel cannot keep offering a gate action on a row that just
 *    had changes asked of it.
 *  - `"note"` — a plain board note through `orch_upsert_task`, which has no
 *    status gate at all and still reaches the orchestrator (`notify_board_edit`).
 *    This is what a `prototype` row gets, and it is the plan's own answer (D7:
 *    a comment without findings rides the ordinary board-note path).
 *
 *  **Why a `prototype` row keeps the button at all**, rather than having it
 *  hidden: `prototype` IS the #147 demo gate, so it is precisely the row where
 *  "I looked, and this is not right" has to be sayable. Removing the
 *  affordance would leave the tier half-actionable — Proceed or nothing — which
 *  is the opposite of what the gate is for. And unlike the merge-gate verb, a
 *  note deliberately does NOT move the status: a prototype that has received
 *  feedback is still a prototype until the orchestrator or the human's own
 *  Proceed moves it, and flipping it here would silently consume the gate. */
export type FeedbackRoute = "merge-gate" | "note";

/** Which verb `status` admits. TOTAL over the demo tier — every demo-gated row
 *  has a working feedback path, so there is no third "cannot give feedback"
 *  state for a caller to forget to handle.
 *
 *  `canApprove` is imported rather than re-spelled: it is the frontend's
 *  existing mirror of `ensure_at_merge_gate`, already used by the board's own
 *  Changes button, and two spellings of one backend guard is exactly the drift
 *  this module's header warns about. */
export function feedbackRoute(status: string): FeedbackRoute {
  return canApprove(status) ? "merge-gate" : "note";
}

/** What one press of **Send** (or Ctrl+Enter) on the feedback dialog owes,
 *  given the route and what an earlier press of the same dialog already
 *  achieved.
 *
 *  This lives here rather than inline in the dialog because the dialog cannot
 *  pin it: `decisionsview.ts`'s `submit()` is DOM + IPC wiring, so a mutation
 *  deleting the guard there reddens nothing. Here it reddens a named test.
 *
 *  Two duplicate-write windows, both opened by the fix that made the dialog
 *  close on SUCCESS rather than before the write — the right trade (keeping the
 *  human's text beats losing it), but the old close-first shape could not be
 *  re-submitted because the dialog was already gone:
 *
 *  1. **A second press while the first write is outstanding.** Hitting
 *     Ctrl+Enter twice is an ordinary habit when a dialog does not visibly
 *     respond. On a merge-gate row that used to issue two `orch_request_changes`
 *     calls — two `Requested changes: …` notes on one task and two `[loomux]`
 *     deliveries for one decision. `inFlight` makes the second press a no-op.
 *  2. **A retry after a PARTIAL failure.** The merge-gate route is a two-call
 *     chain. If `orch_request_changes` lands and the status flip then fails, the
 *     dialog re-enables Send — and a retry that re-ran the whole chain would
 *     record the findings a second time. `findingsLanded` narrows the retry to
 *     the call that actually failed.
 *
 *  The note route is a single call, so it has no partial state to remember and
 *  `findingsLanded` never applies to it.
 *
 *  **`status-only` deliberately re-sends nothing, and that is a trade, not an
 *  oversight.** The findings the backend already holds are the ones from the
 *  press that succeeded; if the human amends the text in the still-open
 *  textarea before retrying, the amendment is DROPPED. The alternative —
 *  re-recording — is the duplicate write this whole guard exists to stop, so
 *  losing the edit is the better failure, but it is silent, and a caller that
 *  wants to surface it should do so at the dialog rather than by widening this
 *  step. The same edge in the other direction: the caller checks for empty text
 *  BEFORE asking for a step, so a retry with the box cleared is a no-op and the
 *  owed status flip never happens.
 *
 *  **Scope: one dialog instance.** `state` is per-dialog, so this closes a
 *  second PRESS, not a second DIALOG. A human who dismisses the dialog while a
 *  write is in flight and reopens it gets fresh state, and a Send there is a
 *  genuine second write. That takes two deliberate gestures rather than one
 *  habitual double-tap, so it is left as it is — but it is the one crossing
 *  this function does not cover, and it is named here rather than left for the
 *  next reader to discover. */
export interface FeedbackSubmitState {
  /** A write is outstanding for this dialog — Send is disabled, but the
   *  keyboard path reaches `submit` regardless of the button's state. */
  inFlight: boolean;
  /** `orch_request_changes` has already succeeded for this dialog. Merge-gate
   *  only; always `false` on the note route. */
  findingsLanded: boolean;
}

/** TOTAL over `(route, state)` — there is no "cannot submit" value, because a
 *  press that must do nothing is `"ignore"`, which the caller must handle
 *  explicitly rather than by falling through. */
export type FeedbackStep =
  /** Do nothing: a write for this dialog is already outstanding. */
  | "ignore"
  /** Record the findings, then flip the status. The merge-gate chain. */
  | "findings-then-status"
  /** The findings are already recorded; only the status flip is still owed. */
  | "status-only"
  /** One board note, and deliberately no status change. */
  | "note";

export function feedbackSubmitStep(
  route: FeedbackRoute,
  state: FeedbackSubmitState
): FeedbackStep {
  if (state.inFlight) return "ignore";
  if (route === "note") return "note";
  return state.findingsLanded ? "status-only" : "findings-then-status";
}

/** The board facts an item's card shows, joined LIVE from `tasks.json`.
 *
 *  Never stored on the item and never cached: the item owns the ask, the task
 *  keeps owning the facts (needs-you-items.md). `path` is `null` when the
 *  orchestrator recorded none — the panel then shows the PR link alone rather
 *  than guessing a worktree, because a caption is a claim. */
export interface LinkedTask {
  id: string;
  title: string;
  status: string;
  path: string | null;
  pr: string | null;
  assignee: string | null;
  /** Whether **Proceed** applies — only a `prototype` can be promoted, the
   *  same guard the board's button uses and the backend's `ensure_prototype`
   *  enforces. A `human-testing` row gets feedback, not a promote. */
  canProceed: boolean;
  /** Which verb this row's **Feedback** button must call. See
   *  [`feedbackRoute`] — the two demo statuses do not accept the same one, and
   *  sending the merge-gate verb at a `prototype` row is refused backend-side
   *  with the human's typed findings already gone. */
  feedback: FeedbackRoute;
}

/** Join the board row `taskId` names, or `null` when there is nothing to join.
 *
 *  **`null` is a first-class outcome, not a failure.** An item outlives the row
 *  it names: a task can be pruned, renamed or never have existed (nothing
 *  validates the `task` string an agent attaches to a `feedback` ask — see
 *  `validate_raise`'s own note on that), and a `feedback` item may legitimately
 *  carry no task at all. The card degrades to the item's own text and STAYS
 *  RESOLVABLE, because an item the human can see but cannot clear is worse than
 *  one with a missing caption. */
export function linkTask(
  taskId: string | null | undefined,
  tasks: readonly DemoTask[]
): LinkedTask | null {
  const want = (taskId ?? "").trim();
  if (!want) return null;
  const t = tasks.find((row) => row.id === want);
  if (!t) return null;
  const clean = (v: string | null | undefined): string | null => {
    const s = (v ?? "").trim();
    return s || null;
  };
  return {
    id: t.id,
    title: t.title,
    status: t.status,
    path: clean(t.demo_path),
    pr: clean(t.pr),
    assignee: clean(t.assignee),
    // `canProceed` from taskboard.ts, not a second inline `=== "prototype"`:
    // it is the frontend's existing mirror of the backend's `ensure_prototype`,
    // and one rule spelled twice is how the two drift.
    canProceed: canProceed(t.status),
    feedback: feedbackRoute(t.status),
  };
}

// ---------- the unified projection ----------

/** One row of the panel's open list — a pending question or an open item.
 *
 *  A tagged union rather than a lowest-common-denominator record: the two
 *  sources are genuinely different things (one is answered, the other is
 *  acknowledged), and flattening them into one shape is how a panel starts
 *  offering the wrong verb on the wrong card. */
export type OpenRow =
  | {
      source: "question";
      /** What the card carries as `data-item-id` — see `anchorOf`. */
      anchor: string;
      urgent: boolean;
      createdMs: number;
      question: OrchQuestion;
    }
  | {
      source: "item";
      anchor: string;
      urgent: boolean;
      createdMs: number;
      item: NeedsYouItem;
      /** The live join, `null` when the linked row is absent or there is none. */
      task: LinkedTask | null;
    };

/** One row of the faded tail: a settled question or a resolved item. */
export type SettledRow =
  | { source: "question"; anchor: string; settledMs: number; question: OrchQuestion }
  | { source: "item"; anchor: string; settledMs: number; item: NeedsYouItem };

/** What the panel renders in one pass. */
export interface PanelProjection {
  /** Urgency-pinned, then newest-first. */
  open: OpenRow[];
  /** Newest-settled-first, watermark-filtered, capped. */
  settled: SettledRow[];
  /** How many settled rows the CAP dropped. Deliberately not counting the ones
   *  the watermark hid: those the human cleared on purpose, and reporting them
   *  back as "12 older not shown" would contradict the gesture. */
  omitted: number;
}

/** What a card carries as `data-item-id`, which is also what a deep-link
 *  targets (`focusItem`).
 *
 *  **An OPEN DEMO item anchors on its TASK id**, because that is what the
 *  board's marker chip emits (`boardMarker` → `{kind:"demo", target: task.id}`,
 *  #1091 slice G) and the board has no idea an `n-N` exists. Everything else
 *  anchors on its own id — a question keeps `q-N`, and a feedback item or any
 *  settled row keeps `n-N`.
 *
 *  The rule is exactly as wide as the link that needs it. A settled demo row
 *  anchoring on `t-N` too would give one task id two cards, and
 *  `querySelector` would resolve the deep-link to whichever rendered first;
 *  the dedupe guarantees at most one OPEN demo item per task, so the open
 *  anchor is unique. */
export function anchorOf(row: NeedsYouItem, open: boolean): string {
  if (open && row.kind === "demo") return itemTask(row) ?? row.id;
  return row.id;
}

/** Whether a settled row is hidden by the clear-completed watermark.
 *
 *  **`cleared === 0` hides nothing, and that guard is load-bearing.** `0` is
 *  the "never cleared" sentinel, and a row with no settle timestamp reads as
 *  `0` too, so a bare `settled <= cleared` would blank the entire tail of a
 *  group nobody has ever cleared.
 *
 *  A settled row that carries no timestamp (a file written before the field
 *  existed) IS hidden once the human clears — it is by definition older than
 *  anything stamped, and the alternative is a row that can never be cleared. */
export function isCleared(settledMs: number, cleared: number): boolean {
  return cleared > 0 && settledMs <= cleared;
}

/** Reconcile a freshly-read watermark with the one already held.
 *
 *  **The watermark only ever moves FORWARD, so `max` is the rule and not a
 *  heuristic**: `clear_needs_you` stamps `now`, and nothing anywhere lowers it.
 *
 *  This exists because the panel learns the stamp two ways — from a read, and
 *  from the value `orch_needs_you_clear` hands back — and the second is not a
 *  read, so it can be newer than a read already in flight. A refresh that
 *  started before the human clicked **Clear completed** carries the PRE-clear
 *  stamp, and assigning it wholesale would bring back the tail they just
 *  dismissed, until some later event happened to re-read the marker.
 *
 *  `NeedsYouView`'s one-call design closes the two-READS version of this (rows
 *  rendered against a stamp fetched a moment apart). This closes the same
 *  symptom arriving from the local-apply side, which that design cannot see. */
export function mergeCleared(fresh: number, held: number): number {
  return Math.max(fresh, held);
}

/** The sort (#1151 decision D1): **urgency-pinned, then newest-first.**
 *
 *  Newest-first because the old oldest-first order forced a long scroll to
 *  reach the thing that just arrived, which is the complaint. Urgency above it
 *  because an ask that said `high` said so precisely to jump the queue, and a
 *  strict newest-first would bury it under every routine row raised since.
 *
 *  **Ties keep input order** (`Array.prototype.sort` is stable), and the input
 *  is items-then-questions, each in its own file order — so two rows created in
 *  the same millisecond, or two carrying no `created_ms` at all, render in a
 *  fixed order rather than shuffling between refreshes. A panel that reorders
 *  under a cursor for no reason is one the human stops trusting. */
export function compareOpen(a: OpenRow, b: OpenRow): number {
  if (a.urgent !== b.urgent) return a.urgent ? -1 : 1;
  return b.createdMs - a.createdMs;
}

/** Everything waiting on the human, in one list, plus the settled tail.
 *
 *  Takes the whole `NeedsYouView` rather than rows and a watermark separately,
 *  so the two cannot come from different reads — the reason the backend returns
 *  them together in the first place. */
export function projectPanel(
  view: NeedsYouView,
  questions: readonly OrchQuestion[],
  tasks: readonly DemoTask[],
  cap: number = SETTLED_SHOWN
): PanelProjection {
  const open: OpenRow[] = [];
  for (const item of view.items) {
    if (!isOpenItem(item)) continue;
    open.push({
      source: "item",
      anchor: anchorOf(item, true),
      urgent: isUrgent(item),
      createdMs: item.created_ms ?? 0,
      item,
      task: linkTask(itemTask(item), tasks),
    });
  }
  for (const question of questions) {
    if (!isPending(question)) continue;
    open.push({
      source: "question",
      anchor: question.id,
      urgent: isUrgent(question),
      createdMs: question.created_ms ?? 0,
      question,
    });
  }
  open.sort(compareOpen);

  const settledAll: SettledRow[] = [];
  for (const item of view.items) {
    if (isOpenItem(item)) continue;
    settledAll.push({
      source: "item",
      anchor: anchorOf(item, false),
      settledMs: item.resolved_ms ?? 0,
      item,
    });
  }
  for (const question of questions) {
    if (isPending(question)) continue;
    settledAll.push({
      source: "question",
      anchor: question.id,
      settledMs: question.settled_ms ?? 0,
      question,
    });
  }
  const visible = settledAll
    .filter((r) => !isCleared(r.settledMs, view.cleared_ms))
    .sort((a, b) => b.settledMs - a.settledMs);
  const omitted = Math.max(0, visible.length - cap);
  return { open, settled: visible.slice(0, cap), omitted };
}

/** What the header chip reports: everything actually waiting on the human.
 *
 *  One number over both registries, because "needs you" is one question.
 *  Settled rows never count — the faded tail is history, not work — so the
 *  count clears itself as each row is answered or resolved, and the
 *  clear-completed watermark can never change it (it only ever touches settled
 *  rows). `projectPanel(...).open.length` is the same number by construction,
 *  which is pinned as a relation rather than left to two spellings. */
export function needsYouCount(
  view: NeedsYouView,
  questions: readonly OrchQuestion[]
): number {
  return view.items.filter(isOpenItem).length + questions.filter(isPending).length;
}

// ---------- the selection state machine ----------

/** The human's in-progress answer to ONE question. Options are held by INDEX,
 *  not by label: two options may legitimately carry the same text, and keying
 *  by label would silently collapse them into one selectable thing. */
export interface AnswerDraft {
  /** Chosen option indices. Order is not meaningful — `composeAnswer` emits
   *  them in option order so the answer reads like the list the human saw. */
  chosen: readonly number[];
  freeText: string;
}

export const EMPTY_DRAFT: AnswerDraft = { chosen: [], freeText: "" };

/** Apply a click on option `index`.
 *
 *  `single` SWAPS — clicking another option moves the choice, and clicking the
 *  already-chosen one leaves it chosen rather than clearing it: a single-select
 *  question is a decision, and there is no "unanswered" state worth one click
 *  of the human's to get back to (Cancel closes the card). `multi` toggles,
 *  because there the whole point is that a choice is a set. An out-of-range
 *  index is ignored rather than stored — it can only come from a stale render
 *  racing a question whose options changed under it. */
export function toggleChoice(
  draft: AnswerDraft,
  index: number,
  mode: SelectMode,
  optionCount: number
): AnswerDraft {
  if (!Number.isInteger(index) || index < 0 || index >= optionCount) return draft;
  if (mode === "single") return { ...draft, chosen: [index] };
  const has = draft.chosen.includes(index);
  const chosen = has
    ? draft.chosen.filter((i) => i !== index)
    : [...draft.chosen, index].sort((a, b) => a - b);
  return { ...draft, chosen };
}

/** Replace the free-text box's contents. Stored verbatim — trimming happens at
 *  composition, so the human can still type a space mid-sentence. */
export function setFreeText(draft: AnswerDraft, text: string): AnswerDraft {
  return { ...draft, freeText: text };
}

/** The one string that travels to `orch_question_answer`.
 *
 *  Chosen labels VERBATIM in option order, joined with `"; "`; free text after
 *  an em-dash, or alone when nothing was chosen. Verbatim matters: the consumer
 *  is an LLM reading a pane notice, and a label quoted exactly is unambiguous
 *  in a way a re-worded summary of it is not (D3 — the answer stays one
 *  string, with no structured `selected[]` beside it). */
export function composeAnswer(
  draft: AnswerDraft,
  options: readonly DecisionOption[]
): string {
  const labels = [...draft.chosen]
    .sort((a, b) => a - b)
    .map((i) => options[i]?.label)
    .filter((l): l is string => !!l);
  const free = draft.freeText.trim();
  if (labels.length === 0) return free;
  const joined = labels.join("; ");
  return free ? `${joined} — ${free}` : joined;
}

/** Why a draft cannot be submitted yet, or `null` when it can.
 *
 *  A REASON rather than a bare boolean, so the panel can say what is missing
 *  instead of disabling a button silently — and so the over-cap case, which is
 *  the one a human can hit without doing anything obviously wrong, arrives as
 *  a sentence rather than as a backend rejection after the click. */
export type SubmitBlock = "empty" | "no-choice" | "too-long";

export function submitBlock(q: OrchQuestion, draft: AnswerDraft): SubmitBlock | null {
  if (!isPending(q)) return "empty";
  const options = normalizeOptions(q.options);
  const free = freeTextAllowed(q) ? draft.freeText.trim() : "";
  const chosen = draft.chosen.filter((i) => i >= 0 && i < options.length);
  if (options.length > 0 && chosen.length === 0 && !free) {
    // Distinguish the two dead ends: with free text denied there is literally
    // nothing to type, so "pick one" is the only honest instruction.
    return freeTextAllowed(q) ? "empty" : "no-choice";
  }
  if (options.length === 0 && !free) return "empty";
  const composed = composeAnswer(
    { chosen, freeText: freeTextAllowed(q) ? draft.freeText : "" },
    options
  );
  if (!composed.trim()) return "empty";
  if ([...composed].length > ANSWER_MAX) return "too-long";
  return null;
}

/** Sugar over `submitBlock` for the enabled/disabled bit itself. */
export function canSubmit(q: OrchQuestion, draft: AnswerDraft): boolean {
  return submitBlock(q, draft) === null;
}

/** The exact string this draft would send for `q`, with the same free-text
 *  suppression `submitBlock` applies — so what the panel validates and what it
 *  submits can never be two different strings. */
export function answerFor(q: OrchQuestion, draft: AnswerDraft): string {
  const options = normalizeOptions(q.options);
  return composeAnswer(
    {
      chosen: draft.chosen.filter((i) => i >= 0 && i < options.length),
      freeText: freeTextAllowed(q) ? draft.freeText : "",
    },
    options
  );
}

/** Drop drafts whose question is no longer pending, returning a fresh map.
 *
 *  Drafts are frontend-only, so they outlive their rows: the orchestrator can
 *  withdraw a question, or the human can answer it in another window, while a
 *  half-typed draft sits here. Run this on every refresh — the same
 *  `retainExisting` housekeeping the board does for its selection, and the
 *  reason a settled question's card cannot come back holding stale input. */
export function retainDrafts(
  drafts: ReadonlyMap<string, AnswerDraft>,
  questions: readonly OrchQuestion[]
): Map<string, AnswerDraft> {
  const answerable = new Set(questions.filter(isPending).map((q) => q.id));
  const live = new Map<string, AnswerDraft>();
  for (const [id, d] of drafts) if (answerable.has(id)) live.set(id, d);
  return live;
}

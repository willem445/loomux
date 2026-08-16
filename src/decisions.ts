// Pure NEEDS-YOU helpers (#1091 slice C), kept DOM/Tauri-free so they can be
// unit-tested (`decisionsview.ts` wires the DOM + IPC and imports these).
// See test/decisions.test.ts.
//
// The panel is ONE human-attention surface over TWO records that already
// exist, and it owns neither of them:
//
//   DECISIONS — pending `ask_human` questions from `questions.json`, read
//   through the existing `orch_questions_list` command and answered through
//   the existing `orch_question_answer` one. The registry is the record; this
//   is the trusted surface that settles a row (doc/design/human-questions.md).
//
//   DEMOS — board tasks parked in a demo-gated status, PROJECTED from
//   `tasks.json` (`orch_tasks`). Deliberately not a second registry: the board
//   already carries status, assignee, PR, notes and durability, and two records
//   for one item is the drift machine. Proceed and Changes here call the SAME
//   commands the board's own buttons call, so the two surfaces cannot disagree.
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

/** How many settled rows the panel keeps in its faded tail. Mirrors
 *  `humanq::LIST_SETTLED_CAP`, which is the same cap the MCP `list_questions`
 *  projection applies — the two surfaces show the human and the orchestrator
 *  the same depth of history. The registry retains more than this
 *  (`SETTLED_RETAINED`); this is a display cap, not a deletion. */
export const SETTLED_SHOWN = 10;

// ---------- decisions: projection ----------

/** True for a question that can still be answered. */
export function isPending(q: OrchQuestion): boolean {
  return q.status === "pending";
}

/** What the panel renders, split into its two tiers.
 *
 *  Order mirrors `humanq::project_list`: pending rows first in the order the
 *  file holds them, which is ask order, which is oldest-first — the order they
 *  should be answered in. `orch_questions_list` deliberately returns the raw
 *  file rather than that projection (its return type is a list, with nowhere
 *  to put the omitted count), so the split happens here.
 *
 *  `settled` keeps the NEWEST `SETTLED_SHOWN`, and `omitted` says how many
 *  older ones were dropped — the same "a filtered list is never mistaken for
 *  the whole one" contract `project_list` keeps. */
export interface QuestionProjection {
  pending: OrchQuestion[];
  settled: OrchQuestion[];
  omitted: number;
}

export function projectQuestions(
  questions: readonly OrchQuestion[],
  cap: number = SETTLED_SHOWN
): QuestionProjection {
  const pending = questions.filter(isPending);
  const allSettled = questions.filter((q) => !isPending(q));
  const omitted = Math.max(0, allSettled.length - cap);
  return { pending, settled: allSettled.slice(omitted), omitted };
}

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
 *  urgent question is not answered differently, it is just harder to miss. */
export function isUrgent(q: OrchQuestion): boolean {
  return q.urgency === "high";
}

// ---------- demos: projection ----------

/** The statuses that put a board row in front of the human for a LOOK, as
 *  opposed to a decision — accepted H5 on #1091. `prototype` is the #147 demo
 *  gate; `human-testing` is a visible-UI park. Deliberately NARROWER than
 *  `taskboard.ts`'s `isAwaitingHuman`, which also covers `pr` and `blocked`:
 *  those are the merge gate and a stall, both of which the board already owns
 *  and neither of which is a demo to go run. */
export const DEMO_STATUSES = ["prototype", "human-testing"] as const;

export function isDemoGated(status: string): boolean {
  return (DEMO_STATUSES as readonly string[]).includes(status);
}

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

/** One demo card. `path` is `null` when the orchestrator recorded none — the
 *  panel then shows the PR link alone rather than guessing a worktree, because
 *  a caption is a claim. */
export interface DemoItem {
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

/** The demo tier: every board row in a demo-gated status, in board order.
 *
 *  A PROJECTION, which is the whole design — a demo item leaves this panel
 *  exactly when its task leaves the gated status set, so there is nothing to
 *  settle separately and no second record to drift. */
export function projectDemos(tasks: readonly DemoTask[]): DemoItem[] {
  const clean = (v: string | null | undefined): string | null => {
    const s = (v ?? "").trim();
    return s || null;
  };
  return tasks
    .filter((t) => isDemoGated(t.status))
    .map((t) => ({
      id: t.id,
      title: t.title,
      status: t.status,
      path: clean(t.demo_path),
      pr: clean(t.pr),
      assignee: clean(t.assignee),
      // `canProceed` from taskboard.ts, not a second inline `=== "prototype"`:
      // it is the frontend's existing mirror of the backend's
      // `ensure_prototype`, and one rule spelled twice is how the two drift.
      canProceed: canProceed(t.status),
      feedback: feedbackRoute(t.status),
    }));
}

/** What the header chip reports: everything actually waiting on the human.
 *
 *  One number over both tiers, because "needs you" is one question. Settled
 *  questions never count — the faded tail is history, not work — which is what
 *  makes the count clear itself the moment the last row is answered, with no
 *  dismiss gesture anywhere. */
export function needsYouCount(
  questions: readonly OrchQuestion[],
  tasks: readonly DemoTask[]
): number {
  return questions.filter(isPending).length + projectDemos(tasks).length;
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

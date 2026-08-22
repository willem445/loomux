// Unit tests for the NEEDS-YOU panel's pure core (#1091 slice C) — the
// projections that decide WHAT the panel shows, and the selection state
// machine that decides what a human's answer actually says. Run with
// `npm test`.
//
// These are the parts worth pinning because they are the parts that can be
// wrong without looking wrong: a projection that quietly includes a settled
// question, a compose step that re-words a label the orchestrator will read
// back verbatim, a submit gate that lets through a string the backend will
// reject at 2001 characters. The DOM half (`decisionsview.ts`) is validated by
// hand, per this repo's convention.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  anchorOf,
  ANSWER_MAX,
  answerFor,
  canSubmit,
  citedTask,
  composeAnswer,
  EMPTY_DRAFT,
  EMPTY_VIEW,
  feedbackRoute,
  feedbackSubmitStep,
  freeTextAllowed,
  isCleared,
  isOpenItem,
  isPending,
  isUrgent,
  itemTask,
  linkTask,
  mergeCleared,
  needsYouCount,
  normalizeOptions,
  projectPanel,
  resolveBlock,
  resolveNote,
  RESOLUTION_MAX,
  retainDrafts,
  selectMode,
  setFreeText,
  SETTLED_SHOWN,
  submitBlock,
  toggleChoice,
  type AnswerDraft,
  type DemoTask,
  type FeedbackSubmitState,
  type NeedsYouItem,
  type NeedsYouView,
  type OrchQuestion,
} from "../src/decisions.ts";
import {
  canApprove,
  DEMO_STATUSES,
  isAwaitingHuman,
  isDemoGated,
  STATUSES,
} from "../src/taskboard.ts";

// ---------- fixtures ----------

let seq = 0;
const q = (over: Partial<OrchQuestion> = {}): OrchQuestion => ({
  id: `q-${++seq}`,
  asker: "orch-1",
  text: "which way?",
  status: "pending",
  created_ms: 1000 + seq,
  ...over,
});

const task = (over: Partial<DemoTask> = {}): DemoTask => ({
  id: "t-1",
  title: "a thing",
  status: "queued",
  ...over,
});

let iseq = 0;
const item = (over: Partial<NeedsYouItem> = {}): NeedsYouItem => ({
  id: `n-${++iseq}`,
  kind: "demo",
  raiser: "board",
  text: "a thing — parked in prototype for your look",
  task: "t-1",
  status: "open",
  created_ms: 1000 + iseq,
  ...over,
});

const view = (items: NeedsYouItem[], cleared_ms = 0): NeedsYouView => ({ items, cleared_ms });

// ---------- the unified projection ----------

test("the open list holds exactly what is still waiting, from BOTH registries", () => {
  const pend = q({ id: "q-1" });
  const answered = q({ id: "q-2", status: "answered", answer: "yes" });
  const withdrawn = q({ id: "q-3", status: "withdrawn" });
  const open = item({ id: "n-1" });
  const resolved = item({ id: "n-2", status: "resolved", resolved_ms: 9, resolved_by: "webview" });
  const p = projectPanel(view([open, resolved]), [answered, pend, withdrawn], []);
  assert.deepEqual(p.open.map((r) => (r.source === "item" ? r.item.id : r.question.id)), [
    "n-1",
    "q-1",
  ]);
  // `withdrawn` is settled too — a panel that only treated `answered` as
  // settled would keep offering the human a question the asker took back. The
  // same for an item: `board:<status>` and `withdrawn:<agent>` are resolutions.
  assert.deepEqual(
    p.settled.map((r) => (r.source === "item" ? r.item.id : r.question.id)).sort(),
    ["n-2", "q-2", "q-3"]
  );
  assert.equal(isPending(pend), true);
  assert.equal(isPending(withdrawn), false);
  assert.equal(isOpenItem(open), true);
  assert.equal(isOpenItem(resolved), false);
});

test("the open list is urgency-pinned, then newest-first (#1151 D1)", () => {
  // The complaint this answers: the old order was oldest-first, so the thing
  // that just arrived was at the bottom of a long scroll. Urgency still wins
  // over recency — an ask that said `high` said so precisely to jump the queue,
  // and a strict newest-first would bury it under every routine row since.
  const rows = projectPanel(
    view([
      item({ id: "n-old", created_ms: 100 }),
      item({ id: "n-urgent-old", created_ms: 200, urgency: "high" }),
      item({ id: "n-new", created_ms: 900 }),
    ]),
    [q({ id: "q-newest", created_ms: 950 }), q({ id: "q-urgent", created_ms: 300, urgency: "high" })],
    []
  ).open;
  assert.deepEqual(
    rows.map((r) => (r.source === "item" ? r.item.id : r.question.id)),
    ["q-urgent", "n-urgent-old", "q-newest", "n-new", "n-old"]
  );
});

test("rows tied on urgency and timestamp keep a FIXED order rather than shuffling", () => {
  // A panel that reorders under the cursor between two refreshes is one the
  // human stops trusting, and an agent burst can genuinely stamp two rows in
  // the same millisecond. The tie order is items-then-questions, each in its
  // own file order — the input order, kept because the sort is stable.
  const items = [item({ id: "n-a", created_ms: 500 }), item({ id: "n-b", created_ms: 500 })];
  const qs = [q({ id: "q-a", created_ms: 500 }), q({ id: "q-b", created_ms: 500 })];
  const order = () =>
    projectPanel(view(items), qs, []).open.map((r) =>
      r.source === "item" ? r.item.id : r.question.id
    );
  assert.deepEqual(order(), ["n-a", "n-b", "q-a", "q-b"]);
  assert.deepEqual(order(), order(), "the same input renders the same order every time");
  // A row with no `created_ms` at all (a file written by an older build) sorts
  // last rather than randomly, and stays there.
  const withNulls = projectPanel(view([item({ id: "n-x", created_ms: undefined })]), qs, []).open;
  const last = withNulls[withNulls.length - 1];
  assert.equal(last.source === "item" && last.item.id, "n-x");
});

test("the settled tail is newest-settled-first and reports only what the CAP dropped", () => {
  // 13 settled rows against a display cap of 3: the panel shows the three most
  // recently settled and SAYS that ten are missing, never implying the tail is
  // the whole history (the contract `project_list` keeps for the MCP surface).
  const rows = Array.from({ length: 13 }, (_, i) =>
    q({ id: `s-${i}`, status: "answered", settled_ms: 100 + i })
  );
  const p = projectPanel(EMPTY_VIEW, rows, [], 3);
  assert.deepEqual(
    p.settled.map((r) => (r.source === "question" ? r.question.id : r.item.id)),
    ["s-12", "s-11", "s-10"]
  );
  assert.equal(p.omitted, 10);
  // Under the cap nothing is dropped, and the count says so.
  assert.equal(projectPanel(EMPTY_VIEW, rows.slice(0, 2), [], 3).omitted, 0);
});

test("the tail interleaves both registries by WHEN they settled, not by which file", () => {
  const p = projectPanel(
    view([
      item({ id: "n-mid", status: "resolved", resolved_ms: 200, resolved_by: "webview" }),
      item({ id: "n-old", status: "resolved", resolved_ms: 50, resolved_by: "board:done" }),
    ]),
    [q({ id: "q-new", status: "answered", settled_ms: 300 })],
    []
  );
  assert.deepEqual(
    p.settled.map((r) => (r.source === "item" ? r.item.id : r.question.id)),
    ["q-new", "n-mid", "n-old"]
  );
});

test("the default settled cap is the same depth the MCP lists show", () => {
  assert.equal(SETTLED_SHOWN, 10);
});

// ---------- clear-completed: the watermark ----------

test("the watermark hides settled rows only — an OPEN row is untouchable by it", () => {
  // The property that makes the header button safe to click without a confirm:
  // an open item raised long before the clear is still waiting on the human,
  // and no stamp may make it disappear. The backend enforces this structurally
  // (clear_needs_you never opens needs-you.json); this is the panel's half.
  const stale = item({ id: "n-open", created_ms: 10 });
  const settledBefore = item({
    id: "n-gone",
    status: "resolved",
    resolved_ms: 500,
    resolved_by: "webview",
  });
  const settledAfter = item({
    id: "n-kept",
    status: "resolved",
    resolved_ms: 1500,
    resolved_by: "webview",
  });
  const oldQuestion = q({ id: "q-gone", status: "answered", settled_ms: 400 });
  const newQuestion = q({ id: "q-kept", status: "answered", settled_ms: 1400 });
  const p = projectPanel(
    view([stale, settledBefore, settledAfter], 1000),
    [oldQuestion, newQuestion],
    []
  );
  assert.deepEqual(p.open.map((r) => r.anchor), ["t-1"], "the open row survived its own clear");
  assert.deepEqual(
    p.settled.map((r) => (r.source === "item" ? r.item.id : r.question.id)),
    ["n-kept", "q-kept"],
    "both registries' tails are filtered by the ONE watermark"
  );
  // Exactly at the stamp is cleared — the marker means "at or before".
  assert.equal(isCleared(1000, 1000), true);
  assert.equal(isCleared(1001, 1000), false);
});

test("`omitted` counts what the CAP dropped, never what the human cleared", () => {
  // A cleared row is HANDLED, not hidden-but-outstanding. Reporting it back as
  // "…older rows not shown" would contradict the gesture the human just made —
  // and would leave a permanent "12 not shown" under a tail they emptied on
  // purpose. (The cap's own truncation still has to be reported: that one the
  // human did not choose.)
  const rows = Array.from({ length: 8 }, (_, i) =>
    q({ id: `s-${i}`, status: "answered", settled_ms: 100 + i })
  );
  // Clear the first five: three visible rows against a cap of 3 → nothing is
  // dropped by the cap, so `omitted` is 0 even though five rows are hidden.
  const cleared = projectPanel(EMPTY_VIEW, rows, [], 3);
  assert.equal(cleared.omitted, 5, "precondition: with no clear, the cap drops five");
  const afterClear = projectPanel({ items: [], cleared_ms: 104 }, rows, [], 3);
  assert.deepEqual(
    afterClear.settled.map((r) => (r.source === "question" ? r.question.id : r.item.id)),
    ["s-7", "s-6", "s-5"]
  );
  assert.equal(afterClear.omitted, 0, "the five cleared rows are not 'not shown'");
});

test("a watermark of 0 hides NOTHING, because 0 is the never-cleared sentinel", () => {
  // The bug a bare `settledMs <= cleared` would ship: `0` means "never
  // cleared", and a settled row with no timestamp also reads as `0`, so the
  // whole tail of a group nobody has ever cleared would blank on first render.
  const rows = [q({ id: "q-1", status: "answered", settled_ms: 0 })];
  assert.equal(projectPanel(EMPTY_VIEW, rows, []).settled.length, 1);
  assert.equal(isCleared(0, 0), false);
  // Once something IS cleared, a timestamp-less settled row goes with it — it
  // is by definition older than anything stamped, and the alternative is a row
  // the human can never clear.
  assert.equal(isCleared(0, 1), true);
});

test("the watermark only ever moves forward, so a slow read cannot undo a clear", () => {
  // The failure this closes (rev-lead round 1, non-blocking 3): a refresh
  // starts and reads the marker while it still holds 0; the human then clicks
  // Clear completed, the marker is stamped T and the tail goes; the in-flight
  // read resolves LAST with the pre-clear 0 and, assigned wholesale, brings
  // the dismissed tail straight back until some later event re-read the file.
  assert.equal(mergeCleared(0, 1700), 1700, "a stale read cannot lower the stamp");
  // …and a genuinely newer stamp (another window cleared) still wins, which is
  // what makes this a merge rather than "ignore the read".
  assert.equal(mergeCleared(1800, 1700), 1800);
  assert.equal(mergeCleared(0, 0), 0, "never-cleared stays the sentinel");
  // The property, not the three cases: max is total and monotonic, so no pair
  // of stamps can produce one lower than either.
  for (const [a, b] of [[0, 0], [0, 5], [5, 0], [5, 5], [9, 5], [5, 9]]) {
    assert.ok(mergeCleared(a, b) >= a && mergeCleared(a, b) >= b, `${a},${b} went backwards`);
  }
});

test("clearing does not change the count — the count never held settled rows", () => {
  const items = [
    item({ id: "n-1" }),
    item({ id: "n-2", status: "resolved", resolved_ms: 5, resolved_by: "webview" }),
  ];
  const qs = [q({ id: "q-1" }), q({ id: "q-2", status: "answered", settled_ms: 5 })];
  assert.equal(needsYouCount(view(items, 0), qs), 2);
  assert.equal(needsYouCount(view(items, 9999), qs), 2, "a clear cannot move the count");
});

test("the header count and the rendered list cannot disagree", () => {
  // Two spellings of "what is waiting on you" is how a badge starts lying about
  // the list under it. Stated as a relation so a change to either one reddens.
  const items = [
    item({ id: "n-1" }),
    item({ id: "n-2", kind: "feedback", task: null }),
    item({ id: "n-3", status: "resolved", resolved_ms: 5, resolved_by: "webview" }),
  ];
  const qs = [q({ id: "q-1" }), q({ id: "q-2", status: "withdrawn", settled_ms: 5 })];
  for (const cleared of [0, 3, 9999]) {
    const v = view(items, cleared);
    assert.equal(needsYouCount(v, qs), projectPanel(v, qs, []).open.length, `cleared=${cleared}`);
  }
  assert.equal(needsYouCount(EMPTY_VIEW, []), 0);
});

// ---------- the deep-link anchor (slice G) ----------

test("an open demo card anchors on its TASK id, which is what the board marker emits", () => {
  // #1091 slice G's board chip emits `{kind:"demo", target: task.id}` — it has
  // no idea an `n-N` exists. An item card that anchored on its own id would
  // leave that chip landing on nothing, silently.
  const p = projectPanel(view([item({ id: "n-4", task: "t-12" })]), [q({ id: "q-9" })], []);
  assert.deepEqual(p.open.map((r) => r.anchor).sort(), ["q-9", "t-12"]);
  assert.equal(anchorOf(item({ id: "n-4", task: "t-12" }), true), "t-12");
});

test("a feedback item and every settled row anchor on their OWN id", () => {
  // The rule is exactly as wide as the link that needs it. A settled demo row
  // anchoring on `t-N` too would give one task id two cards, and the deep-link
  // would resolve to whichever rendered first.
  assert.equal(anchorOf(item({ id: "n-5", kind: "feedback", task: "t-3" }), true), "n-5");
  assert.equal(anchorOf(item({ id: "n-6", task: "t-3" }), false), "n-6");
  // A demo item with no linked row falls back to its own id rather than to a
  // blank anchor no selector could ever match.
  assert.equal(anchorOf(item({ id: "n-7", task: "  " }), true), "n-7");
});

// ---------- options: the untagged wire shape ----------

test("options normalize from either wire shape, since both are live on disk at once", () => {
  // A Q1-era file carries bare strings; a richer ask writes an object. The
  // panel must render both without knowing which build wrote the file.
  assert.deepEqual(normalizeOptions(["ship it", { label: "wait", description: "one more pass" }]), [
    { label: "ship it" },
    { label: "wait", description: "one more pass" },
  ]);
});

test("a whitespace-only description is dropped, not rendered as an empty line", () => {
  assert.deepEqual(normalizeOptions([{ label: "go", description: "   " }]), [{ label: "go" }]);
});

test("a blank label is dropped — there is nothing to put on that button", () => {
  assert.deepEqual(normalizeOptions(["  ", { label: "" }, "real"]), [{ label: "real" }]);
});

test("absent options is an empty list, because the key is omitted and not []", () => {
  assert.deepEqual(normalizeOptions(undefined), []);
  assert.deepEqual(normalizeOptions(q().options), []);
});

test("free text is allowed unless the ask opted out, and can never be denied without options", () => {
  assert.equal(freeTextAllowed(q({ options: ["a"] })), true, "absent key means allowed");
  assert.equal(freeTextAllowed(q({ options: ["a"], allow_free_text: false })), false);
  // The denial is meaningless with no options — the backend refuses to store
  // that pair, and this agrees rather than trusting a file that has it.
  assert.equal(freeTextAllowed(q({ allow_free_text: false })), true);
});

test("select is single unless the ask said multi, and an unknown value is not a multi", () => {
  assert.equal(selectMode(q()), "single");
  assert.equal(selectMode(q({ select: "multi" })), "multi");
  assert.equal(selectMode(q({ select: "several" as never })), "single");
});

test("the cited task is the cross-link, normalized — blank is no link", () => {
  assert.equal(citedTask(q({ task: "t-7" })), "t-7");
  assert.equal(citedTask(q({ task: "  " })), null);
  assert.equal(citedTask(q()), null);
});

test("urgency is presentation only, and normal is the quiet default", () => {
  assert.equal(isUrgent(q({ urgency: "high" })), true);
  assert.equal(isUrgent(q({ urgency: "normal" })), false);
  assert.equal(isUrgent(q()), false);
});

// ---------- the live task join ----------

test("an item's card reads the board LIVE — it never carries a snapshot", () => {
  // The whole point of the entity split: the item owns the ask, the task keeps
  // owning the facts. Move the row and the same item renders the new truth.
  const before = linkTask("t-1", [task({ id: "t-1", status: "prototype", demo_path: "C:/wt/x" })]);
  assert.equal(before!.status, "prototype");
  assert.equal(before!.canProceed, true);
  const after = linkTask("t-1", [task({ id: "t-1", status: "human-testing", demo_path: "C:/wt/x" })]);
  assert.equal(after!.status, "human-testing");
  assert.equal(after!.canProceed, false, "the item did not remember the old status");
});

test("a missing task DEGRADES the join to null — it never throws and never guesses", () => {
  // An item outlives the row it names: a task can be pruned or renamed under an
  // open item, and nothing validates the `task` string a feedback ask attaches.
  // The card then loses its board affordances and keeps its Resolve, which the
  // view hangs on the item rather than on the join.
  assert.equal(linkTask("t-404", [task({ id: "t-1", status: "prototype" })]), null);
  assert.equal(linkTask(null, [task({ id: "t-1" })]), null, "a feedback ask may name no row");
  assert.equal(linkTask("  ", [task({ id: "t-1" })]), null);
  assert.equal(linkTask("t-1", []), null, "an empty board is not an exception");
  // …and the projection carries that null through rather than dropping the row.
  const p = projectPanel(view([item({ id: "n-1", task: "t-404" })]), [], [task({ id: "t-1" })]);
  assert.equal(p.open.length, 1, "the item is still waiting on the human");
  assert.equal(p.open[0].source === "item" && p.open[0].task, null);
  assert.equal(p.open[0].anchor, "t-404", "and still deep-linkable by the id it names");
});

test("the linked task is matched by exact id, so t-1 never joins t-10", () => {
  const rows = [task({ id: "t-10", title: "ten" }), task({ id: "t-1", title: "one" })];
  assert.equal(linkTask("t-1", rows)!.title, "one");
  assert.equal(linkTask("t-10", rows)!.title, "ten");
});

test("the demo-gate vocabulary still lives on the board, not in this panel", () => {
  // The panel no longer filters the board by status — the backend's transition
  // hook does, and it owns its own copy of the set. What this pins is that the
  // FRONTEND's copy stays the board's single one: a second spelling here is the
  // drift #1091 slice G removed.
  for (const s of STATUSES) {
    assert.equal(isDemoGated(s), (DEMO_STATUSES as readonly string[]).includes(s));
  }
});

test("the demo set is strictly narrower than the board's awaiting-human set", () => {
  // `pr` and `blocked` are also waiting on the human, but neither is a demo to
  // go run — the merge gate and a stall belong to the board, not this panel.
  // Stated as a relation rather than a second hard-coded list, so widening
  // either set without thinking about the other goes red here.
  const awaiting = STATUSES.filter(isAwaitingHuman);
  const demo = STATUSES.filter(isDemoGated);
  assert.ok(demo.every((s) => awaiting.includes(s)), "every demo status is awaiting-human");
  assert.ok(demo.length < awaiting.length, "but not every awaiting-human status is a demo");
  assert.deepEqual(
    awaiting.filter((s) => !demo.includes(s)).sort(),
    ["blocked", "pr"]
  );
});

test("a joined row carries the recorded path, and says nothing when none was recorded", () => {
  const withPath = linkTask("t-1", [
    task({ id: "t-1", status: "prototype", demo_path: "C:/wt/feat-x", pr: "#12", assignee: "w-3" }),
  ])!;
  assert.equal(withPath.path, "C:/wt/feat-x");
  assert.equal(withPath.pr, "#12");
  assert.equal(withPath.assignee, "w-3");
  // No path recorded is NOT a guessed path: the panel shows the PR alone.
  const without = linkTask("t-2", [task({ id: "t-2", status: "prototype" })])!;
  assert.equal(without.path, null);
  // An empty string clears the field backend-side; the panel reads it the same
  // way rather than rendering a blank mono chip.
  const blank = linkTask("t-3", [task({ id: "t-3", status: "prototype", demo_path: "  " })])!;
  assert.equal(blank.path, null);
});

test("feedback on a prototype routes to the note verb, NOT the merge-gate one", () => {
  // The defect this pins: `orch_request_changes` calls `ensure_at_merge_gate`,
  // which admits only MERGE_GATE_STATUSES (pr / human-testing). Sending it at a
  // `prototype` row is refused EVERY time — and the dialog had already closed,
  // so the human's typed findings were gone with no way back. A prototype is
  // the #147 demo gate, so the answer is a verb the backend accepts, not a
  // hidden button.
  const proto = linkTask("t-1", [task({ status: "prototype" })])!;
  assert.equal(proto.feedback, "note");
  const testing = linkTask("t-1", [task({ status: "human-testing" })])!;
  assert.equal(testing.feedback, "merge-gate");
});

test("every joinable row has a working feedback verb — a card is never half-actionable", () => {
  // Totality is the property, not the two cases above: a future status that
  // reached a card with no accepted verb would silently drop the human's input
  // again, so there is deliberately no third "cannot" value. Widened past the
  // demo gate on purpose — an item can now name ANY board row, so the verb has
  // to be total over the whole status set rather than over two of them.
  for (const s of STATUSES) {
    const d = linkTask("t-x", [task({ id: "t-x", status: s })])!;
    assert.ok(
      d.feedback === "merge-gate" || d.feedback === "note",
      `${s} has no feedback route`
    );
  }
});

test("the feedback route mirrors the backend's merge-gate predicate, not a second spelling", () => {
  // `canApprove` IS the frontend's mirror of `ensure_at_merge_gate`, and the
  // board's own Changes button already gates on it. Stated as a relation so
  // that changing one and not the other reddens here rather than shipping a
  // panel that disagrees with the board about the same backend guard.
  for (const s of STATUSES) {
    assert.equal(
      feedbackRoute(s) === "merge-gate",
      canApprove(s),
      `${s} disagrees with canApprove`
    );
  }
});

// ---------- the feedback dialog's submit gate ----------
//
// The dialog closes on SUCCESS rather than before the write, so `submit` is
// reachable a second time while the first write is still in the air — a plain
// double Ctrl+Enter, which is what a human does when a dialog does not visibly
// respond. Every crossing of {route} × {inFlight} × {findingsLanded} is pinned
// below, because a guard that reads one input and not the other is a bypass
// exactly the width of the asymmetry.

const fresh: FeedbackSubmitState = { inFlight: false, findingsLanded: false };

test("a second submit while the first write is outstanding is a no-op, on BOTH routes", () => {
  // The defect: two concurrent chains for one decision. On a merge-gate row
  // that is two `orch_request_changes` calls — two `Requested changes: …` notes
  // on one task and two `[orrerix]` deliveries the orchestrator must reconcile.
  const busy = { ...fresh, inFlight: true };
  assert.equal(feedbackSubmitStep("merge-gate", busy), "ignore");
  assert.equal(feedbackSubmitStep("note", busy), "ignore");
  // ...and it stays a no-op whatever the OTHER input says, so the guard cannot
  // be walked around by getting one call of the two-call chain through.
  assert.equal(feedbackSubmitStep("merge-gate", { inFlight: true, findingsLanded: true }), "ignore");
  assert.equal(feedbackSubmitStep("note", { inFlight: true, findingsLanded: true }), "ignore");
});

test("a retry after a PARTIAL merge-gate failure re-runs only the call that failed", () => {
  // The merge-gate route is a two-call chain. If `orch_request_changes` lands
  // and the status flip then fails, the dialog re-enables Send — and a retry
  // that re-ran the whole chain would record the human's findings twice for one
  // failure they did not cause.
  assert.equal(
    feedbackSubmitStep("merge-gate", { inFlight: false, findingsLanded: true }),
    "status-only"
  );
  assert.equal(feedbackSubmitStep("merge-gate", fresh), "findings-then-status");
});

test("the note route is one call, so nothing about it is ever partially done", () => {
  // `findingsLanded` is merge-gate state; the note route must not read it, or a
  // stale flag would silently downgrade a real note to a status flip the note
  // route deliberately never performs.
  assert.equal(feedbackSubmitStep("note", fresh), "note");
  assert.equal(feedbackSubmitStep("note", { inFlight: false, findingsLanded: true }), "note");
});

test("the gate is not simply closed — a first press on an idle dialog always writes", () => {
  // The negative control. Without it, `() => "ignore"` passes every assertion
  // above, and a dialog that refuses every submit is a worse defect than the
  // duplicate write this gate exists to stop.
  for (const route of ["merge-gate", "note"] as const) {
    assert.notEqual(
      feedbackSubmitStep(route, fresh),
      "ignore",
      `${route}: an idle dialog's first press must do something`
    );
  }
});

test("Proceed offers only on a prototype — the same guard the backend enforces", () => {
  const proto = linkTask("t-1", [task({ status: "prototype" })])!;
  const testing = linkTask("t-1", [task({ status: "human-testing" })])!;
  assert.equal(proto.canProceed, true);
  assert.equal(testing.canProceed, false, "human-testing takes feedback, not a promote");
});

test("a row leaves the panel when its ITEM resolves, not when its task moves", () => {
  // The model #1151 replaced: the card used to disappear the moment the board
  // left the demo gate, because the card WAS the task. Now the item is the
  // record — a task that moves on has its item auto-resolved by the backend
  // hook, and until that lands the human still sees the ask they were given.
  const parked = view([item({ id: "n-1", task: "t-9" })]);
  const moved = [task({ id: "t-9", status: "done" })];
  assert.equal(projectPanel(parked, [], moved).open.length, 1, "the ask outlives the status");
  const resolved = view([
    item({ id: "n-1", task: "t-9", status: "resolved", resolved_ms: 7, resolved_by: "board:done" }),
  ]);
  assert.equal(projectPanel(resolved, [], moved).open.length, 0);
  assert.equal(projectPanel(resolved, [], moved).settled.length, 1, "it recedes, it does not vanish");
});

// ---------- the count ----------

test("the count is pending questions plus OPEN items, and clears as each settles", () => {
  const questions = [q({ id: "q-1" }), q({ id: "q-2" }), q({ id: "q-3", status: "answered" })];
  const items = [
    item({ id: "n-1" }),
    item({ id: "n-2", status: "resolved", resolved_ms: 5, resolved_by: "webview" }),
    item({ id: "n-3", kind: "feedback", task: null }),
  ];
  assert.equal(needsYouCount(view(items), questions), 4);
  // Answer both questions and resolve one item: only the feedback ask is left,
  // and the only gesture involved was settling the rows themselves.
  assert.equal(
    needsYouCount(
      view(items.map((i) => (i.id === "n-1" ? { ...i, status: "resolved" as const } : i))),
      questions.map((x) => ({ ...x, status: "answered" as const }))
    ),
    1
  );
  assert.equal(needsYouCount(EMPTY_VIEW, []), 0);
});

// ---------- the close-out note ----------

test("an empty note box resolves with NULL, never with an empty string", () => {
  // `validate_resolution` REFUSES an empty note ("resolve without one to close
  // it silently"), so sending "" would turn the ordinary tidy — the common
  // case, and the one that deliberately delivers no pane notice — into an error
  // the human did nothing to earn.
  assert.equal(resolveNote(""), null);
  assert.equal(resolveNote("   \n "), null);
  assert.equal(resolveNote("  looks good  "), "looks good", "and a real note is trimmed, not padded");
});

test("an empty box is not a block — the note is genuinely optional", () => {
  assert.equal(resolveBlock(""), null);
  assert.equal(resolveBlock("looks good"), null);
});

test("the note cap is the backend's, counted in characters and not UTF-16 units", () => {
  // `validate_resolution` counts `chars()` and REJECTS over the cap rather than
  // truncating, so the panel must block before the click — and must not refuse
  // an all-astral note the backend would have taken.
  assert.equal(RESOLUTION_MAX, 2000);
  assert.equal(resolveBlock("x".repeat(RESOLUTION_MAX)), null, "exactly at the cap is fine");
  assert.equal(resolveBlock("x".repeat(RESOLUTION_MAX + 1)), "too-long");
  const astral = "🙂".repeat(1100);
  assert.equal(astral.length, 2200, "precondition: over the cap by UTF-16 units");
  assert.equal(resolveBlock(astral), null);
  // The cap applies to what TRAVELS, which is the trimmed string.
  assert.equal(resolveBlock(` ${"x".repeat(RESOLUTION_MAX)} `), null);
});

test("the linked task an item names is normalized the way a question's cited one is", () => {
  assert.equal(itemTask(item({ task: "t-7" })), "t-7");
  assert.equal(itemTask(item({ task: "  " })), null);
  assert.equal(itemTask(item({ task: null })), null);
});

test("urgency reads the same way on both registries, from one function", () => {
  // A question and an item carry the same `humanq::Urgency` — imported rather
  // than cloned backend-side for exactly this reason. Two readers of it here
  // would be two spellings of one word that the union sort must reconcile.
  assert.equal(isUrgent(item({ urgency: "high" })), true);
  assert.equal(isUrgent(item({ urgency: "normal" })), false);
  assert.equal(isUrgent(item()), false, "absent is the quiet default, on an item too");
});

// ---------- the selection state machine ----------

const opts3 = normalizeOptions(["A", "B", "C"]);

test("single-select swaps, and re-clicking the chosen option leaves it chosen", () => {
  let d: AnswerDraft = EMPTY_DRAFT;
  d = toggleChoice(d, 0, "single", 3);
  assert.deepEqual(d.chosen, [0]);
  d = toggleChoice(d, 2, "single", 3);
  assert.deepEqual(d.chosen, [2], "the choice moved rather than accumulating");
  d = toggleChoice(d, 2, "single", 3);
  assert.deepEqual(d.chosen, [2], "a decision does not un-decide on a second click");
});

test("multi-select toggles both ways and stays in option order", () => {
  let d: AnswerDraft = EMPTY_DRAFT;
  d = toggleChoice(d, 2, "multi", 3);
  d = toggleChoice(d, 0, "multi", 3);
  assert.deepEqual(d.chosen, [0, 2], "kept in option order, not click order");
  d = toggleChoice(d, 2, "multi", 3);
  assert.deepEqual(d.chosen, [0]);
});

test("an out-of-range index is ignored rather than stored", () => {
  // Only reachable from a stale render racing a question whose options moved —
  // storing it would compose an answer with a hole in it.
  assert.deepEqual(toggleChoice(EMPTY_DRAFT, 5, "multi", 3).chosen, []);
  assert.deepEqual(toggleChoice(EMPTY_DRAFT, -1, "single", 3).chosen, []);
});

test("options are keyed by index, so two identically-labelled options stay distinct", () => {
  const dup = normalizeOptions(["same", "same"]);
  let d = toggleChoice(EMPTY_DRAFT, 0, "multi", 2);
  d = toggleChoice(d, 1, "multi", 2);
  assert.deepEqual(d.chosen, [0, 1], "keying by label would have collapsed these to one");
  assert.equal(composeAnswer(d, dup), "same; same");
});

// ---------- answer composition ----------

test("chosen labels travel verbatim, joined, in option order", () => {
  const d = { chosen: [2, 0], freeText: "" };
  assert.equal(composeAnswer(d, opts3), "A; C");
});

test("free text alone is the whole answer when nothing was chosen", () => {
  assert.equal(composeAnswer(setFreeText(EMPTY_DRAFT, "  neither  "), opts3), "neither");
});

test("free text beside a choice is appended after an em-dash", () => {
  const d = setFreeText({ chosen: [1], freeText: "" }, "but only after CI");
  assert.equal(composeAnswer(d, opts3), "B — but only after CI");
});

test("a label is never re-worded — an em-dash inside one survives composition", () => {
  const tricky = normalizeOptions(["ship it — carefully"]);
  assert.equal(composeAnswer({ chosen: [0], freeText: "" }, tricky), "ship it — carefully");
});

test("answerFor drops free text the ask denied, so validation and submission agree", () => {
  const denied = q({ options: ["A", "B"], allow_free_text: false });
  const d = setFreeText({ chosen: [0], freeText: "sneaking this in" }, "sneaking this in");
  assert.equal(answerFor(denied, d), "A", "the denied box's contents never travel");
  const allowed = q({ options: ["A", "B"] });
  assert.equal(answerFor(allowed, d), "A — sneaking this in");
});

// ---------- the submit gate ----------

test("a free-text-only question needs text", () => {
  const free = q();
  assert.equal(submitBlock(free, EMPTY_DRAFT), "empty");
  assert.equal(canSubmit(free, EMPTY_DRAFT), false);
  assert.equal(canSubmit(free, setFreeText(EMPTY_DRAFT, "   ")), false, "whitespace is not an answer");
  assert.equal(canSubmit(free, setFreeText(EMPTY_DRAFT, "do it")), true);
});

test("options with free text DENIED and nothing picked cannot submit, and says pick one", () => {
  // The plan's named edge case: the human has no box to type in, so the only
  // honest instruction is to choose.
  const strict = q({ options: ["A", "B"], allow_free_text: false });
  assert.equal(submitBlock(strict, EMPTY_DRAFT), "no-choice");
  assert.equal(canSubmit(strict, setFreeText(EMPTY_DRAFT, "other")), false, "typing cannot rescue it");
  assert.equal(canSubmit(strict, toggleChoice(EMPTY_DRAFT, 1, "single", 2)), true);
});

test("options with free text ALLOWED submit on either a choice or typed text", () => {
  const open = q({ options: ["A", "B"] });
  assert.equal(canSubmit(open, EMPTY_DRAFT), false);
  assert.equal(canSubmit(open, toggleChoice(EMPTY_DRAFT, 0, "single", 2)), true);
  assert.equal(canSubmit(open, setFreeText(EMPTY_DRAFT, "neither, actually")), true);
});

test("the COMPOSED string is what the cap applies to — the mirror of validate_answer", () => {
  // The backend REJECTS over 2000 chars rather than truncating, so the panel
  // must block on the string that actually travels, not on the box alone: here
  // the free text fits on its own and only the composed form goes over.
  const label = "L".repeat(200);
  const withLabel = q({ options: [label] });
  const draft = setFreeText({ chosen: [0], freeText: "" }, "F".repeat(ANSWER_MAX - 100));
  assert.ok(
    [...draft.freeText].length <= ANSWER_MAX,
    "precondition: the typed text alone is under the cap"
  );
  assert.equal(submitBlock(withLabel, draft), "too-long");

  // Exactly at the cap is fine; one over is not.
  const exact = setFreeText(EMPTY_DRAFT, "x".repeat(ANSWER_MAX));
  assert.equal(submitBlock(q(), exact), null);
  const over = setFreeText(EMPTY_DRAFT, "x".repeat(ANSWER_MAX + 1));
  assert.equal(submitBlock(q(), over), "too-long");
});

test("the cap counts characters the way the backend does, not UTF-16 units", () => {
  // `humanq::validate_answer` counts `chars()`. An answer of 1100 astral
  // characters is 2200 UTF-16 units — a naive `.length` would refuse it, and
  // the human would be blocked from an answer the backend would have taken.
  const astral = "🙂".repeat(1100);
  assert.equal(astral.length, 2200, "precondition: over the cap by UTF-16 units");
  assert.equal(submitBlock(q(), setFreeText(EMPTY_DRAFT, astral)), null);
});

test("a settled question can never be submitted, however complete the draft looks", () => {
  const done = q({ status: "answered", options: ["A"], answer: "A" });
  assert.equal(canSubmit(done, toggleChoice(EMPTY_DRAFT, 0, "single", 1)), false);
});

// ---------- draft housekeeping ----------

test("drafts are dropped once their question settles, so stale input cannot come back", () => {
  const live = q({ id: "q-1" });
  const settled = q({ id: "q-2", status: "answered" });
  const drafts = new Map<string, AnswerDraft>([
    ["q-1", setFreeText(EMPTY_DRAFT, "half typed")],
    ["q-2", setFreeText(EMPTY_DRAFT, "answered elsewhere")],
    ["q-9", setFreeText(EMPTY_DRAFT, "withdrawn out from under me")],
  ]);
  const kept = retainDrafts(drafts, [live, settled]);
  assert.deepEqual([...kept.keys()], ["q-1"]);
  assert.equal(kept.get("q-1")!.freeText, "half typed", "the surviving draft is untouched");
});

test("an item-driven refresh cannot drop a half-typed answer", () => {
  // #1151 added a THIRD refresh trigger (`orch-needs-you-changed`), and every
  // one of them re-runs this prune. An agent raising or resolving an item while
  // the human is mid-answer must not cost them what they typed — so the prune
  // is keyed on questions ALONE, and a refresh where no question changed is a
  // no-op for the drafts however much else moved.
  const live = q({ id: "q-1" });
  const drafts = new Map<string, AnswerDraft>([["q-1", setFreeText(EMPTY_DRAFT, "half typed")]]);
  let kept = retainDrafts(drafts, [live]);
  kept = retainDrafts(kept, [live]);
  kept = retainDrafts(kept, [live]);
  assert.deepEqual([...kept.keys()], ["q-1"]);
  assert.equal(kept.get("q-1")!.freeText, "half typed");
  // The projection is where items and questions meet, and it reads no draft at
  // all — the two halves of a refresh cannot reach each other.
  assert.equal(
    projectPanel(view([item({ id: "n-1" })]), [live], []).open.length,
    2,
    "the item and the question both render; neither consumed the other's state"
  );
});

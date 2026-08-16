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
  ANSWER_MAX,
  answerFor,
  canSubmit,
  citedTask,
  composeAnswer,
  DEMO_STATUSES,
  EMPTY_DRAFT,
  feedbackRoute,
  feedbackSubmitStep,
  freeTextAllowed,
  isDemoGated,
  isPending,
  isUrgent,
  needsYouCount,
  normalizeOptions,
  projectDemos,
  projectQuestions,
  retainDrafts,
  selectMode,
  setFreeText,
  SETTLED_SHOWN,
  submitBlock,
  toggleChoice,
  type AnswerDraft,
  type DemoTask,
  type FeedbackSubmitState,
  type OrchQuestion,
} from "../src/decisions.ts";
import { canApprove, isAwaitingHuman, STATUSES } from "../src/taskboard.ts";

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

// ---------- decisions: projection ----------

test("the pending tier holds exactly the answerable rows, and both terminal states settle", () => {
  const pend = q({ id: "q-1" });
  const answered = q({ id: "q-2", status: "answered", answer: "yes" });
  const withdrawn = q({ id: "q-3", status: "withdrawn" });
  const p = projectQuestions([answered, pend, withdrawn]);
  assert.deepEqual(p.pending.map((x) => x.id), ["q-1"]);
  // `withdrawn` is settled too — a panel that only treated `answered` as
  // settled would keep offering the human a question the asker took back.
  assert.deepEqual(p.settled.map((x) => x.id), ["q-2", "q-3"]);
  assert.equal(isPending(pend), true);
  assert.equal(isPending(withdrawn), false);
});

test("pending rows keep file order — which is ask order, which is oldest first", () => {
  const rows = [q({ id: "q-1" }), q({ id: "q-2" }), q({ id: "q-3" })];
  assert.deepEqual(projectQuestions(rows).pending.map((x) => x.id), ["q-1", "q-2", "q-3"]);
});

test("the settled tail keeps the NEWEST rows and reports how many it dropped", () => {
  // 13 settled rows against a display cap of 3: the panel must show the last
  // three and SAY that ten are missing, never imply the tail is the whole
  // history (the contract `humanq::project_list` keeps for the MCP surface).
  const rows = Array.from({ length: 13 }, (_, i) =>
    q({ id: `s-${i}`, status: "answered" })
  );
  const p = projectQuestions(rows, 3);
  assert.deepEqual(p.settled.map((x) => x.id), ["s-10", "s-11", "s-12"]);
  assert.equal(p.omitted, 10);
  // Under the cap nothing is dropped, and the count says so.
  assert.equal(projectQuestions(rows.slice(0, 2), 3).omitted, 0);
});

test("the default settled cap is the same depth the MCP list shows", () => {
  assert.equal(SETTLED_SHOWN, 10);
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

// ---------- demos: projection ----------

test("the demo tier is exactly the two gated statuses, and nothing else on the board", () => {
  const rows = STATUSES.map((s) => task({ id: `t-${s}`, status: s }));
  assert.deepEqual(
    projectDemos(rows).map((d) => d.status),
    ["prototype", "human-testing"]
  );
  for (const s of STATUSES) assert.equal(isDemoGated(s), (DEMO_STATUSES as readonly string[]).includes(s));
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

test("a demo item carries the recorded path, and says nothing when none was recorded", () => {
  const [withPath] = projectDemos([
    task({ id: "t-1", status: "prototype", demo_path: "C:/wt/feat-x", pr: "#12", assignee: "w-3" }),
  ]);
  assert.equal(withPath.path, "C:/wt/feat-x");
  assert.equal(withPath.pr, "#12");
  assert.equal(withPath.assignee, "w-3");
  // No path recorded is NOT a guessed path: the panel shows the PR alone.
  const [without] = projectDemos([task({ id: "t-2", status: "prototype" })]);
  assert.equal(without.path, null);
  // An empty string clears the field backend-side; the panel reads it the same
  // way rather than rendering a blank mono chip.
  const [blank] = projectDemos([task({ id: "t-3", status: "prototype", demo_path: "  " })]);
  assert.equal(blank.path, null);
});

test("feedback on a prototype routes to the note verb, NOT the merge-gate one", () => {
  // The defect this pins: `orch_request_changes` calls `ensure_at_merge_gate`,
  // which admits only MERGE_GATE_STATUSES (pr / human-testing). Sending it at a
  // `prototype` row is refused EVERY time — and the dialog had already closed,
  // so the human's typed findings were gone with no way back. A prototype is
  // the #147 demo gate, so the answer is a verb the backend accepts, not a
  // hidden button.
  const [proto] = projectDemos([task({ status: "prototype" })]);
  assert.equal(proto.feedback, "note");
  const [testing] = projectDemos([task({ status: "human-testing" })]);
  assert.equal(testing.feedback, "merge-gate");
});

test("every demo-gated row has a working feedback verb — the tier is never half-actionable", () => {
  // Totality is the property, not the two cases above: a future demo status
  // that reached this tier with no accepted verb would silently drop the
  // human's input again, so there is deliberately no third "cannot" value.
  for (const d of projectDemos(STATUSES.map((s) => task({ id: `t-${s}`, status: s })))) {
    assert.ok(
      d.feedback === "merge-gate" || d.feedback === "note",
      `${d.status} has no feedback route`
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
  // on one task and two `[loomux]` deliveries the orchestrator must reconcile.
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
  const [proto] = projectDemos([task({ status: "prototype" })]);
  const [testing] = projectDemos([task({ status: "human-testing" })]);
  assert.equal(proto.canProceed, true);
  assert.equal(testing.canProceed, false, "human-testing takes feedback, not a promote");
});

test("a demo item leaves the panel when its task leaves the gated status", () => {
  const before = projectDemos([task({ id: "t-9", status: "prototype" })]);
  const after = projectDemos([task({ id: "t-9", status: "done" })]);
  assert.equal(before.length, 1);
  assert.equal(after.length, 0);
});

// ---------- the count ----------

test("the count is pending decisions plus open demos, and clears as each settles", () => {
  const questions = [q({ id: "q-1" }), q({ id: "q-2" }), q({ id: "q-3", status: "answered" })];
  const tasks = [
    task({ id: "t-1", status: "prototype" }),
    task({ id: "t-2", status: "done" }),
    task({ id: "t-3", status: "human-testing" }),
  ];
  assert.equal(needsYouCount(questions, tasks), 4);
  // Answer both questions and promote the prototype: only the human-testing
  // row is left, and no dismiss gesture was involved anywhere.
  assert.equal(
    needsYouCount(
      questions.map((x) => ({ ...x, status: "answered" as const })),
      [tasks[0], tasks[1], tasks[2]].map((t) => (t.id === "t-1" ? { ...t, status: "in-progress" } : t))
    ),
    1
  );
  assert.equal(needsYouCount([], []), 0);
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

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  DEFAULT_SESSION_MODE,
  OPERATOR_ROLE,
  decodeSessionMode,
  delegateToggleLabel,
  isOperatorSession,
  isOrchestrationSession,
  partitionSessions,
} from "../src/sessionfilter.ts";

// The rows the default list is FOR: the human's own sessions, and the one
// recorded role a human restarts by hand.

test("a session with no orchestration identity is the human's own and is shown", () => {
  // #2116 leans on this beyond its own contract: `partitionSessions`' delegate
  // branch is gated on the orchestration mode, and that gate is redundant only
  // BECAUSE an unrecorded row reads as an operator here. If this ever flips,
  // read the "ONE GUARD BELOW IS REDUNDANT TODAY" note in `sessionfilter.ts`
  // before deleting anything that then looks dead.
  assert.equal(isOperatorSession(undefined), true);
});

test("an orchestrator row is shown", () => {
  assert.equal(isOperatorSession({ role: OPERATOR_ROLE }), true);
});

test("worker and reviewer rows are delegates", () => {
  assert.equal(isOperatorSession({ role: "worker" }), false);
  assert.equal(isOperatorSession({ role: "reviewer" }), false);
});

// The property, not the role list. This is the assertion that fails if anybody
// rewrites the rule as an exclusion list of today's role names: a role nobody
// has invented yet must be hidden, because the reason delegates are noise (the
// orchestrator respawns them from the group's roster) is a fact about being a
// delegate, not a fact about being called "worker".
test("a role nobody has invented yet is a delegate, not a shown row", () => {
  for (const role of ["planner", "manager", "process", "rev-lead", "worker-deep", "wibble"]) {
    assert.equal(isOperatorSession({ role }), false, `${role} must default to hidden`);
  }
});

test("a stray space neither promotes a delegate nor demotes an orchestrator", () => {
  assert.equal(isOperatorSession({ role: " orchestrator " }), true);
  assert.equal(isOperatorSession({ role: " worker " }), false);
});

// ---- partitionSessions ----

interface Row {
  id: string;
  role?: string;
}
const roleOf = (r: Row) => (r.role === undefined ? undefined : { role: r.role });

const CORPUS: Row[] = [
  { id: "human-a" },
  { id: "orch-1", role: "orchestrator" },
  { id: "w-1", role: "worker" },
  { id: "w-2", role: "worker" },
  { id: "rev-1", role: "reviewer" },
  { id: "human-b" },
];

// THE FOUR CROSSINGS of {mode} x {showDelegates} (#2116). The two controls
// answer different questions and must compose rather than override each other,
// so every combination is pinned — a guard that only ever sees three of four
// states is a guard with an untested arm.

test("mine + delegates off: the human's own sessions, and nothing is hidden", () => {
  const { shown, hidden } = partitionSessions(CORPUS, roleOf, "mine", false);
  assert.deepEqual(
    shown.map((r) => r.id),
    ["human-a", "human-b"]
  );
  assert.equal(hidden, 0, "the delegate toggle hid nothing: there are no delegates here");
});

test("mine + delegates ON is IDENTICAL — the toggle is inert outside orchestration", () => {
  // The negative control for the composition. If the delegate toggle leaked
  // into `mine` it would either reveal orchestration rows in a view the human
  // asked to be their own, or put a count on a control that reveals nothing.
  const off = partitionSessions(CORPUS, roleOf, "mine", false);
  const on = partitionSessions(CORPUS, roleOf, "mine", true);
  assert.deepEqual(
    on.shown.map((r) => r.id),
    off.shown.map((r) => r.id)
  );
  assert.equal(on.hidden, 0);
});

test("mine NEVER shows a delegate, whatever the toggle says", () => {
  for (const showDelegates of [false, true]) {
    const { shown } = partitionSessions(CORPUS, roleOf, "mine", showDelegates);
    for (const row of shown) {
      assert.equal(
        roleOf(row),
        undefined,
        `${row.id} has a recorded role and must not appear in "mine" (toggle=${showDelegates})`
      );
    }
  }
});

test("orchestration + delegates off: the operator only, delegates counted", () => {
  const { shown, hidden } = partitionSessions(CORPUS, roleOf, "orchestration", false);
  assert.deepEqual(
    shown.map((r) => r.id),
    ["orch-1"]
  );
  assert.equal(hidden, 3, "w-1, w-2 and rev-1");
});

test("orchestration + delegates on: every orchestration row, and nothing hidden", () => {
  const { shown, hidden } = partitionSessions(CORPUS, roleOf, "orchestration", true);
  assert.deepEqual(
    shown.map((r) => r.id),
    ["orch-1", "w-1", "w-2", "rev-1"]
  );
  assert.equal(hidden, 0);
});

test("the two modes are a partition: every row is in exactly one of them", () => {
  // The rows the MODE excluded must not be counted as "hidden" — they are one
  // click away on a control that names where they went, not behind the
  // delegate toggle. So the arithmetic that holds is across the two modes, and
  // `hidden` accounts only for what the delegate rule held back.
  const mine = partitionSessions(CORPUS, roleOf, "mine", true);
  const orch = partitionSessions(CORPUS, roleOf, "orchestration", true);
  assert.equal(mine.shown.length + orch.shown.length, CORPUS.length);
  const ids = new Set([...mine.shown, ...orch.shown].map((r) => r.id));
  assert.equal(ids.size, CORPUS.length, "no row appears in both modes");
});

test("within orchestration, shown + hidden is one partition of that population", () => {
  // The count is what the toggle's label promises to reveal. If it and the
  // list are computed by two different passes they can disagree, and the
  // human clicks "show 3" and gets 4 — so pin the arithmetic, not just the
  // list.
  const all = partitionSessions(CORPUS, roleOf, "orchestration", true);
  const { shown, hidden } = partitionSessions(CORPUS, roleOf, "orchestration", false);
  assert.equal(shown.length + hidden, all.shown.length);
});

test("the input array is never mutated, and the result is not an alias of it", () => {
  const before = CORPUS.map((r) => r.id);
  const on = partitionSessions(CORPUS, roleOf, "orchestration", true);
  const off = partitionSessions(CORPUS, roleOf, "mine", false);
  on.shown.push({ id: "intruder" });
  off.shown.push({ id: "intruder" });
  assert.deepEqual(
    CORPUS.map((r) => r.id),
    before,
    "the caller's array is the session store's own list"
  );
});

test("order is preserved — the scan already sorted newest-first", () => {
  for (const mode of ["mine", "orchestration"] as const) {
    const { shown } = partitionSessions(CORPUS, roleOf, mode, false);
    const positions = shown.map((r) => CORPUS.indexOf(r));
    assert.deepEqual(positions, [...positions].sort((a, b) => a - b), mode);
  }
});

// ---- the mode split, as a property ----

test("a row belongs to an orchestration when it has a recorded role AT ALL", () => {
  assert.equal(isOrchestrationSession(undefined), false);
  assert.equal(isOrchestrationSession({ role: OPERATOR_ROLE }), true);
  // Same reason `isOperatorSession` is written as a property: a role nobody
  // has invented yet must land in the orchestration view rather than silently
  // joining the human's own sessions.
  for (const role of ["planner", "process", "rev-lead", "wibble"]) {
    assert.equal(isOrchestrationSession({ role }), true, role);
  }
});

test("a persisted mode decodes totally, and anything unrecognised opens the default", () => {
  assert.equal(decodeSessionMode("orchestration"), "orchestration");
  assert.equal(decodeSessionMode("mine"), "mine");
  for (const raw of [null, undefined, "", "Mine", "agents", 7, {}, ["orchestration"]]) {
    assert.equal(decodeSessionMode(raw), DEFAULT_SESSION_MODE, `raw: ${JSON.stringify(raw)}`);
  }
});

test("the default view is the human's own sessions", () => {
  assert.equal(DEFAULT_SESSION_MODE, "mine");
});

// ---- the toggle's label ----

test("the label says how many are hidden, singular and plural", () => {
  assert.equal(delegateToggleLabel(1, false, "orchestration"), "Show 1 hidden agent session");
  assert.equal(delegateToggleLabel(7, false, "orchestration"), "Show 7 hidden agent sessions");
});

test("with the toggle on, the label offers the way back", () => {
  assert.equal(delegateToggleLabel(0, true, "orchestration"), "Hide agent sessions");
  assert.equal(delegateToggleLabel(7, true, "orchestration"), "Hide agent sessions");
});

test("no toggle at all when nothing is hidden", () => {
  // Not "Show 0 hidden" and not a disabled control: offering to reveal rows
  // that do not exist promises something that is not there.
  assert.equal(delegateToggleLabel(0, false, "orchestration"), null);
});

test("the label is silent in `mine`, in BOTH toggle states", () => {
  // The `showDelegates === true` half is the one that matters: the state is
  // remembered across a mode switch, so a human who revealed delegates in the
  // orchestration view and then switched to `mine` would otherwise be offered
  // "Hide agent sessions" beside a list that has none.
  assert.equal(delegateToggleLabel(0, false, "mine"), null);
  assert.equal(delegateToggleLabel(3, true, "mine"), null);
  assert.equal(delegateToggleLabel(3, false, "mine"), null);
});

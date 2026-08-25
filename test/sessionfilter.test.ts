import { test } from "node:test";
import assert from "node:assert/strict";
import {
  OPERATOR_ROLE,
  delegateToggleLabel,
  isOperatorSession,
  partitionSessions,
} from "../src/sessionfilter.ts";

// The rows the default list is FOR: the human's own sessions, and the one
// recorded role a human restarts by hand.

test("a session with no orchestration identity is the human's own and is shown", () => {
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

test("the default hides every delegate and counts them", () => {
  const { shown, hidden } = partitionSessions(CORPUS, roleOf, false);
  assert.deepEqual(
    shown.map((r) => r.id),
    ["human-a", "orch-1", "human-b"]
  );
  assert.equal(hidden, 3);
});

test("the shown rows and the hidden count are one partition of the input", () => {
  // The count is what the toggle's label promises to reveal. If it and the
  // list are computed by two different passes they can disagree, and the
  // human clicks "show 3" and gets 4 — so pin the arithmetic, not just the
  // list.
  const { shown, hidden } = partitionSessions(CORPUS, roleOf, false);
  assert.equal(shown.length + hidden, CORPUS.length);
});

test("the toggle on shows everything, in the input's order, and hides nothing", () => {
  const { shown, hidden } = partitionSessions(CORPUS, roleOf, true);
  assert.deepEqual(
    shown.map((r) => r.id),
    CORPUS.map((r) => r.id)
  );
  assert.equal(hidden, 0, "nothing is hidden while the toggle is on");
});

test("the input array is never mutated, and the result is not an alias of it", () => {
  const before = CORPUS.map((r) => r.id);
  const on = partitionSessions(CORPUS, roleOf, true);
  const off = partitionSessions(CORPUS, roleOf, false);
  on.shown.push({ id: "intruder" });
  off.shown.push({ id: "intruder" });
  assert.deepEqual(
    CORPUS.map((r) => r.id),
    before,
    "the caller's array is the session store's own list"
  );
});

test("order is preserved — the scan already sorted newest-first", () => {
  const { shown } = partitionSessions(CORPUS, roleOf, false);
  const positions = shown.map((r) => CORPUS.indexOf(r));
  assert.deepEqual(positions, [...positions].sort((a, b) => a - b));
});

// ---- the toggle's label ----

test("the label says how many are hidden, singular and plural", () => {
  assert.equal(delegateToggleLabel(1, false), "Show 1 hidden agent session");
  assert.equal(delegateToggleLabel(7, false), "Show 7 hidden agent sessions");
});

test("with the toggle on, the label offers the way back", () => {
  assert.equal(delegateToggleLabel(0, true), "Hide agent sessions");
  assert.equal(delegateToggleLabel(7, true), "Hide agent sessions");
});

test("no toggle at all when nothing is hidden", () => {
  // Not "Show 0 hidden" and not a disabled control: offering to reveal rows
  // that do not exist promises something that is not there.
  assert.equal(delegateToggleLabel(0, false), null);
});

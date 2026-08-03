// Unit tests for the attention-scan gate (src/attentiongate.ts) — the bound
// `orch-attention` declares in test/perfpolicy.test.ts's stream manifest.
//
// Two properties, and the second is the one that makes the first safe:
//   1. an unchanged 3 s tick costs no pass at all; and
//   2. anything the pass reads — payload OR pane population — forces one.
//
// Red arm: make `shouldApply` return true unconditionally and the "unchanged"
// tests fail; drop `topology` from the recorded state and the rejoin test
// fails, which is the regression a payload-only diff would have shipped.
import { test } from "node:test";
import assert from "node:assert/strict";
import { AttentionGate, attentionSignature, type AttentionLike } from "../src/attentiongate.ts";

const item = (pty: number | null, reason = "waiting", detail = "asked a question"): AttentionLike => ({
  pty_id: pty,
  reason,
  detail,
});

/** One tab holding `n` panes, as main.ts's paneTopology() renders it. */
const topo = (...counts: number[]): string => counts.map((c, i) => `ws${i}:${c};`).join("");

test("the first scan after startup always applies", () => {
  const g = new AttentionGate();
  assert.equal(g.shouldApply([], topo(3)), true, "the UI has never been painted from a scan yet");
});

test("an unchanged tick costs no pass", () => {
  const g = new AttentionGate();
  const items = [item(7, "blocked", "waiting on approval"), item(9)];
  assert.equal(g.shouldApply(items, topo(2, 1)), true);
  for (let i = 0; i < 20; i++) {
    assert.equal(
      g.shouldApply([...items], topo(2, 1)),
      false,
      "a minute of identical 3 s re-emits must not walk every pane of every tab"
    );
  }
});

test("a reordered payload is not a change", () => {
  const g = new AttentionGate();
  const a = item(1, "blocked", "x");
  const b = item(2, "waiting", "y");
  assert.equal(g.shouldApply([a, b], topo(2)), true);
  assert.equal(
    g.shouldApply([b, a], topo(2)),
    false,
    "the backend's iteration order is not a contract; a reshuffle is not a state change"
  );
});

test("every kind of payload change forces a pass", () => {
  const base = [item(1, "waiting", "asked a question")];
  const cases: [string, AttentionLike[]][] = [
    ["a different reason", [item(1, "blocked", "asked a question")]],
    ["a different detail", [item(1, "waiting", "asked something else")]],
    ["an added pane", [item(1), item(2)]],
    ["a removed pane", []],
    ["the same reason on a different pty", [item(2, "waiting", "asked a question")]],
  ];
  for (const [what, next] of cases) {
    const g = new AttentionGate();
    assert.equal(g.shouldApply(base, topo(2)), true);
    assert.equal(g.shouldApply(next, topo(2)), true, `${what} must re-badge`);
  }
});

test("items bound to no pane are not part of what changed", () => {
  // The pass skips `pty_id: null` items entirely (they badge nothing), so one
  // appearing or vanishing must not cost a whole-window walk.
  const g = new AttentionGate();
  assert.equal(g.shouldApply([item(1)], topo(1)), true);
  assert.equal(g.shouldApply([item(1), item(null)], topo(1)), false);
});

test("a pane population change forces a pass under an IDENTICAL payload", () => {
  // The rejoin case, and the reason the gate takes a topology token at all: a
  // restored layout creates panes for agents ALREADY in the attention set, so
  // every tick around it carries a byte-identical payload. A payload-only diff
  // would leave those panes unbadged until some agent's state happened to move.
  const g = new AttentionGate();
  const items = [item(7, "blocked", "waiting on approval")];
  assert.equal(g.shouldApply(items, topo(1)), true);
  assert.equal(g.shouldApply(items, topo(1)), false);
  assert.equal(
    g.shouldApply(items, topo(3)),
    true,
    "two panes appeared under a steady payload — they must be badged, not stranded"
  );
  assert.equal(
    g.shouldApply(items, topo(3, 1)),
    true,
    "a new tab is a new place a badge can be owed"
  );
});

test("a detail string cannot forge another set's signature", () => {
  // Escaping, not paranoia: `detail` is backend text and a joined-string
  // fingerprint could be spelled by a payload that contains the separator. A
  // false "unchanged" holds a stale badge until the next real change.
  const a = attentionSignature([item(1, "waiting", 'x"],[2,"blocked","y')]);
  const b = attentionSignature([item(1, "waiting", "x"), item(2, "blocked", "y")]);
  assert.notEqual(a, b);
});

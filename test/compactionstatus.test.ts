// Compact-nudge lifecycle-panel surfacing (PR #329 round 6) — the pure
// derivations behind the group lifecycle panel's compaction status line and
// context-usage badge. What these tests defend: every `CompactionStatus`
// variant the backend can actually send maps to a label (or `null` for
// "none", so the row is omitted rather than rendered idle every tick), and
// the context badge never renders a placeholder before the first reading.

import { test } from "node:test";
import assert from "node:assert/strict";
import { compactionStatusLabel, compactionStatusTitle, contextUsageLabel } from "../src/compactionstatus.ts";
import type { CompactionStatus } from "../src/orchestration.ts";

test("compactionStatusLabel: none omits the row entirely", () => {
  const status: CompactionStatus = { status: "none" };
  assert.equal(compactionStatusLabel(status), null);
  assert.equal(compactionStatusTitle(status), null);
});

test("compactionStatusLabel: armed names the trust source", () => {
  assert.equal(compactionStatusLabel({ status: "armed", trusted: true, source: null }), "compact armed");
  assert.equal(compactionStatusLabel({ status: "armed", trusted: false, source: null }), "compact armed (unconfirmed)");
});

test("compactionStatusLabel: awaiting_evidence names the trust source", () => {
  assert.equal(
    compactionStatusLabel({ status: "awaiting_evidence", trusted: true, source: null }),
    "compact awaiting evidence"
  );
  assert.equal(
    compactionStatusLabel({ status: "awaiting_evidence", trusted: false, source: null }),
    "compact awaiting evidence (unconfirmed)"
  );
});

test("compactionStatusLabel: #417 hook-sourced evidence beats trusted/unconfirmed wording", () => {
  // A hook-confirmed arm IS trusted (no inference gate), but the label must
  // still distinguish it from the loomux-initiated trusted arm — a human
  // watching the panel should be able to tell "a hook told us" from "loomux
  // decided to compact" at a glance.
  assert.equal(
    compactionStatusLabel({ status: "armed", trusted: true, source: "hook" }),
    "compact armed (hook-confirmed)"
  );
});

test("compactionStatusLabel: round 10 — hook-confirmed awaiting_evidence reads as progress, not limbo", () => {
  // #428 follow-up, user-directed: a live re-test showed "compact awaiting
  // evidence (hook-confirmed)" read as stuck even though a hook had already
  // confirmed the outcome directly — only loomux's own poll was left to
  // consume the marker. The non-hook awaiting_evidence cases are genuinely
  // still undecided (busy-then-quiet hasn't resolved either way), so their
  // wording is unchanged — this is scoped to the hook source only.
  assert.equal(
    compactionStatusLabel({ status: "awaiting_evidence", trusted: true, source: "hook" }),
    "compact confirmed — finalizing"
  );
  assert.equal(
    compactionStatusLabel({ status: "awaiting_evidence", trusted: true, source: null }),
    "compact awaiting evidence",
    "unchanged: a genuinely undecided trusted arm"
  );
  assert.equal(
    compactionStatusLabel({ status: "awaiting_evidence", trusted: false, source: null }),
    "compact awaiting evidence (unconfirmed)",
    "unchanged: a genuinely undecided, unconfirmed arm"
  );
});

test("compactionStatusLabel: reinjecting shows the bounded attempt count", () => {
  assert.equal(
    compactionStatusLabel({ status: "reinjecting", attempt: 2, max_attempts: 3 }),
    "re-grounding (attempt 2/3)"
  );
});

test("compactionStatusLabel: abandoned names the three real lost-outcome reasons", () => {
  assert.equal(
    compactionStatusLabel({ status: "abandoned", reason: "arm-timeout", since_ms: 0 }),
    "compact timed out (no evidence)"
  );
  // Round 7: a PreCompact-only hook arm can still legitimately time out if
  // the agent's own turn never settles — but hook evidence WAS seen, so the
  // label must say so rather than falsely claiming none was recorded. (A
  // SessionStart-evidenced arm resolves immediately now and never reaches
  // "abandoned" at all — see the backend's compact_nudge_tick doc.)
  assert.equal(
    compactionStatusLabel({ status: "abandoned", reason: "arm-timeout-with-evidence", since_ms: 0 }),
    "compact timed out after hook evidence — resolution never observed"
  );
  assert.equal(
    compactionStatusLabel({ status: "abandoned", reason: "reinjection-abandoned", since_ms: 0 }),
    "compact re-grounding lost"
  );
  // An unrecognized reason (a future backend addition this frontend hasn't
  // learned yet) degrades to the raw string rather than throwing or hiding it.
  assert.equal(
    compactionStatusLabel({ status: "abandoned", reason: "something-new", since_ms: 0 }),
    "compact something-new"
  );
});

// #546 (option 3): `acked` closes the re-grounding phase on one of two
// evidence sources that are NOT equally strong — "delivery" is loomux watching
// its own Enter land, "activity" is only proof the agent is alive. What this
// test defends is that a reader can tell which one they got from the label
// alone, and that the weaker one's tooltip says outright what it does not
// prove. A label that read "re-grounding acked" for both would be the exact
// silent-conflation #546 filed.
test("compactionStatusLabel: acked names its evidence source, not just the outcome", () => {
  assert.equal(
    compactionStatusLabel({ status: "acked", source: "delivery", since_ms: 0 }),
    "re-grounding acked (delivery)"
  );
  assert.equal(
    compactionStatusLabel({ status: "acked", source: "activity", since_ms: 0 }),
    "re-grounding acked (activity)"
  );
  // The two must not render identically — that is the whole finding.
  assert.notEqual(
    compactionStatusLabel({ status: "acked", source: "delivery", since_ms: 0 }),
    compactionStatusLabel({ status: "acked", source: "activity", since_ms: 0 })
  );
});

test("compactionStatusTitle: an activity-sourced ack says what it does NOT prove", () => {
  const activity = compactionStatusTitle({ status: "acked", source: "activity", since_ms: 0 });
  assert.ok(activity?.includes("NOT that it read"), `must name the residual, got: ${activity}`);
  const delivery = compactionStatusTitle({ status: "acked", source: "delivery", since_ms: 0 });
  assert.ok(delivery?.includes("submit sampler"), `must name the mechanism, got: ${delivery}`);
  assert.notEqual(activity, delivery, "two different claims must not share one tooltip");
});

test("compactionStatusTitle: every non-none status has an explanatory tooltip", () => {
  const statuses: CompactionStatus[] = [
    { status: "armed", trusted: true, source: null },
    { status: "armed", trusted: false, source: null },
    { status: "armed", trusted: true, source: "hook" },
    { status: "awaiting_evidence", trusted: true, source: null },
    { status: "awaiting_evidence", trusted: false, source: null },
    { status: "awaiting_evidence", trusted: true, source: "hook" },
    { status: "reinjecting", attempt: 1, max_attempts: 3 },
    { status: "abandoned", reason: "arm-timeout", since_ms: 0 },
    { status: "abandoned", reason: "arm-timeout-with-evidence", since_ms: 0 },
    { status: "abandoned", reason: "reinjection-abandoned", since_ms: 0 },
    { status: "acked", source: "delivery", since_ms: 0 },
    { status: "acked", source: "activity", since_ms: 0 },
  ];
  for (const s of statuses) {
    const title = compactionStatusTitle(s);
    assert.ok(title && title.length > 0, `expected a tooltip for ${JSON.stringify(s)}`);
  }
});

test("contextUsageLabel: null before the first reading, not a placeholder", () => {
  assert.equal(contextUsageLabel({ tokens: null, percent: null }), null);
  assert.equal(contextUsageLabel({ tokens: null, percent: 10 }), null, "half-populated is still no reading");
  assert.equal(contextUsageLabel({ tokens: 40000, percent: null }), null, "half-populated is still no reading");
});

test("contextUsageLabel: formats tokens with separators", () => {
  assert.equal(contextUsageLabel({ tokens: 46120, percent: 23 }), "ctx 23% (46,120 tok)");
});

test("contextUsageLabel: zero is a real reading, not absence", () => {
  assert.equal(contextUsageLabel({ tokens: 0, percent: 0 }), "ctx 0% (0 tok)");
});

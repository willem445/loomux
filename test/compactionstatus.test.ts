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
  assert.equal(compactionStatusLabel({ status: "armed", trusted: true }), "compact armed");
  assert.equal(compactionStatusLabel({ status: "armed", trusted: false }), "compact armed (unconfirmed)");
});

test("compactionStatusLabel: awaiting_evidence names the trust source", () => {
  assert.equal(compactionStatusLabel({ status: "awaiting_evidence", trusted: true }), "compact awaiting evidence");
  assert.equal(
    compactionStatusLabel({ status: "awaiting_evidence", trusted: false }),
    "compact awaiting evidence (unconfirmed)"
  );
});

test("compactionStatusLabel: reinjecting shows the bounded attempt count", () => {
  assert.equal(
    compactionStatusLabel({ status: "reinjecting", attempt: 2, max_attempts: 3 }),
    "re-grounding (attempt 2/3)"
  );
});

test("compactionStatusLabel: abandoned names the two real lost-outcome reasons", () => {
  assert.equal(
    compactionStatusLabel({ status: "abandoned", reason: "arm-timeout", since_ms: 0 }),
    "compact timed out (no evidence)"
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

test("compactionStatusTitle: every non-none status has an explanatory tooltip", () => {
  const statuses: CompactionStatus[] = [
    { status: "armed", trusted: true },
    { status: "armed", trusted: false },
    { status: "awaiting_evidence", trusted: true },
    { status: "awaiting_evidence", trusted: false },
    { status: "reinjecting", attempt: 1, max_attempts: 3 },
    { status: "abandoned", reason: "arm-timeout", since_ms: 0 },
    { status: "abandoned", reason: "reinjection-abandoned", since_ms: 0 },
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

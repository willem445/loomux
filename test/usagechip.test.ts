// Unit tests for the bottom-toolbar agent usage-limit chip view (issue #80).
// The chip surfaces each agent CLI's limit consumption next to CPU/GPU/MEM. The
// pure `usageChipView` decides which scope is most-constrained, the chip text,
// the tooltip, and when to grey out to n/a — this is what those tests pin.
// Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { usageChipView } from "../src/usagechip.ts";
import type { UsageLimits } from "../src/metrics.ts";

const limits = (
  claude: UsageLimits["claude"]
): UsageLimits => ({ claude, copilot: null, note: "" });

// --- n/a fallback: nothing to show ---------------------------------------

test("null claude => n/a chip, and the tooltip explains why (never fabricated)", () => {
  const v = usageChipView(limits(null));
  assert.equal(v.na, true);
  assert.equal(v.text, "n/a");
  assert.equal(v.pct, 0);
  assert.match(v.title, /Copilot: it exposes no/i);
});

test("both scopes null => still n/a (no percentage to render)", () => {
  const v = usageChipView(limits({ session_pct: null, weekly_pct: null, source: "statusline" }));
  assert.equal(v.na, true);
});

// --- most-constrained selection ------------------------------------------

test("shows the higher (most-constrained) of session vs weekly", () => {
  const v = usageChipView(limits({ session_pct: 34, weekly_pct: 12, source: "statusline" }));
  assert.equal(v.na, false);
  assert.equal(v.pct, 34);
  assert.equal(v.text, "34%");
  // Tooltip carries BOTH scopes, not just the shown one.
  assert.match(v.title, /session 34%/);
  assert.match(v.title, /weekly 12%/);
});

test("weekly wins when it is the hotter scope", () => {
  const v = usageChipView(limits({ session_pct: 20, weekly_pct: 88, source: "statusline" }));
  assert.equal(v.pct, 88);
  assert.equal(v.text, "88%");
});

test("a single present scope drives the chip alone", () => {
  const v = usageChipView(limits({ session_pct: null, weekly_pct: 9, source: "statusline" }));
  assert.equal(v.na, false);
  assert.equal(v.pct, 9);
  assert.match(v.title, /weekly 9%/);
  assert.doesNotMatch(v.title, /session/);
});

// --- honesty labelling (consistent with the #42 cost work) ---------------

test('tooltip labels the figure "reported by the CLI, not estimated"', () => {
  const v = usageChipView(limits({ session_pct: 50, weekly_pct: null, source: "statusline" }));
  assert.match(v.title, /reported by the CLI, not estimated/i);
});

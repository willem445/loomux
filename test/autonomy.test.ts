// Unit tests for the pure autonomous-mode helpers (#83). Run with `npm test`
// (Node's built-in runner strips the TypeScript types natively).
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  requireApprovalChecked,
  autoMergeFromApproval,
  approvalControl,
  AUTO_MERGE_REQUIRES_AUTONOMOUS,
  budgetMeter,
  formatTokens,
  formatCountdown,
  tickStatusLabel,
  type TickStatus,
} from "../src/autonomy.ts";

// ---------- the auto-merge / require-approval inversion ----------

test("require-approval checkbox is the inverse of auto_merge", () => {
  // Default backend state (auto_merge off = human merge gate) → box checked.
  assert.equal(requireApprovalChecked(false), true);
  // Auto-merge on → approval not required → box unchecked.
  assert.equal(requireApprovalChecked(true), false);
});

test("toggling the approval checkbox sends the inverted auto_merge", () => {
  // Checking "require approval" turns auto_merge OFF.
  assert.equal(autoMergeFromApproval(true), false);
  // Unchecking it (let the orchestrator merge) turns auto_merge ON.
  assert.equal(autoMergeFromApproval(false), true);
});

test("inversion round-trips both directions", () => {
  for (const autoMerge of [true, false]) {
    const checked = requireApprovalChecked(autoMerge);
    assert.equal(autoMergeFromApproval(checked), autoMerge);
  }
});

// ---------- auto-merge depends on autonomous mode (#83 enforced gate) ----------

test("approval control is locked-checked while autonomous is off", () => {
  // Autonomous OFF: auto-merge can't exist, so the control is forced to
  // "approval required" and disabled with the explanatory tooltip — regardless of
  // any stale auto_merge flag (the backend reconciles it off too).
  for (const stale of [false, true]) {
    const c = approvalControl(false, stale);
    assert.equal(c.checked, true, "approval required while autonomous off");
    assert.equal(c.disabled, true, "the control is locked while autonomous off");
    assert.equal(c.tooltip, AUTO_MERGE_REQUIRES_AUTONOMOUS);
  }
});

test("approval control is editable and reflects auto_merge while autonomous on", () => {
  // Autonomous ON, auto_merge OFF → approval required, editable, no tooltip.
  const off = approvalControl(true, false);
  assert.deepEqual(off, { checked: true, disabled: false, tooltip: "" });
  // Autonomous ON, auto_merge ON → approval not required, editable.
  const on = approvalControl(true, true);
  assert.deepEqual(on, { checked: false, disabled: false, tooltip: "" });
});

// ---------- budget meter math ----------

test("no cap (budget 0) yields an empty, non-exhausted meter", () => {
  const m = budgetMeter(5000, 0);
  assert.equal(m.hasCap, false);
  assert.equal(m.fraction, 0);
  assert.equal(m.percent, 0);
  assert.equal(m.exhausted, false);
});

test("meter fraction and percent track spend against the cap", () => {
  const m = budgetMeter(2500, 10_000);
  assert.equal(m.hasCap, true);
  assert.equal(m.fraction, 0.25);
  assert.equal(m.percent, 25);
  assert.equal(m.exhausted, false);
});

test("meter clamps over-budget spend to 100% and marks exhausted", () => {
  const m = budgetMeter(15_000, 10_000);
  assert.equal(m.fraction, 1);
  assert.equal(m.percent, 100);
  assert.equal(m.exhausted, true);
});

test("exhaustion boundary matches the backend rule (spend >= budget)", () => {
  // Mirrors autonomy_budget_exhausted: crosses at exactly the cap.
  assert.equal(budgetMeter(9_999, 10_000).exhausted, false);
  assert.equal(budgetMeter(10_000, 10_000).exhausted, true);
  assert.equal(budgetMeter(10_001, 10_000).exhausted, true);
});

test("negative/skewed inputs floor at zero", () => {
  const m = budgetMeter(-500, -10);
  assert.equal(m.spend, 0);
  assert.equal(m.budget, 0);
  assert.equal(m.hasCap, false);
  assert.equal(m.fraction, 0);
});

// ---------- token formatting ----------

test("formatTokens is compact and honest", () => {
  assert.equal(formatTokens(0), "0");
  assert.equal(formatTokens(845), "845");
  assert.equal(formatTokens(1200), "1.2K");
  assert.equal(formatTokens(12_000), "12K");
  assert.equal(formatTokens(1_200_000), "1.20M");
});

// ---------- idle-tick countdown formatting ----------

test("formatCountdown renders compact human durations", () => {
  assert.equal(formatCountdown(0), "~0s");
  assert.equal(formatCountdown(45), "~45s");
  assert.equal(formatCountdown(60), "~1m");
  assert.equal(formatCountdown(200), "~3m 20s");
  assert.equal(formatCountdown(180), "~3m");
});

test("formatCountdown floors negative/skewed input at zero", () => {
  assert.equal(formatCountdown(-30), "~0s");
});

// ---------- idle-tick status → label mapping ----------

test("countdown-bearing statuses render the time", () => {
  assert.equal(tickStatusLabel("counting_down", 200), "next tick in ~3m 20s");
  assert.equal(tickStatusLabel("rate_capped", 90), "hourly cap — next in ~1m 30s");
});

test("eligible reads 'imminent' with no number (secs is 0, not rendered)", () => {
  assert.equal(tickStatusLabel("eligible", 0), "tick imminent");
});

test("non-time-gated statuses NEVER render a countdown, even if secs is passed", () => {
  // The null-countdown discipline: a stray number must not leak a lying timer.
  for (const status of ["starting", "paused", "waiting_for_activity"] as TickStatus[]) {
    const withNum = tickStatusLabel(status, 999);
    const withNull = tickStatusLabel(status, null);
    assert.equal(withNum, withNull, `${status} must ignore eligibleInSecs`);
    assert.ok(!/\d/.test(withNum), `${status} label must contain no digits: "${withNum}"`);
  }
});

test("specific non-time-gated labels", () => {
  assert.equal(tickStatusLabel("starting", null), "starting…");
  assert.equal(tickStatusLabel("paused", null), "paused — ticks suspended");
  assert.equal(
    tickStatusLabel("waiting_for_activity", null),
    "waiting (orchestrator recently active)"
  );
});

test("off renders empty (the caller hides the line)", () => {
  assert.equal(tickStatusLabel("off", null), "");
});

test("countdown statuses degrade gracefully if secs is unexpectedly null", () => {
  // Contract says these carry a real secs, but never throw / print 'null'.
  assert.equal(tickStatusLabel("counting_down", null), "counting down…");
  assert.equal(tickStatusLabel("rate_capped", null), "hourly cap reached");
});

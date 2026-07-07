// Unit tests for the pure autonomous-mode helpers (#83). Run with `npm test`
// (Node's built-in runner strips the TypeScript types natively).
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  requireApprovalChecked,
  autoMergeFromApproval,
  budgetMeter,
  formatTokens,
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

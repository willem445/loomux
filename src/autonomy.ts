// Pure decision/format helpers for the group panel's "Autonomous mode" section
// (#83, W2). No Tauri or DOM imports, so it's unit-testable under `node --test`
// (mirrors spawnexpiry.ts / orchbadge.ts). groupview.ts owns the DOM wiring and
// imports from here; orchestration.ts owns the typed command wrappers.
//
// The tricky bits this isolates and tests:
//   1. The auto-merge inversion. The backend flag is `auto_merge` (ON = the
//      orchestrator may merge itself). The human-facing control is the opposite
//      framing — a "Require human approval before merge" checkbox that is ON by
//      default (today's behaviour). Checkbox ON ⇔ auto_merge OFF. One place to
//      get the negation right.
//   2. The budget meter math (spend vs cap → fraction / percent / exhausted),
//      mirroring the backend `autonomy_budget_exhausted` rule.
//
// Suspension is *not* reconstructed here: `orch_autonomy` reports it directly
// via `suspended` (true iff the budget enforcer flipped autonomy off, from a
// durable marker), so the panel just reads the flag.

/** The whole autonomous-mode panel state, as returned by `orch_autonomy`.
 *  `spend_since_enable_tokens` is null when autonomous is off (no live meter).
 *  `suspended` is true only while off, and only when the budget enforcer (not
 *  the user) turned autonomy off — so the UI can show a distinct exhausted
 *  state vs a plain toggle-off. */
export interface AutonomyState {
  autonomous: boolean;
  auto_merge: boolean;
  budget_tokens: number;
  budget_anchor_tokens: number;
  spend_since_enable_tokens: number | null;
  suspended: boolean;
}

// ---------- auto-merge ⇔ require-approval inversion ----------

/** Whether the "Require human approval before merge" checkbox is checked, given
 *  the backend `auto_merge` flag. Checked = approval required = auto_merge OFF.
 *  Default (auto_merge false) → checked, i.e. today's human merge gate. */
export function requireApprovalChecked(autoMerge: boolean): boolean {
  return !autoMerge;
}

/** The `auto_merge` value to send when the approval checkbox is toggled to
 *  `checked`. The inverse of `requireApprovalChecked` — checking the box (demand
 *  approval) means auto_merge OFF. */
export function autoMergeFromApproval(checked: boolean): boolean {
  return !checked;
}

// ---------- budget meter math ----------

/** A rendered view of autonomous-era spend against the token budget. */
export interface BudgetMeter {
  /** A cap is set (budget > 0). When false there is no meter, just a spend read. */
  hasCap: boolean;
  spend: number;
  budget: number;
  /** Spend / budget, clamped to 0..1 (0 when there's no cap). Drives the bar. */
  fraction: number;
  /** `fraction` as a 0..100 integer, for the label. */
  percent: number;
  /** Cap set and spend has reached it — mirrors the backend suspension rule. */
  exhausted: boolean;
}

/** Meter a spend against a budget. Mirrors the backend `autonomy_budget_exhausted`
 *  (`budget != 0 && spend >= budget`) so the UI and the enforcement agree on the
 *  crossing point. Negative inputs (clock/label skew) are floored at 0. */
export function budgetMeter(spend: number, budget: number): BudgetMeter {
  const s = Math.max(0, spend);
  const b = Math.max(0, budget);
  const hasCap = b > 0;
  const fraction = hasCap ? Math.min(1, s / b) : 0;
  return {
    hasCap,
    spend: s,
    budget: b,
    fraction,
    percent: Math.round(fraction * 100),
    exhausted: hasCap && s >= b,
  };
}

/** Compact human token count: "845", "12K", "1.20M". Matches the group panel's
 *  cost formatting so the meter reads consistently with the cost lines. */
export function formatTokens(n: number): string {
  const v = Math.max(0, Math.round(n));
  if (v < 1000) return `${v}`;
  if (v < 1_000_000) return `${(v / 1000).toFixed(v < 10_000 ? 1 : 0)}K`;
  return `${(v / 1_000_000).toFixed(2)}M`;
}

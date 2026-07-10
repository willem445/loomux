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
//   3. The idle-tick status → human label mapping, including the null-countdown
//      discipline: a countdown is rendered ONLY for the statuses the backend
//      gives a real `eligible_in_secs` (counting_down / eligible / rate_capped);
//      the others (starting / paused / waiting_for_activity) never show a number,
//      even if one were passed — the whole point of the backend's rev-59 rework.
//
// Suspension is *not* reconstructed here: `orch_autonomy` reports it directly
// via `suspended` (true iff the budget enforcer flipped autonomy off, from a
// durable marker), so the panel just reads the flag.

/** The idle-tick lifecycle status the backend surfaces (`tick_status`). Only
 *  `counting_down` / `eligible` / `rate_capped` carry a real `eligible_in_secs`
 *  countdown; the rest are gated by something other than time. */
export type TickStatus =
  | "off"
  | "starting"
  | "paused"
  | "counting_down"
  | "eligible"
  | "waiting_for_activity"
  | "rate_capped";

/** The whole autonomous-mode panel state, as returned by `orch_autonomy`.
 *  `spend_since_enable_tokens` is null when autonomous is off (no live meter).
 *  `suspended` is true only while off, and only when the budget enforcer (not
 *  the user) turned autonomy off — so the UI can show a distinct exhausted
 *  state vs a plain toggle-off. The idle-tick fields drive the observability
 *  line + the two knobs (`idle_tick_minutes`, `idle_activity_floor_bytes`);
 *  `eligible_in_secs`/`quiet_secs` are null unless a live countdown exists. */
export interface AutonomyState {
  autonomous: boolean;
  auto_merge: boolean;
  /** #83: whether the orchestrator may publish releases/tags itself (independent
   *  of auto_merge; default OFF = releases need a per-tag human grant). */
  auto_release: boolean;
  /** #83 supervised dangerous mode: the human is present and authorized manual
   *  merges/releases WITHOUT autonomous. Mutually exclusive with `autonomous`
   *  (enabling autonomous clears it; enabling this while autonomous is rejected). */
  dangerous_mode: boolean;
  budget_tokens: number;
  budget_anchor_tokens: number;
  spend_since_enable_tokens: number | null;
  suspended: boolean;
  idle_tick_minutes: number;
  idle_activity_floor_bytes: number;
  tick_status: TickStatus;
  eligible_in_secs: number | null;
  quiet_secs: number | null;
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

/** How the "Require human approval before merge" checkbox renders, given the two
 *  backend flags. Encodes the #83 **dependency**: auto-merge authority exists ONLY
 *  in autonomous mode (the backend rejects enabling it otherwise, and force-clears
 *  it when autonomous turns off), so with autonomous OFF the control is locked to
 *  checked (= approval required = the enforced human gate) and disabled with an
 *  explanatory tooltip. With autonomous ON it reflects `auto_merge` and is
 *  editable. Pure so the disabled/tooltip logic is tested without a DOM. */
export interface ApprovalControl {
  /** "Require human approval" checkbox state (checked = auto_merge OFF). */
  checked: boolean;
  /** True when the control can't be changed (autonomous off → auto-merge forbidden). */
  disabled: boolean;
  /** Tooltip explaining the disabled state; "" when editable. */
  tooltip: string;
}

/** The disabled tooltip — one place so the UI and its tests agree. */
export const AUTO_MERGE_REQUIRES_AUTONOMOUS = "auto-merge requires Autonomous mode";

export function approvalControl(autonomous: boolean, autoMerge: boolean): ApprovalControl {
  if (!autonomous) {
    // Auto-merge is impossible while autonomous is off, so approval is forced on
    // and locked — never surface an editable "allow auto-merge" the backend would
    // reject. Ignores any stale `autoMerge` (the backend reconciles it off too).
    return { checked: true, disabled: true, tooltip: AUTO_MERGE_REQUIRES_AUTONOMOUS };
  }
  return { checked: requireApprovalChecked(autoMerge), disabled: false, tooltip: "" };
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

// ---------- idle-tick observability ----------

/** Approximate human countdown: "~45s", "~3m", "~3m 20s". Floors negatives at 0. */
export function formatCountdown(secs: number): string {
  const s = Math.max(0, Math.round(secs));
  if (s < 60) return `~${s}s`;
  const m = Math.floor(s / 60);
  const rem = s % 60;
  return rem === 0 ? `~${m}m` : `~${m}m ${rem}s`;
}

/** The idle-tick status as a human line for the panel. Enforces the null-
 *  countdown discipline: a time is rendered ONLY for `counting_down` and
 *  `rate_capped` (which carry a real `eligibleInSecs`); `eligible` reads
 *  "imminent" without a number; and `starting` / `paused` /
 *  `waiting_for_activity` never show a countdown, even if a stray `eligibleInSecs`
 *  is passed — those gates aren't time-based, so a ticking number there would
 *  lie (the exact bug the backend rev-59 rework removed). Returns "" for `off`
 *  (the caller hides the line when autonomy is off). */
export function tickStatusLabel(status: TickStatus, eligibleInSecs: number | null): string {
  switch (status) {
    case "off":
      return "";
    case "starting":
      return "starting…";
    case "paused":
      return "paused — ticks suspended";
    case "eligible":
      return "tick imminent";
    case "waiting_for_activity":
      return "waiting (orchestrator recently active)";
    case "counting_down":
      return eligibleInSecs == null
        ? "counting down…"
        : `next tick in ${formatCountdown(eligibleInSecs)}`;
    case "rate_capped":
      return eligibleInSecs == null
        ? "hourly cap reached"
        : `hourly cap — next in ${formatCountdown(eligibleInSecs)}`;
    default:
      return "";
  }
}

// ---------- human grant inputs (approve-with-comment / release, #83) ----------

/** Normalize an optional free-text grant comment to what the backend commands
 *  expect: the trimmed string, or `null` when empty/whitespace (→ Rust
 *  `Option::None`, i.e. "grant only, no note"). Used by both the board
 *  approve-with-comment flow and the release-grant control. */
export function normalizeComment(raw: string): string | null {
  const t = raw.trim();
  return t === "" ? null : t;
}

/** Whether a release tag is well-formed enough to authorize: non-empty after
 *  trim and free of internal whitespace (a git tag can't contain spaces; the
 *  backend sanitizes further). Gates the release-grant button so an obviously
 *  invalid tag never round-trips. */
export function isValidReleaseTag(raw: string): boolean {
  const t = raw.trim();
  return t.length > 0 && !/\s/.test(t);
}

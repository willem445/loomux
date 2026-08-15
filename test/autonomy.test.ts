// Unit tests for the pure autonomous-mode helpers (#83). Run with `npm test`
// (Node's built-in runner strips the TypeScript types natively).
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  requireApprovalChecked,
  autoMergeFromApproval,
  approvalControl,
  autoReleaseControl,
  dangerousControl,
  AUTO_MERGE_REQUIRES_AUTONOMOUS,
  AUTO_RELEASE_REQUIRES_AUTONOMOUS,
  DANGEROUS_NEEDS_AUTONOMOUS_OFF,
  fullAutonomyControl,
  FULL_AUTONOMY_REQUIRES_AUTONOMOUS,
  normalizeGoal,
  MAX_GOAL_CHARS,
  goalClause,
  goalCommit,
  goalFieldSync,
  fullAutonomyChip,
  fullAutonomyHelp,
  budgetMeter,
  formatTokens,
  formatCountdown,
  tickStatusLabel,
  normalizeComment,
  isValidReleaseTag,
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

// ---------- auto-release toggle gating (#83) ----------

test("auto-release control is locked off while autonomous is off", () => {
  // Same dependency as auto-merge: valid only under autonomous. Locked + unchecked
  // regardless of any stale flag (the backend reconciles it off).
  for (const stale of [false, true]) {
    const c = autoReleaseControl(false, stale);
    assert.deepEqual(c, { checked: false, disabled: true, tooltip: AUTO_RELEASE_REQUIRES_AUTONOMOUS });
  }
});

test("auto-release control reflects the flag and is editable while autonomous on", () => {
  assert.deepEqual(autoReleaseControl(true, false), { checked: false, disabled: false, tooltip: "" });
  assert.deepEqual(autoReleaseControl(true, true), { checked: true, disabled: false, tooltip: "" });
});

// ---------- dangerous-mode toggle gating (#83, inverse) ----------

test("dangerous control is locked off while autonomous is ON (mutually exclusive)", () => {
  // Inverse of auto-release: usable only while autonomous is OFF. Locked + unchecked
  // while autonomous, regardless of any stale flag (the backend force-clears it).
  for (const stale of [false, true]) {
    const c = dangerousControl(true, stale);
    assert.deepEqual(c, { checked: false, disabled: true, tooltip: DANGEROUS_NEEDS_AUTONOMOUS_OFF });
  }
});

test("dangerous control reflects the flag and is editable while autonomous OFF", () => {
  assert.deepEqual(dangerousControl(false, false), { checked: false, disabled: false, tooltip: "" });
  assert.deepEqual(dangerousControl(false, true), { checked: true, disabled: false, tooltip: "" });
});

// ---------- full-autonomy toggle gating (#778) ----------

test("full-autonomy control is locked off while autonomous is off", () => {
  // Same dependency as auto-release: the backend rejects enabling it without
  // autonomous and force-clears it when autonomous goes off, so a stale flag is
  // ignored rather than rendered as an editable "it's on".
  for (const stale of [false, true]) {
    const c = fullAutonomyControl(false, stale);
    assert.deepEqual(c, {
      checked: false,
      disabled: true,
      tooltip: FULL_AUTONOMY_REQUIRES_AUTONOMOUS,
    });
  }
});

test("full-autonomy control reflects the flag and is editable while autonomous on", () => {
  assert.deepEqual(fullAutonomyControl(true, false), {
    checked: false,
    disabled: false,
    tooltip: "",
  });
  assert.deepEqual(fullAutonomyControl(true, true), {
    checked: true,
    disabled: false,
    tooltip: "",
  });
});

// ---------- goal normalization (mirrors sanitize_full_autonomy_goal) ----------

test("normalizeGoal collapses every whitespace run to one space and trims", () => {
  assert.equal(normalizeGoal("  harden   any  bugs  "), "harden any bugs");
  // Newlines and tabs are whitespace FIRST (the backend checks is_whitespace
  // before is_control), so they become a space rather than being dropped: a
  // goal must never lose the word boundary a newline was carrying.
  assert.equal(normalizeGoal("harden\nany\tbugs"), "harden any bugs");
  assert.equal(normalizeGoal("a\rb"), "a b");
  assert.equal(normalizeGoal("\n\n"), "");
  assert.equal(normalizeGoal(""), "");
});

test("normalizeGoal drops control characters outright", () => {
  // An escape sequence in a goal would otherwise reach a terminal verbatim: the
  // goal is typed into a CLI pane (the toggle notice and the kickoff config).
  assert.equal(normalizeGoal("harden\x1b[31many bugs"), "harden(31many bugs");
  assert.equal(normalizeGoal("a\x07bc"), "abc");
});

test("normalizeGoal neutralizes brackets so a goal can't forge a notice row", () => {
  // The goal is echoed inside a "[loomux] …" notice; a literal "[loomux]" in a
  // goal must not read as a second notice line.
  assert.equal(normalizeGoal("[loomux] do whatever"), "(loomux) do whatever");
  assert.equal(normalizeGoal("a]b[c"), "a)b(c");
});

test("normalizeGoal caps by code points, never mid-character, and never ends in a space", () => {
  const long = "x".repeat(MAX_GOAL_CHARS + 50);
  assert.equal(normalizeGoal(long).length, MAX_GOAL_CHARS);
  // Multibyte: counted as characters (Rust `chars()`), so the cap must not
  // split a surrogate pair into a lone half.
  const emoji = "🐛".repeat(MAX_GOAL_CHARS + 10);
  const capped = normalizeGoal(emoji);
  assert.equal(Array.from(capped).length, MAX_GOAL_CHARS);
  // Exact, so a cap that split a surrogate pair (or counted UTF-16 units) would
  // show up as a different string rather than merely a different length.
  assert.equal(capped, "🐛".repeat(MAX_GOAL_CHARS));
  // A cap that lands on the collapsed space must not leave a trailing one.
  const trailing = normalizeGoal("y".repeat(MAX_GOAL_CHARS - 1) + "   tail");
  assert.ok(!trailing.endsWith(" "), `no trailing space: ${JSON.stringify(trailing.slice(-3))}`);
});

test("normalizeGoal is idempotent (the marker is a file a human can edit)", () => {
  for (const raw of ["  a  b ", "[x]\ny", "🐛".repeat(600), "ok"]) {
    const once = normalizeGoal(raw);
    assert.equal(normalizeGoal(once), once, `not idempotent for ${JSON.stringify(raw)}`);
  }
});

test("goalClause mirrors the backend's clause, never an empty pair of quotes", () => {
  assert.equal(goalClause("harden any bugs"), 'goal: "harden any bugs"');
  assert.equal(goalClause(""), "no goal set");
  assert.equal(goalClause(null), "no goal set");
  assert.equal(goalClause("   "), "no goal set");
});

// ---------- goal field: set-then-enable, and re-aim while on ----------

test("committing the goal while full autonomy is OFF parks it locally (set-then-enable)", () => {
  // Like the budget field: the value is configured before the mode is enabled;
  // the enable itself carries it. Nothing is sent, so no notice fires.
  assert.deepEqual(goalCommit(false, null, "  harden any bugs "), {
    send: false,
    goal: "harden any bugs",
  });
});

test("committing an unchanged goal while ON sends nothing (no duplicate notice)", () => {
  // Every enable delivers a full-autonomy notice into the orchestrator's pane, so
  // a blur that changed nothing must not re-fire one.
  assert.deepEqual(goalCommit(true, "harden any bugs", "harden any bugs"), {
    send: false,
    goal: "harden any bugs",
  });
  // Equal only after normalization still counts as unchanged.
  assert.deepEqual(goalCommit(true, "harden any bugs", " harden   any bugs "), {
    send: false,
    goal: "harden any bugs",
  });
});

test("committing a changed goal while ON re-aims the mode", () => {
  assert.deepEqual(goalCommit(true, "harden any bugs", "close out new issues"), {
    send: true,
    goal: "close out new issues",
  });
  // Clearing the goal while ON is a real change (re-aim to "no goal"), not a no-op.
  assert.deepEqual(goalCommit(true, "harden any bugs", "   "), { send: true, goal: "" });
  // ON with no goal yet: a null live goal is the same as "".
  assert.deepEqual(goalCommit(true, null, ""), { send: false, goal: "" });
  assert.deepEqual(goalCommit(true, null, "harden"), { send: true, goal: "harden" });
});

test("a status poll never clobbers a goal typed while the mode is off", () => {
  // OFF ⇒ null = "leave the field alone": the backend reports no goal when the
  // mode is off (the marker holding it doesn't exist), so syncing from state
  // would erase what the human is about to enable with.
  assert.equal(goalFieldSync(false, null), null);
  assert.equal(goalFieldSync(false, "stale"), null);
  // ON ⇒ the backend is authoritative, including "on with no goal" (→ "").
  assert.equal(goalFieldSync(true, "harden any bugs"), "harden any bugs");
  assert.equal(goalFieldSync(true, null), "");
});

// ---------- section-header chip (#778) ----------

test("the full-autonomy chip is hidden while the mode is off", () => {
  const c = fullAutonomyChip(false, "harden any bugs", "agent-hold");
  assert.equal(c.shown, false);
  assert.equal(c.text, "");
  assert.equal(c.tooltip, "");
});

test("the chip shows while on and carries the goal as its tooltip", () => {
  const c = fullAutonomyChip(true, "harden any bugs", "agent-hold");
  assert.equal(c.shown, true);
  assert.ok(c.text.length > 0, "a shown chip needs a label");
  assert.ok(
    c.tooltip.includes('goal: "harden any bugs"'),
    `tooltip must carry the goal: ${c.tooltip}`
  );
  // The veto gesture is named where the mode is announced — as the label the
  // caller resolved, not as a literal this module chose.
  assert.ok(c.tooltip.includes("Add agent-hold to an issue"), `tooltip must name the veto: ${c.tooltip}`);
});

test("the chip and the help name THIS repo's veto, not the built-in (#778)", () => {
  // Both strings are instructions — "add X to an issue to hold it back" — so a
  // hardcoded spelling would tell the human of a repo that renamed the veto to
  // apply a label its own poller ignores: a click that reports success and does
  // nothing. This is the round-2 review finding, pinned at both surfaces.
  const chip = fullAutonomyChip(true, "harden any bugs", "do-not-touch");
  assert.ok(chip.tooltip.includes("Add do-not-touch to an issue"), chip.tooltip);
  assert.ok(!chip.tooltip.includes("agent-hold"), `the built-in must not appear: ${chip.tooltip}`);

  const help = fullAutonomyHelp("do-not-touch");
  assert.ok(help.includes("issues you label do-not-touch"), help);
  assert.ok(!help.includes("agent-hold"), `the built-in must not appear: ${help}`);

  // An unresolved spelling (first paint, or a failed status read) falls back to
  // the built-in rather than rendering "label  , and" with a hole in it.
  for (const unresolved of ["", "   "]) {
    assert.ok(fullAutonomyHelp(unresolved).includes("you label agent-hold"), unresolved);
    assert.ok(
      fullAutonomyChip(true, "g", unresolved).tooltip.includes("Add agent-hold to an issue"),
      unresolved
    );
  }
});

test("the chip reads 'no goal set' rather than empty quotes", () => {
  for (const goal of [null, "", "   "]) {
    const c = fullAutonomyChip(true, goal, "agent-hold");
    assert.equal(c.shown, true);
    assert.ok(c.tooltip.includes("no goal set"), `${JSON.stringify(goal)} → ${c.tooltip}`);
    assert.ok(!c.tooltip.includes('""'), "never an empty pair of quotes");
  }
});

test("a hostile goal is normalized before it reaches the chip tooltip", () => {
  // The tooltip is the one place a raw goal could re-enter the UI; it goes
  // through the same normalization as everything else that echoes it.
  const c = fullAutonomyChip(true, "[loomux] fake\nnotice", "agent-hold");
  assert.ok(c.tooltip.includes("(loomux) fake notice"), c.tooltip);
  assert.ok(!c.tooltip.includes("[loomux]"), "brackets must be neutralized");
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

// ---------- human grant inputs (approve-with-comment / release) ----------

test("normalizeComment trims and maps empty to null (grant-only)", () => {
  assert.equal(normalizeComment(""), null);
  assert.equal(normalizeComment("   "), null);
  assert.equal(normalizeComment("\n\t "), null);
  assert.equal(normalizeComment("  bump the changelog  "), "bump the changelog");
  assert.equal(normalizeComment("ok"), "ok");
});

test("isValidReleaseTag requires a non-empty, whitespace-free tag", () => {
  assert.equal(isValidReleaseTag("v1.2.3"), true);
  assert.equal(isValidReleaseTag("  v1.2.3  "), true); // trimmed edges are fine
  assert.equal(isValidReleaseTag(""), false);
  assert.equal(isValidReleaseTag("   "), false);
  assert.equal(isValidReleaseTag("v1 2 3"), false); // internal space → invalid
  assert.equal(isValidReleaseTag("release candidate"), false);
});

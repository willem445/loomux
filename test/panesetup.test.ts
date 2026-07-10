// Unit tests for the pure pane-setup core (#194): the kind → result matrix and
// the validation rules that back the welcome/pane-setup screen. Run with
// `npm test`. DOM-free — the form's async side effects (probe, worktree
// creation, autopilot flags) are validated by hand, not here.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  planPaneSetup,
  pathTail,
  worktreeNameFor,
  SubmitLatch,
  type PaneSetupInput,
} from "../src/panesetup.ts";

/** A fully-populated input; each test overrides only the fields it exercises. */
function input(over: Partial<PaneSetupInput>): PaneSetupInput {
  return {
    kind: "agent",
    agentId: "claude",
    isCustom: false,
    builtinCommand: "claude",
    customCommand: "",
    count: 1,
    repo: "",
    worktree: "",
    name: "",
    autopilot: true,
    shellKind: "powershell",
    ...over,
  };
}

// ---------- terminal ----------

test("terminal always validates; empty repo means home", () => {
  const res = planPaneSetup(input({ kind: "terminal", repo: "" }));
  assert.equal(res.ok, true);
  assert.ok(res.ok && res.plan.kind === "terminal");
  if (res.ok && res.plan.kind === "terminal") {
    assert.equal(res.plan.cwd, null);
    assert.equal(res.plan.shellKind, "powershell");
    assert.equal(res.plan.name, "terminal");
  }
});

test("terminal carries the chosen shell kind through (plumbed for P2)", () => {
  for (const shellKind of ["powershell", "gitbash", "cmd"] as const) {
    const res = planPaneSetup(input({ kind: "terminal", shellKind }));
    assert.ok(res.ok && res.plan.kind === "terminal" && res.plan.shellKind === shellKind);
  }
});

test("terminal cwd + default name come from the repo path tail", () => {
  const res = planPaneSetup(input({ kind: "terminal", repo: "  C:\\Projects\\loomux\\  " }));
  assert.ok(res.ok && res.plan.kind === "terminal");
  if (res.ok && res.plan.kind === "terminal") {
    // Whitespace is trimmed (the raw path is otherwise passed through as the
    // shell cwd, exactly like the agent path — no slash normalization).
    assert.equal(res.plan.cwd, "C:\\Projects\\loomux\\");
    assert.equal(res.plan.name, "loomux");
  }
});

// ---------- orchestrator ----------

test("orchestrator requires a repository", () => {
  const res = planPaneSetup(input({ kind: "orchestrator", repo: "  " }));
  assert.equal(res.ok, false);
  assert.ok(!res.ok && res.focus === "repo");
});

test("orchestrator with a repo validates and trims", () => {
  const res = planPaneSetup(input({ kind: "orchestrator", repo: "  /repo/x  " }));
  assert.ok(res.ok && res.plan.kind === "orchestrator" && res.plan.repo === "/repo/x");
});

// ---------- agent ----------

test("agent with a built-in CLI validates", () => {
  const res = planPaneSetup(input({ kind: "agent", builtinCommand: "claude" }));
  assert.ok(res.ok && res.plan.kind === "agent");
  if (res.ok && res.plan.kind === "agent") {
    assert.equal(res.plan.command, "claude");
    assert.equal(res.plan.count, 1);
    assert.equal(res.plan.baseName, "claude"); // blank name → command
  }
});

test("custom agent needs a command", () => {
  const res = planPaneSetup(input({ isCustom: true, builtinCommand: "", customCommand: "   " }));
  assert.equal(res.ok, false);
  assert.ok(!res.ok && res.focus === "custom");
});

test("custom agent uses the custom command, not the built-in", () => {
  const res = planPaneSetup(
    input({ isCustom: true, builtinCommand: "claude", customCommand: " aider --model sonnet " })
  );
  assert.ok(res.ok && res.plan.kind === "agent");
  if (res.ok && res.plan.kind === "agent") {
    assert.equal(res.plan.command, "aider --model sonnet");
    assert.equal(res.plan.isCustom, true);
  }
});

test("a worktree without a repo is rejected", () => {
  const res = planPaneSetup(input({ worktree: "fix-auth", repo: "" }));
  assert.equal(res.ok, false);
  assert.ok(!res.ok && res.focus === "repo");
});

test("a worktree with a repo is allowed", () => {
  const res = planPaneSetup(input({ worktree: "fix-auth", repo: "/repo" }));
  assert.ok(res.ok && res.plan.kind === "agent" && res.plan.worktree === "fix-auth");
});

test("pane count is clamped into [1, 8]", () => {
  assert.equal(pick(planPaneSetup(input({ count: 0 }))), 1);
  assert.equal(pick(planPaneSetup(input({ count: 3 }))), 3);
  assert.equal(pick(planPaneSetup(input({ count: 99 }))), 8);
  assert.equal(pick(planPaneSetup(input({ count: 2.9 }))), 2); // truncated
  assert.equal(pick(planPaneSetup(input({ count: NaN }))), 1);
});

/** Extract the clamped count from an agent result (test helper). */
function pick(res: ReturnType<typeof planPaneSetup>): number {
  assert.ok(res.ok && res.plan.kind === "agent");
  return res.ok && res.plan.kind === "agent" ? res.plan.count : -1;
}

test("a typed name overrides the command default", () => {
  const res = planPaneSetup(input({ name: "  my pane  " }));
  assert.ok(res.ok && res.plan.kind === "agent" && res.plan.baseName === "my pane");
});

test("autopilot flag rides through to the plan", () => {
  const on = planPaneSetup(input({ autopilot: true }));
  const off = planPaneSetup(input({ autopilot: false }));
  assert.ok(on.ok && on.plan.kind === "agent" && on.plan.autopilot === true);
  assert.ok(off.ok && off.plan.kind === "agent" && off.plan.autopilot === false);
});

// ---------- pure helpers ----------

test("pathTail returns the last non-empty segment", () => {
  assert.equal(pathTail("C:\\a\\b\\c"), "c");
  assert.equal(pathTail("/x/y/z/"), "z");
  assert.equal(pathTail(""), "");
  assert.equal(pathTail("solo"), "solo");
});

test("worktreeNameFor keeps a single name but fans out a fleet", () => {
  assert.equal(worktreeNameFor("fix-auth", 1, 1), "fix-auth");
  assert.equal(worktreeNameFor("fix-auth", 1, 3), "fix-auth-1");
  assert.equal(worktreeNameFor("fix-auth", 3, 3), "fix-auth-3");
});

// ---------- submit latch (rev-74 HIGH-1: no duplicate launches) ----------

test("SubmitLatch admits only the first of concurrent begins", () => {
  const latch = new SubmitLatch();
  assert.equal(latch.begin(), true); // first click enters
  assert.equal(latch.begin(), false); // double-click / Enter-repeat is rejected
  assert.equal(latch.begin(), false); // …and every further re-entry while in flight
});

test("SubmitLatch reopens after a validation error so the user can retry", () => {
  const latch = new SubmitLatch();
  assert.equal(latch.begin(), true);
  latch.release(); // planPaneSetup returned an error; allow a fixed retry
  assert.equal(latch.settled, false);
  assert.equal(latch.begin(), true); // retry admitted
});

test("SubmitLatch is one-shot once a submit finishes", () => {
  const latch = new SubmitLatch();
  assert.equal(latch.begin(), true);
  latch.finish(); // onSubmit fired — the pane is being converted/retired
  assert.equal(latch.settled, true);
  assert.equal(latch.begin(), false); // a late re-entry must never fire again
  latch.release(); // even an errant release can't reopen a finished latch
  assert.equal(latch.begin(), false);
});

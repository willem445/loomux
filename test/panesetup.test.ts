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
  shellKindOptions,
  resolveShellKind,
  isContentKind,
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

test("terminal carries the chosen shell kind through", () => {
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

// ---------- shell kinds (#194 P2) ----------

test("PowerShell and cmd are always enabled; Git Bash follows discovery", () => {
  const withBash = shellKindOptions({ gitBashPath: "C:\\Program Files\\Git\\bin\\bash.exe" });
  const ps = withBash.find((o) => o.key === "powershell");
  const cmd = withBash.find((o) => o.key === "cmd");
  const bash = withBash.find((o) => o.key === "gitbash");
  assert.ok(ps?.enabled && cmd?.enabled && bash?.enabled);
  // No reason text on an enabled option.
  assert.equal(bash?.reason, "");
});

test("Git Bash is disabled with a reason when not installed", () => {
  const opts = shellKindOptions({ gitBashPath: null });
  const bash = opts.find((o) => o.key === "gitbash");
  assert.equal(bash?.enabled, false);
  assert.match(bash?.reason ?? "", /Git for Windows/);
  // The always-available kinds are unaffected.
  assert.ok(opts.find((o) => o.key === "powershell")?.enabled);
  assert.ok(opts.find((o) => o.key === "cmd")?.enabled);
});

test("resolveShellKind keeps an available kind but falls unavailable ones back to PowerShell", () => {
  const installed = { gitBashPath: "C:\\Git\\bin\\bash.exe" };
  const missing = { gitBashPath: null };
  // Available → unchanged.
  assert.equal(resolveShellKind("gitbash", installed), "gitbash");
  assert.equal(resolveShellKind("cmd", missing), "cmd");
  assert.equal(resolveShellKind("powershell", missing), "powershell");
  // Requested-but-unavailable Git Bash → PowerShell fallback.
  assert.equal(resolveShellKind("gitbash", missing), "powershell");
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

// ---------- files (#214) ----------

test("a file explorer needs a folder — it does NOT fall back to home like a terminal", () => {
  // A terminal with no repo opens in home, which is useful. A file tree over the
  // whole home directory is not, and a rootless files pane has no content at all —
  // so this is a hard error that bounces the user back to the field.
  const res = planPaneSetup(input({ kind: "files", repo: "  " }));
  assert.equal(res.ok, false);
  assert.ok(!res.ok && res.focus === "repo");
  assert.match(res.ok ? "" : res.error, /folder/i);
});

test("a file explorer plans a root + name, and nothing else — no command, no shell", () => {
  const res = planPaneSetup(input({ kind: "files", repo: "  C:/Projects/loomux  ", name: " code " }));
  assert.ok(res.ok);
  // Deep-equal, not a field spot-check: the whole point of this kind is that it
  // carries NO spawn inputs. An extra command/argv/shellKind sneaking into the plan
  // would mean something is about to start a process in a pane that must never have one.
  assert.deepEqual(res.plan, { kind: "files", root: "C:/Projects/loomux", name: "code" });
});

test("a file explorer's name defaults to the folder's own short name", () => {
  const res = planPaneSetup(input({ kind: "files", repo: "C:\\Projects\\loomux\\", name: "" }));
  assert.ok(res.ok && res.plan.kind === "files");
  assert.equal(res.plan.name, "loomux");
});

// ---------- editor + git (#217) ----------

test("an editor pane needs a folder, and a git pane needs a repository", () => {
  // Same rule as the files kind, same reason: "home" is not a project to edit, and it is
  // certainly not a repository. A content pane with no root has no content — so this
  // bounces the user back to the field instead of opening an empty pane.
  const editor = planPaneSetup(input({ kind: "editor", repo: "  " }));
  assert.ok(!editor.ok && editor.focus === "repo");
  assert.match(editor.ok ? "" : editor.error, /folder/i);

  const git = planPaneSetup(input({ kind: "git", repo: "" }));
  assert.ok(!git.ok && git.focus === "repo");
  assert.match(git.ok ? "" : git.error, /repositor/i);
});

test("an editor pane plans a root + name, and nothing else — no command, no shell", () => {
  // Deep-equal, not a spot-check: this kind carries NO spawn inputs, and a command/argv/
  // shellKind sneaking into the plan would mean something is about to start a process in
  // a pane that must never have one.
  const res = planPaneSetup(input({ kind: "editor", repo: "  C:/Projects/loomux  ", name: " code " }));
  assert.ok(res.ok);
  assert.deepEqual(res.plan, { kind: "editor", root: "C:/Projects/loomux", name: "code" });
});

test("a git pane plans a root + name, and nothing else", () => {
  const res = planPaneSetup(input({ kind: "git", repo: " /repo/x ", name: "  " }));
  assert.ok(res.ok);
  // `root`, not `repo`: a content pane has ONE input and every consumer (the pane, the
  // capture, the restore) treats it identically — a synonym here would buy nothing and
  // cost a special case. Whether /repo/x is REALLY a git work tree is I/O: the form asks
  // git (gitRepoRoot) before it fires, and this module doesn't pretend it can know.
  assert.deepEqual(res.plan, { kind: "git", root: "/repo/x", name: "x" });
});

// ---------- workflow (#222) ----------

test("a workflow pane needs a repository, and plans a root + name and nothing else", () => {
  // Same one rule as its three siblings, for the same reason: `.loomux/workflow.yml` is a
  // file IN a repo, so a rootless workflow pane has no workflow to show. What it must NOT
  // do is demand the FILE exist — a repo without one is the normal starting point, and the
  // pane offers to create it (see the launcher's probe, which stops at the directory).
  const missing = planPaneSetup(input({ kind: "workflow", repo: "  " }));
  assert.ok(!missing.ok && missing.focus === "repo");
  assert.match(missing.ok ? "" : missing.error, /repositor/i);

  const res = planPaneSetup(input({ kind: "workflow", repo: " C:/Projects/loomux ", name: " flow " }));
  assert.ok(res.ok);
  assert.deepEqual(res.plan, { kind: "workflow", root: "C:/Projects/loomux", name: "flow" });
});

test("every content kind is a content kind — the predicate the form hides fields off", () => {
  // The form hides its CLI / count / worktree / autopilot / shell fields off this ONE
  // predicate rather than listing the kinds at each site. A kind missing from it would
  // silently render an agent's fields on a pane that can never spawn a process.
  for (const kind of ["files", "editor", "git", "workflow"] as const) {
    assert.equal(isContentKind(kind), true, `${kind} must be a content kind`);
  }
  for (const kind of ["agent", "orchestrator", "terminal"] as const) {
    assert.equal(isContentKind(kind), false);
  }
});

test("both new kinds default their name to the root's own short name", () => {
  const editor = planPaneSetup(input({ kind: "editor", repo: "C:\\Projects\\loomux\\", name: "" }));
  assert.ok(editor.ok && editor.plan.kind === "editor");
  assert.equal(editor.plan.name, "loomux");

  const git = planPaneSetup(input({ kind: "git", repo: "C:\\Projects\\loomux\\", name: "" }));
  assert.ok(git.ok && git.plan.kind === "git");
  assert.equal(git.plan.name, "loomux");
});

test("isContentKind names exactly the PTY-less kinds — the ones that spawn nothing", () => {
  // The welcome form hides every CLI/shell/worktree field off this one predicate, and the
  // pane system keys "no PTY, ever" off the same idea. A kind added to one list and not
  // the other is how a content pane ends up being asked which shell it wants.
  assert.deepEqual(
    (["agent", "orchestrator", "terminal", "files", "editor", "git"] as const).filter(isContentKind),
    ["files", "editor", "git"]
  );
});

// ---------- SubmitLatch's second consumer: the app-quit confirm (#219) ----------

test("the quit confirm reuses the latch: a second ✕ while the dialog is up is refused", () => {
  // Same async-reentrancy shape as the welcome form's submit (#194 P1) and Pane.
  // requestClose (#217): the guard awaits a modal, and meanwhile a second ✕ / Alt+F4 /
  // impatient double-click fires the close request again. Without the latch that stacks a
  // SECOND quit dialog whose answer races the first one's. The in-flight ask owns the
  // decision, so the duplicate is refused and the window simply stays.
  const latch = new SubmitLatch();
  assert.equal(latch.begin(), true, "the ✕ that opened the dialog");
  assert.equal(latch.begin(), false, "a second ✕ while it is up — no second dialog");
  assert.equal(latch.begin(), false, "…nor a third");

  // Cancel → the app stays, and a LATER ✕ must ask again (release, not finish).
  latch.release();
  assert.equal(latch.begin(), true);

  // "Quit anyway" → finish: the window is going away, so nothing further is admitted even
  // if a late close event lands while it does.
  latch.finish();
  assert.equal(latch.begin(), false);
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

// The per-pane restore policy + layout flattening (#194). Pure — panerestore.ts.
// Pins the adopted hybrid: agents auto-resume via a recorded session id, groups
// stay dormant, terminals re-spawn — and the ordered rebuild sequence for a
// nested layout.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  planPaneRestore,
  planLayoutRestore,
  agentResumeCommand,
  agentFreshCommand,
  stripSoloMcpFlags,
  appendSoloMcpArgs,
  sessionIdFromCommand,
  adoptableSessionId,
  hasForkSession,
  shouldRespawnFresh,
  findResumedPaneIndex,
  programFromRestore,
  normalizeAgentProgram,
  shouldWatchCopilotOnRestore,
  AUTO_RESUME_AGENTS,
  type RestoreAction,
  type RestoreOpenStep,
} from "../src/panerestore.ts";
import type { PersistedPane, PersistedLayoutNode } from "../src/tabstore.ts";

const pane = (over: Partial<PersistedPane>): PersistedPane => ({
  paneKind: "terminal",
  name: "p",
  cwd: null,
  command: null,
  argv: null,
  shellKind: null,
  role: null,
  sessionId: null,
  file: null,
  embeds: [],
  ...over,
});

test("a terminal re-spawns a fresh shell in its recorded cwd + shell kind", () => {
  const action = planPaneRestore(pane({ paneKind: "terminal", name: "shell", cwd: "/repo", shellKind: "gitbash" }));
  assert.deepEqual(action, { type: "spawn-terminal", name: "shell", cwd: "/repo", shellKind: "gitbash" });
});

test("an agent WITH a session id auto-resumes (never replays a prompt)", () => {
  const action = planPaneRestore(
    pane({
      paneKind: "agent",
      name: "claude",
      cwd: "/repo",
      command: "claude",
      argv: ["claude"],
      sessionId: "abc-123",
    })
  );
  assert.deepEqual(action, {
    type: "resume-agent",
    name: "claude",
    cwd: "/repo",
    command: "claude",
    argv: ["claude"],
    sessionId: "abc-123",
  });
});

test("an agent WITHOUT a session id falls back to a dormant Start placeholder", () => {
  const action = planPaneRestore(
    pane({ paneKind: "agent", name: "copilot", cwd: "/repo", command: "copilot", argv: null, sessionId: null })
  );
  assert.deepEqual(action, {
    type: "dormant-agent",
    name: "copilot",
    cwd: "/repo",
    command: "copilot",
    argv: null,
  });
});

// ---------- files (#214) ----------

test("a file explorer pane reopens at its recorded root — no process, no session", () => {
  const action = planPaneRestore(pane({ paneKind: "files", name: "loomux", cwd: "C:/Projects/loomux" }));
  assert.deepEqual(action, { type: "open-files", name: "loomux", root: "C:/Projects/loomux" });
});

test("a files pane is never asked to resume, even if a session id somehow rode along", () => {
  // The rule is keyed on KIND, like the group rule above: a files pane has no
  // process to resume into, so a stray sessionId must not send it down an agent path.
  const action = planPaneRestore(
    pane({ paneKind: "files", name: "docs", cwd: "/docs", sessionId: "abc-123", command: "claude" }),
    () => true
  );
  assert.deepEqual(action, { type: "open-files", name: "docs", root: "/docs" });
});

test("a files pane with NO recorded root surfaces a null root for the caller to fail soft on", () => {
  // Whether the folder still EXISTS is I/O this pure module can't do, so the missing
  // /deleted-root case is expressed as `root: null | <path>` and resolved by main.ts
  // (which probes it and falls back to the welcome form in that slot). Pinning null
  // here keeps that contract: we hand the caller the problem, we don't invent a root.
  const action = planPaneRestore(pane({ paneKind: "files", name: "files", cwd: null }));
  assert.deepEqual(action, { type: "open-files", name: "files", root: null });
});

// ---------- editor + git (#217) ----------

test("an editor pane reopens at its recorded root, showing the file it had open", () => {
  // The FILE rides along (a path, never a buffer): a pane opened on src/pane.ts is
  // TITLED after it, so restoring a bare tree under that title would name a file the
  // pane isn't showing.
  assert.deepEqual(
    planPaneRestore(
      pane({ paneKind: "editor", name: "pane.ts", cwd: "C:/Projects/loomux", file: "src/pane.ts" })
    ),
    { type: "open-editor", name: "pane.ts", root: "C:/Projects/loomux", file: "src/pane.ts" }
  );
  // No file open when it was captured → none reopened. Not an error, just a tree.
  assert.deepEqual(
    planPaneRestore(pane({ paneKind: "editor", name: "loomux", cwd: "C:/Projects/loomux" })),
    { type: "open-editor", name: "loomux", root: "C:/Projects/loomux", file: null }
  );
  assert.deepEqual(
    planPaneRestore(pane({ paneKind: "git", name: "loomux", cwd: "C:/Projects/loomux" })),
    { type: "open-git", name: "loomux", root: "C:/Projects/loomux" }
  );
});

test("neither new kind is ever asked to resume, even with a session id and a command", () => {
  // Keyed on KIND, exactly like the group and files rules above. A content pane has no
  // process to resume into, so a stray sessionId (a record hand-edited, or written by a
  // future version) must not send it down an agent path and spawn a CLI in a pane that
  // is supposed to be a viewer.
  const stray = { cwd: "/repo", sessionId: "abc-123", command: "claude" };
  assert.equal(planPaneRestore(pane({ paneKind: "editor", name: "e", ...stray }), () => true).type, "open-editor");
  assert.equal(planPaneRestore(pane({ paneKind: "git", name: "g", ...stray }), () => true).type, "open-git");
});

test("a rootless editor/git record surfaces null for the caller to fail soft on", () => {
  // Same contract as files: whether the folder still exists — and, for git, whether it is
  // still a work tree — is I/O this pure module can't do. We hand the caller the problem
  // (null, or a path it must probe), we don't invent a root.
  assert.deepEqual(planPaneRestore(pane({ paneKind: "editor", name: "e", cwd: null })), {
    type: "open-editor",
    name: "e",
    root: null,
    file: null,
  });
  assert.deepEqual(planPaneRestore(pane({ paneKind: "git", name: "g", cwd: null })), {
    type: "open-git",
    name: "g",
    root: null,
  });
});

// ---------- workflow (#222) ----------

test("a workflow pane reopens over its repo, on the workflow file it was editing", () => {
  // The file rides the same field the editor's does (a PATH, never a buffer), so a pane
  // opened on a NON-default workflow (from the browser's "Open in workflow pane") comes
  // back on that one rather than silently on `.loomux/workflow.yml`.
  assert.deepEqual(
    planPaneRestore(
      pane({ paneKind: "workflow", name: "workflow.yml", cwd: "C:/Projects/loomux", file: ".loomux/workflow.yml" })
    ),
    {
      type: "open-workflow",
      name: "workflow.yml",
      root: "C:/Projects/loomux",
      file: ".loomux/workflow.yml",
    }
  );
  // No file recorded → null, and the caller opens the repo's default. Not an error.
  assert.deepEqual(planPaneRestore(pane({ paneKind: "workflow", name: "w", cwd: "/repo" })), {
    type: "open-workflow",
    name: "w",
    root: "/repo",
    file: null,
  });
  // A stray session id must not send a viewer down an agent path (same rule as its siblings).
  assert.equal(
    planPaneRestore(
      pane({ paneKind: "workflow", name: "w", cwd: "/repo", sessionId: "abc", command: "claude" }),
      () => true
    ).type,
    "open-workflow"
  );
});

test("an orchestration pane ALWAYS restores dormant — never auto-resumed", () => {
  // The one credit/process-storm-sensitive case (#83/#78): a group is only ever
  // revived by the human via resumeOrchSession, so restore must not spawn it.
  const action = planPaneRestore(pane({ paneKind: "orch", name: "orchestrator", cwd: "/repo", role: "orchestrator" }));
  assert.deepEqual(action, {
    type: "dormant-group",
    name: "orchestrator",
    sessionId: null,
    role: "orchestrator",
    embeds: [],
  });
});

test("even with a session id, a group stays dormant (the rule is keyed on kind, not id)", () => {
  // A worker pane could carry a resumable session id; auto-resuming it would be
  // exactly the process storm we refuse. Kind wins over the presence of an id.
  const action = planPaneRestore(pane({ paneKind: "orch", name: "worker-1", cwd: "/wt", sessionId: "xyz-1", role: "worker" }));
  assert.deepEqual(action, {
    type: "dormant-group",
    name: "worker-1",
    sessionId: "xyz-1",
    role: "worker",
    embeds: [],
  });
});

test("captured embed preferences (#361), one per docked side, ride the dormant placeholder", () => {
  // Carried the same way role/sessionId are, so main.ts's resumeDormantGroup can
  // match them back to the resumed pane and reapply them (Pane.restoreEmbeds).
  const embeds = [
    { view: "group" as const, side: "bottom" as const, share: 0.4 },
    { view: "tasks" as const, side: "left" as const, share: 0.3 },
  ];
  const action = planPaneRestore(
    pane({ paneKind: "orch", name: "orchestrator", sessionId: "s1", role: "orchestrator", embeds })
  );
  assert.deepEqual(action, {
    type: "dormant-group",
    name: "orchestrator",
    sessionId: "s1",
    role: "orchestrator",
    embeds,
  });
});

test("git and editor dock preferences ride the dormant placeholder too (#361 scope increase)", () => {
  const embeds = [
    { view: "git" as const, side: "left" as const, share: 0.3 },
    { view: "editor" as const, side: "right" as const, share: 0.35 },
  ];
  const action = planPaneRestore(
    pane({ paneKind: "orch", name: "orchestrator", sessionId: "s1", role: "orchestrator", embeds })
  );
  assert.deepEqual(action, {
    type: "dormant-group",
    name: "orchestrator",
    sessionId: "s1",
    role: "orchestrator",
    embeds,
  });
});

// ---------- #361 whole-group resume: locating the resumed pane ----------

test("findResumedPaneIndex matches a live pane by session id", () => {
  const candidates = [
    { isDormant: false, sessionId: "other" },
    { isDormant: false, sessionId: "s1" },
  ];
  assert.equal(findResumedPaneIndex(candidates, "s1"), 1);
});

test("a dormant placeholder carrying the SAME session id never shadows the live match", () => {
  // The exact bug this predicate exists to prevent: the tab's own dormant
  // ORCH placeholder for this member is still in the tree when resume looks
  // for it, and it carries the identical captured session id — that's the
  // whole point of a captured record. Listed FIRST here on purpose: a naive
  // `.find(p => p.sessionId === sessionId)` returns index 0 (the stale
  // placeholder); only excluding dormant candidates reaches the live pane.
  const candidates = [
    { isDormant: true, sessionId: "s1" }, // the stale dormant placeholder
    { isDormant: false, sessionId: "s1" }, // the freshly resumed live pane
  ];
  assert.equal(findResumedPaneIndex(candidates, "s1"), 1);
});

test("no live match at all (only a dormant one, or none) yields -1, not a false positive", () => {
  assert.equal(findResumedPaneIndex([{ isDormant: true, sessionId: "s1" }], "s1"), -1);
  assert.equal(findResumedPaneIndex([{ isDormant: false, sessionId: "other" }], "s1"), -1);
  assert.equal(findResumedPaneIndex([], "s1"), -1);
});

test("AUTO_RESUME_AGENTS is the adopted default (the one-line all-dormant flip)", () => {
  // Guards the promise that flipping this single constant makes agents dormant.
  assert.equal(AUTO_RESUME_AGENTS, true);
});

// ---------- session id extraction (orch capture, #194.5) ----------

test("sessionIdFromCommand pulls the id from --session-id or --resume (space + = forms)", () => {
  assert.equal(sessionIdFromCommand("claude --session-id abc --model opus", null), "abc");
  assert.equal(sessionIdFromCommand("claude --resume def", null), "def");
  assert.equal(sessionIdFromCommand("claude --session-id=ghi", null), "ghi");
  assert.equal(sessionIdFromCommand("claude --resume=jkl", null), "jkl");
});

test("sessionIdFromCommand falls back to argv, and is null with no session flag", () => {
  assert.equal(sessionIdFromCommand(null, ["claude", "--session-id", "xyz"]), "xyz");
  assert.equal(sessionIdFromCommand("claude --model opus", null), null);
  assert.equal(sessionIdFromCommand("copilot", null), null); // copilot mints its own id later
  assert.equal(sessionIdFromCommand(null, null), null);
});

// ---------- #440 D1/D1c: adopting an id a custom command line already names ----------

test("adoptableSessionId pulls the id from --session-id or --resume, like sessionIdFromCommand", () => {
  assert.equal(adoptableSessionId("claude --session-id abc --model opus", null), "abc");
  assert.equal(adoptableSessionId("claude --resume def", null), "def");
  assert.equal(adoptableSessionId(null, ["claude", "--resume", "xyz"]), "xyz");
  assert.equal(adoptableSessionId("claude --model opus", null), null);
});

test("adoptableSessionId refuses to adopt when --fork-session is present (a NEW id will be minted)", () => {
  // Per the CLI reference, --fork-session makes --resume create a fresh session
  // id rather than reusing the named one — adopting the named id here would be
  // wrong. sessionIdFromCommand (used unguarded for orch capture) does NOT know
  // this and would still return "def" — that's the behavior this guard adds.
  assert.equal(adoptableSessionId("claude --resume def --fork-session", null), null);
  assert.equal(sessionIdFromCommand("claude --resume def --fork-session", null), "def");
  assert.equal(adoptableSessionId(null, ["claude", "--resume", "def", "--fork-session"]), null);
});

test("hasForkSession scans BOTH command and argv, not just whichever is non-empty (review NB1)", () => {
  // The bug this pins: a caller with a non-empty `command` string that has no
  // flag of its own, but a SEPARATE `argv` that carries --fork-session, used
  // to skip the argv check entirely (mirroring sessionIdFromCommand's own
  // command-then-argv-fallback precedence for the id, but only in the id
  // extraction — the OLD fork check stopped at "command is non-empty",
  // never falling through to check argv too). Both directions must catch it.
  assert.equal(hasForkSession("claude", ["claude", "--resume", "x", "--fork-session"]), true);
  assert.equal(hasForkSession("claude --fork-session", []), true);
  assert.equal(hasForkSession("claude --resume x", ["claude", "--resume", "x"]), false);
  assert.equal(hasForkSession(null, null), false);
});

test("adoptableSessionId refuses via the argv-only fork flag too, even with a flag-free command string", () => {
  // Concrete regression for NB1: command alone names no flags and yields no id
  // of its own, so extraction falls through to argv and would have returned
  // "x" despite the fork flag sitting right there in argv.
  assert.equal(adoptableSessionId("claude", ["claude", "--resume", "x", "--fork-session"]), null);
});

// ---------- BUG-1: resume vs fresh when the conversation is gone ----------

test("an agent whose session HAS a resumable conversation still resumes", () => {
  const action = planPaneRestore(
    pane({ paneKind: "agent", name: "claude", cwd: "/repo", command: "claude --session-id s1", sessionId: "s1" }),
    (id) => id === "s1" // predicate says the transcript exists
  );
  assert.equal(action.type, "resume-agent");
});

test("an agent whose session has NO conversation restores FRESH, keeping its identity", () => {
  // The BUG-1 crash: `claude --resume <id>` exits 1 ("No conversation found") when
  // the session was never prompted. With a predicate that says the id is gone, we
  // plan a fresh start in place — same name/cwd/CLI/id — instead of the doomed resume.
  const action = planPaneRestore(
    pane({ paneKind: "agent", name: "claude", cwd: "/repo", command: "claude --session-id s2", sessionId: "s2" }),
    () => false // no transcript for any id
  );
  assert.deepEqual(action, {
    type: "fresh-agent",
    name: "claude",
    cwd: "/repo",
    command: "claude --session-id s2",
    argv: null,
    sessionId: "s2",
  });
});

test("with NO predicate, an agent with a session id resumes (unchanged behavior)", () => {
  const action = planPaneRestore(
    pane({ paneKind: "agent", name: "claude", command: "claude", sessionId: "s3" })
  );
  assert.equal(action.type, "resume-agent");
});

test("planLayoutRestore threads the resumable predicate to every leaf", () => {
  const tree: PersistedLayoutNode = {
    kind: "split",
    dir: "row",
    weight: 1,
    children: [
      leaf(1, { paneKind: "agent", name: "live", command: "claude", sessionId: "here" }),
      leaf(1, { paneKind: "agent", name: "gone", command: "claude", sessionId: "missing" }),
    ],
  };
  const steps = planLayoutRestore(tree, (id) => id === "here");
  const types = steps.map((s) => s.action.type).sort();
  assert.deepEqual(types, ["fresh-agent", "resume-agent"], "one resumes, the missing one goes fresh");
});

test("agentFreshCommand pins the recorded id via --session-id (not --resume), stripping stale flags", () => {
  // From a resume line — becomes a fresh-start line with the same id, so the fresh
  // session is itself resumable next boot, and it never carries a prompt.
  assert.deepEqual(agentFreshCommand("claude --resume old --model opus", null, "s1"), {
    command: "claude --model opus --session-id s1",
  });
  // From the original launch line — the stale --session-id is replaced, not doubled.
  assert.deepEqual(agentFreshCommand("claude --session-id old", null, "s2"), {
    command: "claude --session-id s2",
  });
  // argv + bare fallbacks.
  assert.deepEqual(agentFreshCommand(null, ["claude", "--resume", "old"], "s3"), {
    argv: ["claude", "--session-id", "s3"],
  });
  assert.deepEqual(agentFreshCommand(null, null, "s4"), { command: "claude --session-id s4" });
});

test("shouldRespawnFresh: fresh-respawn only on an unexpected non-zero exit", () => {
  assert.equal(shouldRespawnFresh({ exit_code: 1, expected: false }), true, "resume-not-found (exit 1)");
  assert.equal(shouldRespawnFresh({ exit_code: 2, expected: false }), true, "any resume-time failure");
  assert.equal(shouldRespawnFresh({ exit_code: 0, expected: false }), false, "clean exit — the human quit");
  assert.equal(shouldRespawnFresh({ exit_code: 1, expected: true }), false, "loomux killed it (pane close)");
  assert.equal(shouldRespawnFresh({ exit_code: null, expected: false }), false, "no code — signal/kill");
});

// ---------- resume command building ----------

test("resume appends --resume to a plain claude command, keeping other flags", () => {
  assert.deepEqual(agentResumeCommand("claude --dangerously-skip-permissions", null, "s1"), {
    command: "claude --dangerously-skip-permissions --resume s1",
  });
});

test("resume replaces a recorded --session-id (space form) rather than doubling it", () => {
  assert.deepEqual(agentResumeCommand("claude --session-id old-id --model opus", null, "s2"), {
    command: "claude --model opus --resume s2",
  });
});

test("resume replaces a recorded --resume/--session-id in the `=` form too", () => {
  assert.deepEqual(agentResumeCommand("claude --session-id=old --resume=stale", null, "s3"), {
    command: "claude --resume s3",
  });
});

test("resume never carries a prompt — only the launch flags plus --resume", () => {
  // Guards the no-replay rule: whatever was recorded, the output is just the
  // program + surviving flags + the resume id, never a queued prompt.
  const out = agentResumeCommand("claude", null, "abc");
  assert.equal(out.command, "claude --resume abc");
});

test("resume falls back to argv when there is no string command", () => {
  assert.deepEqual(agentResumeCommand(null, ["claude", "--session-id", "old"], "s4"), {
    argv: ["claude", "--resume", "s4"],
  });
});

test("resume with neither command nor argv best-efforts a bare claude --resume", () => {
  assert.deepEqual(agentResumeCommand(null, null, "s5"), { command: "claude --resume s5" });
});

// ---------- #449: property — session-flag excision must never reflow untouched bytes ----------
//
// The bug: the old implementation ran `command.trim().split(/\s+/)` and
// rejoined with a single space, which collapses any whitespace RUN inside a
// quoted arg on EVERY restore, silently and permanently (the corrupted form
// gets persisted into the next layout snapshot). Windows paths with spaces
// are the recurring killer in this codebase — this was #442's own blocking
// bug, in an adjacent function of this same file — so a spaced path is the
// MAIN fixture below, not a one-off edge case appended at the end.
//
// These pin the PROPERTY the fix owes, not a scenario: (1) a command with no
// session flag comes back byte-identical apart from the appended flag, and
// (2) a command that DOES carry one loses only that flag's own bytes — every
// other byte, whitespace runs included, survives character-for-character.
// A test that only ever tries single-spaced input would pass against the OLD
// split/rejoin code too (the exact trap #439's own review called out), so
// every fixture here carries a real internal whitespace run, most of them
// inside a quoted Windows path.

const UNTOUCHED_COMMAND_FIXTURES = [
  // Spaced Windows path in a flag value — the common case, not the edge case.
  'claude --append-system-prompt "C:\\Users\\Will H\\prompts\\system.txt" --model opus',
  'claude --mcp-config "C:\\Users\\Old User\\configs\\solo-6.json" --dangerously-skip-permissions',
  // Multiple internal whitespace runs inside one quoted value.
  'claude --append-system-prompt "two  spaces  and  three   here"',
  // No flags at all.
  "claude",
];

test("agentResumeCommand: a command with NO session flag comes back byte-identical apart from the appended --resume — spaced Windows paths are the pinned case, not an edge case", () => {
  for (const command of UNTOUCHED_COMMAND_FIXTURES) {
    const out = agentResumeCommand(command, null, "new-id");
    assert.equal(out.command, `${command} --resume new-id`, `must not reflow: ${command}`);
  }
});

test("agentFreshCommand: a command with NO session flag comes back byte-identical apart from the appended --session-id — spaced Windows paths are the pinned case, not an edge case", () => {
  for (const command of UNTOUCHED_COMMAND_FIXTURES) {
    const out = agentFreshCommand(command, null, "new-id");
    assert.equal(out.command, `${command} --session-id new-id`, `must not reflow: ${command}`);
  }
});

// Fixtures: [prefix, flag, suffix] — `flag` is the exact recorded session-flag
// substring (space or `=` form) sandwiched between two chunks that each carry
// their OWN meaningful whitespace (a spaced quoted path, a multi-space run).
// The property: excising `flag` and appending the new one must leave `prefix`
// and `suffix` EXACTLY as recorded — never reflowed, never touched.
//
// #471 review round 4 (rev-10): every irregular-whitespace fixture above this
// point kept its multi-space runs INSIDE a quoted argument — which a
// join-of-surviving-TOKENS reconstruction (the exact regression #449 exists
// to prevent, since a quoted run is kept as one token either way) would
// still have passed, undetected. The last fixture below closes that: its
// multi-space runs sit BETWEEN bare, UNQUOTED survivor tokens, nowhere near
// a quote — so this pins inter-token spacing itself, not one string that
// happens to survive for an unrelated reason.
const SESSION_FLAG_FIXTURES: Array<{ prefix: string; flag: string; suffix: string }> = [
  {
    prefix: 'claude --append-system-prompt "two  spaces  here"',
    flag: " --session-id old-id",
    suffix: ' --mcp-config "C:\\Users\\Will H\\solo-6.json" --model opus',
  },
  {
    prefix: 'claude --mcp-config "C:\\Users\\Old  User\\dead\\solo-6.json"',
    flag: " --resume=stale-id",
    suffix: ' --append-system-prompt "trailing   run"',
  },
  { prefix: "claude", flag: " --session-id=old", suffix: "" },
  {
    // No quotes anywhere in prefix/suffix — irregular multi-space runs sit
    // directly between bare tokens (claude/--model, --model/opus, and
    // --dangerously-skip-permissions/--verbose in the suffix).
    prefix: "claude   --model    opus",
    flag: " --session-id old-id",
    suffix: " --dangerously-skip-permissions    --verbose",
  },
];

test("agentResumeCommand: excises ONLY the recorded session flag's own bytes — every other byte, whitespace runs inside quoted args included, survives untouched", () => {
  for (const { prefix, flag, suffix } of SESSION_FLAG_FIXTURES) {
    const recorded = prefix + flag + suffix;
    const out = agentResumeCommand(recorded, null, "new-id");
    assert.equal(out.command, `${prefix}${suffix} --resume new-id`, `flag=${JSON.stringify(flag)}`);
  }
});

test("agentFreshCommand: excises ONLY the recorded session flag's own bytes — every other byte, whitespace runs inside quoted args included, survives untouched", () => {
  for (const { prefix, flag, suffix } of SESSION_FLAG_FIXTURES) {
    const recorded = prefix + flag + suffix;
    const out = agentFreshCommand(recorded, null, "new-id");
    assert.equal(out.command, `${prefix}${suffix} --session-id new-id`, `flag=${JSON.stringify(flag)}`);
  }
});

// argv path: no reflow risk (each element is already discrete), but pinned
// explicitly per #449's own test-strategy note to cover "both the command and
// argv paths" — an internal-whitespace argv element must survive as one
// element, untouched, exactly like the command-string fixtures above.
test("agentResumeCommand: argv form leaves a non-flag element's internal whitespace untouched (each element is already discrete)", () => {
  const argv = ["claude", "--append-system-prompt", "two  spaces  here", "--session-id", "old", "--model", "opus"];
  assert.deepEqual(agentResumeCommand(null, argv, "new-id"), {
    argv: ["claude", "--append-system-prompt", "two  spaces  here", "--model", "opus", "--resume", "new-id"],
  });
});

test("agentFreshCommand: argv form leaves a non-flag element's internal whitespace untouched (each element is already discrete)", () => {
  const argv = ["claude", "--append-system-prompt", "two  spaces  here", "--resume", "old"];
  assert.deepEqual(agentFreshCommand(null, argv, "new-id"), {
    argv: ["claude", "--append-system-prompt", "two  spaces  here", "--session-id", "new-id"],
  });
});

// ---------- #471 review round 2: the excision property, hardened ----------
//
// rev-10 found the round-1 fix missed a BARE valueless flag (`claude
// --resume` with nothing after it): the excision regex required a value, so
// a bare flag survived untouched and got a SECOND `--resume` appended on top
// — and the stale id from that doubled command lands as a bare POSITIONAL
// argument on the *next* restore, which `claude` treats as a prompt. Not
// cosmetic: it compounds every restore cycle and silently spends the user's
// own credits. The orchestrator additionally required: the `=` form pinned
// per flag name explicitly (a sibling PR, #473/#458, is about to start
// recording `--resume=<id>` for copilot panes — this module doesn't care
// which CLI recorded the command, only the flag's literal text, so it must
// already handle that shape), and the flag-NAME set itself pinned (a
// mutation removing `--session-id` from recognition passed all 67 round-1
// tests untouched).
//
// These pin the PROPERTY the fix now owes: for EACH flag name
// (`--session-id`, `--resume`) and EACH form a recorded command can carry it
// in, excising it and appending the new flag changes ONLY that occurrence's
// own bytes — nothing else ever moves, and a flag-looking WORD inside a
// quoted argument is never mistaken for a real flag.

const SESSION_FLAG_NAMES_UNDER_TEST = ["--session-id", "--resume"];

// Forms whose ENTIRE occurrence (flag + any value) is meant to disappear.
// Deliberately excludes "bare, followed by another flag" — that form must
// drop ONLY the flag name and leave what follows untouched, so it gets its
// own explicitly-computed test below rather than forcing an ill-fitting
// shared formula.
const WHOLE_OCCURRENCE_FORMS: Array<{ label: string; build: (flag: string) => string; atEnd?: boolean }> = [
  { label: "space form", build: (f) => `${f} old-id` },
  { label: "space form, quoted value", build: (f) => `${f} "old id"` },
  { label: "= form", build: (f) => `${f}=old-id` },
  { label: "= form, EMPTY value — the #473/#458 shape", build: (f) => `${f}=` },
  { label: "bare, end of command", build: (f) => f, atEnd: true },
];

for (const flag of SESSION_FLAG_NAMES_UNDER_TEST) {
  for (const form of WHOLE_OCCURRENCE_FORMS) {
    const occurrence = form.build(flag);
    const prefix = 'claude --append-system-prompt "two  spaces  here"';
    const suffix = form.atEnd ? "" : ' --mcp-config "C:\\Users\\Will H\\solo-6.json" --model opus';
    const recorded = `${prefix} ${occurrence}${suffix}`;

    test(`agentResumeCommand: ${flag} written as "${form.label}" is excised whole — nothing else moves`, () => {
      const out = agentResumeCommand(recorded, null, "new-id");
      assert.equal(out.command, `${prefix}${suffix} --resume new-id`, recorded);
    });

    test(`agentFreshCommand: ${flag} written as "${form.label}" is excised whole — nothing else moves`, () => {
      const out = agentFreshCommand(recorded, null, "new-id");
      assert.equal(out.command, `${prefix}${suffix} --session-id new-id`, recorded);
    });
  }

  test(`agentResumeCommand: ${flag} written bare and immediately followed by ANOTHER flag is dropped alone — the following flag is never swallowed as its "value" (the round-1 regression)`, () => {
    const recorded = `claude ${flag} --model opus`;
    const out = agentResumeCommand(recorded, null, "new-id");
    assert.equal(out.command, "claude --model opus --resume new-id", recorded);
  });

  test(`agentFreshCommand: ${flag} written bare and immediately followed by ANOTHER flag is dropped alone`, () => {
    const recorded = `claude ${flag} --model opus`;
    const out = agentFreshCommand(recorded, null, "new-id");
    assert.equal(out.command, "claude --model opus --session-id new-id", recorded);
  });

  test(`agentResumeCommand: repeated ${flag} occurrences (space form, then = form) are ALL excised, not just the first`, () => {
    const recorded = `claude ${flag} old1 ${flag}=old2 --model opus`;
    const out = agentResumeCommand(recorded, null, "new-id");
    assert.equal(out.command, "claude --model opus --resume new-id", recorded);
  });

  test(`agentFreshCommand: repeated ${flag} occurrences (space form, then = form) are ALL excised, not just the first`, () => {
    const recorded = `claude ${flag} old1 ${flag}=old2 --model opus`;
    const out = agentFreshCommand(recorded, null, "new-id");
    assert.equal(out.command, "claude --model opus --session-id new-id", recorded);
  });

  test(`agentResumeCommand: argv form — a bare ${flag} is dropped without swallowing a following flag element`, () => {
    assert.deepEqual(agentResumeCommand(null, ["claude", flag, "--model", "opus"], "new-id"), {
      argv: ["claude", "--model", "opus", "--resume", "new-id"],
    });
  });

  test(`agentFreshCommand: argv form — a bare ${flag} is dropped without swallowing a following flag element`, () => {
    assert.deepEqual(agentFreshCommand(null, ["claude", flag, "--model", "opus"], "new-id"), {
      argv: ["claude", "--model", "opus", "--session-id", "new-id"],
    });
  });

  test(`agentResumeCommand: argv form — a bare ${flag} as the LAST element is dropped cleanly (no crash, no doubling)`, () => {
    assert.deepEqual(agentResumeCommand(null, ["claude", flag], "new-id"), {
      argv: ["claude", "--resume", "new-id"],
    });
  });
}

test("agentResumeCommand: a flag-looking WORD inside a quoted argument is never treated as a flag — only the real, unquoted --session-id is excised", () => {
  const recorded =
    'claude --append-system-prompt "please explain what --resume and --session-id do" --session-id real-old-id';
  const out = agentResumeCommand(recorded, null, "new-id");
  assert.equal(
    out.command,
    'claude --append-system-prompt "please explain what --resume and --session-id do" --resume new-id'
  );
});

test("agentFreshCommand: a flag-looking WORD inside a quoted argument is never treated as a flag — only the real, unquoted --resume is excised", () => {
  const recorded =
    'claude --append-system-prompt "please explain what --resume and --session-id do" --resume real-old-id';
  const out = agentFreshCommand(recorded, null, "new-id");
  assert.equal(
    out.command,
    'claude --append-system-prompt "please explain what --resume and --session-id do" --session-id new-id'
  );
});

// ---------- #471 review round 3: CLI-aware emission (rev-11, via #473/#458) ----------
//
// rev-11 traced a cross-PR oscillation: a copilot session resumed from the
// Sessions sidebar records `--resume=<id>` (#440 D1c). #473 (#458) fixes
// `scan_copilot()` to keep recording it in the `=` form, because copilot's
// CLI reference says the space form does not reliably bind. But
// agentResumeCommand was CLI-agnostic — it always appended Claude's
// `--resume <id>` (space form) — so restoring that same pane the very next
// time would silently rewrite the `=` form BACK to the space form, and the
// two PRs' fixes would fight forever instead of converging.
//
// These pin the two additional properties rev-11 asked stated explicitly:
// copilot's `--resume` is emitted in `=` form; copilot's `--session-id`
// (agentFreshCommand) stays SPACE form, deliberately — NOT "made consistent"
// with `--resume`, because the copilot CLI reference documents the two flags
// with different forms. Claude (and any unrecognized/absent CLI) is
// completely unaffected — every pre-existing test above uses a `claude …`
// command and still expects the space form throughout, which is itself part
// of the pin: it proves this change is copilot-scoped, not global.

test("agentResumeCommand: copilot's --resume is emitted in the = form (space form does not reliably bind, per the CLI reference)", () => {
  const out = agentResumeCommand("copilot --model gpt-4", null, "new-id");
  assert.equal(out.command, "copilot --model gpt-4 --resume=new-id");
});

test("agentResumeCommand: round-trip stability — a copilot command already recording --resume=<old> (the #473/#458 shape) comes back with --resume=<new>, never oscillating back to the space form", () => {
  const out = agentResumeCommand("copilot --resume=old-id --model gpt-4", null, "new-id");
  assert.equal(out.command, "copilot --model gpt-4 --resume=new-id");
});

test("agentResumeCommand: copilot's --resume in = form is excised just like every other form, including EMPTY value", () => {
  assert.equal(agentResumeCommand("copilot --resume=", null, "new-id").command, "copilot --resume=new-id");
});

test("agentResumeCommand: copilot detection is case-insensitive and reads the FIRST token only (programFromRestore's own contract)", () => {
  const out = agentResumeCommand("Copilot --model gpt-4", null, "new-id");
  assert.equal(out.command, "Copilot --model gpt-4 --resume=new-id");
});

test("agentResumeCommand: claude is completely unaffected by the copilot special-case — still space form", () => {
  const out = agentResumeCommand("claude --model opus", null, "new-id");
  assert.equal(out.command, "claude --model opus --resume new-id");
});

test("agentResumeCommand: an UNRECOGNIZED/absent CLI (no command match) also defaults to space form, same as claude", () => {
  assert.equal(agentResumeCommand(null, null, "new-id").command, "claude --resume new-id");
});

test("agentResumeCommand: copilot argv form also emits the = form, as ONE element (not two)", () => {
  assert.deepEqual(agentResumeCommand(null, ["copilot", "--model", "gpt-4"], "new-id"), {
    argv: ["copilot", "--model", "gpt-4", "--resume=new-id"],
  });
});

test("agentResumeCommand: claude argv form is unaffected — still two elements, space form", () => {
  assert.deepEqual(agentResumeCommand(null, ["claude", "--model", "opus"], "new-id"), {
    argv: ["claude", "--model", "opus", "--resume", "new-id"],
  });
});

test("agentFreshCommand: copilot's --session-id STAYS space form — deliberately asymmetric with agentResumeCommand's copilot --resume, per the copilot CLI reference", () => {
  const out = agentFreshCommand("copilot --resume=old-id --model gpt-4", null, "new-id");
  assert.equal(out.command, "copilot --model gpt-4 --session-id new-id");
});

test("agentFreshCommand: copilot argv form also stays space form / two elements for --session-id", () => {
  assert.deepEqual(agentFreshCommand(null, ["copilot", "--resume", "old-id"], "new-id"), {
    argv: ["copilot", "--session-id", "new-id"],
  });
});

// ---------- solo channel-identity MCP flags (#439) ----------
// A restored solo agent pane's recorded command can carry the flags
// `soloPrepare` appended at launch — they point at a `configs/solo-N.json`
// deleted at the pane's last exit. Replaying them hard-errors claude and
// authenticates nothing on copilot; stripSoloMcpFlags removes them so the
// caller (main.ts) can re-mint a fresh identity instead.

test("stripSoloMcpFlags removes claude's solo MCP flags and reports the CLI", () => {
  assert.deepEqual(
    stripSoloMcpFlags(
      'claude --model opus --mcp-config "C:/Users/w/AppData/Roaming/loomux/orchestration/__solo__/configs/solo-6.json" --strict-mcp-config --allowedTools mcp__loomux --resume abc',
      null
    ),
    { cli: "claude", command: "claude --model opus --resume abc" }
  );
});

test("stripSoloMcpFlags removes copilot's solo MCP flags and reports the CLI", () => {
  assert.deepEqual(
    stripSoloMcpFlags(
      'copilot --additional-mcp-config "@C:/Users/w/AppData/Roaming/loomux/orchestration/__solo__/configs/solo-11.json" --allow-tool loomux',
      null
    ),
    { cli: "copilot", command: "copilot" }
  );
});

// #439 review B1: the minted path is a real Windows profile path, and the
// backend quotes it BECAUSE a username can contain a space ("Will H") — a
// naive `.split(/\s+/)` tokenizer fractures the quoted path across two
// tokens, so the fixed-offset flag match silently never fires and the dead
// path survives restore intact (the exact #439 bug, reintroduced). These pin
// the real shape of the field data, for both CLI forms.
test("stripSoloMcpFlags removes claude's solo MCP flags when the config path contains a space", () => {
  assert.deepEqual(
    stripSoloMcpFlags(
      'claude --model opus --mcp-config "C:/Users/Will H/AppData/Roaming/loomux/orchestration/__solo__/configs/solo-6.json" --strict-mcp-config --allowedTools mcp__loomux --resume abc',
      null
    ),
    { cli: "claude", command: "claude --model opus --resume abc" }
  );
});

test("stripSoloMcpFlags removes copilot's solo MCP flags when the config path contains a space", () => {
  assert.deepEqual(
    stripSoloMcpFlags(
      'copilot --additional-mcp-config "@C:/Users/Will H/AppData/Roaming/loomux/orchestration/__solo__/configs/solo-11.json" --allow-tool loomux',
      null
    ),
    { cli: "copilot", command: "copilot" }
  );
});

// #439 review N1 (escalated to blocking): a `cli: null` command must come back
// byte-identical — not merely "the same flags in the same order" but the exact
// same string, including whitespace RUNS inside quoted arguments that have
// nothing to do with a solo identity. A tokenize/`.join(" ")` round trip would
// silently collapse those on every restore of every pane that never had a
// solo identity at all — a regression in the general restore path. The
// reviewer's own single-spaced probe couldn't catch this; this one uses a
// double space inside an unrelated quoted flag specifically to prove it.
test("stripSoloMcpFlags leaves a command with NO solo identity completely untouched — including whitespace runs inside unrelated quoted args", () => {
  const command = 'claude --append-system-prompt "two  spaces  inside" --resume abc';
  assert.deepEqual(stripSoloMcpFlags(command, null), { cli: null, command });
});

test("stripSoloMcpFlags is a no-op (cli: null) on a command with no solo identity", () => {
  // A custom command, or a channel-tools-off launch: nothing to re-mint, and the
  // command must come back byte-identical (no accidental reflow of unrelated flags).
  assert.deepEqual(stripSoloMcpFlags("claude --model opus --resume abc", null), {
    cli: null,
    command: "claude --model opus --resume abc",
  });
});

test("stripSoloMcpFlags handles the argv form the same way", () => {
  assert.deepEqual(
    stripSoloMcpFlags(null, [
      "claude",
      "--mcp-config",
      "C:/configs/solo-6.json",
      "--strict-mcp-config",
      "--allowedTools",
      "mcp__loomux",
      "--resume",
      "abc",
    ]),
    { cli: "claude", argv: ["claude", "--resume", "abc"] }
  );
  assert.deepEqual(stripSoloMcpFlags(null, ["copilot", "--resume", "abc"]), {
    cli: null,
    argv: ["copilot", "--resume", "abc"],
  });
});

test("stripSoloMcpFlags on neither command nor argv reports no CLI and nothing to append to", () => {
  assert.deepEqual(stripSoloMcpFlags(null, null), { cli: null });
  assert.deepEqual(stripSoloMcpFlags("", []), { cli: null });
});

test("appendSoloMcpArgs appends a freshly-minted identity's flags to a cleaned command", () => {
  assert.deepEqual(
    appendSoloMcpArgs("claude --model opus --resume abc", undefined, '--mcp-config "C:/new/solo-9.json" --strict-mcp-config --allowedTools mcp__loomux'),
    { command: 'claude --model opus --resume abc --mcp-config "C:/new/solo-9.json" --strict-mcp-config --allowedTools mcp__loomux' }
  );
});

test("appendSoloMcpArgs appends a freshly-minted SPACED path just as well — it's still a plain string append", () => {
  assert.deepEqual(
    appendSoloMcpArgs(
      "claude --model opus --resume abc",
      undefined,
      '--mcp-config "C:/Users/Will H/solo-9.json" --strict-mcp-config --allowedTools mcp__loomux'
    ),
    { command: 'claude --model opus --resume abc --mcp-config "C:/Users/Will H/solo-9.json" --strict-mcp-config --allowedTools mcp__loomux' }
  );
});

test("appendSoloMcpArgs works on the argv form, tokenizing mcpArgs", () => {
  assert.deepEqual(appendSoloMcpArgs(undefined, ["claude", "--resume", "abc"], "--additional-mcp-config @/new/solo-9.json --allow-tool loomux"), {
    argv: ["claude", "--resume", "abc", "--additional-mcp-config", "@/new/solo-9.json", "--allow-tool", "loomux"],
  });
});

// #439 review N2: the argv form must not embed literal quote characters, and
// a spaced path must land as ONE argv element — not two, and not with a
// stray `"` glued to either half. Latent today (solo panes are never
// argv-spawned), but latent-and-wrong is not an acceptable resting state.
test("appendSoloMcpArgs on the argv form strips quotes and keeps a SPACED path as one element", () => {
  assert.deepEqual(
    appendSoloMcpArgs(
      undefined,
      ["claude", "--resume", "abc"],
      '--mcp-config "C:/Users/Will H/solo-9.json" --strict-mcp-config --allowedTools mcp__loomux'
    ),
    {
      argv: [
        "claude",
        "--resume",
        "abc",
        "--mcp-config",
        "C:/Users/Will H/solo-9.json",
        "--strict-mcp-config",
        "--allowedTools",
        "mcp__loomux",
      ],
    }
  );
});

test("strip + append round-trips: a resumed claude command replaces the dead SPACED path with a fresh SPACED path, never the recorded one", () => {
  // This is the actual bug (#439): agentResumeCommand alone keeps EVERY flag
  // except the session ones, so the dead --mcp-config path survives it intact.
  // Both the dead and the fresh path carry a space (the real shape of a
  // Windows profile path), pinning B1's fix end to end, not just in isolation.
  const recorded =
    'claude --mcp-config "C:/Users/Old User/dead/configs/solo-6.json" --strict-mcp-config --allowedTools mcp__loomux --session-id old';
  const resumed = agentResumeCommand(recorded, null, "new-session");
  assert.match(resumed.command!, /Old User\/dead\/configs\/solo-6\.json/, "sanity: the dead path is still there pre-strip");
  // The fix: strip the dead flags, then append a freshly-minted config's flags.
  const stripped = stripSoloMcpFlags(resumed.command, resumed.argv);
  assert.equal(stripped.cli, "claude");
  const final = appendSoloMcpArgs(
    stripped.command,
    stripped.argv,
    '--mcp-config "C:/Users/New User/fresh/configs/solo-42.json" --strict-mcp-config --allowedTools mcp__loomux'
  );
  assert.doesNotMatch(final.command!, /Old User\/dead\/configs\/solo-6\.json/, "the dead path must never survive restore");
  assert.match(final.command!, /New User\/fresh\/configs\/solo-42\.json/, "a freshly-minted path replaces it");
  assert.match(final.command!, /--resume new-session/, "still a real resume, never a replayed prompt");
});

// ---------- layout plan: reconstructible round-trip ----------
//
// The plan must be replayable into the EXACT tree — structure and weights.
// `rebuild` below is a pure model of grid.ts's `insertBeside` + the weight rule
// panerestore documents, so a serialize → plan → replay round-trip can be
// asserted here without a DOM. If P4's real grid wiring matches this model,
// restore is faithful; the model IS the contract.

type SimNode =
  | { kind: "leaf"; weight: number; pending: number[]; action: RestoreAction; parent: SimSplit | null }
  | SimSplit;
interface SimSplit {
  kind: "split";
  dir: "row" | "column";
  weight: number;
  children: SimNode[];
  parent: SimSplit | null;
}

/** Replay an open-plan through a model of grid.insertBeside, returning the rebuilt
 *  tree. Mirrors: same-direction parent → add a sibling; else wrap the anchor in a
 *  new 2-way split that inherits the anchor's slot weight. Applies the weight
 *  chain the same way the real restore must (outer slot weight to the new split,
 *  next weight to the anchor inside it). */
function rebuild(steps: RestoreOpenStep[]): SimNode {
  const leaves: SimNode[] = [];
  let root: SimNode = {
    kind: "leaf",
    weight: steps[0].weights[0],
    pending: steps[0].weights.slice(1),
    action: steps[0].action,
    parent: null,
  };
  leaves.push(root);
  for (let i = 1; i < steps.length; i++) {
    const s = steps[i];
    const anchor = leaves[s.relativeTo!];
    const leaf: SimNode = {
      kind: "leaf",
      weight: s.weights[0],
      pending: s.weights.slice(1),
      action: s.action,
      parent: null,
    };
    leaves.push(leaf);
    const parent = anchor.parent;
    if (parent && parent.dir === s.dir) {
      // Mirror grid.insertBeside: a same-direction sibling splices in AFTER the
      // anchor (idx+1), NOT appended at the end — this is what makes a
      // wrong-anchor plan reorder middle siblings and get caught here.
      leaf.parent = parent;
      parent.children.splice(parent.children.indexOf(anchor) + 1, 0, leaf);
    } else {
      const split: SimSplit = {
        kind: "split",
        dir: s.dir,
        weight: anchor.weight, // new split takes the anchor's outer slot
        children: [anchor, leaf],
        parent,
      };
      anchor.weight = anchor.pending.shift()!; // anchor's weight one level in
      anchor.parent = split;
      leaf.parent = split;
      if (parent) parent.children[parent.children.indexOf(anchor)] = split;
      else root = split;
    }
  }
  return root;
}

/** Strip a persisted tree to the comparable shape `rebuild` produces (panes →
 *  their restore actions, no parent pointers). */
function actionTree(node: PersistedLayoutNode): unknown {
  return node.kind === "leaf"
    ? { kind: "leaf", weight: node.weight, action: planPaneRestore(node.pane) }
    : { kind: "split", dir: node.dir, weight: node.weight, children: node.children.map(actionTree) };
}

/** Strip a rebuilt SimNode to the same comparable shape. */
function simShape(node: SimNode): unknown {
  return node.kind === "leaf"
    ? { kind: "leaf", weight: node.weight, action: node.action }
    : { kind: "split", dir: node.dir, weight: node.weight, children: node.children.map(simShape) };
}

const leaf = (weight: number, over: Partial<PersistedPane>): PersistedLayoutNode => ({
  kind: "leaf",
  weight,
  pane: pane(over),
});

// A 2×2 grid: row of two column-splits. Distinct weights everywhere so a weight
// drop or a mis-nesting is caught.
const GRID_2x2: PersistedLayoutNode = {
  kind: "split",
  dir: "row",
  weight: 1,
  children: [
    { kind: "split", dir: "column", weight: 3, children: [leaf(1, { name: "A" }), leaf(2, { name: "B" })] },
    { kind: "split", dir: "column", weight: 6, children: [leaf(4, { name: "C" }), leaf(5, { name: "D" })] },
  ],
};

// Four stacked panes: a single column split of four leaves.
const STACK_4: PersistedLayoutNode = {
  kind: "split",
  dir: "column",
  weight: 1,
  children: [leaf(1, { name: "A" }), leaf(2, { name: "B" }), leaf(3, { name: "C" }), leaf(4, { name: "D" })],
};

// Asymmetric nesting with a weighted subtree (the divided divider case).
const ASYMMETRIC: PersistedLayoutNode = {
  kind: "split",
  dir: "row",
  weight: 1,
  children: [
    leaf(1, { paneKind: "terminal", name: "left", cwd: "/a", shellKind: "cmd" }),
    {
      kind: "split",
      dir: "column",
      weight: 3, // outer divider dragged to 25/75 — must survive
      children: [
        leaf(1, { paneKind: "agent", name: "top", command: "claude", sessionId: "s1" }),
        leaf(2, { paneKind: "orch", name: "bottom" }),
      ],
    },
  ],
};

for (const [label, tree] of [
  ["2×2 grid", GRID_2x2],
  ["4-pane stack", STACK_4],
  ["asymmetric weighted nesting", ASYMMETRIC],
] as const) {
  test(`round-trip is structure- AND weight-identical: ${label}`, () => {
    const rebuilt = rebuild(planLayoutRestore(tree));
    assert.deepEqual(simShape(rebuilt), actionTree(tree));
  });
}

/** In-order leaf names of a rebuilt tree — to pin sibling ORDER, not just shape. */
function leafOrder(node: SimNode): string[] {
  if (node.kind === "leaf") {
    const a = node.action;
    return [a.name];
  }
  return node.children.flatMap(leafOrder);
}

test("≥3 siblings replay in insertion order (grid splices after the anchor, so anchoring must walk forward)", () => {
  // The exact regression: col[A,B,C,D] must NOT come back as col[A,D,C,B].
  assert.deepEqual(leafOrder(rebuild(planLayoutRestore(STACK_4))), ["A", "B", "C", "D"]);
  // And a 3-wide row, for good measure.
  const ROW_3: PersistedLayoutNode = {
    kind: "split",
    dir: "row",
    weight: 1,
    children: [leaf(1, { name: "X" }), leaf(1, { name: "Y" }), leaf(1, { name: "Z" })],
  };
  assert.deepEqual(leafOrder(rebuild(planLayoutRestore(ROW_3))), ["X", "Y", "Z"]);
});

test("a 2×2 grid and 4 stacked panes produce DIFFERENT plans (the ambiguity the flat list lost)", () => {
  // The whole point: distinct nestings must not flatten to the same sequence, or
  // no P4 wiring could tell them apart.
  assert.notDeepEqual(planLayoutRestore(GRID_2x2), planLayoutRestore(STACK_4));
  // And each rebuilds only to itself.
  assert.deepEqual(simShape(rebuild(planLayoutRestore(GRID_2x2))), actionTree(GRID_2x2));
  assert.deepEqual(simShape(rebuild(planLayoutRestore(STACK_4))), actionTree(STACK_4));
  assert.notDeepEqual(simShape(rebuild(planLayoutRestore(GRID_2x2))), actionTree(STACK_4));
});

test("the second+ child of a split carries the dir; the first child never re-opens", () => {
  // left is the row's anchor (relativeTo null, the grid-filling root); the column
  // subtree's ENTRY (top) opens with the ROW dir relative to left, and only its
  // sibling (bottom) opens with the column dir — the reviewer's "dir belongs to
  // the second leaf" rule.
  const steps = planLayoutRestore(ASYMMETRIC);
  assert.deepEqual(
    steps.map((s) => ({ type: s.action.type, rel: s.relativeTo, dir: s.dir })),
    [
      { type: "spawn-terminal", rel: null, dir: "row" }, // left (root fill)
      { type: "resume-agent", rel: 0, dir: "row" }, // column subtree entry, opened in ROW beside left
      { type: "dormant-group", rel: 1, dir: "column" }, // bottom, opened in COLUMN beside top
    ]
  );
  // The weighted subtree's weight (3) is carried, not dropped.
  assert.deepEqual(steps[1].weights, [3, 1], "column-subtree slot weight 3, then top's own weight 1");
});

test("planLayoutRestore on a single leaf yields one root-fill step", () => {
  const steps = planLayoutRestore(leaf(1, { paneKind: "terminal", name: "solo" }));
  assert.equal(steps.length, 1);
  assert.equal(steps[0].relativeTo, null);
  assert.equal(steps[0].action.type, "spawn-terminal");
  assert.deepEqual(simShape(rebuild(steps)), actionTree(leaf(1, { paneKind: "terminal", name: "solo" })));
});

// #456: programFromRestore — which CLI a restored command/argv would invoke,
// used to gate the copilot autopilot-dialog watcher onto restore actions the
// same way a fresh launch gets it.
test("programFromRestore reads the first token of a string command, lowercased", () => {
  assert.equal(programFromRestore("copilot --resume abc-123 --autopilot", null), "copilot");
  assert.equal(programFromRestore("Claude --resume abc-123", null), "claude");
});

test("programFromRestore falls back to argv when there's no string command", () => {
  assert.equal(programFromRestore(null, ["copilot", "--resume", "abc-123"]), "copilot");
  assert.equal(programFromRestore("", ["copilot", "--resume", "abc-123"]), "copilot");
});

test("programFromRestore is null with neither a command nor argv", () => {
  assert.equal(programFromRestore(null, null), null);
  assert.equal(programFromRestore("", []), null);
  assert.equal(programFromRestore("   ", null), null);
});

// #457 (finding 3 from the design intake): the concrete named bug —
// programFromRestore (and its two now-converged siblings, Pane.agentCli and
// main.ts's D2 dormant-card sniff) used to compare the raw first token
// directly against "claude"/"copilot", so a path-qualified or
// `.exe`/`.cmd`-suffixed recorded command silently matched neither and every
// per-CLI restore behavior (autopilot watcher, resume-candidate card,
// Pane.agentCli) just didn't apply — no error, just silently wrong.
test("programFromRestore recognizes an .exe-suffixed command", () => {
  assert.equal(programFromRestore("copilot.exe --resume abc-123", null), "copilot");
  assert.equal(programFromRestore("Claude.EXE --resume abc-123", null), "claude", "case-insensitive suffix too");
});

test("programFromRestore recognizes a path-qualified command", () => {
  assert.equal(programFromRestore("C:\\tools\\copilot.exe --resume abc", null), "copilot");
  assert.equal(programFromRestore("/usr/local/bin/claude --resume abc", null), "claude");
  assert.equal(programFromRestore(null, ["C:\\tools\\copilot.exe", "--resume", "abc"]), "copilot");
});

test("normalizeAgentProgram: bare name, path-qualified, and .exe/.cmd/.bat suffixed all converge to the same lowercase program", () => {
  for (const raw of [
    "claude",
    "Claude",
    "claude.exe",
    "CLAUDE.EXE",
    "claude.cmd",
    "claude.bat",
    "C:\\Users\\me\\AppData\\Roaming\\npm\\claude.cmd",
    "/usr/local/bin/claude",
  ]) {
    assert.equal(normalizeAgentProgram(raw), "claude", `expected "claude" from ${JSON.stringify(raw)}`);
  }
});

test("normalizeAgentProgram: an unrelated program is left alone (lowercased, extension stripped) — never coerced to claude/copilot", () => {
  assert.equal(normalizeAgentProgram("bash"), "bash");
  assert.equal(normalizeAgentProgram("PowerShell.exe"), "powershell");
});

// #456 review NB1: shouldWatchCopilotOnRestore replaces three copy-pasted
// inline checks in main.ts. THE property it must hold: the watcher fires
// exactly when a restored pane is (a) copilot AND (b) actually carries
// --autopilot on whichever representation the caller has — string command
// or structured argv — never only one of the two representations.
test("shouldWatchCopilotOnRestore is true only for a copilot command that carries --autopilot", () => {
  assert.equal(shouldWatchCopilotOnRestore("copilot --resume abc --autopilot --allow-all-tools", null), true);
  assert.equal(shouldWatchCopilotOnRestore("copilot --resume abc", null), false, "copilot, but no --autopilot");
  assert.equal(shouldWatchCopilotOnRestore("claude --resume abc --autopilot", null), false, "not copilot");
  assert.equal(shouldWatchCopilotOnRestore(null, null), false);
});

test("shouldWatchCopilotOnRestore scans argv for --autopilot too — not just the string command (the NB1 asymmetry)", () => {
  // The pre-fix bug: programFromRestore falls back to argv, but the
  // --autopilot check read only the string command — an argv-only copilot
  // autopilot pane would silently skip the watcher. Assert the PROPERTY
  // (checked across whichever representation is present), not just one
  // example of it.
  assert.equal(
    shouldWatchCopilotOnRestore(null, ["copilot", "--resume", "abc", "--autopilot"]),
    true,
    "an argv-only representation must be scanned for --autopilot, same as the string form"
  );
  assert.equal(
    shouldWatchCopilotOnRestore(null, ["copilot", "--resume", "abc"]),
    false,
    "argv present but no --autopilot token — must not fire"
  );
  // A non-empty but flag-free command string must not mask an argv-only flag
  // (the same "scan both, not just whichever is non-empty" shape hasForkSession
  // guards against for --fork-session).
  assert.equal(
    shouldWatchCopilotOnRestore("", ["copilot", "--resume", "abc", "--autopilot"]),
    true
  );
});

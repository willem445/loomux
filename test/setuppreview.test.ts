// The pane-setup card's CLI preview (#1020 item 4). Run with `npm test`.
//
// What is worth pinning here is NOT that "claude shows the claude mark" — `agentMarkFor`
// already owns that and test/agenticons.test.ts already pins it. It is the half this module
// adds: WHEN the card is allowed to make a claim at all. Every assertion below is about a
// form state that names no agent, or names one through something other than its own
// dropdown, because those are the states where a plausible implementation quietly asserts
// something false — a `?` over a file-explorer setup, the transport's own name over an SSH
// pane, a `C` badge for the literal word "custom".
import { test } from "node:test";
import assert from "node:assert/strict";
import { setupPreviewMark, type SetupPreviewInput } from "../src/setuppreview.ts";
import { REMOTE_UNKNOWN_LABEL } from "../src/agenticons.ts";
import type { PaneKind } from "../src/panesetup.ts";

/** A card with nothing filled in beyond the kind — every test states only what it varies. */
function input(over: Partial<SetupPreviewInput> = {}): SetupPreviewInput {
  return {
    kind: "agent",
    agentId: "claude",
    customCommand: "",
    sshCli: "",
    orchestratorCli: "",
    ...over,
  };
}

test("a built-in agent shows that CLI's own mark", () => {
  const claude = setupPreviewMark(input({ agentId: "claude" }));
  assert.equal(claude?.program, "claude");
  assert.equal(claude?.kind, "letter"); // no licensed Claude glyph — see agenticons §Licensing
  const copilot = setupPreviewMark(input({ agentId: "copilot" }));
  assert.equal(copilot?.program, "copilot");
  assert.equal(copilot?.kind, "mark");
  // The two must be distinguishable at a glance, or the preview answers nothing.
  assert.notEqual(claude?.svg, copilot?.svg);
});

test("a pane that runs no agent CLI shows nothing at all", () => {
  // The rule this module exists to hold. A terminal and the four content kinds spawn no
  // CLI, so there is no program to name — and the neutral `?` badge is NOT the right answer
  // for them, because `?` means "loomux cannot tell which agent this is", which asserts
  // there is one.
  for (const kind of ["terminal", "files", "editor", "git", "workflow"] as PaneKind[]) {
    assert.equal(
      setupPreviewMark(input({ kind, agentId: "claude" })),
      null,
      `${kind} drew a mark; it runs no agent CLI`
    );
  }
});

test("custom… draws nothing until a command is typed, then reads the command", () => {
  // Picking `custom…` is the human saying "I will type one" — the badge has to wait for
  // them rather than report a failure to identify a command that does not exist yet, and it
  // must never badge the literal word "custom" (a `C`, indistinguishable from Claude's).
  assert.equal(setupPreviewMark(input({ agentId: "custom", customCommand: "" })), null);
  assert.equal(setupPreviewMark(input({ agentId: "custom", customCommand: "   " })), null);

  const aider = setupPreviewMark(input({ agentId: "custom", customCommand: "aider --model sonnet" }));
  assert.equal(aider?.program, "aider");
  assert.equal(aider?.kind, "letter");
  assert.ok(aider?.svg.includes(">A</text>"), `expected an A badge, got ${aider?.svg}`);

  // A path-qualified, .exe-suffixed command resolves the same way a restored pane's does —
  // this module must not grow a second first-token parse of its own.
  const qualified = setupPreviewMark(
    input({ agentId: "custom", customCommand: `C:\\tools\\copilot.exe --banner` })
  );
  assert.equal(qualified?.program, "copilot");
  assert.equal(qualified?.kind, "mark");
});

test("a hand-typed shell or transport is refused, not badged", () => {
  // agenticons' denylist has to survive the trip through this module: a human who types
  // `bash` into the custom box has not configured an agent, and "Agent CLI: bash" is the
  // confident-wrong-answer #992 was written to avoid.
  for (const command of ["bash", "pwsh -NoLogo", "ssh build-box"]) {
    const view = setupPreviewMark(input({ agentId: "custom", customCommand: command }));
    assert.equal(view?.kind, "unknown", `${command} was badged as an agent`);
    assert.equal(view?.program, null);
    assert.equal(/Agent CLI:/.test(view?.label ?? ""), false, `${command} was captioned as an agent`);
  }
});

test("orchestrator mode previews the ORCHESTRATOR ROLE's CLI, not the group default", () => {
  // rev-740 blocking 1. The card sits above two controls that answer different questions:
  // the top Agent select is the GROUP DEFAULT (it seeds every role, and a declared block
  // with no `cli:` inherits it), while the orchestrator ROLE row is what
  // `create_orchestration` launches the ORCH pane on (`orchestrator_cli`, issue #4). They
  // diverge the moment the role row is touched, and the pane that appears wears the role's
  // CLI — so previewing the group default is exactly the confident-wrong-answer this
  // module refuses everywhere else. Since #1020 removed the starter workers, that ORCH
  // pane is the ONLY pane a launch opens, so this badge is a claim about all of it.
  const overridden = setupPreviewMark(
    input({ kind: "orchestrator", agentId: "claude", orchestratorCli: "copilot" })
  );
  assert.equal(overridden?.program, "copilot", "the role's CLI must win over the group default");
  assert.equal(overridden?.kind, "mark");
  // ...and it must genuinely differ from what the group default would have drawn, or the
  // assertion above would pass for a build that still reads `agentId`.
  assert.notEqual(overridden?.svg, setupPreviewMark(input({ kind: "orchestrator", agentId: "claude" }))?.svg);
});

test("an orchestrator role that overrides nothing inherits the group default", () => {
  // The empty per-role value is not "unset, show nothing" — the backend reads it as "use
  // `agent_cli`" (`create_orchestration`'s per-role override comment), so the preview has
  // to resolve it the same way or it disagrees with the spawn one level down.
  const inherited = setupPreviewMark(input({ kind: "orchestrator", agentId: "opencode", orchestratorCli: "" }));
  assert.equal(inherited?.program, "opencode");
  // The seeded case — `applyOrchCli` copies the group default into every role — must agree
  // with the inherited one rather than being a second code path.
  const seeded = setupPreviewMark(
    input({ kind: "orchestrator", agentId: "opencode", orchestratorCli: "opencode" })
  );
  assert.deepEqual(seeded, inherited);
});

test("orchestrator mode refuses custom…, from whichever control supplies it", () => {
  // `custom` is not launchable as a group (no orchestration adapter behind a hand-typed
  // command), so it is a value the form is about to replace — never a `C` badge, and never
  // one indistinguishable from Claude's. Checked on BOTH controls: the resolution above
  // means either one can be the value that reaches the badge.
  assert.equal(
    setupPreviewMark(input({ kind: "orchestrator", agentId: "custom", customCommand: "aider" })),
    null
  );
  assert.equal(
    setupPreviewMark(input({ kind: "orchestrator", agentId: "claude", orchestratorCli: "custom" })),
    null
  );
});

test("an SSH pane previews its REMOTE CLI, never the local ssh client", () => {
  // The one case where the launch line describes something other than the agent: the
  // process is the local ssh.exe and the agent is on the far end. Reading the launch line
  // would caption a Claude session "Agent CLI: ssh".
  const remote = setupPreviewMark(input({ kind: "ssh", sshCli: "claude" }));
  assert.equal(remote?.program, "claude");
  assert.match(remote?.label ?? "", /claude/);

  // "None — a plain login shell" is a real choice, and it is NOT the remote-unknown tier:
  // loomux has been told there is no far-end agent rather than failing to identify one.
  const shell = setupPreviewMark(input({ kind: "ssh", sshCli: "" }));
  assert.equal(shell, null);
  assert.notEqual(shell?.label, REMOTE_UNKNOWN_LABEL);
});

test("an empty agent id draws nothing rather than the unknown badge", () => {
  // Reachable only transiently (a picker rebuilt with no options yet), and the cheap wrong
  // answer is a `?` that reads as "loomux could not identify your choice" when the truth is
  // that nothing has been chosen.
  assert.equal(setupPreviewMark(input({ agentId: "" })), null);
  assert.equal(setupPreviewMark(input({ agentId: "   " })), null);
});

test("the size is passed through, so the card can draw a larger mark than a pane header", () => {
  // The preview is a card-header element, not a 13px pane-header glyph; if the size were
  // dropped the icon would be correct and illegible.
  const big = setupPreviewMark(input({ agentId: "copilot" }), 20);
  assert.ok(big?.svg.includes(`width="20" height="20"`), big?.svg);
  const custom = setupPreviewMark(input({ agentId: "custom", customCommand: "aider" }), 20);
  assert.ok(custom?.svg.includes(`width="20" height="20"`), custom?.svg);
  const ssh = setupPreviewMark(input({ kind: "ssh", sshCli: "copilot" }), 20);
  assert.ok(ssh?.svg.includes(`width="20" height="20"`), ssh?.svg);
});

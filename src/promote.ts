// Promote-a-standalone-pane-to-orchestrator (#407), pure half. DOM-free and
// backend-free — mirrors resumeerror.ts / panemenu.ts: everything about this
// gesture that is a DECISION (what the human is told before they consent, what
// the relaunched pane is spawned with, what they are told when it fails after
// the kill) lives here and is unit-tested; orchestration.ts is the DOM/IPC shell
// that shows the dialog, calls the command, and drives the pane.
//
// The gesture, in one line: the pane's own CLI session is relaunched IN PLACE
// with the full orchestrator contract, so an hour of prototype conversation
// becomes the orchestrator's own context instead of being hand-summarized into a
// fresh one. See doc/design/orchestration.md's #407 section for the backend half.

import type { OrchSpawnRequest } from "./orchestration";
import type { PaneOptions } from "./pane";
// Explicit `.ts` (as sessionroute.ts does) — a VALUE import in a module that
// `node --test` loads directly has to resolve without a bundler.
import { badgeFor } from "./orchbadge.ts";

/** Strip the machine-readable `promote-<tag>:` prefix off a backend refusal,
 *  leaving the sentence behind it.
 *
 *  Every promote refusal is tagged (mod.rs, mirroring the `resume-<tag>:`
 *  convention `resumeerror.ts` parses) so the frontend can route on the reason.
 *  Promotion has exactly one route for all seven — "say why, change nothing" —
 *  because every refusal happens BEFORE anything is created, retired or killed,
 *  so there is no recovery to offer and no state to clean up. What is left is
 *  presentation: the tag is for code, the sentence is for the human, and showing
 *  both makes a toast the human has to parse before they can read it.
 *
 *  Anything without a recognized tag (an IPC failure, a `String(err)` of
 *  something that was never a backend refusal) comes back untouched — better a
 *  raw message than a swallowed one. */
export function promoteFailureText(message: string): string {
  const text = String(message).trim();
  const m = /^promote-[a-z-]+:\s*/.exec(text);
  return m ? text.slice(m[0].length) : text;
}

/** The repo's `.loomux/workflow.yml` as the confirm needs it: `null` when there
 *  is no file at all, otherwise its `name` (`""` for a nameless one) and whether
 *  it actually validated.
 *
 *  `valid` is carried rather than collapsed into "present" because the three
 *  states need three different sentences — and, more importantly, only ONE of
 *  them may offer the roster checkbox. A file that fails validation launches the
 *  built-in roster (backend-side, `Launch::Promote`), so a ticked box promising
 *  its roster would be a promise the promotion cannot keep. */
export interface PromoteWorkflow {
  name: string;
  valid: boolean;
}

/** Whether the roster checkbox is offered at all — true only for a workflow file
 *  that is present AND valid. The modal reads this instead of re-deriving the
 *  condition beside `promoteConfirmLines`, so the line the human reads and the
 *  control they can tick can never disagree about the same file. */
export function promoteOffersRoster(workflow: PromoteWorkflow | null): boolean {
  return workflow !== null && workflow.valid;
}

/** The confirm dialog's itemized body: what this one click is about to do, in
 *  the order the human needs to know it.
 *
 *  The group-case line states the POLICY rather than naming the resolved group,
 *  deliberately: which case applies is decided by the backend's candidate scan
 *  under its creation lock (`create_group_ex`), and there is no read-only
 *  command that previews it. Re-deriving it here from `orch_session_roles` would
 *  be a second, partial implementation of a backend policy — so the modal states
 *  all three cases and the caller names the group the backend actually resolved
 *  once it has. */
export function promoteConfirmLines(repo: string, workflow: PromoteWorkflow | null): string[] {
  const lines = [
    `Repository: ${repo} — this pane's own working directory becomes the group's repo.`,
    "This pane's Claude session is relaunched in place with the orchestrator contract: " +
      "the conversation carries over, but the turn it is in right now is interrupted.",
    "Group: a new group for this repo, or — if this repo already has one — the existing " +
      "dormant group is reattached (its board and audit history carry over), or a sibling " +
      "group beside a live one. orrerix tells you which once it resolves.",
  ];
  if (workflow === null) return lines;
  const named = workflow.name || ".loomux/workflow.yml";
  // The reattach clause is on BOTH arms, and that is not symmetry for its own
  // sake: which roster a promote ends up running turns on the group case as much
  // as on this file, and that stays true when the file is broken — a dormant
  // group reattaches with the roster its own launch approved, so "this file is
  // broken" does not imply "you get the built-in four". Dropping it from one arm
  // would leave the human reading whichever line they happened to get with a
  // different understanding of the same promotion (rev-2 N9).
  const reattachNote =
    " A reattached dormant group keeps the roster it was launched with either way.";
  lines.push(
    workflow.valid
      ? `This repo declares a workflow (${named}). ` +
          "Tick the box below to run its roster." +
          reattachNote
      : // Present but broken: a NEW group launches on the built-in four roles —
        // the launcher says so inline for the same file, and this is the same
        // consent moment, so it says so here rather than offering a box that
        // would promise a roster nothing is going to run.
        `This repo declares a workflow (${named}), but it doesn't validate — ` +
          "a new group runs the built-in four roles instead. Fix it in a workflow pane and relaunch if you wanted its roster." +
          reattachNote
  );
  return lines;
}

/** What the human is told when the promotion fails AFTER the old process was
 *  killed — the one failure this gesture has to get right.
 *
 *  The group exists on disk by this point (state, board, audit), and the pane is
 *  still sitting there holding the conversation. What must NEVER happen is a
 *  silent fresh session: that discards exactly the context the feature exists to
 *  keep, while looking like it worked. So this names the group and points at the
 *  dormant-group Resume card, which is the ordinary, already-built way back in. */
export function promoteRecoveryNote(groupId: string, stage: "spawn" | "bind"): string {
  const what =
    stage === "spawn"
      ? "the promoted session didn't start"
      : "the promoted session started but orrerix couldn't bind it to the group";
  return (
    `Promotion incomplete — ${what}. Group ${groupId} is on disk with its board and audit log: ` +
    `reopen it from the session browser's ${groupId} Resume card, which brings this conversation back with it.`
  );
}

/** The pane options an in-place promotion relaunches with — the whole reason
 *  this is a function rather than an object literal at the call site.
 *
 *  `env` is the trap the plan named: a normal launcher pane carries NO env at
 *  all, so a promoted pane spawned from a request whose `env` was dropped looks
 *  completely fine and silently has no gh/git shim on `PATH` and no
 *  `LOOMUX_GROUP_DIR` — the merge gate simply isn't there. `argv` matters for the
 *  same reason (direct-CLI spawn, #78). Both are asserted in test/promote.test.ts,
 *  which is as close to a proof as a frontend test gets; the live check is the
 *  manual one named in the PR.
 *
 *  Mirrors `openAgentPane`'s own `paneOpts` mapping without sharing it: that path
 *  opens (or converts a welcome pane into) a NEW pane and discards it on any
 *  failure, which is the exact inverse of what a promoted pane needs — see
 *  `relaunchPaneAsOrchestrator`. `background`/`minimized` have no meaning here at
 *  all: the pane already exists, is already placed, and an orchestrator pane is
 *  never minimized. */
export function promotePaneOptions(req: OrchSpawnRequest, sessionId: string): PaneOptions {
  return {
    name: req.name,
    cwd: req.cwd,
    command: req.command,
    argv: req.argv,
    env: req.env,
    badge: badgeFor(req),
    orchGroup: req.group_id,
    orchRole: req.role,
    orchAgent: req.agent_id,
    // The session being resumed IS this pane's own — recorded so the layout
    // snapshot and any later restore point at the same conversation.
    sessionId,
  };
}

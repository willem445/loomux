// Pure per-pane restore policy for session restore (#194). DOM-free so the
// hybrid decision (below) is unit-tested (test/panerestore.test.ts); the actual
// grid rebuild — feeding these actions to grid.openPane / resumeOrchSession —
// is main.ts wiring (Phase 4).
//
// THE ADOPTED HYBRID (issue #194, plan comment). Resuming a CLI session re-opens
// its context but costs NOTHING until a prompt is sent, so:
//
//   - Terminal  → re-spawn a fresh shell in the recorded cwd + shell kind. No
//                 session to resume; zero cost; layout/cwd back instantly.
//   - Agent     → AUTO-RESUME via the recorded session id (--resume into the idle
//                 TUI): loads context, spends no credits, delivers "near-exact
//                 state". NEVER replays a queued prompt. With no resumable id
//                 (best-effort CLIs) it falls back to a DORMANT pane with a Start
//                 button in the same cwd.
//   - Orch      → NEVER auto-resumed. An orchestration pane (orchestrator /
//                 worker / reviewer) restores DORMANT; the human resumes the
//                 whole group via the existing resumeOrchSession path. This is
//                 the ONE place a resume can actually burn credits — a resumed
//                 autonomous orchestrator (#83) may idle-tick and spawn a worker
//                 storm (#78) — so the credit-safety stance stays exactly here.
//   - Content   → re-open the pane at its recorded root: the file MANAGER (#214),
//                 the file EDITOR, or the GIT view (#217). No process, no session,
//                 nothing to resume — they're pure content, so they come straight
//                 back. Whether the root still exists (and, for git, is still a
//                 repo) is I/O, which this pure module can't do: each action carries
//                 the recorded root (possibly null) and the caller fails soft to the
//                 welcome form in that slot when the probe says no.
//
// Flip AUTO_RESUME_AGENTS to false to make EVERY agent restore dormant instead —
// the plan's promised one-line switch, kept literally one line here.

import type { PersistedPane, PersistedLayoutNode } from "./tabstore";

/** The adopted default (#194): auto-resume agent panes into their prior session.
 *  Set to false for the conservative all-dormant behavior (every agent gets a
 *  Start button; groups are dormant regardless). */
export const AUTO_RESUME_AGENTS = true;

/** What to do with one persisted pane on restore. `relaunch` carries the fields
 *  main.ts needs to open (or leave dormant) the pane; none of these actions ever
 *  replays a prompt or auto-resumes a group. */
export type RestoreAction =
  | { type: "spawn-terminal"; name: string; cwd: string | null; shellKind: PersistedPane["shellKind"] }
  | {
      type: "resume-agent";
      name: string;
      cwd: string | null;
      command: string | null;
      argv: string[] | null;
      /** The recorded session id to --resume into (guaranteed present here). */
      sessionId: string;
    }
  | {
      // An agent whose recorded session id has NO resumable conversation on disk
      // (never prompted → no transcript, or the transcript was deleted). Resuming
      // it would exit 1 ("No conversation found …") and strand a dead pane, so we
      // start a FRESH session in place instead — SAME cwd / CLI / name — reusing the
      // recorded id so the fresh session is itself resumable next boot. (#194 BUG-1)
      type: "fresh-agent";
      name: string;
      cwd: string | null;
      command: string | null;
      argv: string[] | null;
      sessionId: string;
    }
  | {
      type: "dormant-agent";
      name: string;
      cwd: string | null;
      command: string | null;
      argv: string[] | null;
    }
  | {
      // The orchestration pane's whole group stays dormant; the human resumes it
      // via resumeOrchSession. main.ts does NOT spawn a pane for this action. The
      // recorded session id + role ride along so the dormant placeholder carries
      // the CAPTURED group member — the set a whole-group resume restores (#194.5).
      type: "dormant-group";
      name: string;
      sessionId: string | null;
      role: string | null;
    }
  | {
      // A file-explorer pane (#214), back at its recorded root. Nothing to spawn
      // or resume — but `root` may be null (a record written without one) or name
      // a folder that has since been deleted/renamed/unmounted. The caller probes
      // it and, when it isn't a readable directory, opens the WELCOME form in that
      // slot with a message instead — a broken listing pane would be worse than a
      // legible "pick a folder".
      type: "open-files";
      name: string;
      root: string | null;
    }
  | {
      // A file-EDITOR pane (#217), back at its recorded root. Same contract as
      // open-files, same probe (is this still a readable directory?), same fail-soft.
      // Unsaved buffers are NOT persisted: the layout records where the pane was
      // rooted, never what was typed into it — capturing an unsaved buffer would be a
      // second, silent copy of the user's file, and the close guard (confirmClose) is
      // what makes sure they were asked before it could be lost.
      type: "open-editor";
      name: string;
      root: string | null;
    }
  | {
      // A GIT pane (#217), back over its recorded repo. The probe here is stricter
      // than a directory check — the folder can still exist and no longer be a git
      // work tree (deleted .git, a worktree pruned since) — so the caller resolves
      // it with `gitRepoRoot` and fails soft to the welcome form when it isn't one.
      type: "open-git";
      name: string;
      repo: string | null;
    };

/** True when a recorded agent session id still has a resumable conversation on
 *  disk. The wiring builds this from `listSessions()` (which lists exactly the
 *  sessions that HAVE a transcript) and passes it in, keeping this module pure —
 *  the alternative would be a Tauri call from here (#194 BUG-1). */
export type SessionResumable = (sessionId: string) => boolean;

/** Map ONE persisted pane to its restore action, per the adopted hybrid.
 *
 *  @param resumable optional predicate: does this session id still have a
 *  resumable conversation? When omitted, an agent with an id is assumed
 *  resumable (the original behavior). When provided and it returns false, the
 *  agent restores FRESH (same identity) instead of a doomed `--resume`. */
export function planPaneRestore(pane: PersistedPane, resumable?: SessionResumable): RestoreAction {
  switch (pane.paneKind) {
    case "terminal":
      return { type: "spawn-terminal", name: pane.name, cwd: pane.cwd, shellKind: pane.shellKind };
    case "orch":
      // Never auto-resume a group — dormant, human-triggered Resume only. Carry
      // the captured session id + role so the placeholder knows which group member
      // it is (a whole-group resume reads these off the tab's placeholders).
      return { type: "dormant-group", name: pane.name, sessionId: pane.sessionId, role: pane.role };
    case "files":
      // Pure content: no process, no credits, no session — it just comes back at
      // the root it was captured with (which lives in `cwd`).
      return { type: "open-files", name: pane.name, root: pane.cwd };
    case "editor":
      // Same deal (#217). The pane comes back rooted where it was; the FILE that was
      // open — and anything unsaved in it — is deliberately not persisted (see the
      // action's comment above).
      return { type: "open-editor", name: pane.name, root: pane.cwd };
    case "git":
      // Same deal (#217), over a repo instead of a folder. The worktree SELECTION and
      // the read-only unlock (#208) are view state, not layout: a restored git pane
      // opens on the primary worktree, locked, exactly like a freshly opened one — an
      // unlock that survived a restart would be the one piece of this pane's state
      // that could quietly cost you something.
      return { type: "open-git", name: pane.name, repo: pane.cwd };
    case "agent":
      // Auto-resume when we have a session id AND the hybrid is enabled; else a
      // dormant Start placeholder (no id to resume into, or the flip is off).
      if (AUTO_RESUME_AGENTS && pane.sessionId) {
        // If we can tell the session has no resumable conversation, start fresh in
        // place rather than crash on `--resume` (BUG-1). Unknown (no predicate) →
        // attempt the resume; the runtime backstop (shouldRespawnFresh) catches a
        // resume that fails anyway (deleted transcript, CLI error).
        if (resumable && !resumable(pane.sessionId)) {
          return {
            type: "fresh-agent",
            name: pane.name,
            cwd: pane.cwd,
            command: pane.command,
            argv: pane.argv,
            sessionId: pane.sessionId,
          };
        }
        return {
          type: "resume-agent",
          name: pane.name,
          cwd: pane.cwd,
          command: pane.command,
          argv: pane.argv,
          sessionId: pane.sessionId,
        };
      }
      return {
        type: "dormant-agent",
        name: pane.name,
        cwd: pane.cwd,
        command: pane.command,
        argv: pane.argv,
      };
  }
}

/** Turn a resumed agent's recorded launch line into the command that re-opens
 *  its prior session — the "resume/reattach command from a recorded sessionId"
 *  the plan calls for. Pure so it's unit-tested; main.ts feeds the result to
 *  grid.openPane.
 *
 *  Only Claude has a clean resumable id (it's the only CLI we mint a session id
 *  for at launch), so this rewrites a `claude …` line: drop any recorded
 *  `--session-id`/`--resume` (both the space and `=` forms) so we never carry a
 *  stale id or double the flag, KEEP every other flag (model, the autopilot
 *  permission flag) so the resumed pane matches how it was launched, then append
 *  `--resume <id>`. Resuming into the idle TUI costs nothing until a prompt is
 *  sent — and we never append one (the no-replay rule). Prefers the string
 *  `command`; falls back to structured `argv`, then to a bare `claude --resume`. */
export function agentResumeCommand(
  command: string | null,
  argv: string[] | null,
  sessionId: string
): { command?: string; argv?: string[] } {
  const strip = (tokens: string[]): string[] => {
    const out: string[] = [];
    for (let i = 0; i < tokens.length; i++) {
      const t = tokens[i];
      if (t === "--session-id" || t === "--resume") {
        i++; // drop the flag AND its separate value token
        continue;
      }
      if (t.startsWith("--session-id=") || t.startsWith("--resume=")) continue; // `=` form: one token
      out.push(t);
    }
    return out;
  };
  if (command && command.trim()) {
    return { command: [...strip(command.trim().split(/\s+/)), "--resume", sessionId].join(" ") };
  }
  if (argv && argv.length) {
    return { argv: [...strip(argv), "--resume", sessionId] };
  }
  return { command: `claude --resume ${sessionId}` };
}

/** Build the command that starts a FRESH agent session in place, for the
 *  fallback when a recorded session has no resumable conversation (#194 BUG-1).
 *  Same shape as agentResumeCommand but pins the recorded id via `--session-id`
 *  (not `--resume`), so the fresh session is created with that id and becomes
 *  resumable itself once a prompt is sent — and, like resume, never carries a
 *  prompt. Drops any stale `--resume`/`--session-id` first so we don't double or
 *  attempt a resume. */
export function agentFreshCommand(
  command: string | null,
  argv: string[] | null,
  sessionId: string
): { command?: string; argv?: string[] } {
  const strip = (tokens: string[]): string[] => {
    const out: string[] = [];
    for (let i = 0; i < tokens.length; i++) {
      const t = tokens[i];
      if (t === "--session-id" || t === "--resume") {
        i++;
        continue;
      }
      if (t.startsWith("--session-id=") || t.startsWith("--resume=")) continue;
      out.push(t);
    }
    return out;
  };
  if (command && command.trim()) {
    return { command: [...strip(command.trim().split(/\s+/)), "--session-id", sessionId].join(" ") };
  }
  if (argv && argv.length) {
    return { argv: [...strip(argv), "--session-id", sessionId] };
  }
  return { command: `claude --session-id ${sessionId}` };
}

/** Extract the session id a spawn command carries via `--session-id <id>` or
 *  `--resume <id>` (both the space and `=` forms). Used to populate an
 *  orchestration pane's recorded session id from its backend-built command
 *  (which embeds the id rather than passing it as a field), so `capture()` can
 *  persist it for a whole-group resume (#194.5). Null when the command carries no
 *  session flag. Prefers the string command; falls back to structured argv. */
export function sessionIdFromCommand(command: string | null, argv: string[] | null): string | null {
  const scan = (tokens: string[]): string | null => {
    for (let i = 0; i < tokens.length; i++) {
      const t = tokens[i];
      if (t === "--session-id" || t === "--resume") return tokens[i + 1] ?? null;
      if (t.startsWith("--session-id=")) return t.slice("--session-id=".length) || null;
      if (t.startsWith("--resume=")) return t.slice("--resume=".length) || null;
    }
    return null;
  };
  if (command && command.trim()) {
    const id = scan(command.trim().split(/\s+/));
    if (id) return id;
  }
  if (argv && argv.length) return scan(argv);
  return null;
}

/** The runtime backstop decision (#194 BUG-1): a resumed agent pane whose PTY
 *  just exited — should we respawn it FRESH in place instead of stranding a dead
 *  pane? Yes for any UNEXPECTED non-zero exit — a `--resume` against a missing/
 *  deleted transcript exits non-zero ("No conversation found …"), and any other
 *  resume-time CLI failure is handled the same honest way. A loomux-initiated kill
 *  (`expected`) or a clean exit (0, the human quit the resumed session) is left
 *  alone. Pure so the caller can unit-test it; the caller makes it one-shot so a
 *  fresh respawn (which is not a resume) never loops. */
export function shouldRespawnFresh(exit: { exit_code: number | null; expected: boolean }): boolean {
  return !exit.expected && exit.exit_code !== null && exit.exit_code !== 0;
}

/** One `grid.openPane` call in a layout rebuild — enough to reconstruct ANY
 *  nested split tree, including telling a 2×2 grid apart from four stacked panes
 *  (which a flat leaf list cannot).
 *
 *  - `relativeTo` — the index (into the returned array) of an EARLIER step whose
 *    pane is the anchor this one splits from; null for the first pane, which
 *    fills the empty grid (`dir`/`relativeTo` are then ignored). This anchor is
 *    what a flat `{dir, weight}[]` dropped, making nested layouts unreconstructible.
 *  - `dir` — the split direction to open in. Only the SECOND+ child of a split
 *    carries its split's direction; the split's first child is an anchor reused
 *    from an earlier step, never re-opened.
 *  - `weights` — the flex-grow chain from the inserted subtree's OUTERMOST slot
 *    down its left spine to this entry leaf (length 1 for a plain leaf child).
 *    `grid.openPane` resets flex to equal shares as it splits, so restore applies
 *    these afterward: the outermost entry is the weight of the (possibly new)
 *    split element this insertion creates, and each deeper entry is the weight one
 *    level in — exactly the values `grid.layoutSnapshot()` would read back. This
 *    is how the saved 25/75 divider drag survives instead of snapping to 50/50.
 *
 *  A serialize → planLayoutRestore → replay round-trip is structure- AND
 *  weight-identical; test/panerestore.test.ts pins that with a pure model of
 *  grid's `insertBeside`. */
export interface RestoreOpenStep {
  action: RestoreAction;
  relativeTo: number | null;
  dir: "row" | "column";
  weights: number[];
}

/** The pane at a subtree's entry (its leftmost leaf) — the one leaf a split's
 *  first child contributes as the anchor its siblings open relative to. */
function entryLeafPane(node: PersistedLayoutNode): PersistedPane {
  return node.kind === "leaf" ? node.pane : entryLeafPane(node.children[0]);
}

/** The flex-grow chain from a node's own slot down its left spine to the entry
 *  leaf: `[node.weight, firstChild.weight, …, entryLeaf.weight]`. Carries every
 *  split weight the old flat list discarded (only leaf weights survived it). */
function entryWeightChain(node: PersistedLayoutNode): number[] {
  return node.kind === "leaf"
    ? [node.weight]
    : [node.weight, ...entryWeightChain(node.children[0])];
}

/** Flatten a persisted layout tree into the ordered `grid.openPane` plan that
 *  rebuilds it EXACTLY. Pure tree walk (no live panes, no DOM): the first child
 *  of each split stays put as the anchor, and its siblings open beside it in the
 *  split's direction — so a split's direction and its subtree's weights ride on
 *  the sibling steps, never collapsing distinct nestings into one sequence.
 *  main.ts turns each step into `grid.openPane(opts, dir, relativeTo)` and then
 *  applies the `weights`. */
export function planLayoutRestore(
  layout: PersistedLayoutNode,
  resumable?: SessionResumable
): RestoreOpenStep[] {
  const steps: RestoreOpenStep[] = [
    {
      action: planPaneRestore(entryLeafPane(layout), resumable),
      relativeTo: null,
      dir: "row",
      weights: entryWeightChain(layout),
    },
  ];
  const expand = (node: PersistedLayoutNode, anchorIndex: number): void => {
    if (node.kind === "leaf") return;
    // c0 keeps the anchor slot; c1..cn open beside it in this split's direction.
    // Each sibling anchors to the PREVIOUS one, not to c0: grid.insertBeside
    // splices a same-direction sibling in AFTER its anchor, so anchoring every
    // child to c0 would replay [A,B,C,D] as [A,D,C,B]. Walking the anchor forward
    // keeps insertion order.
    const childAnchors = [anchorIndex];
    for (let i = 1; i < node.children.length; i++) {
      const prevAnchor = childAnchors[i - 1];
      childAnchors.push(steps.length);
      steps.push({
        action: planPaneRestore(entryLeafPane(node.children[i]), resumable),
        relativeTo: prevAnchor,
        dir: node.dir,
        weights: entryWeightChain(node.children[i]),
      });
    }
    // Recurse to subdivide every child (a child that is itself a split gets its
    // own siblings opened relative to the anchor we just recorded for it).
    node.children.forEach((child, i) => expand(child, childAnchors[i]));
  };
  expand(layout, 0);
  return steps;
}

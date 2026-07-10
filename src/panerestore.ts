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
      type: "dormant-agent";
      name: string;
      cwd: string | null;
      command: string | null;
      argv: string[] | null;
    }
  | {
      // The orchestration pane's whole group stays dormant; the human resumes it
      // via resumeOrchSession. main.ts does NOT spawn a pane for this action.
      type: "dormant-group";
      name: string;
    };

/** Map ONE persisted pane to its restore action, per the adopted hybrid. */
export function planPaneRestore(pane: PersistedPane): RestoreAction {
  switch (pane.paneKind) {
    case "terminal":
      return { type: "spawn-terminal", name: pane.name, cwd: pane.cwd, shellKind: pane.shellKind };
    case "orch":
      // Never auto-resume a group — dormant, human-triggered Resume only.
      return { type: "dormant-group", name: pane.name };
    case "agent":
      // Auto-resume when we have a session id AND the hybrid is enabled; else a
      // dormant Start placeholder (no id to resume into, or the flip is off).
      if (AUTO_RESUME_AGENTS && pane.sessionId) {
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

/** One step of a layout rebuild: the restore action for a leaf plus how to place
 *  it. `dir` is the direction of the leaf's parent split (the split direction
 *  main.ts opens the pane in); the first/root leaf carries "row" by convention
 *  and grid ignores direction for the first pane. `weight` is the flex-grow the
 *  leaf held, so the rebuilt split keeps the saved proportions. */
export interface RestoreStep {
  action: RestoreAction;
  dir: "row" | "column";
  weight: number;
}

/** Flatten a persisted layout tree into a pre-order sequence of restore steps —
 *  the order main.ts opens panes in (each subsequent pane splits relative to an
 *  earlier one). Pure tree walk: no live panes, no DOM, so nested splits and
 *  weights are exercised in tests. main.ts owns turning this sequence into the
 *  actual grid.openPane(opts, dir, relativeTo) calls. */
export function planLayoutRestore(layout: PersistedLayoutNode): RestoreStep[] {
  const steps: RestoreStep[] = [];
  const walk = (node: PersistedLayoutNode, dir: "row" | "column"): void => {
    if (node.kind === "leaf") {
      steps.push({ action: planPaneRestore(node.pane), dir, weight: node.weight });
      return;
    }
    // Each child opens in THIS split's direction; recurse so a child that is
    // itself a split re-parents its own children under its own direction.
    for (const child of node.children) walk(child, node.dir);
  };
  walk(layout, "row");
  return steps;
}

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
export function planLayoutRestore(layout: PersistedLayoutNode): RestoreOpenStep[] {
  const steps: RestoreOpenStep[] = [
    { action: planPaneRestore(entryLeafPane(layout)), relativeTo: null, dir: "row", weights: entryWeightChain(layout) },
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
        action: planPaneRestore(entryLeafPane(node.children[i])),
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

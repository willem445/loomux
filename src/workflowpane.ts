// The workflow pane's DECISIONS, pure (#222 v2, rev-15). DOM-free and I/O-free — the same
// move `dirtystate.ts` makes for the editor: the view is left with the wiring, and the rules it
// used to hold in its head are stated once, here, where they are unit-tested.
//
// Every rule in this file is here because getting it wrong cost something real in review:
//
//   * `paneSurface` — the pane showed its "no workflow here" start surface for a file that was
//     THERE and merely unreadable, and then offered to create a starter over the top of it.
//   * `savePlan` — a "create" wrote with a null expected hash, which the backend reads as
//     *write unconditionally*, so a workflow that appeared after the pane opened was destroyed
//     with a green "Saved" toast.
//   * `layoutPruneIds` — a drag pruned the layout file against the UNSAVED buffer, so deleting
//     a block (without saving) and then dragging another one wrote the deletion into
//     `workflow.layout.json` on disk before the human had committed it to `workflow.yml`.
//
// None of those is a rendering bug or a wiring bug. They are all the view answering a question
// it should never have been holding the answer to.

import type { Workflow } from "./workflowmodel";

// ---------- which surface the pane shows ----------

/** The three things a workflow pane can be showing.
 *
 *  `error` and `start` are DIFFERENT, and conflating them is what produced the bug this pane's
 *  v2 exists to fix: "there is no workflow here" and "there is one and I cannot read it" look
 *  the same to a naive `catch`, and they are opposites. One of them invites you to create a
 *  file; the other must not, because creating means overwriting a file we just refused to show
 *  you. */
export type PaneSurface = "error" | "start" | "body";

export function paneSurface(state: {
  /** Why the file could not be READ, or null (including when there simply isn't one). */
  loadError: string | null;
  /** Does the workflow file exist on disk? */
  exists: boolean;
  /** The live buffer. A scaffold the human has not saved yet counts as content. */
  text: string;
}): PaneSurface {
  if (state.loadError !== null) return "error";
  if (!state.exists && !state.text.trim()) return "start";
  return "body";
}

// ---------- how a save is allowed to write ----------

/** How the next write must be performed.
 *
 *  `guarded-write` is an ordinary save: we read the file, we hold its hash, and the backend
 *  refuses the write if the disk has moved under us (an agent, a git pull, another editor) —
 *  the human gets the conflict dialog instead of a silent overwrite.
 *
 *  `claim-then-write` is a CREATE, and it exists because a create used to pass a null expected
 *  hash — which `write_file` reads as "write unconditionally". The pane can sit on its start
 *  surface for minutes; a workflow that arrives in that window (an agent writes one, a `git
 *  pull` brings one in) was overwritten by the scaffold, and the pane reported success. So a
 *  create must first CLAIM the path atomically (`fm_new_file` is `create_new(true)`, which
 *  refuses without truncating if anything is there) and then write against the claimed file's
 *  own hash — which makes even the sliver between the claim and the write an ordinary,
 *  conflict-guarded write.
 *
 *  There is no third option, and in particular there is no "write unconditionally": the only
 *  path that may do that is an explicit human "Overwrite" from the conflict dialog, which is
 *  not a save plan, it is an answer to a question. */
export type SavePlan = { kind: "claim-then-write" } | { kind: "guarded-write"; expectedHash: string };

export function savePlan(state: { exists: boolean; savedHash: string }): SavePlan {
  return state.exists && state.savedHash
    ? { kind: "guarded-write", expectedHash: state.savedHash }
    : { kind: "claim-then-write" };
}

// ---------- what the layout file is allowed to forget ----------

/** When the layout is being written. A drag writes it constantly; a save writes it once, at a
 *  moment when the file on disk and the buffer in memory hold the same roster. */
export type LayoutWrite = "drag" | "save";

/** The block ids whose positions may SURVIVE the next layout write — i.e. what `pruneLayout`
 *  is allowed to prune against.
 *
 *  Pruning exists so a workflow you have edited for a year doesn't accumulate the coordinates
 *  of every block you ever deleted. The mistake was pruning against the buffer on every DRAG:
 *  a block deleted but not yet saved does not exist in the buffer, so its position was pruned
 *  out of the layout file ON DISK before the human had committed the deletion to `workflow.yml`
 *  — and then discarding the edit brought the block back with its position gone. A position is
 *  disposable, so this only ever cost a drag; but it is a write to disk performed on the
 *  strength of an edit the human had not made yet, and that is the wrong shape of thing to be
 *  doing regardless of what it costs.
 *
 *  So: a SAVE prunes against the roster (they are the same roster by then — the workflow write
 *  has just succeeded). A DRAG prunes against the UNION of what is on disk and what is in the
 *  buffer: it will still forget a block that exists in neither (the long-dead ones pruning is
 *  for), and it cannot forget one that is merely pending an undo. */
export function layoutPruneIds(
  saved: Workflow | null,
  buffer: Workflow,
  when: LayoutWrite
): string[] {
  const ids = new Set(buffer.blocks.map((b) => b.id).filter(Boolean));
  if (when === "drag") {
    for (const b of saved?.blocks ?? []) if (b.id) ids.add(b.id);
  }
  return [...ids];
}

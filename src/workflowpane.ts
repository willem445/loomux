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

// ---------- what a canonical save is about to destroy ----------

/** What saving would do to the file that is on disk, when it would do something the human did
 *  not ask for. Null when the save is faithful — which is the common case, and which must stay
 *  silent.
 *
 *  THE TRADE THIS EXISTS TO SURFACE. A form or canvas edit re-serializes the whole workflow
 *  through the canonical formatter (that is what keeps the file legible and the diffs small),
 *  and the formatter does not preserve comments — it cannot, because the model it serializes
 *  from does not carry them. For a file loomux itself wrote, that costs nothing. For a file a
 *  HUMAN wrote, the comments are frequently the most valuable lines in it: this repo's own
 *  `.loomux/workflow.yml` is 126 lines of which 60 are comments explaining the roster and the
 *  `.github/agents/` convention, and one dragged edge would have silently taken all 60.
 *
 *  Comment-preserving serialization is the real fix and it is a feature with its own design
 *  (round-tripping comments through an AST that the form can still rewrite). Until then, the
 *  honest thing is not to pretend the loss doesn't happen — it is to say so, once, before it
 *  does, and let the human decide. A rewrite they consented to is a trade; a rewrite they
 *  discovered in `git diff` is a bug. */
export interface RewriteImpact {
  /** Comment lines the rewrite would drop. */
  droppedComments: number;
  /** True when the write replaces a non-canonical file with the canonical form — i.e. the diff
   *  is the whole file, not the lines that changed. */
  reformats: boolean;
}

/** A `#` at the start of a line or after whitespace — a whole-line comment OR a trailing one
 *  (`kind: worker   # the one that opens the PR`). Both are dropped by a canonical re-serialize,
 *  so counting only the whole-line ones would under-report what the human is about to lose. */
const CARRIES_COMMENT = /(^|\s)#/;

/** Lines carrying a comment. Deliberately approximate in one direction: a `#` inside a quoted
 *  scalar or a prompt body matches too — but it matches in BOTH texts and survives serialization,
 *  so it cancels out of the DIFFERENCE, which is the only thing this is ever used for. */
const commentLines = (text: string): number =>
  text.split(/\r?\n/).filter((l) => CARRIES_COMMENT.test(l)).length;

/** Would writing `next` over `disk` cost the human something they didn't ask to spend?
 *
 *  `isCanonical` is the signal the whole thing turns on: we are about to write canonical text
 *  over a file that was NOT in canonical form, which is precisely the "the diff is the entire
 *  file" case. A file already in canonical form — anything loomux wrote — saves silently, as it
 *  should: there is nothing to warn about, and a confirm that fires when nothing is at stake is
 *  a confirm people learn to click through. */
export function rewriteImpact(
  disk: string,
  next: string,
  isCanonical: (text: string) => boolean
): RewriteImpact | null {
  if (!disk.trim()) return null; // creating a file destroys nothing
  if (next === disk) return null; // writing the same bytes is not a rewrite
  const droppedComments = Math.max(0, commentLines(disk) - commentLines(next));
  // Only a canonical write over a non-canonical file reformats. A human editing the raw YAML
  // tab is writing THEIR text — they can see exactly what they are saving, and warning them
  // about their own keystrokes would be absurd.
  const reformats = isCanonical(next) && !isCanonical(disk);
  if (!droppedComments && !reformats) return null;
  return { droppedComments, reformats };
}

/** What to tell the human, in the words that name what they lose. */
export function rewriteImpactMessage(impact: RewriteImpact, file: string): string {
  const n = impact.droppedComments;
  const comments = n > 0 ? `the comments on ${n} line${n === 1 ? "" : "s"} will be dropped` : "";
  const shape = impact.reformats ? "the file will be rewritten in loomux's canonical form" : "";
  const both = [comments, shape].filter(Boolean).join(", and ");
  return (
    `Saving ${file} from the form or the canvas re-writes the whole file from the workflow model — ` +
    `so ${both}. The workflow itself is unchanged; the comments and the layout of the text are not recoverable. ` +
    `(Editing the YAML tab directly saves exactly what you typed.)`
  );
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

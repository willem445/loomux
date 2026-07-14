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

/** May the pane CREATE a starter workflow right now?
 *
 *  THE LIVE BUG, and it is the one this whole file was supposed to have already prevented. The
 *  "Create workflow" button lives on the start surface, and the pane's only defence against
 *  scaffolding over a workflow that was already loaded was that the button is *supposed to be
 *  invisible* anywhere else. It wasn't: a `display: flex` class rule out-specifies the `hidden`
 *  attribute (see the `[hidden]` note at the top of styles.css), so all three surfaces rendered
 *  at once and the button sat there, live, on top of a workflow the pane had read and validated.
 *  Pressing it destroyed that workflow.
 *
 *  And `savePlan`'s claim-then-write (F2) did not catch it, because from the save's point of view
 *  nothing was wrong: the pane KNEW the file was there and held its hash, so the plan was an
 *  ordinary guarded write, the hash matched, and the backend wrote exactly what it was told to
 *  write. Every guard downstream of the button was working. The button should not have been
 *  pressable.
 *
 *  So: VISIBILITY IS NOT A SAFETY PROPERTY. Creating is permitted on the START surface and
 *  nowhere else — the same single decision that puts the button on screen — which means the
 *  button cannot be pressed in a state where pressing it would overwrite anything. And because
 *  the start surface is *by definition* "no file on disk, nothing in the buffer", every create it
 *  permits is a `claim-then-write`, which is the property the test in workflowpane.test.ts pins:
 *  a create can never even be *asked* to clobber. */
export function createAllowed(state: {
  loadError: string | null;
  exists: boolean;
  text: string;
}): boolean {
  return paneSurface(state) === "start";
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

// ---------- what a canonical rewrite is about to destroy ----------

/** What rewriting the file in FULLY canonical form would cost, when it would do something the
 *  human did not ask for. Null when the rewrite is faithful — which is the common case, and
 *  which must stay silent.
 *
 *  THE TRADE THIS EXISTS TO SURFACE, and — since #233 — where it still applies. Before #233,
 *  every form or canvas edit re-serialized the WHOLE workflow through the canonical formatter,
 *  every time, and the formatter did not preserve comments. Now an ordinary edit goes through
 *  `serializeWorkflowPreserving` (workflowmodel.ts), which reuses the original text for
 *  whatever it didn't touch — so this guard no longer needs to fire on every save. What is
 *  left is the explicit **Format** action: a human asking, in one step, to rewrite the whole
 *  file into canonical form — fixed key order, no comments, whatever was there before. For a
 *  file loomux itself wrote that costs nothing. For a file a HUMAN wrote, the comments are
 *  frequently the most valuable lines in it: this repo's own `.loomux/workflow.yml` is 126
 *  lines of which 60 are comments explaining the roster and the `.github/agents/` convention —
 *  and Format would silently take all 60 without this.
 *
 *  So the honest thing is not to pretend the loss doesn't happen — it is to say so, once,
 *  before it does, and let the human decide. A rewrite they consented to is a trade; a rewrite
 *  they discovered in `git diff` is a bug. */
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

  // REFORMATTING IS THE WHOLE TRIGGER (rev-15 F7). Warning on dropped comments *alone* looked
  // like belt-and-braces and was in fact the one case the docblock above says must never fire:
  // `next` here is always the FULLY canonical form (Format's only caller, since #233 — see
  // `workflowview.ts`'s `confirmFormatRewrite`), so a comments-only branch would be reachable
  // only when `disk` was ALSO already canonical, i.e. nothing worth warning about ever reaches
  // it. A dialog that explains your own keystroke back to you is how a guard becomes noise, and
  // noise is how the guard that matters gets clicked through.
  if (!reformats) return null;
  return { droppedComments, reformats };
}

/** What to tell the human, in the words that name what they lose. */
export function rewriteImpactMessage(impact: RewriteImpact, file: string): string {
  const n = impact.droppedComments;
  const comments = n > 0 ? `the comments on ${n} line${n === 1 ? "" : "s"} will be dropped` : "";
  const shape = impact.reformats ? "the file will be rewritten in loomux's canonical form" : "";
  const both = [comments, shape].filter(Boolean).join(", and ");
  return (
    `Formatting ${file} re-writes the whole file from the workflow model, in canonical form — ` +
    `so ${both}. The workflow itself is unchanged; the comments and the layout of the text are not recoverable. ` +
    `(An ordinary edit through the form or the canvas does not do this — only Format rewrites the whole file.)`
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

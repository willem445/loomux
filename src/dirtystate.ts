// Pure decisions for the editor's unsaved-changes + conflict handling (issue
// #174). No DOM — the FileEditView calls these to decide whether to show the
// dirty dot, whether closing/switching needs a confirm, and whether an on-disk
// hash change since open is a conflict. node:test-covered.

/** True when the live buffer differs from the last-saved snapshot. A strict
 *  string compare: re-typing the original text clears the dirty state, exactly
 *  what a user expects from "no unsaved changes". */
export function isDirty(original: string, current: string): boolean {
  return original !== current;
}

/** What to do when the user tries to close the overlay or switch to another
 *  file: a clean buffer just closes; a dirty one must confirm (discard / cancel)
 *  so edits aren't silently lost. */
export type CloseDecision = "close" | "confirm";

export function closeDecision(dirty: boolean): CloseDecision {
  return dirty ? "confirm" : "close";
}

/** Whether the file changed on disk since it was opened: the hash captured at
 *  read time no longer matches the current on-disk hash. The backend enforces
 *  this on write (returning a `conflict` error); this mirror lets the frontend
 *  reason about it too (e.g. after a git-watcher refresh) and is the tested
 *  statement of the rule. An empty expected hash means "new file, nothing to
 *  conflict with". */
export function hasConflict(expectedHash: string, diskHash: string): boolean {
  if (expectedHash === "") return false;
  return expectedHash !== diskHash;
}

/** How the UI should resolve a detected conflict — the three choices offered in
 *  the conflict dialog. Modelled as a type so the view's branching is explicit
 *  and the option set is single-sourced. */
export type ConflictChoice = "overwrite" | "reload" | "cancel";

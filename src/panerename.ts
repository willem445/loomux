// Pure, DOM-free core of the pane inline-rename commit, split out so it's
// unit-testable under `node --test` (see pane.ts startRename for the DOM
// wiring). This is the twin of the #77 tasksview double-commit fix: the same
// latent crash lived in the pane-title rename on the badge/naming path (#75).
//
// The bug: commit() is wired to BOTH the Enter/Escape key handler and blur.
// The first commit swaps the focused <input> back to the title element, and
// detaching a focused element makes the browser fire blur → commit() runs a
// second time and calls replaceWith on the now-detached node, which throws
// "NotFoundError: ... node ... is no longer a child". The exception escapes the
// handler into main.ts's global "error" listener, which paints it as the
// app-wide fatal banner — even though the rename visibly succeeded.

export interface RenameCommitOps {
  /** The input's current (untrimmed) value. */
  value: () => string;
  /** Persist an accepted, non-empty new name. */
  save: (name: string) => void;
  /** Swap the input back to the title element and restore focus. Runs exactly
   *  once (guarded), while the input is still connected, so its replaceWith is
   *  always safe. */
  restore: () => void;
}

/** Build an idempotent rename-commit callback. Only the first invocation does
 *  anything; the redundant blur-driven call (or a stray second key event) is a
 *  no-op, so the DOM swap never runs against a detached node. A side benefit of
 *  the guard: Escape (save=false) wins over the trailing blur-save, so cancel
 *  truly cancels instead of being overwritten by the blur committing the edit. */
export function makeRenameCommit(ops: RenameCommitOps): (save: boolean) => void {
  let committed = false;
  return (save: boolean) => {
    if (committed) return;
    committed = true;
    if (save) {
      const v = ops.value().trim();
      if (v) ops.save(v);
    }
    ops.restore();
  };
}

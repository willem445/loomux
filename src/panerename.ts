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
  /** Whether the input is still attached to the live document. A `false` here
   *  means the edit ended *involuntarily* — an orchestrator-driven grid/dock
   *  restructure moved the pane's subtree and blurred the input — rather than by
   *  an explicit Enter/click. See {@link makeRenameCommit}. */
  isConnected: () => boolean;
  /** Persist an accepted, non-empty new name. */
  save: (name: string) => void;
  /** Swap the input back to the title element and restore focus. Runs exactly
   *  once (guarded); tolerates a detached/reparented input so the header is left
   *  consistent regardless of how the edit ended (see domutil.swapEditor). */
  restore: () => void;
}

/** Build an idempotent rename-commit callback. Only the first invocation does
 *  anything; the redundant blur-driven call (or a stray second key event) is a
 *  no-op, so the DOM swap never runs twice. Two guards:
 *
 *  - Escape (save=false) wins over the trailing blur-save, so cancel truly
 *    cancels instead of being overwritten by the blur committing the edit.
 *  - An *involuntary* end — blur fired because an orchestrator-driven grid/dock
 *    restructure detached the input mid-edit (`isConnected()` is false) — is
 *    treated as a cancel, never a save. Only an explicit commit while the input
 *    is still on the document (Enter, or a real click-away blur) persists the
 *    name. That keeps a half-typed title from silently syncing to the roster,
 *    and stops a stale blur from clobbering a concurrent orch-rename echo. */
export function makeRenameCommit(ops: RenameCommitOps): (save: boolean) => void {
  let committed = false;
  return (save: boolean) => {
    if (committed) return;
    committed = true;
    // Persist only on an explicit commit that is still connected — an involuntary
    // detach (grid move) blurs with save=true but must not write a half-typed name.
    if (save && ops.isConnected()) {
      const v = ops.value().trim();
      if (v) ops.save(v);
    }
    ops.restore();
  };
}

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

// ---------- discard means discard (#219) ----------

/** The buffer after the human confirms a **Discard**.
 *
 *  It is the last-saved snapshot — i.e. the file as it is on disk. The rule looks too
 *  small to name, and that is exactly why it needs naming: the overlay used to answer
 *  "Discard unsaved changes?" by HIDING itself and keeping the buffer, so the edits
 *  came back — still dirty — the next time you pressed Alt+F, and the same question was
 *  asked again. "Discard" that doesn't discard is a dialog that lies, and a second ask
 *  trains people to click through the first one.
 *
 *  So the view calls this rather than inlining `setValue(savedContent)`, and
 *  `isDirty(saved, discardEdits(saved))` is false by construction: one confirmed
 *  discard, one question, edits gone. (Hiding a view WITHOUT dropping its buffer is a
 *  legitimate thing to want — it is just not "discard", and it would need its own
 *  affordance and its own word.) */
export function discardEdits(saved: string): string {
  return saved;
}

// ---------- who is holding unsaved work (#219) ----------

/** Where an unsaved buffer lives. The human needs to be told which: an editor PANE is
 *  visibly an editor, while an OVERLAY is the Alt+F editor tucked inside a terminal or
 *  agent pane — which is precisely the one you forget you left open. */
export type DirtyHost = "pane" | "overlay";

/** One pane's unsaved-work report, as the DOM layer sees it. A pane with no editor at
 *  all reports nothing; a pane with a clean one reports `dirty: false`. */
export interface PaneBufferReport {
  /** The tab the pane lives in — a quit confirm spans every tab, and "config.ts" means
   *  nothing until you know which project's. */
  tab: string;
  /** The pane's title. */
  pane: string;
  host: DirtyHost;
  /** Root-relative path of the open file, or null if somehow none is. */
  file: string | null;
  dirty: boolean;
}

/** One unsaved buffer: a report that answered yes. */
export type DirtyBuffer = Omit<PaneBufferReport, "dirty">;

/** Every unsaved buffer across the app, in the order the caller walked its panes.
 *
 *  Pure so the enumeration itself is testable: the app-quit guard has to see EVERY
 *  holder — editor panes, and the Alt+F overlays inside terminal/agent panes, across
 *  every tab including hidden ones and docked panes — and a quit that misses one is a
 *  quit that silently destroys it. */
export function dirtyBuffers(reports: readonly PaneBufferReport[]): DirtyBuffer[] {
  return reports
    .filter((r) => r.dirty)
    .map(({ tab, pane, host, file }) => ({ tab, pane, host, file }));
}

/** May the app quit right now? The SAME gate as a pane close (`closeDecision`), asked
 *  of the whole app instead of one pane — so "dirty means ask" is stated once and the
 *  quit path cannot grow a private rule.
 *
 *  ONE consolidated ask, deliberately, not a save prompt per buffer: a human quitting
 *  an app with six dirty files does not want six dialogs, they want to know that six
 *  files are dirty and decide once. See doc/design/content-panes.md. */
export function quitDecision(dirty: readonly DirtyBuffer[]): CloseDecision {
  return closeDecision(dirty.length > 0);
}

/** How long the quit path waits for its final session save before giving up on it.
 *
 *  Short on purpose. The write is a backend round-trip to a small JSON file; if it hasn't
 *  landed in a second and a half it is wedged (a stalled disk, a hung IPC), and waiting
 *  longer only makes the app look frozen while the human clicks the ✕ again. */
export const QUIT_FLUSH_TIMEOUT_MS = 1500;

/** Wait for `work`, but never longer than `ms` — "done" when it landed, "timeout" when
 *  the deadline won.
 *
 *  This exists for exactly one caller and one reason: the quit path AWAITS its final
 *  session save, and an await with no deadline is an unquittable app. Failing open on a
 *  throw (which the guard already does) does not cover a HANG — a promise that never
 *  settles never throws. So the last write is raced, and on expiry the close proceeds
 *  anyway: a possibly-stale layout snapshot is a small, recoverable loss (the previous
 *  fire-and-forget write is at most one edit behind), while a window whose ✕ does nothing
 *  is not recoverable at all. The trade is stated in doc/design/content-panes.md.
 *
 *  A rejection counts as "done" — not because the write succeeded, but because we are no
 *  longer WAITING on it, and the caller's job here is only to decide when to stop. */
export function withDeadline(work: Promise<unknown>, ms: number): Promise<"done" | "timeout"> {
  return new Promise((resolve) => {
    let settled = false;
    const finish = (outcome: "done" | "timeout") => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(outcome);
    };
    const timer = setTimeout(() => finish("timeout"), ms);
    void work.then(
      () => finish("done"),
      () => finish("done")
    );
  });
}

/** One line per unsaved buffer for that confirm — "tab · pane — file", with the Alt+F
 *  overlays marked, since "which pane is that even in?" is the whole difficulty of the
 *  overlay case. */
export function dirtyBufferLines(dirty: readonly DirtyBuffer[]): string[] {
  return dirty.map((d) => {
    const where = d.host === "overlay" ? `${d.pane} (Alt+F editor)` : d.pane;
    return `${d.tab} · ${where} — ${d.file ?? "unsaved file"}`;
  });
}

// ---------- a pane whose process just died (#219) ----------

/** What a PTY exit reports: its code, and whether loomux itself killed it. */
export interface ExitInfo {
  exit_code: number | null;
  expected: boolean;
}

/** Why a pane whose process exited is being KEPT open, or null to dispose it. */
export type KeepOpenReason =
  /** A command pane died unexpectedly — the pane stays so its error is readable. */
  | "output"
  /** It holds unsaved editor edits. Disposing it would destroy them, and NOTHING that
   *  the human didn't ask for is allowed to do that (#219). */
  | "unsaved";

/** Should a pane survive its process's death — and if so, why?
 *
 *  Two independent reasons, composed here rather than in the pane, so the composition is
 *  testable and so the second one cannot be forgotten by the next path that reaps a pane:
 *
 *   - `output` (the original rule): a COMMAND pane that died unexpectedly stays open so
 *     the human can read the crash. A clean exit, or a loomux-initiated kill, closes.
 *   - `unsaved` (#219): the pane holds a dirty Alt+F buffer. The process is already gone,
 *     so keeping the pane costs nothing — while disposing it costs the human their edits,
 *     with no prompt, on a path they never invoked. Every AUTOMATIC teardown (a PTY
 *     exiting, a group ending) therefore keeps such a pane; only the human-initiated
 *     closes may destroy a buffer, and those ask first.
 *
 *  `output` wins the label when both apply: the dead process is the louder fact, and the
 *  banner names the unsaved buffer separately anyway. */
export function keepOpenOnExit(state: {
  /** True for agent/command panes (vs plain shells) — the original rule's gate. */
  launchedCommand: boolean;
  exit: ExitInfo;
  hasUnsavedWork: boolean;
}): KeepOpenReason | null {
  const crashed = state.launchedCommand && !state.exit.expected && state.exit.exit_code !== 0;
  if (crashed) return "output";
  if (state.hasUnsavedWork) return "unsaved";
  return null;
}

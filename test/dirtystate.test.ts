// Unit tests for the pure dirty/conflict decisions (issue #174), extended for the
// unsaved-buffer LIFECYCLE (#219): who is holding unsaved work, whether the app may
// quit, whether a dead pane stays, and what "discard" means. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  isDirty,
  closeDecision,
  hasConflict,
  discardEdits,
  dirtyBuffers,
  dirtyBufferLines,
  quitDecision,
  exitDiagnosticLine,
  keepOpenOnExit,
  isDoaRevival,
  withDeadline,
  QUIT_FLUSH_TIMEOUT_MS,
  type PaneBufferReport,
} from "../src/dirtystate.ts";

test("isDirty reflects buffer vs last-saved", () => {
  assert.equal(isDirty("abc", "abc"), false);
  assert.equal(isDirty("abc", "abcd"), true);
  // Edge: re-typing the original clears dirty.
  assert.equal(isDirty("abc", "abx"), true);
  assert.equal(isDirty("", ""), false);
});

test("closeDecision confirms only when dirty", () => {
  assert.equal(closeDecision(false), "close");
  assert.equal(closeDecision(true), "confirm");
});

test("reload-after-replace guard: a clean buffer reloads, a dirty one confirms", () => {
  // Finding #2 — a cross-file replace that touches the open file must not
  // silently overwrite unsaved edits. The decision is exactly the close-guard:
  // clean → reload freely; dirty → confirm before discarding.
  const clean = isDirty("saved", "saved");
  const dirty = isDirty("saved", "saved + edits");
  assert.equal(closeDecision(clean), "close"); // reload without prompting
  assert.equal(closeDecision(dirty), "confirm"); // prompt before losing edits
});

test("pane-close guard: closing an EDITOR pane is the SAME decision (#217)", () => {
  // An editor pane is the pane kind where loomux itself owns an unsaved buffer, so the
  // human-initiated single-pane closes — the header ✕, the DOCK CHIP's ✕ (the one that
  // bypassed the guard in #214 and silently discarded a docked pane's edits), and
  // Ctrl+Shift+W — all funnel through Pane.requestClose → Pane.confirmClose →
  // FileEditView.canDiscard(), which is THIS decision and nothing else.
  //
  // Pinned here so "dirty means ask" stays stated once: a fourth consumer must not grow
  // its own private rule, and the pane path must not drift from the overlay's own Esc/✕.
  const clean = isDirty("saved", "saved");
  const dirty = isDirty("saved", "saved + edits");
  assert.equal(closeDecision(clean), "close"); // close the pane, no prompt
  assert.equal(closeDecision(dirty), "confirm"); // ask before dropping the buffer
});

test("hasConflict fires when the on-disk hash drifted from the opened hash", () => {
  assert.equal(hasConflict("aaaa", "aaaa"), false);
  assert.equal(hasConflict("aaaa", "bbbb"), true);
  // Edge: a new file (no expected hash) never conflicts.
  assert.equal(hasConflict("", "bbbb"), false);
});

// ---------- discard means discard (#219) ----------

test("a confirmed Discard drops the edits — it does not hide them and ask again", () => {
  // THE BUG: the overlay answered "Discard unsaved changes?" by hiding itself with the
  // dirty buffer intact. Press Alt+F again and the edits were back — still unsaved — and
  // the next close asked the same question. A dialog that discards nothing is a dialog
  // that lies, and a second ask is how people learn to click through the first.
  const saved = "line one\n";
  const edited = "line one\nline two (unsaved)\n";
  assert.equal(isDirty(saved, edited), true);
  assert.equal(closeDecision(isDirty(saved, edited)), "confirm"); // it asks, once

  const after = discardEdits(saved);
  assert.equal(after, saved, "the buffer goes back to what is on disk");
  assert.equal(isDirty(saved, after), false, "…so it is clean");
  // Re-opening the editor finds disk state and NOTHING to ask about — the whole point.
  assert.equal(closeDecision(isDirty(saved, after)), "close");
});

// ---------- who is holding unsaved work (#219) ----------

const report = (over: Partial<PaneBufferReport>): PaneBufferReport => ({
  tab: "loomux",
  pane: "shell",
  host: "overlay",
  file: "src/pane.ts",
  dirty: false,
  ...over,
});

test("dirtyBuffers finds every holder — both hosts, every tab, clean ones dropped", () => {
  // The app-quit guard has to see ALL of them: an editor PANE's own buffer, and the Alt+F
  // OVERLAY tucked inside a terminal/agent pane — which is the one a human forgets they
  // left open, especially in a background tab. A quit that misses one destroys it.
  const found = dirtyBuffers([
    report({ tab: "loomux", pane: "pane.ts", host: "pane", file: "src/pane.ts", dirty: true }),
    report({ tab: "loomux", pane: "claude · fix", host: "overlay", file: "src/git.ts", dirty: false }),
    report({ tab: "docs", pane: "shell", host: "overlay", file: "README.md", dirty: true }),
    report({ tab: "docs", pane: "notes", host: "pane", file: "TODO.md", dirty: false }),
  ]);
  assert.deepEqual(found, [
    { tab: "loomux", pane: "pane.ts", host: "pane", file: "src/pane.ts" },
    { tab: "docs", pane: "shell", host: "overlay", file: "README.md" },
  ]);
  // Order is the caller's walk order (tab, then pane) — the confirm's list reads like the
  // window looks, rather than in whatever order a filter happened to visit.
});

test("quitDecision: nothing unsaved quits SILENTLY; anything unsaved asks", () => {
  // The common case must not grow a dialog — a quit confirm that fires when there is
  // nothing to lose is a confirm people stop reading.
  assert.equal(quitDecision([]), "close");
  assert.equal(quitDecision(dirtyBuffers([report({ dirty: false })])), "close");
  // And it is the SAME gate as a pane close (closeDecision), so "dirty means ask" stays
  // stated once — the quit path cannot grow a private rule.
  assert.equal(quitDecision(dirtyBuffers([report({ dirty: true })])), "confirm");
  assert.equal(quitDecision(dirtyBuffers([report({ dirty: true })])), closeDecision(true));
});

test("the quit confirm names WHERE each buffer is — and marks the hidden ones", () => {
  // "config.ts is unsaved" is useless across five tabs. The line says which tab, which
  // pane, and — for the Alt+F overlay — that it is an editor you can't see from here.
  const lines = dirtyBufferLines(
    dirtyBuffers([
      report({ tab: "loomux", pane: "pane.ts", host: "pane", file: "src/pane.ts", dirty: true }),
      report({ tab: "docs", pane: "claude · fix", host: "overlay", file: "README.md", dirty: true }),
    ])
  );
  assert.deepEqual(lines, [
    "loomux · pane.ts — src/pane.ts",
    "docs · claude · fix (Alt+F editor) — README.md",
  ]);
});

// ---------- a pane whose process just died (#219) ----------

const exited = (code: number | null, expected = false) => ({ exit_code: code, expected });

test("keepOpenOnExit: a crashed command pane stays to show its output (the original rule)", () => {
  assert.equal(
    keepOpenOnExit({ launchedCommand: true, exit: exited(1), hasUnsavedWork: false }),
    "output"
  );
  // A clean exit, a loomux-initiated kill, and a plain shell all still close.
  assert.equal(keepOpenOnExit({ launchedCommand: true, exit: exited(0), hasUnsavedWork: false }), null);
  assert.equal(
    keepOpenOnExit({ launchedCommand: true, exit: exited(1, true), hasUnsavedWork: false }),
    null
  );
  assert.equal(keepOpenOnExit({ launchedCommand: false, exit: exited(1), hasUnsavedWork: false }), null);
});

test("keepOpenOnExit: an UNSAVED buffer keeps the pane, however the process died", () => {
  // The point of #219: no automatic teardown may destroy work the human never agreed to
  // lose. A clean exit, an expected kill (a group ending kills its agents), a plain shell
  // — all of them close a pane today, and all of them would have taken a dirty Alt+F
  // buffer with them.
  for (const exit of [exited(0), exited(1, true), exited(null, true)]) {
    assert.equal(
      keepOpenOnExit({ launchedCommand: true, exit, hasUnsavedWork: true }),
      "unsaved",
      "a dirty buffer keeps the pane"
    );
    assert.equal(
      keepOpenOnExit({ launchedCommand: false, exit, hasUnsavedWork: true }),
      "unsaved",
      "…even in a plain shell, which has no output worth keeping"
    );
  }
});

test("keepOpenOnExit: a crash AND a dirty buffer reports the crash — the banner says both", () => {
  // The dead process is the louder fact and gets the label; the pane's banner names the
  // unsaved buffer separately, so neither is silent.
  assert.equal(
    keepOpenOnExit({ launchedCommand: true, exit: exited(1), hasUnsavedWork: true }),
    "output"
  );
});

test("exitDiagnosticLine: names the DOA-silent-death case, but only when nothing was ever printed (#281)", () => {
  assert.equal(
    exitDiagnosticLine(false),
    "[loomux] produced no output before exiting — it likely died before printing " +
      "anything at all (a missing/corrupt session, a rejected resume flag, or a gone " +
      "working directory are the usual causes)"
  );
  // A crash that produced real output reads fine as the plain "process exited"
  // banner — inventing a diagnostic here would just be noise.
  assert.equal(exitDiagnosticLine(true), null);
});

// ---------- a DOA orchestration-delegate revival auto-closes (#280) ----------

test("isDoaRevival: a delegate crash with NO output at all is closed, not kept open", () => {
  assert.equal(
    isDoaRevival({ orchRole: "worker", keep: "output", receivedOutput: false, hasUnsavedWork: false }),
    true
  );
  assert.equal(
    isDoaRevival({ orchRole: "reviewer", keep: "output", receivedOutput: false, hasUnsavedWork: false }),
    true
  );
  assert.equal(
    isDoaRevival({ orchRole: "planner", keep: "output", receivedOutput: false, hasUnsavedWork: false }),
    true
  );
});

test("isDoaRevival: a delegate crash that DID produce output is a real crash — stays open", () => {
  // #281 already told the human/orchestrator why via the exit diagnostic AND
  // the pane's own output; if there IS output, it's the original "kept open
  // to read" case, not clutter.
  assert.equal(
    isDoaRevival({ orchRole: "worker", keep: "output", receivedOutput: true, hasUnsavedWork: false }),
    false
  );
});

test("isDoaRevival: the orchestrator's own pane is never auto-closed out from under the human", () => {
  assert.equal(
    isDoaRevival({ orchRole: "orchestrator", keep: "output", receivedOutput: false, hasUnsavedWork: false }),
    false
  );
});

test("isDoaRevival: a plain (non-orchestration) pane keeps the original crash-stays-open behavior", () => {
  assert.equal(
    isDoaRevival({ orchRole: null, keep: "output", receivedOutput: false, hasUnsavedWork: false }),
    false
  );
});

test("isDoaRevival: never overrides the UNSAVED reason (#219) — only 'output' is in scope", () => {
  assert.equal(
    isDoaRevival({ orchRole: "worker", keep: "unsaved", receivedOutput: false, hasUnsavedWork: false }),
    false
  );
  assert.equal(
    isDoaRevival({ orchRole: "worker", keep: null, receivedOutput: false, hasUnsavedWork: false }),
    false
  );
});

test("isDoaRevival: a DOA crash that ALSO holds an unsaved Alt+F buffer is NEVER auto-closed (#219)", () => {
  // The bug: keepOpenOnExit labels a crash "output" even when hasUnsavedWork is
  // true (the dead process is the louder fact, so it wins the LABEL) — but the
  // unsaved buffer is still there. isDoaRevival must not read "output" as proof
  // there's nothing to protect; it has to ask about the buffer directly, or an
  // AUTOMATIC teardown silently destroys work the human never agreed to lose.
  assert.equal(
    isDoaRevival({ orchRole: "worker", keep: "output", receivedOutput: false, hasUnsavedWork: true }),
    false,
    "a DOA revival with an unsaved buffer must stay open, not auto-close"
  );
});

// ---------- the quit path's final save must not be able to wedge the app (#219) ----------

test("withDeadline: a save that lands is waited for", async () => {
  const landed = await withDeadline(Promise.resolve("written"), 1000);
  assert.equal(landed, "done");
});

test("withDeadline: a save that HANGS is abandoned — the app still quits", async () => {
  // The failure the fail-open catch does NOT cover: a promise that never settles never
  // throws. Without a deadline, awaiting the final session save means a stalled disk or a
  // hung IPC leaves the human with a ✕ that does nothing. A possibly-stale snapshot is a
  // small, recoverable loss (the fire-and-forget write is at most one edit behind); an
  // unquittable app is not recoverable at all.
  const neverSettles = new Promise<never>(() => {});
  assert.equal(await withDeadline(neverSettles, 20), "timeout");
});

test("withDeadline: a save that REJECTS stops the wait too (we were only ever waiting)", async () => {
  // Not a claim that the write succeeded — flushTabs owns that (it catches and lets the
  // next change retry the bytes). This function's only job is deciding when to stop
  // waiting, and a rejection has answered that.
  assert.equal(await withDeadline(Promise.reject(new Error("disk full")), 1000), "done");
});

test("the quit flush deadline is short enough to be invisible, long enough for a real write", () => {
  // A small JSON write to the backend that hasn't landed in this long is wedged, and
  // waiting longer only makes the app look frozen while the human clicks ✕ again.
  assert.ok(QUIT_FLUSH_TIMEOUT_MS >= 500 && QUIT_FLUSH_TIMEOUT_MS <= 3000);
});

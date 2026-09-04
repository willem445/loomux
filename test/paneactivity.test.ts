// Unit tests for the per-pane activity reducer (src/paneactivity.ts, #2122
// slice A2). The clock is a plain argument, so the whole state machine —
// including the four-second output window — is driven here without a timer.
//
// The property that matters most is a NEGATIVE one: `noteAttention(null)` is
// the FOCUS ACK (the human clicked the pane), and it must NOT clear the
// at-prompt latch. A reducer that cleared on it would flip a still-parked pane
// back to "working" the moment anyone looked at it, which is exactly the defect
// the latch exists to avoid — and an absence-only assertion is vacuous unless
// something first proves the latch was ever set, so every such case sets it and
// asserts it, then acts, then asserts it survived.
//
// Red arm (mechanically): delete the `this.atPrompt = false` line in
// `noteHumanInput` and `a keystroke clears the latch` reddens alone; delete the
// floor comparison in `noteOutput` and `output above the floor clears the
// latch` reddens alone; change `reason === "waiting"` to `reason !== null` and
// `the focus ack must not clear the latch` stays green while
// `an urgent reason does not itself set the latch` reddens.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { PaneActivity, ACTIVITY_FLOOR_BYTES, ACTIVITY_WINDOW_MS } from "../src/paneactivity.ts";

const T0 = 1_000_000;

/** A pane parked at a prompt, with the latch proved set — the positive control
 *  every "does not clear" case below needs before it asserts an absence. */
function parked(): PaneActivity {
  const a = new PaneActivity();
  a.noteAttention("waiting");
  assert.equal(a.snapshot(T0).atPrompt, true, "fixture is not parked — the case below would be vacuous");
  return a;
}

test("a waiting sighting latches at-prompt", () => {
  const a = new PaneActivity();
  assert.equal(a.snapshot(T0).atPrompt, false);
  a.noteAttention("waiting");
  assert.equal(a.snapshot(T0).atPrompt, true);
});

test("the focus ack must not clear the latch", () => {
  const a = parked();
  // `setAttention(null)` is what `Pane.ackAttention` produces when the human
  // clicks a pane. Clicking is not evidence the agent resumed.
  a.noteAttention(null);
  assert.equal(a.snapshot(T0).atPrompt, true);
});

test("an urgent reason does not itself set the latch", () => {
  const a = new PaneActivity();
  a.noteAttention("blocked");
  assert.equal(a.snapshot(T0).atPrompt, false);
});

test("another reason arriving over a park does not clear it", () => {
  const a = parked();
  a.noteAttention("blocked");
  assert.equal(a.snapshot(T0).atPrompt, true);
});

test("a keystroke clears the latch", () => {
  const a = parked();
  a.noteHumanInput(T0 + 10);
  const s = a.snapshot(T0 + 10);
  assert.equal(s.atPrompt, false);
  assert.equal(s.lastHumanInputMs, T0 + 10);
});

test("output above the floor clears the latch", () => {
  const a = parked();
  a.noteOutput(ACTIVITY_FLOOR_BYTES, T0 + 10);
  assert.equal(a.snapshot(T0 + 10).atPrompt, false);
});

test("a sub-floor repaint does not clear the latch", () => {
  const a = parked();
  // ~164 bytes is the measured size of an idle Claude Code input-box repaint —
  // the exact thing the floor exists to ignore.
  a.noteOutput(164, T0 + 10);
  const s = a.snapshot(T0 + 10);
  assert.equal(s.atPrompt, true);
  // Positive control: the chunk WAS seen, so the assertion above is about the
  // floor rather than about a reducer that ignored the call.
  assert.equal(s.bytesInWindow, 164);
  assert.equal(s.lastOutputMs, T0 + 10);
});

test("sub-floor repaints spread across windows never accumulate to the floor", () => {
  const a = parked();
  // Twenty idle repaints of 164 bytes is 3280 — over the floor in total, but
  // each one lands in its own window because the pane went quiet in between.
  let t = T0;
  for (let i = 0; i < 20; i++) {
    t += ACTIVITY_WINDOW_MS + 1;
    a.noteOutput(164, t);
  }
  const s = a.snapshot(t);
  assert.equal(s.atPrompt, true);
  assert.equal(s.bytesInWindow, 164, "each lapsed window must start the count over");
});

test("sub-floor chunks inside ONE window do accumulate past the floor", () => {
  const a = parked();
  let t = T0;
  // The same twenty chunks, arriving in a burst: that IS a repaint worth
  // noticing, and the reducer must not be a per-chunk floor.
  for (let i = 0; i < 20; i++) {
    t += 100;
    a.noteOutput(164, t);
  }
  const s = a.snapshot(t);
  assert.equal(s.bytesInWindow, 20 * 164);
  assert.equal(s.atPrompt, false);
});

test("a lapsed window reports no bytes even with nothing new arriving", () => {
  const a = new PaneActivity();
  a.noteOutput(500, T0);
  assert.equal(a.snapshot(T0).bytesInWindow, 500);
  assert.equal(a.snapshot(T0 + ACTIVITY_WINDOW_MS).bytesInWindow, 500, "still inside the window");
  assert.equal(a.snapshot(T0 + ACTIVITY_WINDOW_MS + 1).bytesInWindow, 0);
  // `lastOutputMs` is a fact about the past and does not lapse with the window.
  assert.equal(a.snapshot(T0 + ACTIVITY_WINDOW_MS + 1).lastOutputMs, T0);
});

test("an empty chunk moves nothing", () => {
  const a = parked();
  a.noteOutput(0, T0 + 10);
  const s = a.snapshot(T0 + 10);
  assert.equal(s.lastOutputMs, null);
  assert.equal(s.bytesInWindow, 0);
  assert.equal(s.atPrompt, true);
});

test("a fresh reducer knows nothing", () => {
  const s = new PaneActivity().snapshot(T0);
  assert.deepEqual(s, {
    lastOutputMs: null,
    bytesInWindow: 0,
    lastHumanInputMs: null,
    atPrompt: false,
    rosterIdle: null,
  });
});

test("roster idleness is carried through unchanged, including its null", () => {
  const a = new PaneActivity();
  assert.equal(a.snapshot(T0).rosterIdle, null, "a pane the roster does not cover reads null, not false");
  a.noteRosterIdle(true);
  assert.equal(a.snapshot(T0).rosterIdle, true);
  a.noteRosterIdle(false);
  assert.equal(a.snapshot(T0).rosterIdle, false);
  a.noteRosterIdle(null);
  assert.equal(a.snapshot(T0).rosterIdle, null);
});

test("roster idleness does not touch the latch in either direction", () => {
  // #2089: `idle_since_ms` means "the reaper would call this idle", NOT "parked
  // at a prompt". The two must stay separable or the Agents tab reports a
  // finished turn on a pane that merely holds no assignment.
  const parkedPane = parked();
  parkedPane.noteRosterIdle(false);
  assert.equal(parkedPane.snapshot(T0).atPrompt, true);
  const busy = new PaneActivity();
  busy.noteRosterIdle(true);
  assert.equal(busy.snapshot(T0).atPrompt, false);
});

// ---------------------------------------------------------------------------

/** Every `.rs` file under a production source root. */
function rsFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const p = join(dir, name);
    return statSync(p).isDirectory() ? rsFiles(p) : p.endsWith(".rs") ? [p] : [];
  });
}

/** Read a `const NAME: <int type> = <literal>;` out of the production Rust,
 *  scanning BOTH source roots rather than one hardcoded path — the engine
 *  extraction (#888) keeps moving constants between them, and a reader pinned
 *  to the old file answers "the shape changed", which is the wrong diagnosis.
 *  `test/selectorknobs.test.ts` is the precedent. */
function rustConst(name: string): number {
  const here = dirname(fileURLToPath(import.meta.url));
  const ROOTS = [join(here, "..", "src-tauri", "src"), join(here, "..", "crates", "loomux-engine", "src")];
  const pattern = new RegExp(`\\bconst ${name}: u\\d+ = (\\d+);`);
  const hits = ROOTS.flatMap((root) => {
    const files = rsFiles(root);
    // A mistyped or stale root reads as zero files and would otherwise hide
    // behind the other root's hit.
    assert.ok(files.length > 0, `no .rs files under ${root} — did the tree move?`);
    return files
      .map((f) => ({ file: f, m: pattern.exec(readFileSync(f, "utf8")) }))
      .filter((h) => h.m !== null);
  });
  assert.equal(
    hits.length,
    1,
    `${name} must be declared in exactly one production source file, found ${hits.length} (${hits
      .map((h) => h.file)
      .join(", ")})`,
  );
  return Number(hits[0]!.m![1]);
}

test("ACTIVITY_FLOOR_BYTES still matches the Rust literal it was copied from", () => {
  // Nothing at the type level ties a TS copy to a Rust literal, so read the
  // Rust back and fail loudly the day someone changes one without the other.
  assert.equal(ACTIVITY_FLOOR_BYTES, rustConst("DEFAULT_IDLE_ACTIVITY_FLOOR_BYTES"));
});

test("ACTIVITY_WINDOW_MS still matches the backend's own quiet threshold", () => {
  assert.equal(ACTIVITY_WINDOW_MS, rustConst("ATTENTION_QUIET_MS"));
});

test("the Rust reader is not vacuous — a name that is not there fails", () => {
  // The two pins above are the only thing keeping the duplicated constants in
  // step, and a reader that silently matched nothing would pass them by
  // accident. It must throw rather than return a default.
  assert.throws(() => rustConst("DEFINITELY_NOT_A_REAL_CONSTANT_NAME"), /exactly one production source file/);
});

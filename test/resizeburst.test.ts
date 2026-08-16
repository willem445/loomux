// #1149: the Sessions strip's open/close lagged because the fit debounce could
// not coalesce. These tests measure that in the only unit that means anything —
// HOW MANY FITS a given stream of geometry changes produces — rather than
// asserting the delay arithmetic back at itself.
//
// The simulator below is the load-bearing part, so it is worth saying what it
// models and what it does not. It replays a list of `ResizeObserver` delivery
// times through a single re-armable timer, firing a due timer BEFORE the tick
// it is due at or before. That ordering is the browser's: a `setTimeout`
// resolves from the task queue, while a `ResizeObserver` callback is delivered
// later in the same frame's rendering steps (HTML standard, "update the
// rendering"). It is exactly why a 16 ms debounce never coalesced two 60 Hz
// frames, and getting it wrong in the other direction would make the old policy
// look better than it was.
//
// What it does NOT model: the cost of a fit, xterm's write queue (`doFit`
// defers the geometry change behind `term.write("", cb)`, #432 item 2), and the
// `shouldResizePty` skips that can drop a scheduled fit's PTY call afterwards
// (panefit.ts). Those only ever REMOVE resizes, so every count here is an upper
// bound on what reaches ConPTY, for both policies alike.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { planFit, FIT_WINDOW_MS, FIT_MAX_WAIT_MS, type FitPlan } from "../src/resizeburst.ts";

/** The debounce as `pane.ts` runs it: one timer, re-armed on every tick. */
type Schedule = (nowMs: number, burstStartMs: number | null) => FitPlan;

/** The times at which a fit actually runs, for a given tick stream. */
function fitTimes(ticks: readonly number[], schedule: Schedule): number[] {
  const fits: number[] = [];
  let due: number | null = null;
  let burst: number | null = null;
  for (const now of ticks) {
    if (due !== null && due <= now) {
      fits.push(due);
      due = null;
      burst = null; // a fit ends the burst — pane.ts clears it in the same place
    }
    const plan = schedule(now, burst);
    burst = plan.burstStartMs;
    due = now + plan.dueInMs;
  }
  if (due !== null) fits.push(due); // the trailing fit, after the last tick
  return fits;
}

/** The shipped policy. */
const coalesced: Schedule = (nowMs, burstStartMs) =>
  planFit({ nowMs, burstStartMs, windowMs: FIT_WINDOW_MS, maxWaitMs: FIT_MAX_WAIT_MS });

/** What shipped before #1149: `setTimeout(() => this.doFit(), 16)`, re-armed on
 *  every tick and blind to how long the burst had been running. */
const SHIPPED_BEFORE_MS = 16;
const perFrame: Schedule = (nowMs) => ({ dueInMs: SHIPPED_BEFORE_MS, burstStartMs: nowMs });

const FRAME_MS = 1000 / 60;
/** `ResizeObserver` deliveries over `durationMs` of continuous change, at
 *  `hz` frames per second — the first one lands one frame after the change
 *  starts, and the last one at or just past the end. */
function frames(durationMs: number, hz = 60): number[] {
  const step = 1000 / hz;
  const out: number[] = [];
  for (let t = step; t < durationMs + step; t += step) out.push(t);
  return out;
}

// ---------- the case the issue is about ----------

/** `#sessions` is an in-flow flex item with `transition: width 0.24s`
 *  (styles.css), so a toggle drives #grid-area — and therefore every pane's
 *  termEl — through a 240 ms animation. */
const SESSIONS_TRANSITION_MS = 240;

test("a Sessions open/close fits each pane ONCE, not once per animation frame", () => {
  const ticks = frames(SESSIONS_TRANSITION_MS);
  assert.equal(ticks.length, 15, "a 240 ms transition at 60 Hz is 15 ResizeObserver deliveries");

  const before = fitTimes(ticks, perFrame);
  const after = fitTimes(ticks, coalesced);

  assert.equal(
    before.length,
    15,
    "the 16 ms debounce fired once per frame — a window narrower than the interval between " +
      "the events it debounces coalesces nothing, which is #1149's root cause"
  );
  assert.equal(
    after.length,
    1,
    "the whole transition must collapse to one fit at the settled geometry: every fit is an " +
      "xterm reflow AND a ResizePseudoConsole (CLAUDE.md constraint 1)"
  );
  assert.ok(
    after[0] > ticks[ticks.length - 1],
    `the one fit must land after the last geometry change (${after[0]} vs ${ticks[ticks.length - 1]}), ` +
      `or it fits an intermediate size and the settled one never reaches the PTY`
  );
});

test("the Sessions transition never trips the ceiling — that is what FIT_MAX_WAIT_MS is sized for", () => {
  // The two constants are not independent: a ceiling below transition + window
  // would put a fit at an intermediate geometry, which is the ConPTY repaint
  // this module exists to remove. Pinned as the inequality rather than as the
  // resulting count, so a future transition longer than 240 ms fails HERE, with
  // the reason, instead of quietly costing an extra resize.
  assert.ok(
    FIT_MAX_WAIT_MS > SESSIONS_TRANSITION_MS + FIT_WINDOW_MS,
    `FIT_MAX_WAIT_MS (${FIT_MAX_WAIT_MS}) must exceed the longest animated transition ` +
      `(${SESSIONS_TRANSITION_MS} ms) plus one window (${FIT_WINDOW_MS} ms)`
  );
});

test("a slow machine still coalesces: 20 Hz frames are inside the window", () => {
  // The window's real job is to be wider than the gap between deliveries. At
  // 20 fps that gap is 50 ms; below ~17 fps the burst stops being a burst and
  // each frame legitimately fits on its own.
  assert.equal(fitTimes(frames(SESSIONS_TRANSITION_MS, 20), coalesced).length, 1);
});

// ---------- the other consumers of the same path ----------

test("a one-shot layout change still fits exactly once — equalize, autosize, a split", () => {
  // These write flex weights in one pass, so the observer delivers once. The
  // count is unchanged (it was already 1); what changes is only WHEN, and the
  // added latency is the window, not the ceiling.
  const before = fitTimes([FRAME_MS], perFrame);
  const after = fitTimes([FRAME_MS], coalesced);
  assert.equal(before.length, 1);
  assert.equal(after.length, 1);
  assert.equal(
    after[0] - before[0],
    FIT_WINDOW_MS - SHIPPED_BEFORE_MS,
    "a one-shot change may not pay the ceiling — only the difference between the two windows"
  );
});

test("a gesture that never settles still fits, on the ceiling", () => {
  // A window-edge drag has no settled geometry for as long as the human holds
  // the mouse, so the trailing edge alone would withhold the fit for the whole
  // drag. The bound is the clock, not the burst signal (performance.md §2 P4).
  const ticks = frames(2000, 60);
  const after = fitTimes(ticks, coalesced);
  assert.ok(after.length >= 4, `a 2 s drag must keep reflowing, got ${after.length} fits`);
  const gaps = after.slice(1).map((t, i) => t - after[i]);
  for (const gap of gaps) {
    assert.ok(
      gap <= FIT_MAX_WAIT_MS + FRAME_MS + 1,
      `a fit was withheld for ${gap} ms, past the ${FIT_MAX_WAIT_MS} ms ceiling`
    );
  }
  assert.ok(
    after.length < fitTimes(ticks, perFrame).length / 10,
    "and at 60 Hz it must still be an order fewer than one per frame"
  );
});

// ---------- what this can and cannot promise ----------
//
// The claim these two tests replaced was "no tick stream produces MORE fits
// than the 16 ms debounce did", asserted over nine streams. It is FALSE, and
// the nine streams were exactly one short of showing it: the two variables that
// have to be crossed — a refresh rate above 62.5 Hz AND a burst longer than the
// ceiling — were each covered alone and never together (rev-1 on #1157). What
// follows pins the real boundary in both directions instead, because a property
// stated one condition too wide is worth less than a narrower one that is true.

/** Every burst shape the app produces that is SHORTER than the ceiling. */
const SHORT_BURSTS: Record<string, number[]> = {
  "sessions transition": frames(SESSIONS_TRANSITION_MS),
  "sessions transition at 20 Hz": frames(SESSIONS_TRANSITION_MS, 20),
  "sessions transition at 144 Hz": frames(SESSIONS_TRANSITION_MS, 144),
  "one-shot": [FRAME_MS],
  "nothing at all": [],
  "trickle wider than both windows": [0, 500, 1000, 1500],
  "stutter across the window edge": [0, 59, 130, 189, 260, 319],
  "repeated toggles": [...frames(240), ...frames(240).map((t) => t + 1000)],
};

test("for a burst shorter than the ceiling, this never adds a fit — at any refresh rate", () => {
  // The half that IS universal, and the half #1149 is about: every animated
  // transition in the app is shorter than FIT_MAX_WAIT_MS, so no ceiling fit
  // fires inside one and the trailing edge can only ever remove fits. The
  // "trickle" and "stutter" rows are bursts that END and restart rather than
  // one long one, which is why they belong here and not below.
  for (const [name, ticks] of Object.entries(SHORT_BURSTS)) {
    const before = fitTimes(ticks, perFrame).length;
    const after = fitTimes(ticks, coalesced).length;
    assert.ok(
      after <= before,
      `"${name}": the coalescer scheduled ${after} fits where the 16 ms debounce scheduled ` +
        `${before} — inside a burst shorter than the ceiling this may only ever remove resizes`
    );
  }
});

test("a burst that OUTLASTS the ceiling adds fits above 62.5 Hz — pinned, not hidden", () => {
  // The crossed stream the old nine were missing. Above 62.5 Hz the frame gap
  // is under the old 16 ms window, so the old debounce coalesced a burst of ANY
  // length into one trailing fit — leaving the terminal frozen at its
  // pre-gesture size for the whole gesture. The ceiling replaces that with a
  // cadence, and that costs real ConPTY resizes.
  //
  // Asserted as `after > before`, deliberately: this is a trade, and a trade
  // that is merely tolerated by a `<=` nobody crosses is a trade nobody can
  // find. Anyone who makes this equal again has to come here and say so.
  const drag144 = frames(2000, 144);
  const before = fitTimes(drag144, perFrame).length;
  const after = fitTimes(drag144, coalesced).length;
  assert.equal(before, 1, "at 144 Hz the 16 ms window swallowed the whole 2 s drag");
  assert.ok(
    after > before,
    `a 2 s drag at 144 Hz must be recorded as scheduling MORE than the old debounce ` +
      `(${after} vs ${before}); if this went equal, the ceiling changed and the module ` +
      `header's boundary and the PR body's table are now wrong too`
  );
  // ...and bounded: one fit per ceiling, not one per frame. The cost of the
  // trade is capped, which is the reason it is acceptable at all.
  assert.ok(
    after <= Math.ceil(2000 / FIT_MAX_WAIT_MS) + 1,
    `the added fits must stay at one per ${FIT_MAX_WAIT_MS} ms ceiling, got ${after}`
  );
});

test("the boundary is the old 16 ms window, in both directions", () => {
  // A specimen either side of it, on the same 2 s burst. 62.5 Hz is exactly a
  // 16 ms gap — the last rate at which the old debounce fired per frame — and
  // 63 Hz is the first at which it coalesced everything. Both directions run,
  // so neither "always fewer" nor "always more" can pass this.
  const drag = (hz: number): { before: number; after: number } => {
    const ticks = frames(2000, hz);
    return { before: fitTimes(ticks, perFrame).length, after: fitTimes(ticks, coalesced).length };
  };
  const at62 = drag(62.5);
  assert.ok(
    at62.after < at62.before,
    `at 62.5 Hz (gap exactly 16 ms) the coalescer must still remove fits: ` +
      `${at62.after} vs ${at62.before}`
  );
  const at63 = drag(63);
  assert.ok(
    at63.after > at63.before,
    `at 63 Hz (gap just under 16 ms) the old debounce coalesced the whole drag, so the ` +
      `ceiling must show up as an increase: ${at63.after} vs ${at63.before}`
  );
});

test("the ceiling is what crosses the boundary — a burst inside it never does", () => {
  // The OTHER variable, isolated at a rate where the old policy coalesced
  // everything. Below the ceiling the two policies agree; past it they diverge.
  // Pinning both sides here is what stops the finding from being read as "high
  // refresh is worse", which would be the wrong lesson: it is high refresh AND
  // a burst longer than the ceiling.
  const at = (durationMs: number): { before: number; after: number } => {
    const ticks = frames(durationMs, 144);
    return { before: fitTimes(ticks, perFrame).length, after: fitTimes(ticks, coalesced).length };
  };
  const inside = at(FIT_MAX_WAIT_MS);
  assert.equal(
    inside.after,
    inside.before,
    `a ${FIT_MAX_WAIT_MS} ms burst at 144 Hz sits inside the ceiling and must cost the same`
  );
  const past = at(FIT_MAX_WAIT_MS + 40);
  assert.ok(
    past.after > past.before,
    `a burst just past the ceiling must be where the extra fit appears: ` +
      `${past.after} vs ${past.before}`
  );
});

// ---------- the repairs, which each have a way of making things worse ----------

test("a maxWait at or below the window cannot degenerate into fit-every-tick", () => {
  // Unrepaired, a ceiling tighter than the window would bind on EVERY tick and
  // the result would be the pre-#1149 behaviour wearing new constants. Floored
  // at the window, the worst it can decay to is a fixed-interval throttle: one
  // fit per window, which is the property asserted here rather than a count
  // (the count is a consequence of the two numbers and would move with them).
  const bad: Schedule = (nowMs, burstStartMs) =>
    planFit({ nowMs, burstStartMs, windowMs: FIT_WINDOW_MS, maxWaitMs: 5 });
  const ticks = frames(SESSIONS_TRANSITION_MS);
  const fits = fitTimes(ticks, bad);
  assert.ok(
    fits.length < ticks.length,
    `${fits.length} fits for ${ticks.length} ticks — the ceiling made it fire on every tick`
  );
  for (const [i, t] of fits.slice(1).entries()) {
    assert.ok(
      t - fits[i] >= FIT_WINDOW_MS,
      `two fits ${t - fits[i]} ms apart, inside the ${FIT_WINDOW_MS} ms window`
    );
  }
});

test("a burst start in the future (a backwards clock step) restarts the burst", () => {
  // Date.now() follows the wall clock, so an NTP correction can leave a stored
  // burst start ahead of `now`. Unrepaired, a step larger than maxWait makes
  // the ceiling negative for the rest of the burst — dueInMs pinned at 1 ms, a
  // fit every tick, i.e. worse than what this replaced.
  const plan = planFit({
    nowMs: 1000,
    burstStartMs: 9000,
    windowMs: FIT_WINDOW_MS,
    maxWaitMs: FIT_MAX_WAIT_MS,
  });
  assert.equal(plan.burstStartMs, 1000);
  assert.equal(plan.dueInMs, FIT_WINDOW_MS);
});

test("dueInMs is never zero, however far past the ceiling the burst is", () => {
  // A zero-delay re-arm on a tick stream that does not stop is a busy loop.
  const plan = planFit({
    nowMs: 10_000,
    burstStartMs: 0,
    windowMs: FIT_WINDOW_MS,
    maxWaitMs: FIT_MAX_WAIT_MS,
  });
  assert.equal(plan.dueInMs, 1);
});

test("a quiet pane's first tick starts the burst at that tick", () => {
  const plan = planFit({
    nowMs: 4242,
    burstStartMs: null,
    windowMs: FIT_WINDOW_MS,
    maxWaitMs: FIT_MAX_WAIT_MS,
  });
  assert.equal(plan.burstStartMs, 4242);
  assert.equal(plan.dueInMs, FIT_WINDOW_MS);
});

// ---------- the wiring, at the one axis a unit test can see ----------

/** Top-level argument slices of the call whose `(` is at `open`. Same shape as
 *  `perfpolicy.test.ts`'s splitter (not imported — importing a test module runs
 *  its tests a second time); `null` when the call is unterminated, which fails
 *  the assertion below rather than passing over it. */
function callArgs(text: string, open: number): string[] | null {
  const args: string[] = [];
  let depth = 0;
  let start = open + 1;
  let quote: string | null = null;
  for (let i = open; i < text.length; i++) {
    const c = text[i];
    if (quote !== null) {
      if (c === "\\") i++;
      else if (c === quote) quote = null;
      continue;
    }
    if (c === '"' || c === "'" || c === "`") quote = c;
    else if (c === "(" || c === "[" || c === "{") depth++;
    else if (c === ")" || c === "]" || c === "}") {
      depth--;
      if (depth === 0) {
        args.push(text.slice(start, i));
        return args;
      }
    } else if (c === "," && depth === 1) {
      args.push(text.slice(start, i));
      start = i + 1;
    }
  }
  return null;
}

test("pane.ts arms its fit timer from this policy, not from a literal delay", () => {
  // DOM wiring is hand-verified in this repo, but "the pure module is green"
  // says nothing about whether anything calls it, and every count above is a
  // claim about the APP only if `applyFit` is this module's caller.
  //
  // The axis is the DELAY EXPRESSION at the fit timer, not any identifier
  // around it: a re-introduced fixed debounce — the #1149 defect — is a numeric
  // literal in that argument position and cannot be spelled any other way,
  // whatever the surrounding names become. Default-deny: the timer must be
  // found (a rename of the field fails here loudly rather than silently
  // watching nothing) and its delay must not be a literal.
  const src = readFileSync(new URL("../src/pane.ts", import.meta.url), "utf8");
  const sites = [...src.matchAll(/\bthis\.fitTimer\s*=\s*window\.setTimeout\s*\(/g)];
  assert.equal(
    sites.length,
    1,
    "pane.ts must arm exactly one fit timer through `this.fitTimer = window.setTimeout(` — if " +
      "that moved or was renamed, this guard is watching nothing and has to be re-pointed"
  );
  const open = sites[0].index + sites[0][0].length - 1;
  const args = callArgs(src, open);
  assert.ok(args !== null && args.length === 2, "could not read the fit timer's arguments");
  assert.doesNotMatch(
    args[1],
    /^\s*\d/,
    `the fit timer is armed with the literal delay ${args[1].trim()} — that is #1149 itself: a ` +
      `fixed window narrower than the gap between ResizeObserver deliveries coalesces nothing`
  );
  assert.match(
    src,
    /\bplanFit\s*\(/,
    "pane.ts must compute that delay with planFit — a policy nothing calls is not a policy"
  );
});

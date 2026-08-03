// Unit tests for the repo-signal throttle (src/refreshthrottle.ts) — the bound
// `git-changed` declares in test/perfpolicy.test.ts's stream manifest, and the
// window `GitView.notifyPrompt` has always used.
//
// The property under test is again a COUNT: a burst of repository-change
// signals costs one immediate read plus one trailing read per window, not one
// per signal. The last test drives the decision through a fake clock and timer
// exactly as `Pane.signalDirRefresh` does, so what it counts is what the pane
// invokes.
//
// Red arm: set REPO_SIGNAL_WINDOW_MS to 0 (the pre-#743 behavior of the
// dir_info half — no window at all) and the burst test reports 10 reads
// instead of 2, with the "window 0 disables throttling" test the one that
// stays green because that is what it asserts.
import { test } from "node:test";
import assert from "node:assert/strict";
import { decideRefresh, REPO_SIGNAL_WINDOW_MS } from "../src/refreshthrottle.ts";

test("the first signal into a quiet pane runs immediately (leading edge)", () => {
  assert.deepEqual(
    decideRefresh({ nowMs: 10_000, lastRunMs: 0, timerPending: false, windowMs: 500 }),
    { kind: "run" },
    "a cd or an external commit must move the header chip now, not in half a second"
  );
});

test("a second signal inside the window books the remainder of it", () => {
  assert.deepEqual(
    decideRefresh({ nowMs: 10_120, lastRunMs: 10_000, timerPending: false, windowMs: 500 }),
    { kind: "schedule", dueInMs: 380 },
    "the trailing run must fire when the window closes, not a full window later"
  );
});

test("further signals while a trailing run is booked are dropped", () => {
  assert.deepEqual(
    decideRefresh({ nowMs: 10_200, lastRunMs: 10_000, timerPending: true, windowMs: 500 }),
    { kind: "drop" },
    "one trailing run per window — a burst must not book a timer per signal"
  );
});

test("a signal after the window has elapsed runs immediately again", () => {
  assert.deepEqual(
    decideRefresh({ nowMs: 10_500, lastRunMs: 10_000, timerPending: false, windowMs: 500 }),
    { kind: "run" }
  );
});

test("a window of 0 disables throttling entirely (the A/B arm)", () => {
  for (let i = 0; i < 5; i++) {
    assert.deepEqual(
      decideRefresh({ nowMs: 10_000 + i, lastRunMs: 10_000, timerPending: false, windowMs: 0 }),
      { kind: "run" },
      "windowMs <= 0 is the pre-#743 behavior: every signal reads"
    );
  }
});

test("a clock that jumps backwards still releases the pane, within one window", () => {
  // NTP correction / suspend-resume. The pane must not park forever on a window
  // that can never elapse: the decision is a bounded wait, never a negative one.
  const d = decideRefresh({ nowMs: 9_000, lastRunMs: 10_000, timerPending: false, windowMs: 500 });
  assert.equal(d.kind, "schedule");
  assert.ok(d.kind === "schedule" && d.dueInMs >= 1 && d.dueInMs <= 500, `dueInMs=${JSON.stringify(d)}`);
});

test("a burst of git-changed events costs one read plus one trailing read", () => {
  // The shape Pane.signalDirRefresh runs: a fake clock, one pending timer, and
  // a counter standing in for the dir_info invoke. Ten signals 30 ms apart is a
  // rebase or an interactive `git add -p` seen through the 1 Hz watcher plus
  // the shell's own prompt reports.
  let now = 1_000;
  let lastRunMs = 0;
  let timer: { at: number } | null = null;
  let reads = 0;

  const run = (): void => {
    lastRunMs = now;
    reads++;
  };
  const fireDueTimer = (): void => {
    if (timer && now >= timer.at) {
      timer = null;
      run();
    }
  };
  const signal = (): void => {
    fireDueTimer();
    const d = decideRefresh({
      nowMs: now,
      lastRunMs,
      timerPending: timer !== null,
      windowMs: REPO_SIGNAL_WINDOW_MS,
    });
    if (d.kind === "run") run();
    else if (d.kind === "schedule") timer = { at: now + d.dueInMs };
  };

  for (let i = 0; i < 10; i++) {
    signal();
    now += 30;
  }
  now += REPO_SIGNAL_WINDOW_MS; // let the booked trailing run come due
  fireDueTimer();

  assert.equal(
    reads,
    2,
    "ten repo-change signals inside one 500 ms window must cost the leading read plus one " +
      "trailing read — one sync dir_info per signal on the webview thread is the bug this bounds"
  );
});

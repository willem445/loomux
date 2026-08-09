// Unit tests for the audit-timeline summarize() sentences (issue #248). Before
// this, the six watch-* actions from the notification backend (#243/PR #247)
// fell to the raw-JSON default arm — never opaque, but the one action family in
// the timeline without a human sentence, and indistinguishable at a glance from
// a genuinely stuck agent (see the watchdog annotation, backend-side). This pins
// one sentence per action, matching the existing style (task-upsert, task-delete,
// prompt, …). summarize() lives in auditsummary.ts, not auditview.ts, because
// AuditView's constructor uses TS parameter properties that Node's type-stripping
// test runner can't parse — see that file's header. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { summarize, type AuditEntry } from "../src/auditsummary.ts";

function entry(action: string, detail: unknown, actor = "w-1"): AuditEntry {
  return { ts_ms: 0, actor, action, detail };
}

test("watch-register names the target, its TTL, and the watch id", () => {
  const s = summarize(
    entry("watch-register", { id: "n-3", kind: "pr_checks", target: "PR #241 checks", expires_minutes: 60 })
  );
  assert.match(s, /PR #241 checks/);
  assert.match(s, /60m/);
  assert.match(s, /n-3/);
});

test("watch-cancel names the cancelled watch id", () => {
  // Exact-equality, not /n-3/: the raw-JSON fallback for this entry
  // (`{"id":"n-3"}`) also matches /n-3/, so a substring match can't tell
  // "renders the sentence" apart from "dumps raw JSON at the user" — the
  // exact #248 bug this file exists to catch (rev-tests, PR #252).
  const s = summarize(entry("watch-cancel", { id: "n-3" }));
  assert.equal(s, "cancelled watch n-3");
});

test("watch-cleanup names the agent and every dropped watch id", () => {
  const s = summarize(entry("watch-cleanup", { agent: "w-2", ids: ["n-1", "n-2"] }, "loomux"));
  assert.match(s, /w-2/);
  assert.match(s, /n-1/);
  assert.match(s, /n-2/);
  assert.match(s, /2 watches/);
});

test("watch-cleanup uses singular wording for exactly one dropped watch", () => {
  const s = summarize(entry("watch-cleanup", { agent: "w-2", ids: ["n-1"] }, "loomux"));
  assert.match(s, /1 watch\b/);
  assert.doesNotMatch(s, /watches/);
});

test("watch-cleanup with no ids still reads as a sentence, not a dangling parenthesis", () => {
  const s = summarize(entry("watch-cleanup", { agent: "w-2", ids: [] }, "loomux"));
  assert.match(s, /0 watches/);
  assert.doesNotMatch(s, /\(\)/);
});

for (const action of ["watch-fired", "watch-expired", "watch-failed"]) {
  test(`${action} leads with the target agent and the first line of the delivered notice`, () => {
    const s = summarize(
      entry(
        action,
        {
          id: "n-3",
          kind: "pr_checks",
          agent: "w-1",
          text: "[loomux] PR #241 checks: SUCCESS — all 6 checks passed (watch n-3)",
        },
        "loomux"
      )
    );
    assert.match(s, /→ w-1:/);
    assert.match(s, /SUCCESS/);
  });
}

test("watch-fired truncates a multi-line notice text to its first line", () => {
  // The notice text is already newline-sanitized backend-side (notify.rs), but
  // the summary must not depend on that — it truncates independently, like the
  // "prompt" case above it.
  const s = summarize(entry("watch-fired", { id: "n-1", agent: "w-9", text: "line one\nline two" }, "loomux"));
  assert.match(s, /line one/);
  assert.doesNotMatch(s, /line two/);
});

test("an unrecognized action still falls back to compact detail JSON, never opaque", () => {
  // Regression guard for the bug this issue reports: a NEW action with no
  // summarize() case must still show something, not silently render blank.
  const s = summarize(entry("some-future-watch-action", { foo: "bar" }));
  assert.match(s, /foo/);
  assert.match(s, /bar/);
});

// ---------- cross-workspace channels (#271) ----------
// Same style + same exact-equality discipline as the watch-* family above: each
// pin is EXACT text, not a substring match, because the raw-JSON fallback arm
// often also contains the same substrings (the field names/values appear
// verbatim in `JSON.stringify(detail)`) — a regex match can't tell "renders the
// human sentence" apart from "dumps raw JSON at the user" (the #248 bug this
// file exists to catch). Deleting any one of these three summarize() arms
// reddens its own pin below, not just falls through silently.

test("channel-connect names every member and the channel id, exactly", () => {
  const s = summarize(
    entry(
      "channel-connect",
      {
        channel_id: "chan-3",
        members: [
          { group: "g1", agent_id: "w-1", name: "w-1", role: "worker" },
          { group: "g2", agent_id: "rev-2", name: "rev-2", role: "reviewer" },
        ],
      },
      "human"
    )
  );
  assert.equal(s, "connected w-1 (worker) ↔ rev-2 (reviewer) — channel chan-3");
});

test("channel-message names sender, recipient, channel, and the first line of the text, exactly", () => {
  const s = summarize(
    entry("channel-message", { channel_id: "chan-3", from: "w-1", to: "rev-2", text: "the API changed" }, "w-1")
  );
  assert.equal(s, "w-1 → rev-2 (channel chan-3): the API changed");
});

test("channel-message truncates a multi-line text to its first line, like prompt/watch-fired", () => {
  const s = summarize(entry("channel-message", { channel_id: "chan-1", from: "w-1", to: "w-2", text: "line one\nline two" }, "w-1"));
  assert.match(s, /line one/);
  assert.doesNotMatch(s, /line two/);
});

test("channel-disconnect below 2 remaining reads as the channel closing, exactly", () => {
  const s = summarize(entry("channel-disconnect", { channel_id: "chan-3", agent: "w-1", remaining: 0 }, "human"));
  assert.equal(s, "w-1 disconnected from channel chan-3 — channel closed");
});

test("channel-disconnect from a still-live multi-party channel names the remaining count, exactly", () => {
  const s = summarize(entry("channel-disconnect", { channel_id: "chan-3", agent: "w-1", remaining: 2 }, "human"));
  assert.equal(s, "w-1 disconnected from channel chan-3 — 2 members remaining");
});

test("channel-disconnect uses singular wording for exactly one member remaining", () => {
  const s = summarize(entry("channel-disconnect", { channel_id: "chan-3", agent: "w-1", remaining: 1 }, "human"));
  // remaining < 2 means the backend tore the channel down (mod.rs's `closed =
  // remaining.len() < 2`), so this reads as "closed", not "1 member remaining".
  assert.equal(s, "w-1 disconnected from channel chan-3 — channel closed");
});

test("channel-direction (#271 W3 addendum) names the channel and the sender swap, exactly", () => {
  const s = summarize(
    entry("channel-direction", { channel_id: "chan-3", from_sender: "w-1", to_sender: "rev-2" }, "human")
  );
  assert.equal(s, "channel chan-3: sender changed from w-1 to rev-2");
});

// #569: pause WAS the one path that discarded a payload its sender was told
// `Ok` about, and both of its audit actions fell to the raw-JSON default arm —
// a reader scanning the timeline saw a JSON blob where a work-loss event was,
// which is how the defect stayed invisible for as long as it did. Option 2
// replaced the discard with a queue admission; these lines survive in older
// timelines, so the viewer keeps rendering them and now dates them.

test("prompt-suppressed-paused reads as a discard by an OLDER build", () => {
  const s = summarize(
    entry("prompt-suppressed-paused", { to: "orch-1", text: "report: done, PR #123 is green" }, "w-2")
  );
  assert.match(s, /discarded/, "the word has to be there — this payload no longer exists");
  assert.match(s, /older loomux/, "and it must not read as something the current build does");
  assert.match(s, /orch-1/, "and the pane that never got it");
  assert.match(s, /PR #123 is green/, "and the payload, so it can be re-sent");
});

test("prompt-suppressed-paused truncates a multi-line payload to its first line", () => {
  const s = summarize(entry("prompt-suppressed-paused", { to: "w-1", text: "line one\nline two" }, "orch-1"));
  assert.match(s, /line one/);
  assert.doesNotMatch(s, /line two/);
});

test("the resume tally says whether the orchestrator was actually told", () => {
  const told = summarize(entry("pause-suppression-notice", { count: 3, delivered: true }, "loomux"));
  assert.equal(
    told,
    "3 deliveries discarded by an earlier loomux while paused — orchestrator notified on resume"
  );

  // The honesty case: "the orchestrator was told" is a claim, and this line is
  // the one that has to be right about it.
  const untold = summarize(
    entry("pause-suppression-notice", { count: 1, delivered: false, error: "no live orchestrator in this group" }, "loomux")
  );
  assert.equal(
    untold,
    "1 delivery discarded by an earlier loomux while paused — could NOT notify the orchestrator (no live orchestrator in this group); panes badged instead"
  );
});

// #569 option 2: the action a resume writes now. A held pane is work loomux
// still owes an agent, so a timeline has to show the flush starting — and has
// to distinguish "started" from "recorded but never started", which is the only
// state in which those entries sit undelivered.
test("pause-flush names the panes the resume set draining", () => {
  const s = summarize(entry("pause-flush", { panes: [5690, 5691], started: true }, "loomux"));
  assert.match(s, /flushing 2 held panes/);
  assert.match(s, /5690, 5691/, "the panes themselves, so a reader can check them");
  assert.doesNotMatch(s, /NOT started/);

  const singular = summarize(entry("pause-flush", { panes: [7], started: true }, "loomux"));
  assert.match(singular, /1 held pane \(7\)/, "singular reads as singular");

  const stalled = summarize(entry("pause-flush", { panes: [5690], started: false }, "loomux"));
  assert.match(stalled, /NOT started/, "a flush that never began must not read as one that did");
});

// #569 review B1: the audit line is the ONLY evidence this interleaving
// occurred — `ensure_drainer` itself leaves no trace a reader can see — so it
// has to render as a sentence, not fall to the raw-JSON default arm.
test("pause-race-nudge reads as a race the admission caught", () => {
  const s = summarize(entry("pause-race-nudge", { to: "orch-1", pty: 5690, id: 42 }, "loomux"));
  assert.match(s, /raced a resume/);
  assert.match(s, /orch-1/, "the pane whose delivery would otherwise have stranded");
  assert.match(s, /5690/);
  assert.match(s, /42/, "and the delivery id, so it joins to delivery-queued");
});

// #858: the lock lifecycle. Every one of these would fall to the raw-JSON
// default arm without a case, and the audit log is where "why did that build
// take 40 minutes" gets answered after the fact — a JSON blob per line is not
// an answer a human reads.
test("the lock lifecycle reads as sentences, not as detail JSON", () => {
  const taken = summarize(entry("lock-acquire", { resource: "build", note: "cargo test" }));
  assert.match(taken, /took 'build'/);
  assert.match(taken, /cargo test/, "the holder's own note is what makes a long hold legible");

  assert.match(
    summarize(entry("lock-queued", { resource: "build", position: 2 })),
    /queued for 'build' at position 2/
  );
  assert.match(summarize(entry("lock-release", { resource: "build" })), /released 'build'/);

  // loomux's own lines name the AGENT: the actor column reads "loomux", so the
  // whole point of these is which agent lost or gained a slot.
  const granted = summarize(
    entry("lock-grant", { resource: "build", agent: "w-4", waited_ms: 5 * 60_000 }, "loomux")
  );
  assert.match(granted, /'build' → w-4/);
  assert.match(granted, /waited 5 min/);

  const expired = summarize(
    entry("lock-expired", { resource: "build", agent: "w-3", held_ms: 45 * 60_000 }, "loomux")
  );
  assert.match(expired, /reclaimed from w-3/);
  assert.match(expired, /held 45 min/);
  assert.match(expired, /past its max hold/, "an overrun must not read like a crash");

  const dead = summarize(
    entry("lock-reclaim", { resource: "build", agent: "w-3", held_ms: 60_000, why: "agent-gone" }, "loomux")
  );
  assert.match(dead, /its pane is gone/, "…and a crash must not read like an overrun");

  assert.match(
    summarize(entry("lock-wait-timeout", { resource: "build", agent: "w-5", waited_ms: 60 * 60_000 }, "loomux")),
    /w-5 gave up waiting for 'build' after 60 min/
  );
});

test("a lock duration rounds up, so a short hold never reads as zero", () => {
  const s = summarize(entry("lock-expired", { resource: "b", agent: "w-1", held_ms: 40_000 }, "loomux"));
  assert.match(s, /held 1 min/, "0 min would read as a bug in the mechanism, not a short hold");
});

test("a truncated lock record degrades to '?', never to NaN", () => {
  const s = summarize(entry("lock-grant", { resource: "build", agent: "w-4" }, "loomux"));
  assert.match(s, /waited \? min/);
});

// #859 finding 10(c): the five arms the first cut added without asserting.
// Each would have fallen through to the raw-JSON default unnoticed if its
// label were misspelled — and the audit log is where a human reconstructs a
// contention after the fact, so a JSON blob per line is not an answer.
test("the idempotent and withdrawal lock actions read as sentences too", () => {
  assert.match(
    summarize(entry("lock-acquire-repeat", { resource: "build" })),
    /already held 'build' \(no change\)/
  );
  assert.match(
    summarize(entry("lock-queued-repeat", { resource: "build", position: 3 })),
    /already queued for 'build' at position 3/
  );
  assert.match(
    summarize(entry("lock-queue-cancel", { resource: "build", position: 2 })),
    /withdrew from the 'build' queue \(was position 2\)/
  );
  assert.match(
    summarize(entry("lock-wait-cleanup", { resource: "build", agent: "w-7", why: "agent-gone" }, "loomux")),
    /w-7 left the 'build' queue — its pane is gone/
  );
  assert.match(
    summarize(entry("lock-undeclared", { resource: "build", holders: ["w-1"], queued: [] }, "loomux")),
    /'build' is no longer declared in \.loomux\/workflow\.yml/
  );
});

test("every lock action has its own case — none falls through to raw JSON", () => {
  // The default arm dumps compact detail JSON, so a misspelled label is silent.
  // This is the cheap tripwire for that: a rendered sentence never starts with
  // the `{` the fallback would produce.
  const actions = [
    "lock-acquire", "lock-acquire-repeat", "lock-queued", "lock-queued-repeat",
    "lock-release", "lock-queue-cancel", "lock-grant", "lock-expired",
    "lock-reclaim", "lock-wait-timeout", "lock-wait-cleanup", "lock-undeclared",
  ];
  for (const action of actions) {
    const s = summarize(entry(action, { resource: "build", agent: "w-1", position: 1, held_ms: 60_000, waited_ms: 60_000 }, "loomux"));
    assert.ok(s.length > 0, `${action} rendered nothing`);
    assert.ok(!s.startsWith("{"), `${action} fell through to the raw-JSON default: ${s}`);
  }
});

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

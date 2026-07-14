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
  const s = summarize(entry("watch-cancel", { id: "n-3" }));
  assert.match(s, /n-3/);
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

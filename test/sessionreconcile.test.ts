// Pure post-start session-id reconciliation (#440 D1 option B) + the D2
// dormant-resume candidate. Pins the refusal behavior: a matcher that adopts
// too eagerly is worse than the bug it's fixing (see sessionreconcile.ts's
// module comment) — so most of these cases are refusals, not matches.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  planSessionAdoption,
  dormantResumeCandidate,
  type ReconcilePane,
  type SessionRecord,
} from "../src/sessionreconcile.ts";

const session = (over: Partial<SessionRecord>): SessionRecord => ({
  id: "s1",
  cli: "claude",
  cwd: "C:\\repo",
  modifiedMs: 1_000,
  title: "some task",
  resumeCommand: "claude --resume s1",
  ...over,
});

const pane = (over: Partial<ReconcilePane>): ReconcilePane => ({
  key: "p1",
  cli: "claude",
  cwd: "C:\\repo",
  spawnedAtMs: 500,
  ...over,
});

// ---------- planSessionAdoption ----------

test("a unique matching session is adopted", () => {
  const out = planSessionAdoption([pane({})], [session({})], new Set());
  assert.deepEqual(out, [{ key: "p1", sessionId: "s1" }]);
});

test("no matching session -> no adoption (empty candidate list)", () => {
  const out = planSessionAdoption([pane({})], [], new Set());
  assert.deepEqual(out, []);
});

test("a session modified BEFORE the pane spawned is excluded (can't be this pane's)", () => {
  const out = planSessionAdoption(
    [pane({ spawnedAtMs: 10_000 })],
    [session({ modifiedMs: 1_000 })], // well before spawn, outside slack
    new Set()
  );
  assert.deepEqual(out, []);
});

test("a session modified just before spawn, within clock-skew slack, still matches", () => {
  const out = planSessionAdoption(
    [pane({ spawnedAtMs: 10_000 })],
    [session({ modifiedMs: 9_000 })], // 1s before spawn — within the 5s slack
    new Set()
  );
  assert.deepEqual(out, [{ key: "p1", sessionId: "s1" }]);
});

test("an already-claimed session id is excluded, even if it otherwise matches", () => {
  const out = planSessionAdoption([pane({})], [session({})], new Set(["s1"]));
  assert.deepEqual(out, []);
});

test("a pane with two candidate sessions refuses to adopt either (per-pane ambiguity)", () => {
  const out = planSessionAdoption(
    [pane({})],
    [session({ id: "s1" }), session({ id: "s2" })],
    new Set()
  );
  assert.deepEqual(out, []);
});

test("one session matching two panes refuses to adopt for EITHER pane (cross-pane ambiguity)", () => {
  const out = planSessionAdoption(
    [pane({ key: "p1" }), pane({ key: "p2" })],
    [session({ id: "s1" })],
    new Set()
  );
  assert.deepEqual(out, []);
});

test("ambiguity for one pane doesn't block an unrelated pane's unique match", () => {
  const out = planSessionAdoption(
    [pane({ key: "contested" }), pane({ key: "clean", cwd: "C:\\other" })],
    [
      session({ id: "s1", cwd: "C:\\repo" }),
      session({ id: "s2", cwd: "C:\\repo" }), // both match "contested" -> refuse
      session({ id: "s3", cwd: "C:\\other" }), // unique for "clean" -> adopt
    ],
    new Set()
  );
  assert.deepEqual(out, [{ key: "clean", sessionId: "s3" }]);
});

test("cwd matching is case- and separator-insensitive (Windows)", () => {
  const out = planSessionAdoption(
    [pane({ cwd: "C:\\Repo\\" })],
    [session({ cwd: "c:/repo" })],
    new Set()
  );
  assert.deepEqual(out, [{ key: "p1", sessionId: "s1" }]);
});

test("a differently-cased cwd that is genuinely a different folder does not match", () => {
  const out = planSessionAdoption(
    [pane({ cwd: "C:\\repo-a" })],
    [session({ cwd: "C:\\repo-b" })],
    new Set()
  );
  assert.deepEqual(out, []);
});

test("claude and copilot never cross-match, even in the same folder", () => {
  const out = planSessionAdoption(
    [pane({ cli: "claude" })],
    [session({ cli: "copilot" })],
    new Set()
  );
  assert.deepEqual(out, []);
});

test("multiple panes each get their own unique match independently", () => {
  const out = planSessionAdoption(
    [pane({ key: "p1", cwd: "C:\\a" }), pane({ key: "p2", cwd: "C:\\b" })],
    [session({ id: "s1", cwd: "C:\\a" }), session({ id: "s2", cwd: "C:\\b" })],
    new Set()
  );
  assert.deepEqual(
    out.sort((a, b) => a.key.localeCompare(b.key)),
    [
      { key: "p1", sessionId: "s1" },
      { key: "p2", sessionId: "s2" },
    ]
  );
});

// ---------- dormantResumeCandidate ----------

test("dormantResumeCandidate finds the newest matching session in the folder", () => {
  const out = dormantResumeCandidate(
    { cli: "claude", cwd: "C:\\repo" },
    [session({ id: "old", modifiedMs: 100 }), session({ id: "new", modifiedMs: 900 })]
  );
  assert.equal(out?.id, "new");
});

test("dormantResumeCandidate is null with no cwd recorded", () => {
  const out = dormantResumeCandidate({ cli: "claude", cwd: null }, [session({})]);
  assert.equal(out, null);
});

test("dormantResumeCandidate is null with no matching session", () => {
  const out = dormantResumeCandidate({ cli: "claude", cwd: "C:\\nothing-here" }, [session({})]);
  assert.equal(out, null);
});

test("dormantResumeCandidate separates claude from copilot", () => {
  const out = dormantResumeCandidate(
    { cli: "copilot", cwd: "C:\\repo" },
    [session({ cli: "claude" })]
  );
  assert.equal(out, null);
});

test("dormantResumeCandidate is case/separator-insensitive on cwd, same as the adoption matcher", () => {
  const out = dormantResumeCandidate(
    { cli: "claude", cwd: "c:/REPO/" },
    [session({ cwd: "C:\\repo" })]
  );
  assert.equal(out?.id, "s1");
});

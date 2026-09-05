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
  eligibleSinceMs: 500,
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

test("a session modified BEFORE the pane's eligibleSinceMs is excluded (can't be this pane's)", () => {
  const out = planSessionAdoption(
    [pane({ eligibleSinceMs: 10_000 })],
    [session({ modifiedMs: 1_000 })], // well before eligibility
    new Set()
  );
  assert.deepEqual(out, []);
});

test("a session modified even 1ms before eligibleSinceMs is excluded — NO slack of any kind (review round 2, B1)", () => {
  // Was previously admitted by a 5s clock-skew slack; the slack is gone
  // entirely (not shrunk) because it could only ever admit a session that
  // predates — and so cannot belong to — this pane.
  const out = planSessionAdoption(
    [pane({ eligibleSinceMs: 10_000 })],
    [session({ modifiedMs: 9_999 })],
    new Set()
  );
  assert.deepEqual(out, []);
});

test("a session modified exactly AT eligibleSinceMs matches (the boundary is inclusive, not exclusive)", () => {
  const out = planSessionAdoption(
    [pane({ eligibleSinceMs: 10_000 })],
    [session({ modifiedMs: 10_000 })],
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
test("a pi pane adopts a pi session, and never one of another CLI's (#2126)", () => {
  // pi joined `Cli` with #2126 P2. Leaving it out would have made every pi pane
  // unadoptable while the sidebar listed the very session it should have
  // adopted — the same silent hole opencode's row was added to close.
  const out = planSessionAdoption(
    [pane({ cli: "pi" })],
    [session({ id: "pi-1", cli: "pi", resumeCommand: "pi --session pi-1" })],
    new Set()
  );
  assert.deepEqual(out, [{ key: "p1", sessionId: "pi-1" }]);

  // THE FIXTURE THAT MAKES THAT FAIL-ABLE: the two candidates COLLIDE on
  // everything the matcher reads except the CLI — same cwd, same window, both
  // eligible — so a matcher that ignored `cli` would see an ambiguity and refuse
  // BOTH, and one that crossed CLIs would adopt the wrong id. Only a matcher
  // that separates them by CLI returns exactly this.
  const both = planSessionAdoption(
    [pane({ key: "pane-pi", cli: "pi" }), pane({ key: "pane-claude", cli: "claude" })],
    [session({ id: "pi-1", cli: "pi" }), session({ id: "cl-1", cli: "claude" })],
    new Set()
  );
  assert.deepEqual(
    [...both].sort((a, b) => a.key.localeCompare(b.key)),
    [
      { key: "pane-claude", sessionId: "cl-1" },
      { key: "pane-pi", sessionId: "pi-1" },
    ]
  );

  // And a pi pane with only another CLI's session in the folder adopts nothing.
  assert.deepEqual(
    planSessionAdoption([pane({ cli: "pi" })], [session({ id: "cl-1", cli: "claude" })], new Set()),
    []
  );
});

test("a codex pane adopts a codex session, and never one of another CLI's (#2515 C2)", () => {
  // codex joined `Cli` with #2515 C2, and the hole it would otherwise leave is
  // the one #2126 and #722 each left before it: every codex pane unadoptable
  // while the sidebar lists the very session it should adopt. The pi test above
  // is kept intact — this is an ADDITION, since a per-CLI property is only
  // witnessed by the CLI it is about.
  const out = planSessionAdoption(
    [pane({ cli: "codex" })],
    [
      session({
        id: "019ff1a2-b3c4-7d5e-8f60-112233445566",
        cli: "codex",
        resumeCommand: "codex resume 019ff1a2-b3c4-7d5e-8f60-112233445566",
      }),
    ],
    new Set()
  );
  assert.deepEqual(out, [{ key: "p1", sessionId: "019ff1a2-b3c4-7d5e-8f60-112233445566" }]);

  // THE FIXTURE THAT MAKES THAT FAIL-ABLE, copied in shape from the pi test
  // above: the two candidates COLLIDE on everything the matcher reads except
  // the CLI, so a matcher ignoring `cli` sees an ambiguity and refuses both,
  // and one that crossed CLIs adopts the wrong id.
  const both = planSessionAdoption(
    [pane({ key: "pane-codex", cli: "codex" }), pane({ key: "pane-claude", cli: "claude" })],
    [session({ id: "cx-1", cli: "codex" }), session({ id: "cl-1", cli: "claude" })],
    new Set()
  );
  assert.deepEqual(
    [...both].sort((a, b) => a.key.localeCompare(b.key)),
    [
      { key: "pane-claude", sessionId: "cl-1" },
      { key: "pane-codex", sessionId: "cx-1" },
    ]
  );

  // And a codex pane with only another CLI's session in the folder adopts
  // nothing. The negative arm matters more for codex than for pi: a codex
  // thread id is a bare UUID, indistinguishable by SHAPE from a claude session
  // id, so nothing but the `cli` field can keep the two apart.
  assert.deepEqual(
    planSessionAdoption([pane({ cli: "codex" })], [session({ id: "cl-1", cli: "claude" })], new Set()),
    []
  );
});

test("an unknown workspace never matches a pane, on either side (#2515 C2 review premortem 1)", () => {
  // A session whose transcript records no cwd carries `cwd: ""` — a torn
  // header, and, since C2, EVERY codex rollout older than a week, whose
  // `.jsonl.zst` this project deliberately does not decompress. `normalizeCwd`
  // maps that to `""`, which would equal a pane whose own cwd is `""`.
  //
  // The fixture COLLIDES on purpose: same cli, same (empty) cwd, eligible,
  // unclaimed — every field the matcher reads agrees, so this fails against any
  // implementation that does not special-case the empty string, and holds only
  // for one that refuses it.
  assert.deepEqual(
    planSessionAdoption(
      [pane({ cli: "codex", cwd: "" })],
      [session({ id: "cx-unknown", cli: "codex", cwd: "" })],
      new Set()
    ),
    [],
    "an unknown workspace on both sides is two unknowns, not a match"
  );

  // Either side alone is equally refused — the guard reads the SESSION's cwd,
  // so this pins that a real pane cannot adopt an unknown-workspace row.
  assert.deepEqual(
    planSessionAdoption(
      [pane({ cli: "codex", cwd: "C:\\repo" })],
      [session({ id: "cx-unknown", cli: "codex", cwd: "" })],
      new Set()
    ),
    []
  );

  // POSITIVE CONTROL, and the reason the two empties above are not vacuous: the
  // SAME pane and the SAME session adopt normally once the session records a
  // real directory. Without this, a matcher that refused everything would pass.
  assert.deepEqual(
    planSessionAdoption(
      [pane({ cli: "codex", cwd: "C:\\repo" })],
      [session({ id: "cx-known", cli: "codex", cwd: "C:\\repo" })],
      new Set()
    ),
    [{ key: "p1", sessionId: "cx-known" }]
  );
});

test("dormantResumeCandidate refuses an unknown workspace, on either side (#2515 C2 review N2)", () => {
  // THE SIBLING of the premortem-1 guard, and the reason it needed its own
  // test: round 1 fixed `planSessionAdoption`'s comparison and left this one,
  // which does the same `normalizeCwd(s.cwd) === normalizeCwd(cwd)` sixty lines
  // down. Nothing covered this function against an unknown-cwd session at all.
  //
  // A session with no recorded workspace is never the answer, even though the
  // pane here has a perfectly ordinary cwd.
  assert.equal(
    dormantResumeCandidate({ cli: "codex", cwd: "C:\\repo" }, [
      session({ id: "cx-unknown", cli: "codex", cwd: "" }),
    ]),
    null
  );

  // AND THE HALF THE RAW-STRING GUARD MISSED, which is the actual defect:
  // `normalizeCwd` collapses `/`, `\`, `//` and a run of spaces to `""`, so
  // each of these is TRUTHY and passed `!record.cwd` — then matched an
  // unknown-workspace session by `"" === ""`. Every one of them is asserted,
  // because a guard that handled only the empty string would pass a test that
  // used only the empty string.
  for (const paneCwd of ["/", "\\", "//", "   ", "\\\\"]) {
    assert.equal(
      dormantResumeCandidate({ cli: "codex", cwd: paneCwd }, [
        session({ id: "cx-unknown", cli: "codex", cwd: "" }),
      ]),
      null,
      `a pane cwd of ${JSON.stringify(paneCwd)} normalizes to empty and must not match`
    );
  }

  // POSITIVE CONTROL, and what stops every assertion above from passing
  // against a function that returns null unconditionally: a real pane cwd and a
  // real session cwd still produce the candidate.
  assert.equal(
    dormantResumeCandidate({ cli: "codex", cwd: "C:\\repo" }, [
      session({ id: "cx-known", cli: "codex", cwd: "C:\\repo" }),
    ])?.id,
    "cx-known"
  );
  // ...and a drive root still works, since `normalizeCwd("C:\\")` is `"c:"` and
  // not empty — the guard must refuse what normalizes to nothing, not every
  // short path.
  assert.equal(
    dormantResumeCandidate({ cli: "codex", cwd: "C:\\" }, [
      session({ id: "cx-root", cli: "codex", cwd: "C:\\" }),
    ])?.id,
    "cx-root"
  );
});

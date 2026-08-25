// Unit tests for the Orchestrations section's pure model (#1563 slice B).
// Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { orchRows, type RecordedOrchestration } from "../src/orchlist.ts";

const rec = (over: Partial<RecordedOrchestration> = {}): RecordedOrchestration => ({
  group_id: "loomux-aaaa1111",
  repo: "C:/Projects/loomux",
  cli: "opencode",
  session_id: "ses_1508a391dffext5Xb0UUF2UDjk",
  group_live: false,
  resumable: true,
  last_seen_ms: 1_000,
  ...over,
});

// ---------------------------------------------------------------------------
// The button
// ---------------------------------------------------------------------------

test("a resumable opencode orchestrator offers Resume, carrying its session id", () => {
  const [row] = orchRows([rec()]);
  assert.equal(row.state, "resumable");
  assert.equal(row.canResume, true);
  assert.equal(row.sessionId, "ses_1508a391dffext5Xb0UUF2UDjk");
  assert.match(row.detail, /opencode/);
});

test("a row with no learned session id says so instead of offering a dead button", () => {
  const [row] = orchRows([rec({ session_id: null, resumable: false })]);
  assert.equal(row.state, "unidentified");
  assert.equal(row.canResume, false);
  assert.equal(row.sessionId, null);
  assert.match(row.detail, /not yet identified/i);
  // The wording must not be the lost-session wording — these are different
  // problems with different answers (wait, vs. start a fresh orchestrator).
  assert.doesNotMatch(row.detail, /no longer in the/i);
});

test("resumable=false gets its own copy, distinct from the never-identified one", () => {
  const [row] = orchRows([rec({ resumable: false })]);
  assert.equal(row.state, "lost");
  assert.equal(row.canResume, false);
  assert.match(row.detail, /no longer in the opencode session store/i);
  assert.doesNotMatch(row.detail, /not yet identified/i);
});

test("a live group offers no Resume — the backend refuses one, so the row must not promise it", () => {
  // `resume_recorded_session`: "group … already has a live orchestrator —
  // focus its pane instead". `resumable: true` here is deliberate: the row is
  // suppressed by liveness, not by the store lookup, so a future change that
  // dropped the liveness check would redden here.
  const [row] = orchRows([rec({ group_live: true, resumable: true })]);
  assert.equal(row.state, "live");
  assert.equal(row.canResume, false);
  assert.match(row.detail, /focus its orchestrator pane/i);
});

test("a damaged group record is listed, with no CLI guessed and no button", () => {
  const [row] = orchRows([rec({ cli: "", repo: null, resumable: true })]);
  assert.equal(row.state, "damaged");
  assert.equal(row.canResume, false);
  assert.equal(row.cli, "unknown CLI");
  // The DISPLAY label carries a space, which is why the renderer must not key
  // a class off it (#1568 review N4): `session-badge unknown CLI` is two junk
  // classes. `cliKey` is the token, and an unreadable record has none.
  assert.equal(row.cliKey, "");
  // Falls back to the group id rather than rendering a blank title.
  assert.equal(row.title, "loomux-aaaa1111");
  assert.doesNotMatch(row.detail, /claude|copilot|opencode/i);
});

test("cliKey is the raw wire value, never the display label", () => {
  // Trimmed but not relabelled — a class name is a token, not prose. The two
  // fields must not be the same string in the damaged case (asserted above),
  // and must agree in the ordinary one.
  const [ok] = orchRows([rec({ cli: "opencode" })]);
  assert.equal(ok.cliKey, "opencode");
  assert.equal(ok.cli, "opencode");
  const [padded] = orchRows([rec({ cli: "  claude  " })]);
  assert.equal(padded.cliKey, "claude", "a token never carries surrounding whitespace");
  assert.doesNotMatch(padded.cliKey, /\s/, "a cliKey must never contain whitespace");

  // The INTERIOR case, which `trim()` alone does not close and which the
  // surrounding-whitespace assertion above can never catch: one space inside
  // the value would splice the class attribute into two class names, which is
  // the whole hazard `cliKey` exists to avoid. No key at all is the honest
  // answer — the badge renders uncoloured but still labelled.
  const [spaced] = orchRows([rec({ cli: "my cli" })]);
  assert.equal(spaced.cliKey, "", "a value that cannot be one token yields no key");
  assert.equal(spaced.cli, "my cli", "the DISPLAY label still shows it verbatim");
});

test("canResume is never true without a session id, whatever the backend said", () => {
  // Defence in depth against a wire shape that contradicts itself: a null id
  // with resumable=true must still not produce a button, because there is
  // nothing to pass to resumeOrchSession.
  const rows = orchRows([rec({ session_id: null, resumable: true })]);
  assert.equal(rows[0].canResume, false);
  assert.equal(rows.filter((r) => r.canResume && r.sessionId === null).length, 0);
});

// ---------------------------------------------------------------------------
// Ordering
// ---------------------------------------------------------------------------

test("live groups come first, then most recently active", () => {
  // The ids are chosen so ALPHABETICAL order CONTRADICTS timestamp order
  // within each liveness class: sorted by name alone this is
  // ["alpha", "bravo", "yankee", "zeta"], and by liveness+name it is
  // ["alpha", "zeta", "bravo", "yankee"]  neither is the expected answer.
  // A fixture whose names happen to agree with its timestamps cannot fail
  // when the last_seen key is deleted, and this one did until a mutation
  // round found the key unpinned.
  const rows = orchRows([
    rec({ group_id: "bravo", last_seen_ms: 10 }),
    rec({ group_id: "alpha", group_live: true, last_seen_ms: 1 }),
    rec({ group_id: "yankee", last_seen_ms: 500 }),
    rec({ group_id: "zeta", group_live: true, last_seen_ms: 900 }),
  ]);
  assert.deepEqual(
    rows.map((r) => r.groupId),
    ["zeta", "alpha", "yankee", "bravo"]
  );
  // Both keys are load-bearing, and each control names the order the OTHER
  // key alone would produce.
  assert.notDeepEqual(
    rows.map((r) => r.groupId),
    ["zeta", "yankee", "alpha", "bravo"],
    "last_seen alone would lift the newest dormant group above a live one"
  );
  assert.notDeepEqual(
    rows.map((r) => r.groupId),
    ["alpha", "zeta", "bravo", "yankee"],
    "liveness plus the name tiebreak alone would ignore last_seen entirely"
  );
});

test("groups with identical timestamps keep a stable order across refreshes", () => {
  // Two reads of the same unchanged data must not reshuffle the list — a row
  // that moves under the cursor is a misclick, and this list's clicks resume
  // whole orchestrations.
  const input = [
    rec({ group_id: "zebra", last_seen_ms: 7 }),
    rec({ group_id: "alpha", last_seen_ms: 7 }),
  ];
  assert.deepEqual(orchRows(input).map((r) => r.groupId), ["alpha", "zebra"]);
  assert.deepEqual(orchRows(input.slice().reverse()).map((r) => r.groupId), ["alpha", "zebra"]);
});

test("orchRows does not mutate or reorder the array it was handed", () => {
  const input = [rec({ group_id: "b", last_seen_ms: 1 }), rec({ group_id: "a", last_seen_ms: 9 })];
  orchRows(input);
  assert.deepEqual(input.map((r) => r.group_id), ["b", "a"]);
});

// ---------------------------------------------------------------------------
// Filtering
// ---------------------------------------------------------------------------

test("the filter matches group id, repo path and CLI, case-insensitively", () => {
  const list = [
    rec({ group_id: "loomux-1", repo: "C:/Projects/loomux", cli: "opencode" }),
    rec({ group_id: "widget-2", repo: "C:/Projects/widget", cli: "claude" }),
  ];
  assert.deepEqual(orchRows(list, "WIDGET").map((r) => r.groupId), ["widget-2"]);
  assert.deepEqual(orchRows(list, "opencode").map((r) => r.groupId), ["loomux-1"]);
  assert.deepEqual(orchRows(list, "Projects").map((r) => r.groupId), ["loomux-1", "widget-2"]);
  assert.deepEqual(orchRows(list, "   ").length, 2, "a blank filter is no filter");
});

test("a damaged row is still findable by its group id", () => {
  const list = [rec({ group_id: "torn-group", repo: null, cli: "" })];
  assert.deepEqual(orchRows(list, "torn").map((r) => r.groupId), ["torn-group"]);
});

// ---------------------------------------------------------------------------
// Title
// ---------------------------------------------------------------------------

test("the title is the repo's own basename, on either separator", () => {
  assert.equal(orchRows([rec({ repo: "C:/Projects/loomux" })])[0].title, "loomux");
  assert.equal(orchRows([rec({ repo: "C:\\Projects\\loomux" })])[0].title, "loomux");
  assert.equal(orchRows([rec({ repo: "C:/Projects/loomux/" })])[0].title, "loomux");
});

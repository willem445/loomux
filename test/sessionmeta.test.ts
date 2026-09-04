// Unit tests for the pure session-browser metadata formatting (#1). Run with
// `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  taskSummary,
  repoBranchLine,
  notesChipLabel,
  paneNameLine,
  prLabel,
  restoredPaneName,
  sessionBadgeLabel,
} from "../src/sessionmeta.ts";
import type { SessionRoleInfo } from "../src/orchestration.ts";

const role = (over: Partial<SessionRoleInfo> = {}): SessionRoleInfo => ({
  session_id: "s-1",
  group_id: "g-1",
  role: "worker",
  agent_name: "builder",
  group_live: true,
  task: "",
  branch: null,
  repo: null,
  pr: null,
  ...over,
});

test("taskSummary returns the task text verbatim when short", () => {
  assert.equal(taskSummary(role({ task: "implement the thing" })), "implement the thing");
});

test("taskSummary truncates a long task with an ellipsis, not a hard cut", () => {
  const long = "x".repeat(200);
  const out = taskSummary(role({ task: long }));
  assert.ok(out && out.length <= 140, `expected <=140 chars, got ${out?.length}`);
  assert.ok(out?.endsWith("…"), "truncated text must end with an ellipsis marker");
});

test("taskSummary is null for an empty/whitespace task, not an empty string", () => {
  assert.equal(taskSummary(role({ task: "" })), null);
  assert.equal(taskSummary(role({ task: "   " })), null);
});

test("taskSummary is null when no role is recorded at all", () => {
  assert.equal(taskSummary(undefined), null);
});

test("repoBranchLine combines repo and branch when both are known", () => {
  assert.equal(
    repoBranchLine(role({ repo: "C:/Projects/loomux", branch: "feat/thing" })),
    "loomux @ feat/thing"
  );
});

test("repoBranchLine shows branch alone when repo is unknown", () => {
  assert.equal(repoBranchLine(role({ branch: "feat/thing" })), "feat/thing");
});

test("repoBranchLine shows repo alone when branch is unknown (the orchestrator's case)", () => {
  assert.equal(repoBranchLine(role({ repo: "C:/Projects/loomux" })), "loomux");
});

test("repoBranchLine is null when neither is known — never a fabricated placeholder", () => {
  assert.equal(repoBranchLine(role()), null);
  assert.equal(repoBranchLine(undefined), null);
});

test("prLabel normalizes a bare PR number to #N", () => {
  assert.equal(prLabel(role({ pr: "42" })), "#42");
});

test("prLabel passes through an already-shaped PR reference verbatim", () => {
  assert.equal(prLabel(role({ pr: "#42" })), "#42");
  assert.equal(prLabel(role({ pr: "https://github.com/o/r/pull/42" })), "https://github.com/o/r/pull/42");
});

test("prLabel is null when no PR is recorded yet", () => {
  assert.equal(prLabel(role({ pr: null })), null);
  assert.equal(prLabel(undefined), null);
});

// ---------- #722: the restored pane's name ----------

test("restoredPaneName names the CLI the row actually came from", () => {
  assert.equal(restoredPaneName("claude", "fix the login bug"), "claude · fix the login bug");
  assert.equal(restoredPaneName("copilot", "fix the login bug"), "copilot · fix the login bug");
  // The one this replaced a two-arm ternary for: with `source` read instead of
  // branched on, a third scanner's rows are named correctly the day they
  // arrive rather than inheriting whichever CLI sat in the else-branch — which
  // is how an opencode session would have opened a pane called "copilot · …".
  assert.equal(restoredPaneName("opencode", "fix the login bug"), "opencode · fix the login bug");
});

test("restoredPaneName cuts a long title with an ellipsis, keeping the CLI prefix intact", () => {
  const out = restoredPaneName("opencode", "y".repeat(200));
  assert.ok(out.startsWith("opencode · "), "the CLI must survive the truncation");
  assert.ok(out.endsWith("…"), "a cut title must say it was cut");
  assert.equal(out, `opencode · ${"y".repeat(34)}…`);
});

test("restoredPaneName leaves a title that fits exactly alone — no ellipsis on a non-cut", () => {
  const exact = "z".repeat(34);
  assert.equal(restoredPaneName("claude", exact), `claude · ${exact}`);
});

// ---- paneNameLine (#2116) ----

test("a name the human chose is shown", () => {
  assert.equal(paneNameLine("w: #2116 notes", "Fix the login bug", "claude"), "w: #2116 notes");
});

test("nothing recorded shows no line — the row's own title IS the fallback", () => {
  // Never a placeholder and never an empty line: the caller renders nothing at
  // all, and the title the row already shows is what the human reads.
  assert.equal(paneNameLine(undefined, "Fix the login bug", "claude"), null);
  assert.equal(paneNameLine("", "Fix the login bug", "claude"), null);
  assert.equal(paneNameLine("   ", "Fix the login bug", "claude"), null);
});

test("a name equal to the title is not printed twice", () => {
  assert.equal(paneNameLine("Fix the login bug", "Fix the login bug", "claude"), null);
  assert.equal(paneNameLine("  Fix the login bug  ", "Fix the login bug", "claude"), null);
});

test("the auto-name a restore MINTS is not a name the human chose", () => {
  // The row this matters most for is the commonest one on the page: a pane
  // opened by clicking this very list is auto-named `<cli> · <title>`, so
  // without this clause every such row grows a second line restating its own
  // title with a CLI prefix.
  //
  // Built by CALLING restoredPaneName rather than by spelling the format out,
  // so a change to the auto-name cannot leave this test asserting the old one
  // while the rows go noisy.
  const title = "Fix the login bug";
  assert.equal(paneNameLine(restoredPaneName("claude", title), title, "claude"), null);
  // Including the truncating case, where the auto-name is not a prefix-plus-
  // title at all.
  const long = "y".repeat(200);
  assert.equal(paneNameLine(restoredPaneName("opencode", long), long, "opencode"), null);
});

test("the auto-name of a DIFFERENT cli is still a name worth showing", () => {
  // The negative control for the clause above: it must suppress THIS row's
  // auto-name, not every string that happens to look like one. A pane named
  // after another CLI is a fact about the pane, not noise.
  const title = "Fix the login bug";
  assert.equal(
    paneNameLine(restoredPaneName("copilot", title), title, "claude"),
    `copilot · ${title}`
  );
});

test("a rename that only changes case is a rename", () => {
  // This is a report of what the human wrote, not a guess at what they meant.
  assert.equal(paneNameLine("WORKER", "worker", "claude"), "WORKER");
});

test("sessionBadgeLabel names the row's own CLI, whichever it is", () => {
  assert.equal(sessionBadgeLabel("claude"), "CLAUDE");
  assert.equal(sessionBadgeLabel("copilot"), "COPILOT");
  // The sidebar's own version of restoredPaneName's bug: a two-arm ternary
  // labelled an opencode row COPILOT — the badge asserting one CLI while the
  // resume command underneath it named another.
  assert.equal(sessionBadgeLabel("opencode"), "OPENCODE");
});
test("a fourth source names itself, on the badge and on the restored pane (#2126)", () => {
  // Both functions read the row's `source` rather than branching on it, so a new
  // scanner is named correctly on ARRIVAL. This test exists because the shape
  // they replaced — `s.source === "claude" ? … : "COPILOT"` — was correct only
  // while there were exactly two, and #2126 P2 found the LAST copy of it still
  // live in main.ts's dormant card.
  assert.equal(sessionBadgeLabel("pi"), "PI");
  assert.equal(restoredPaneName("pi", "fix the login bug"), "pi · fix the login bug");

  // The property, not the row: every source the wire can carry labels itself,
  // and no two collapse onto one label.
  //
  // THIS LIST IS HAND-MAINTAINED, and saying otherwise would be the claim this
  // very test exists to police. An earlier version of this comment said it was
  // "a loop over the union's own members, so a fifth source has to be added
  // HERE" — it is not: `sources` is a plain string array, and nothing makes a
  // new `SessionSource` appear in it. Typing it `SessionSource[]` would not fix
  // that either, in two ways at once: a union-typed array accepts any SUBSET
  // (the same hole `SOLO_MCP_CLIS` had before it became a `Record`), and the
  // root `tsconfig.json` is `"include": ["src"]`, so `tsc --noEmit` never reads
  // this file at all.
  //
  // What actually keeps the set honest lives in `src/`: `sessionsource.ts` is
  // the single definition, and `main.ts`'s `toRecords` makes `pty.ts` and
  // `sessionreconcile.ts` disagreeing a compile error. A fifth source that
  // reaches the UI without reaching this array costs this test its coverage of
  // that source — it does not fail. Stated so a reader knows which half is
  // enforced.
  const sources = ["claude", "copilot", "opencode", "pi"];
  const labels = sources.map(sessionBadgeLabel);
  assert.deepEqual(labels, ["CLAUDE", "COPILOT", "OPENCODE", "PI"]);
  assert.equal(new Set(labels).size, sources.length, "two sources share one badge label");
});

// ---- notesChipLabel (#2116 slice E2) ----

test("an unread notes file states no count at all", () => {
  // THE ONE THIS FUNCTION EXISTS FOR. `SessionLogStore.notesCount` answers 0
  // for "this session has no notes" AND for "nobody has read the file yet",
  // and its own doc forbids collapsing the two. A chip that printed the count
  // unconditionally would tell a human a session they wrote three notes on has
  // none — the row asserting an absence it cannot know.
  const chip = notesChipLabel(0, false);
  assert.equal(chip.text, "");
  assert.equal(chip.hasNotes, false);
  // And it must not say the absence in WORDS either, which is the same claim
  // one layer over. Pinned as a `doesNotMatch` rather than by quoting the
  // sentence, so a rewording cannot quietly reintroduce it.
  assert.doesNotMatch(chip.title, /\bno notes\b/i);
  // Positive control for the two assertions above, which are both about what
  // is ABSENT: the chip is still rendered, and its tooltip still says what it
  // is for. An empty title would pass every check above and ship a bare glyph.
  assert.match(chip.title, /notes about this session/i);
});

test("an unread store states no count even when a stale count is passed", () => {
  // The caller reads the count and `loaded` separately, so nothing stops a
  // non-zero count arriving with `loaded` false. `loaded` decides, not the
  // number: a count off an unread store is not a number about this session.
  const chip = notesChipLabel(7, false);
  assert.equal(chip.text, "");
  assert.equal(chip.hasNotes, false);
});

test("a read store with no notes shows no number, and says so in the tooltip", () => {
  // A "0" on every row is a mark that means nothing. The tooltip is where the
  // absence is stated — and it names the action, so the chip is an affordance
  // and not a mystery glyph.
  const chip = notesChipLabel(0, true);
  assert.equal(chip.text, "");
  assert.equal(chip.hasNotes, false);
  assert.match(chip.title, /no notes/i);
  assert.match(chip.title, /write one/i);
});

test("a counted session shows the count, and the tooltip agrees with it", () => {
  const one = notesChipLabel(1, true);
  assert.equal(one.text, "1");
  assert.equal(one.hasNotes, true);
  assert.equal(one.title, "1 note about this session");

  const many = notesChipLabel(4, true);
  assert.equal(many.text, "4");
  assert.equal(many.hasNotes, true);
  assert.equal(many.title, "4 notes about this session");
});

test("the singular is used for exactly one, and the plural for everything else", () => {
  // Pinned as a property over a range rather than on the two literals above,
  // so a `count < 2` or `count !== 1` mix-up on the boundary reddens here
  // rather than reading fine on the two cases someone happened to write down.
  for (const n of [1, 2, 3, 11, 500]) {
    const { title } = notesChipLabel(n, true);
    assert.equal(/\bnotes\b/.test(title), n !== 1, `plural for ${n}`);
    assert.equal(/\b1 note\b/.test(title), n === 1, `singular for ${n}`);
  }
});

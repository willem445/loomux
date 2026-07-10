// Unit tests for the git-view worktree selector's pure logic (#208): parsing
// `git worktree list --porcelain` (including detached, bare, locked, prunable
// records) and resolving which worktree the view is pointed at — in particular
// the fail-soft back to primary when a selected worktree has been pruned. Run
// with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  parseWorktrees,
  resolveSelection,
  primaryWorktree,
  findWorktree,
  worktreeLabel,
  normalizePath,
} from "../src/gitworktree.ts";

// A representative `git worktree list --porcelain` dump: main on a branch, a
// linked worktree on a slashed branch, a detached one, and a locked one.
const SAMPLE = [
  "worktree C:/Projects/loomux",
  "HEAD 1111111111111111111111111111111111111111",
  "branch refs/heads/main",
  "",
  "worktree C:/Projects/loomux-worktrees/feat/208",
  "HEAD 2222222222222222222222222222222222222222",
  "branch refs/heads/feat/208-gitview-worktrees",
  "",
  "worktree C:/Projects/loomux-worktrees/detached",
  "HEAD 3333333333333333333333333333333333333333",
  "detached",
  "",
  "worktree C:/Projects/loomux-worktrees/locked",
  "HEAD 4444444444444444444444444444444444444444",
  "branch refs/heads/wip",
  "locked needs the tests dir",
  "",
].join("\n");

test("parses each worktree record with branch, head, and flags", () => {
  const wts = parseWorktrees(SAMPLE);
  assert.equal(wts.length, 4);

  assert.equal(wts[0].path, "C:/Projects/loomux");
  assert.equal(wts[0].branch, "main"); // refs/heads/ prefix stripped
  assert.equal(wts[0].head, "1111111111111111111111111111111111111111");
  assert.equal(wts[0].primary, true); // first record is the main worktree
  assert.equal(wts[0].detached, false);

  assert.equal(wts[1].branch, "feat/208-gitview-worktrees"); // slashes survive
  assert.equal(wts[1].primary, false);

  assert.equal(wts[2].detached, true);
  assert.equal(wts[2].branch, null); // detached → no branch

  assert.equal(wts[3].locked, true);
  assert.equal(wts[3].branch, "wip");
});

test("parses a bare main repo entry", () => {
  const wts = parseWorktrees(
    ["worktree C:/repos/bare.git", "bare", "", "worktree C:/repos/wt", "HEAD abcd", "branch refs/heads/main", ""].join(
      "\n"
    )
  );
  assert.equal(wts[0].bare, true);
  assert.equal(wts[0].head, null);
  assert.equal(wts[0].branch, null);
  assert.equal(wts[0].primary, true);
  assert.equal(wts[1].bare, false);
});

test("marks a prunable worktree", () => {
  const wts = parseWorktrees(
    ["worktree /a", "HEAD ff", "branch refs/heads/main", "", "worktree /gone", "HEAD ee", "detached", "prunable gitdir file points to non-existent location", ""].join(
      "\n"
    )
  );
  assert.equal(wts[1].prunable, true);
  assert.equal(wts[1].detached, true);
});

test("tolerates CRLF line endings and trailing whitespace", () => {
  const wts = parseWorktrees("worktree /a\r\nHEAD ff\r\nbranch refs/heads/main\r\n\r\n");
  assert.equal(wts.length, 1);
  assert.equal(wts[0].branch, "main");
  assert.equal(wts[0].head, "ff");
});

test("empty output yields no worktrees and a null primary", () => {
  assert.deepEqual(parseWorktrees(""), []);
  assert.equal(primaryWorktree([]), null);
});

test("resolveSelection: null selection points at the primary", () => {
  const wts = parseWorktrees(SAMPLE);
  const r = resolveSelection(wts, null);
  assert.equal(r.active, wts[0]);
  assert.equal(r.selected, null);
  assert.equal(r.fellBack, false);
});

test("resolveSelection: an existing selection is honored", () => {
  const wts = parseWorktrees(SAMPLE);
  const r = resolveSelection(wts, "C:/Projects/loomux-worktrees/feat/208");
  assert.equal(r.active, wts[1]);
  assert.equal(r.selected, "C:/Projects/loomux-worktrees/feat/208");
  assert.equal(r.fellBack, false);
});

test("resolveSelection: matches across separator and case differences", () => {
  const wts = parseWorktrees(SAMPLE);
  // Same worktree, back-slashed and upper-cased — still resolves to it.
  const r = resolveSelection(wts, "C:\\PROJECTS\\loomux-worktrees\\feat\\208");
  assert.equal(r.active, wts[1]);
  assert.equal(r.fellBack, false);
});

test("resolveSelection: a pruned selection fails soft back to primary", () => {
  const wts = parseWorktrees(SAMPLE);
  const r = resolveSelection(wts, "C:/Projects/loomux-worktrees/deleted");
  assert.equal(r.active, wts[0]); // primary
  assert.equal(r.selected, null); // selection cleared
  assert.equal(r.fellBack, true); // caller shows a message
});

test("resolveSelection: re-picking the primary canonicalizes to null without a fallback message", () => {
  const wts = parseWorktrees(SAMPLE);
  const r = resolveSelection(wts, "C:/Projects/loomux");
  assert.equal(r.active, wts[0]);
  assert.equal(r.selected, null);
  assert.equal(r.fellBack, false); // it existed — not a fail-soft
});

test("resolveSelection: empty list yields a null active and no crash", () => {
  const r = resolveSelection([], "C:/whatever");
  assert.equal(r.active, null);
  assert.equal(r.selected, null);
  assert.equal(r.fellBack, true);
});

test("findWorktree returns null for a null path", () => {
  const wts = parseWorktrees(SAMPLE);
  assert.equal(findWorktree(wts, null), null);
});

test("worktreeLabel is the folder name", () => {
  assert.equal(worktreeLabel({ path: "C:/Projects/loomux-worktrees/feat/208" } as never), "208");
  assert.equal(worktreeLabel({ path: "C:\\repos\\bare.git" } as never), "bare.git");
});

test("normalizePath unifies separators, trailing slash, and case", () => {
  assert.equal(normalizePath("C:\\A\\B\\"), "c:/a/b");
  assert.equal(normalizePath("C:/A/B"), "c:/a/b");
});

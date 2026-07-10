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
  isMissingDir,
  isWritable,
} from "../src/gitworktree.ts";

const MAIN = "C:/Projects/loomux";
const LINKED = "C:/Projects/loomux-worktrees/feat/208";

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

test("resolveSelection: no selection + pane in the main tree points at the primary", () => {
  const wts = parseWorktrees(SAMPLE);
  const r = resolveSelection(wts, null, MAIN);
  assert.equal(r.active, wts[0]);
  assert.equal(r.selected, null);
  assert.equal(r.fellBack, false);
});

test("resolveSelection: no selection + pane INSIDE a linked worktree defaults to THAT worktree (#208 headline)", () => {
  const wts = parseWorktrees(SAMPLE);
  // The pane cd'd into the agent worktree; with no explicit choice the view
  // must follow the pane, not fall back to the porcelain-first main checkout.
  const r = resolveSelection(wts, null, LINKED);
  assert.equal(r.active, wts[1]);
  assert.equal(r.active?.primary, false);
  assert.equal(r.selected, null); // still "follow the pane", not pinned
  assert.equal(r.fellBack, false);
});

test("resolveSelection: pane-follow matches across separator and case differences", () => {
  const wts = parseWorktrees(SAMPLE);
  const r = resolveSelection(wts, null, "C:\\PROJECTS\\loomux-worktrees\\feat\\208");
  assert.equal(r.active, wts[1]);
});

test("resolveSelection: no selection + pane cwd not in any worktree falls back to primary", () => {
  const wts = parseWorktrees(SAMPLE);
  const r = resolveSelection(wts, null, "C:/somewhere/unrelated");
  assert.equal(r.active, wts[0]);
  assert.equal(r.fellBack, false);
});

test("resolveSelection: an explicit selection is honored", () => {
  const wts = parseWorktrees(SAMPLE);
  const r = resolveSelection(wts, LINKED, MAIN);
  assert.equal(r.active, wts[1]);
  assert.equal(r.selected, LINKED);
  assert.equal(r.fellBack, false);
});

test("resolveSelection: an explicit selection overrides the pane-follow default", () => {
  const wts = parseWorktrees(SAMPLE);
  // Pane sits in the linked worktree, but the user pinned the primary — the
  // explicit choice wins and must stick (stored, not canonicalized to null,
  // which would silently drop back to following the pane).
  const r = resolveSelection(wts, MAIN, LINKED);
  assert.equal(r.active, wts[0]);
  assert.equal(r.selected, MAIN);
  assert.equal(r.fellBack, false);
});

test("resolveSelection: a pruned explicit selection fails soft back to primary", () => {
  const wts = parseWorktrees(SAMPLE);
  const r = resolveSelection(wts, "C:/Projects/loomux-worktrees/deleted", LINKED);
  assert.equal(r.active, wts[0]); // primary
  assert.equal(r.selected, null); // selection cleared
  assert.equal(r.fellBack, true); // caller shows a message
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

test("a worktree deleted without git's knowledge stays a normal listed entry (no prunable marker on git 2.29)", () => {
  // rm -rf of the checkout without `git worktree remove`/prune: on the git 2.29
  // baseline the porcelain has no `prunable` line, so the parser cannot know the
  // dir is gone — it's an ordinary selectable entry. The runtime dir-existence
  // check (isMissingDir on the git call) is what fails soft, not the parser.
  const wts = parseWorktrees(
    ["worktree /a", "HEAD ff", "branch refs/heads/main", "", "worktree /gone", "HEAD ee", "branch refs/heads/side", ""].join(
      "\n"
    )
  );
  assert.equal(wts[1].path, "/gone");
  assert.equal(wts[1].prunable, false); // no marker — indistinguishable from a live worktree
  assert.equal(wts[1].branch, "side");
});

test("isMissingDir recognizes the backend's no-such-directory error", () => {
  assert.equal(isMissingDir("no such directory: C:/Projects/loomux-worktrees/gone"), true);
  assert.equal(isMissingDir(new Error("no such directory: /gone")), true);
  assert.equal(isMissingDir("No Such Directory: /gone"), true); // case-insensitive
  // Unrelated git failures must NOT be mistaken for a deleted worktree.
  assert.equal(isMissingDir("fatal: not a git repository"), false);
  assert.equal(isMissingDir("error: pathspec 'x' did not match"), false);
  assert.equal(isMissingDir(null), false);
});

test("isWritable: the primary worktree is always writable (today's behavior)", () => {
  const wts = parseWorktrees(SAMPLE);
  assert.equal(isWritable(wts[0], null), true); // primary, no unlock
  assert.equal(isWritable(wts[0], LINKED), true); // an unrelated unlock is irrelevant
});

test("isWritable: a plain repo with no worktree context is writable", () => {
  assert.equal(isWritable(null, null), true);
});

test("isWritable: a non-primary worktree is read-only by default", () => {
  const wts = parseWorktrees(SAMPLE);
  assert.equal(isWritable(wts[1], null), false);
  assert.equal(isWritable(wts[2], null), false); // detached one too
});

test("isWritable: unlocking is scoped to the exact worktree", () => {
  const wts = parseWorktrees(SAMPLE);
  assert.equal(isWritable(wts[1], wts[1].path), true); // unlocked THIS worktree
  assert.equal(isWritable(wts[1], "C:\\PROJECTS\\loomux-worktrees\\feat\\208"), true); // separator/case
});

test("isWritable: an unlock does not leak to a different worktree (reset on switch)", () => {
  const wts = parseWorktrees(SAMPLE);
  // Unlock pinned to worktree[1]; the active worktree is now [3] → read-only.
  assert.equal(isWritable(wts[3], wts[1].path), false);
  // And switching back to primary is read-only-moot (primary is always writable).
  assert.equal(isWritable(wts[0], wts[1].path), true);
});

test("normalizePath unifies separators, trailing slash, and case", () => {
  assert.equal(normalizePath("C:\\A\\B\\"), "c:/a/b");
  assert.equal(normalizePath("C:/A/B"), "c:/a/b");
});

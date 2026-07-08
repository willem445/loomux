// Unit tests for the pure file-tree model (issue #174). Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  safeJoin,
  sortEntries,
  entryNodes,
  mergeChildren,
  flatten,
  findNode,
  makeRoot,
  type TreeNode,
} from "../src/filetreemodel.ts";
import type { FtEntry } from "../src/fileapi.ts";

const entry = (name: string, is_dir: boolean): FtEntry => ({
  name,
  is_dir,
  is_symlink: false,
  size: 0,
});

test("sortEntries puts directories first, then case-insensitive by name", () => {
  const sorted = sortEntries([
    entry("Zebra.txt", false),
    entry("apple", true),
    entry("Banana", true),
    entry("aardvark.txt", false),
  ]);
  assert.deepEqual(
    sorted.map((e) => e.name),
    ["apple", "Banana", "aardvark.txt", "Zebra.txt"]
  );
});

test("safeJoin builds nested paths and rejects escapes", () => {
  assert.equal(safeJoin("", "src"), "src");
  assert.equal(safeJoin("src", "main.ts"), "src/main.ts");
  // Edge: traversal, separators, and absolute-ish names must throw — they can
  // never be legitimate dirents and would sidestep the server's within-root check.
  assert.throws(() => safeJoin("src", ".."));
  assert.throws(() => safeJoin("src", "a/b"));
  assert.throws(() => safeJoin("src", "a\\b"));
  assert.throws(() => safeJoin("", "C:\\Windows"));
  assert.throws(() => safeJoin("src", ""));
});

test("flatten emits only visible rows and follows expansion", () => {
  const root = makeRoot();
  root.children = entryNodes("", [entry("src", true), entry("readme.md", false)]);
  const src = root.children[0];
  // Collapsed: just the two top-level rows.
  assert.deepEqual(
    flatten(root).map((r) => r.node.name),
    ["src", "readme.md"]
  );
  // Expand src with children → its children appear at depth 1.
  src.expanded = true;
  src.loaded = true;
  src.children = entryNodes("src", [entry("a.ts", false)]);
  const rows = flatten(root);
  assert.deepEqual(
    rows.map((r) => `${r.depth}:${r.node.name}`),
    ["0:src", "1:a.ts", "0:readme.md"]
  );
  // Collapse again → back to two rows (children hidden, not lost).
  src.expanded = false;
  assert.equal(flatten(root).length, 2);
  assert.equal(src.children.length, 1, "collapsing keeps children in the model");
});

test("mergeChildren preserves expansion of nodes that still exist", () => {
  const existing: TreeNode[] = entryNodes("", [entry("src", true), entry("old", true)]);
  // Expand and load `src` deeply (entryNodes sorts, so find it by name).
  const srcExisting = existing.find((n) => n.name === "src")!;
  srcExisting.expanded = true;
  srcExisting.loaded = true;
  srcExisting.children = entryNodes("src", [entry("deep.ts", false)]);

  // Re-list the root: `src` still there, `old` gone, `new.txt` added.
  const merged = mergeChildren(existing, "", [entry("src", true), entry("new.txt", false)]);
  const src = merged.find((n) => n.name === "src")!;
  assert.ok(src.expanded, "expansion survives a re-list");
  assert.ok(src.loaded);
  assert.equal(src.children.length, 1, "loaded children survive a re-list");
  assert.equal(src.children[0].name, "deep.ts");
  assert.ok(!merged.some((n) => n.name === "old"), "vanished entry drops out");
  const fresh = merged.find((n) => n.name === "new.txt")!;
  assert.ok(!fresh.expanded && !fresh.loaded, "new entry starts collapsed");
});

test("mergeChildren resets a node whose type flipped (dir↔file)", () => {
  const existing = entryNodes("", [entry("x", true)]);
  existing[0].expanded = true;
  existing[0].loaded = true;
  existing[0].children = entryNodes("x", [entry("c", false)]);
  // `x` is now a file, not a dir — the old expanded state is meaningless.
  const merged = mergeChildren(existing, "", [entry("x", false)]);
  assert.ok(!merged[0].expanded && !merged[0].loaded);
  assert.equal(merged[0].children.length, 0);
});

test("findNode walks loaded branches by path", () => {
  const root = makeRoot();
  root.children = entryNodes("", [entry("src", true)]);
  root.children[0].children = entryNodes("src", [entry("main.ts", false)]);
  assert.equal(findNode(root, "src/main.ts")?.name, "main.ts");
  assert.equal(findNode(root, "src")?.name, "src");
  assert.equal(findNode(root, "nope"), null);
  assert.equal(findNode(root, ""), root);
});

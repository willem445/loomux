// The pure core of the file-manager pane (#214) — fileexplorermodel.ts. Pins the
// listing order, the rooted-navigation bound, the display formatting, and the
// inline-edit validation (including the two cases that are easy to get subtly
// wrong: renaming an entry to its own name, and case-insensitive collisions).
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  visibleEntries,
  compareEntries,
  joinRel,
  parentRel,
  breadcrumbs,
  formatSize,
  formatModified,
  clampSelection,
  nameError,
  canCommit,
  isNoopRename,
  noEdit,
  type FmEntry,
  type EditState,
} from "../src/fileexplorermodel.ts";

const entry = (name: string, over: Partial<FmEntry> = {}): FmEntry => ({
  name,
  is_dir: false,
  is_symlink: false,
  size: 0,
  modified_ms: 0,
  is_hidden: false,
  ...over,
});
const dir = (name: string, over: Partial<FmEntry> = {}) => entry(name, { is_dir: true, ...over });

const shown = (entries: FmEntry[], showHidden = false) =>
  visibleEntries(entries, showHidden).map((e) => e.name);

// ---------- listing order ----------

test("folders come first, then files — each ordered case-insensitively", () => {
  // The backend deliberately returns entries unsorted; this ordering IS the product
  // decision, so it's pinned here rather than trusted to readdir().
  const list = [entry("zebra.ts"), dir("src"), entry("Apple.ts"), dir("Assets")];
  assert.deepEqual(shown(list), ["Assets", "src", "Apple.ts", "zebra.ts"]);
});

test("names sort numerically, so file2 precedes file10", () => {
  assert.deepEqual(shown([entry("file10.ts"), entry("file2.ts")]), ["file2.ts", "file10.ts"]);
});

test("the order is TOTAL — same-name-different-case entries never swap between listings", () => {
  // A case-insensitive compare alone returns 0 for these, and an unstable sort could
  // then reorder them run to run, making the listing visibly jitter on refresh.
  const a = [entry("README"), entry("readme")];
  assert.deepEqual(shown(a), shown([...a].reverse()));
});

test("a symlink sorts with FILES even when it points at a directory", () => {
  // We never follow it, so as far as this pane is concerned it is not a folder.
  const link = entry("link", { is_dir: true, is_symlink: true });
  assert.deepEqual(shown([link, dir("real"), entry("a.txt")]), ["real", "a.txt", "link"]);
});

test("hidden entries are filtered unless asked for, and filtering does not mutate the input", () => {
  const list = [entry("visible.ts"), entry(".env", { is_hidden: true }), dir(".git", { is_hidden: true })];
  assert.deepEqual(shown(list), ["visible.ts"]);
  assert.deepEqual(shown(list, true), [".git", ".env", "visible.ts"]);
  // The caller caches the listing and re-filters when the toggle flips; mutating it
  // would mean the hidden entries were gone for good and a refetch was needed.
  assert.equal(list.length, 3);
  assert.equal(list[0].name, "visible.ts");
});

test("compareEntries is usable directly and agrees with the listing", () => {
  assert.ok(compareEntries(dir("z"), entry("a")) < 0, "a folder beats a file regardless of name");
});

// ---------- navigation (rooted) ----------

test("parentRel walks up, and returns null AT the root — the pane cannot climb out", () => {
  // This is the bound that makes the backend's root+rel containment real rather
  // than decorative: there is no `rel` the UI can produce that escapes the root.
  assert.equal(parentRel("a/b/c"), "a/b");
  assert.equal(parentRel("a"), "");
  assert.equal(parentRel(""), null, "at the root, Up is disabled");
  assert.equal(parentRel("/"), null);
});

test("joinRel builds forward-slashed rels and treats the root as empty", () => {
  assert.equal(joinRel("", "src"), "src");
  assert.equal(joinRel("src", "pane.ts"), "src/pane.ts");
  assert.equal(joinRel("/src/", "pane.ts"), "src/pane.ts");
});

test("breadcrumbs start at the root and each crumb navigates to its own level", () => {
  assert.deepEqual(breadcrumbs("loomux", "src/design"), [
    { label: "loomux", rel: "" },
    { label: "src", rel: "src" },
    { label: "design", rel: "src/design" },
  ]);
  assert.deepEqual(breadcrumbs("loomux", ""), [{ label: "loomux", rel: "" }]);
});

// ---------- display ----------

test("formatSize scales units, drops a pointless .0, and leaves folders blank", () => {
  assert.equal(formatSize(entry("a", { size: 0 })), "0 B");
  assert.equal(formatSize(entry("a", { size: 999 })), "999 B");
  assert.equal(formatSize(entry("a", { size: 1024 })), "1 KB", "1.0 KB would be noise");
  assert.equal(formatSize(entry("a", { size: 1536 })), "1.5 KB");
  assert.equal(formatSize(entry("a", { size: 5 * 1024 * 1024 })), "5 MB");
  assert.equal(formatSize(dir("src")), "", "a folder's size would mean walking it");
});

test("formatModified shows a time for today and a date for anything older", () => {
  const now = new Date(2026, 6, 11, 15, 30).getTime();
  const anHourAgo = new Date(2026, 6, 11, 14, 5).getTime();
  const lastYear = new Date(2025, 0, 2, 9, 7).getTime();
  assert.equal(formatModified(anHourAgo, now), "14:05");
  assert.equal(formatModified(lastYear, now), "2025-01-02 09:07");
});

test("an unknown mtime renders as an em dash, not as 1970", () => {
  assert.equal(formatModified(0, Date.parse("2026-07-11T00:00:00Z")), "—");
});

// ---------- selection ----------

test("selection CLAMPS at both ends — a listing is a place, not a menu that wraps", () => {
  // Deliberately different from the Go-to-file result list, which wraps: holding
  // Down here must come to rest on the last row, not teleport past it to the top.
  assert.equal(clampSelection(0, 1, 3), 1);
  assert.equal(clampSelection(2, 1, 3), 2, "Down on the last row stays put");
  assert.equal(clampSelection(0, -1, 3), 0, "Up on the first row stays put");
  assert.equal(clampSelection(-1, 1, 3), 0, "Down with nothing selected lands on the first");
  assert.equal(clampSelection(-1, -1, 3), 2, "Up with nothing selected lands on the last");
  assert.equal(clampSelection(1, 1, 0), -1, "an empty listing has nothing to select");
});

// ---------- inline edit ----------

const newFolder = (draft: string): EditState => ({ kind: "new-folder", draft });
const renameTo = (draft: string, original = "old.txt"): EditState => ({
  kind: "rename",
  rel: `sub/${original}`,
  original,
  draft,
});

test("a name that collides with a sibling is refused — case-insensitively", () => {
  // The filesystems this runs on are case-insensitive, so offering to create `Foo`
  // beside an existing `foo` just fails at the syscall with a worse message.
  assert.match(nameError(newFolder("src"), ["src", "a.txt"])!, /already exists/);
  assert.match(nameError(newFolder("SRC"), ["src"])!, /already exists/);
  assert.equal(nameError(newFolder("fresh"), ["src"]), null);
});

test("renaming an entry to its OWN name is allowed — it is a no-op, not a self-collision", () => {
  // The obvious duplicate check ("is this name in the listing?") rejects this,
  // because the entry being renamed is itself in the listing. Open the rename
  // editor, press Enter without typing: that must work.
  const siblings = ["old.txt", "other.txt"];
  assert.equal(nameError(renameTo("old.txt"), siblings), null);
  assert.ok(canCommit(renameTo("old.txt"), siblings));
  assert.ok(isNoopRename(renameTo("old.txt")), "and the caller skips the round-trip entirely");

  // But a rename onto a DIFFERENT existing sibling is still a collision.
  assert.match(nameError(renameTo("other.txt"), siblings)!, /already exists/);
  assert.ok(!isNoopRename(renameTo("other.txt")));
});

test("a rename that only changes CASE is not a no-op, and does not self-collide", () => {
  // `old.txt` → `Old.txt` is a real, useful rename. It must not be blocked as a
  // duplicate of itself, and it must not be skipped as a no-op.
  const state = renameTo("Old.txt");
  assert.equal(nameError(state, ["old.txt"]), null);
  assert.ok(!isNoopRename(state), "the case DID change — send it to the backend");
});

test("empty, dot, separator and trailing-dot names are refused", () => {
  assert.match(nameError(newFolder("   "), [])!, /empty/);
  assert.match(nameError(newFolder("."), [])!, /'\.'/);
  assert.match(nameError(newFolder(".."), [])!, /'\.'/);
  assert.match(nameError(newFolder("a/b"), [])!, /cannot contain '\/'/);
  assert.match(nameError(newFolder("a\\b"), [])!, /cannot contain/);
  // Windows silently strips a trailing dot, so the folder you get isn't the one you
  // asked for. The backend rejects it too — this just says so while you type.
  assert.match(nameError(newFolder("trailing."), [])!, /end with a dot/);
  for (const bad of ["   ", ".", "a/b", "trailing."]) {
    assert.ok(!canCommit(newFolder(bad), []), `${bad} must not be committable`);
  }
});

test("a leading dot is a perfectly ordinary name", () => {
  assert.equal(nameError(newFolder(".github"), []), null);
});

test("with no edit in flight there is nothing to validate or commit", () => {
  assert.equal(nameError(noEdit, ["anything"]), null);
  assert.ok(!canCommit(noEdit, []));
  assert.ok(!isNoopRename(noEdit));
});

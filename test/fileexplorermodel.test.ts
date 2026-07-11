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
  isCreate,
  noEdit,
  activeTarget,
  baseName,
  editMountFor,
  mountBlocker,
  opBlockedReason,
  isOpBlocked,
  isRowBusy,
  idleOp,
  type PaneOp,
  type OpRequest,
  type FmEntry,
  type EditState,
  type ExplorerView,
  type OpTarget,
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

// ---------- what an operation acts on (the demo bug) ----------
//
// Reported: with the Go-to-file filter active, rename "does nothing"; clear the filter
// and it fires against a DIFFERENT entry — the selection in the unfiltered listing.
// Root cause: ops resolved a target from the listing's selection INDEX even while the
// listing was hidden and the results were on screen. These pin the fix: an op resolves
// a row's IDENTITY from the view actually being shown, and that value can't be
// retargeted by anything that happens to the lists afterwards.

const LISTING: FmEntry[] = [dir("src"), entry("a.txt"), entry("b.txt")];

const listingView = (sel: number, dirRel = ""): ExplorerView => ({
  kind: "listing",
  dir: dirRel,
  rows: visibleEntries(LISTING, false),
  sel,
});
const resultsView = (sel: number, ...rels: string[]): ExplorerView => ({
  kind: "results",
  dir: "",
  hits: rels.map((rel) => ({ rel })),
  sel,
});

test("an op on the LISTING targets the selected row's path, joined to the current dir", () => {
  // rows() is folders-first, so index 1 is a.txt.
  assert.deepEqual(activeTarget(listingView(1, "sub")), {
    rel: "sub/a.txt",
    name: "a.txt",
    isDir: false,
    isSymlink: false,
    from: "listing",
  });
  assert.deepEqual(activeTarget(listingView(0)), {
    rel: "src",
    name: "src",
    isDir: true,
    isSymlink: false,
    from: "listing",
  });
});

test("an op on a FILTERED RESULT targets that result — not the listing's selection", () => {
  // THE BUG, stated as a test. The listing selection (index 1) is irrelevant while the
  // results are what's on screen; resolving against it is what fired the rename at the
  // wrong file. A hit carries its OWN full rel — it isn't relative to the browsed dir.
  const view = resultsView(0, "deep/nested/target.ts", "other.ts");
  assert.deepEqual(activeTarget(view), {
    rel: "deep/nested/target.ts",
    name: "target.ts",
    isDir: false,
    isSymlink: false,
    from: "results",
  });
});

test("the reported sequence cannot fire against a different entry", () => {
  // filter active, hit selected → the op targets the HIT.
  const filtered = resultsView(0, "deep/nested/target.ts");
  const captured = activeTarget(filtered)!;
  assert.equal(captured.rel, "deep/nested/target.ts");

  // The filter now clears — the listing (with its own, unrelated selection) is back.
  // Under the old index-based resolution this is the moment the op would have
  // retargeted onto b.txt. The captured target is a VALUE: it cannot.
  const afterClear = activeTarget(listingView(2));
  assert.equal(afterClear!.rel, "b.txt", "the listing would have resolved to something else entirely");
  assert.notEqual(captured.rel, afterClear!.rel);
  assert.equal(captured.rel, "deep/nested/target.ts", "the captured target is unmoved");
});

test("a target captured mid-op survives the lists changing underneath it", () => {
  // The results list re-ranks and re-renders on every streaming index batch, and the
  // listing reorders on refresh. Neither may retarget an op already in flight.
  const captured = activeTarget(resultsView(1, "a/one.ts", "b/two.ts"))!;
  assert.equal(captured.rel, "b/two.ts");

  // A later batch lands and re-ranks the results; index 1 is now a different file.
  const reranked = activeTarget(resultsView(1, "b/two.ts", "c/three.ts"))!;
  assert.equal(reranked.rel, "c/three.ts", "index 1 now means something else");
  assert.equal(captured.rel, "b/two.ts", "but the captured target still names the file it named");
});

test("nothing selected — or a stale, out-of-range selection — targets nothing", () => {
  // The caller disables its buttons on null, so an op can never fire at nothing (or at
  // whatever happens to sit at a stale index after the list shrank).
  assert.equal(activeTarget(listingView(-1)), null);
  assert.equal(activeTarget(listingView(99)), null);
  assert.equal(activeTarget(resultsView(0)), null, "no hits at all");
  assert.equal(activeTarget(resultsView(5, "only.ts")), null);
});

test("baseName takes the last segment of a rel", () => {
  assert.equal(baseName("a/b/c.ts"), "c.ts");
  assert.equal(baseName("top.ts"), "top.ts");
});

// ---------- where the inline editor is allowed to mount ----------
//
// The OTHER half of the same bug, and the one that got rebuilt on the very path added
// to fix the first half. Capturing the right target is not enough: the inline editor row
// exists ONLY in the directory listing, so mounting it while the Go-to-file results are
// on screen puts it inside a `display:none` list — the row never appears and its focus
// call no-ops. Same visible symptom ("F2 does nothing"), different cause. These pin the
// required POST-OP VIEW STATE, not just the target path.

test("rename from a RESULT must leave the filter — the editor cannot mount in a hidden list", () => {
  const target = activeTarget(resultsView(0, "deep/nested/target.ts"))!;
  const mount = editMountFor(target, resultsView(0, "deep/nested/target.ts"));
  assert.equal(mount.exitFilter, true, "the results list must go, or the editor mounts nowhere");
  assert.equal(mount.navigate, true);
  assert.equal(mount.dir, "deep/nested", "and the listing shown must be the file's own folder");
});

test("rename from a result in the CURRENTLY BROWSED folder STILL leaves the filter", () => {
  // The trap. "Only exit the filter if we also have to navigate" is a reasonable-looking
  // fix and a wrong one: the listing is hidden either way, because it is the QUERY that
  // hides it — not the folder. Nothing to navigate to here, and the filter must still go.
  const view: ExplorerView = { kind: "results", dir: "src", hits: [{ rel: "src/pane.ts" }], sel: 0 };
  const target = activeTarget(view)!;
  const mount = editMountFor(target, view);
  assert.equal(mount.exitFilter, true, "hidden by the QUERY, not by the folder");
  assert.equal(mount.navigate, false, "already in the right folder — nothing to fetch");
  assert.equal(mount.dir, "src");
});

test("rename from the LISTING needs no view change at all", () => {
  const view = listingView(1, "sub");
  const mount = editMountFor(activeTarget(view)!, view);
  assert.deepEqual(mount, { dir: "sub", exitFilter: false, navigate: false });
});

test("a target at the ROOT resolves to the root listing, not to null", () => {
  const view: ExplorerView = { kind: "results", dir: "sub", hits: [{ rel: "README.md" }], sel: 0 };
  const mount = editMountFor(activeTarget(view)!, view);
  assert.equal(mount.dir, "", "parentRel(README.md) is null → the root, which is \"\"");
  assert.equal(mount.navigate, true);
});

// ---------- can the target's row actually be rendered? ----------

test("a HIDDEN target is reported as such — the Go-to-file index reaches files the listing hides", () => {
  // On macOS/Linux every tracked dotfile is `is_hidden`, so renaming `.gitignore` from a
  // search with Hidden OFF is an ordinary thing to do. Left unhandled it mounts no editor
  // at all AND leaves the edit state set with no input to Escape from, deadening the
  // listing's keyboard. The caller turns Hidden on for the op instead.
  const entries = [entry(".gitignore", { is_hidden: true }), entry("visible.ts")];
  const target: OpTarget = { rel: ".gitignore", name: ".gitignore", isDir: false, isSymlink: false, from: "results" };

  assert.deepEqual(mountBlocker(target, entries, false), { kind: "hidden" });
  assert.deepEqual(mountBlocker(target, entries, true), { kind: "ok" }, "with Hidden on it renders fine");
});

test("a target that VANISHED between capture and mount is reported missing, not silently dropped", () => {
  // An agent (or another app) deleted it while the user was picking it out of the results.
  const target: OpTarget = { rel: "sub/gone.ts", name: "gone.ts", isDir: false, isSymlink: false, from: "results" };
  assert.deepEqual(mountBlocker(target, [entry("still-here.ts")], true), { kind: "missing" });
});

test("an ordinary visible target mounts without ceremony", () => {
  const target: OpTarget = { rel: "sub/a.txt", name: "a.txt", isDir: false, isSymlink: false, from: "listing" };
  assert.deepEqual(mountBlocker(target, [entry("a.txt")], false), { kind: "ok" });
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

test("the inline check is a SUBSET of the backend's — reserved device names pass here", () => {
  // Deliberate, and pinned so nobody "fixes" it silently. Inline validation exists to
  // catch the near-misses a user actually makes while typing; the Windows reserved
  // device names (`con`, `nul`, `com1`, …) are long, obscure, and typed by accident by
  // no one. They're left to the backend's `validate_name`, which refuses them on
  // commit with a toast saying why. If this ever starts returning an error, the
  // docs and the human-validation list have to change with it.
  assert.equal(nameError(newFolder("con"), []), null);
  assert.equal(nameError(newFolder("aux.txt"), []), null);
  assert.ok(canCommit(newFolder("con"), []), "the UI lets it through — the BACKEND stops it");
});

test("with no edit in flight there is nothing to validate or commit", () => {
  assert.equal(nameError(noEdit, ["anything"]), null);
  assert.ok(!canCommit(noEdit, []));
  assert.ok(!isNoopRename(noEdit));
});

// ---------- the long-running-op state machine (#216) ----------
//
// A delete used to run synchronously on Tauri's main thread and froze the whole window for
// its duration. It now runs on a worker and reports by event — which gives the pane a state
// it can be IN, and that state has to be modelled rather than implied.

const deleting = (rel: string, name: string): PaneOp => ({ kind: "deleting", rel, name });

test("nothing is blocked while the pane is idle", () => {
  for (const req of [
    "delete",
    "rename",
    "new-folder",
    "new-file",
    "navigate",
    "hash",
    "open",
    "open-with",
    "reveal",
  ] as OpRequest[]) {
    assert.equal(opBlockedReason(idleOp, req), null, `${req} must run when idle`);
    assert.equal(isOpBlocked(idleOp, req), false);
  }
});

test("a delete in flight blocks the MUTATING ops — and only those", () => {
  // THE TABLE. Racing a second destructive op against a shell operation that is halfway
  // through the same tree is how a user ends up unable to say what is actually on disk.
  const op = deleting("sub/tree", "tree");
  for (const req of ["delete", "rename", "new-folder", "new-file"] as OpRequest[]) {
    assert.ok(isOpBlocked(op, req), `${req} mutates the tree — it must wait`);
    assert.match(opBlockedReason(op, req)!, /tree/, "and the reason must name the file");
  }
});

test("a delete in flight must NOT block navigation or hashing", () => {
  // The load-bearing half. Blocking these would reintroduce the freeze one layer up — just
  // implemented in TypeScript instead of on the main thread. They read; they don't write;
  // they touch nothing the delete owns.
  const op = deleting("sub/tree", "tree");
  for (const req of ["navigate", "hash", "open", "open-with", "reveal"] as OpRequest[]) {
    assert.equal(
      opBlockedReason(op, req),
      null,
      `${req} reads but never writes — freezing it is the bug we just removed`
    );
  }
});

test("the reason names the file, so the user knows WHAT they're waiting on", () => {
  assert.match(opBlockedReason(deleting("a/b/node_modules", "node_modules"), "rename")!, /node_modules/);
});

test("only the row being deleted is busy", () => {
  const op = deleting("sub/tree", "tree");
  assert.ok(isRowBusy(op, "sub/tree"));
  assert.ok(!isRowBusy(op, "sub/other"), "a sibling is not busy");
  assert.ok(!isRowBusy(op, "tree"), "and the match is on the full rel, not the name");
  assert.ok(!isRowBusy(idleOp, "sub/tree"), "nothing is busy when idle");
});

test("there is no cancel — deliberately, and the model offers none to be tempted by", () => {
  // SHFileOperationW is ONE call with no cancel handle, and a delete stopped mid-tree leaves
  // half its children in the Recycle Bin and half on disk. So the UI says "in progress" and
  // never offers a Cancel it could not honor. If a `cancel` ever appears in PaneOp, this test
  // is the place someone has to come and argue for it.
  const op = deleting("sub/tree", "tree");
  assert.deepEqual(Object.keys(op).sort(), ["kind", "name", "rel"]);
});

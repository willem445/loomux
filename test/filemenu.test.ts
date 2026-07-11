// The pure context-menu model (#214) — filemenu.ts. The load-bearing test here is the
// first one: a context menu is the identity-vs-index trap with a longer fuse, since it
// is built at right-click and acted on seconds later, after the lists have had every
// chance to change underneath it.
import { test } from "node:test";
import assert from "node:assert/strict";
import { buildContextMenu, menuActions, type FmCaps, type MenuItem } from "../src/filemenu.ts";
import { HASH_ALGOS } from "../src/filehashmodel.ts";
import type { OpTarget } from "../src/fileexplorermodel.ts";

const WIN: FmCaps = { delete_mode: "recycle", open_with: true, reveal: true, reveal_selects: true };
const MAC: FmCaps = { delete_mode: "permanent", open_with: false, reveal: true, reveal_selects: true };
const LINUX: FmCaps = { delete_mode: "permanent", open_with: false, reveal: true, reveal_selects: false };

const fileTarget: OpTarget = {
  rel: "deep/nested/report.pdf",
  name: "report.pdf",
  isDir: false,
  from: "results",
};
const dirTarget: OpTarget = { rel: "src", name: "src", isDir: true, from: "listing" };

const labels = (items: MenuItem[]) => items.filter((i) => !i.separator).map((i) => i.label);
const find = (items: MenuItem[], label: string) => items.find((i) => i.label === label);

// ---------- the rule: the target is BOUND at menu-open ----------

test("EVERY row-scoped action carries the target the menu was built on", () => {
  // The trap this exists to close: a menu built at right-click and clicked seconds later
  // must not go looking up "what's selected now". Each action is a complete instruction —
  // the path is in it. Nothing is left to re-resolve against a list that has since
  // re-ranked, re-sorted, or been replaced by search results.
  const menu = buildContextMenu(fileTarget, WIN, HASH_ALGOS);
  const rowActions = menuActions(menu).filter(
    (a) => a.kind !== "new-folder" && a.kind !== "new-file"
  );
  assert.ok(rowActions.length >= 5, "open, open-with, reveal, rename, delete, hash×6");
  for (const action of rowActions) {
    assert.equal(
      (action as { target: OpTarget }).target.rel,
      "deep/nested/report.pdf",
      `${action.kind} must carry the bound target, not a lookup`
    );
  }
});

test("a hit's OWN rel is bound — not a name joined to whatever folder is being browsed", () => {
  // The target came from the Go-to-file results, so it lives in `deep/nested/`, which is
  // very probably NOT the directory on screen. Rebuilding its path from the browsed dir
  // is exactly the class of bug that produced a rename against the wrong file.
  const hashes = menuActions(buildContextMenu(fileTarget, WIN, HASH_ALGOS)).filter((a) => a.kind === "hash");
  assert.equal(hashes.length, 6);
  for (const h of hashes) {
    assert.equal((h as { target: OpTarget }).target.rel, "deep/nested/report.pdf");
  }
});

test("the CREATES carry no target — they act on the directory, not on a row", () => {
  // Deliberate, and typed that way: a "new file" bound to a row would be nonsense, and a
  // target sitting unused on it would invite someone to start using it.
  const creates = menuActions(buildContextMenu(fileTarget, WIN, HASH_ALGOS)).filter(
    (a) => a.kind === "new-folder" || a.kind === "new-file"
  );
  assert.equal(creates.length, 2);
  for (const c of creates) {
    assert.ok(!("target" in c), "a create has no row target");
  }
});

// ---------- shape ----------

test("empty space offers only the creates — there is no row to act on", () => {
  const menu = buildContextMenu(null, WIN, HASH_ALGOS);
  assert.deepEqual(labels(menu), ["New"]);
  assert.deepEqual(
    menuActions(menu).map((a) => a.kind),
    ["new-folder", "new-file"]
  );
});

test("a FILE gets the full menu, including Hash with all six algorithms", () => {
  const menu = buildContextMenu(fileTarget, WIN, HASH_ALGOS);
  assert.deepEqual(labels(menu), [
    "Open (default app)",
    "Open with…",
    "Reveal in file explorer",
    "Rename…",
    "Delete (to Recycle Bin)",
    "Hash",
    "New",
  ]);
  assert.equal(find(menu, "Hash")!.children!.length, 6);
});

test("a FOLDER cannot be hashed or opened-with, and says why instead of vanishing", () => {
  // Disabled-with-a-reason, not omitted: the menu's shape shouldn't shift depending on
  // what you clicked, or you never learn where anything is. (Contrast the OS-capability
  // items below, which ARE omitted — those would never work here at all.)
  const menu = buildContextMenu(dirTarget, WIN, HASH_ALGOS);
  const hash = find(menu, "Hash")!;
  assert.equal(hash.disabled, true);
  assert.equal(hash.children, undefined, "no submenu to open on a folder");
  assert.match(hash.reason!, /no file contents/i);

  const openWith = find(menu, "Open with…")!;
  assert.equal(openWith.disabled, true);
  assert.match(openWith.reason!, /folder/i);

  // But Open still works — on a folder it means "navigate into it".
  assert.equal(find(menu, "Open")!.disabled, undefined);
});

// ---------- OS capabilities: hide what would always fail, label what is approximate ----------

test("Open-with is OMITTED where the OS has no chooser, not shown-and-broken", () => {
  assert.ok(find(buildContextMenu(fileTarget, WIN, HASH_ALGOS), "Open with…"), "Windows has one");
  assert.equal(find(buildContextMenu(fileTarget, MAC, HASH_ALGOS), "Open with…"), undefined);
  assert.equal(find(buildContextMenu(fileTarget, LINUX, HASH_ALGOS), "Open with…"), undefined);
  // …and no action for it can be reached either, so a keyboard walk can't fire it.
  assert.ok(!menuActions(buildContextMenu(fileTarget, MAC, HASH_ALGOS)).some((a) => a.kind === "open-with"));
});

test("Reveal admits when it cannot actually select the entry", () => {
  // Linux has no portable "reveal": we can open the containing folder and nothing more.
  // The label says that, rather than promising a selection that won't happen.
  assert.ok(find(buildContextMenu(fileTarget, WIN, HASH_ALGOS), "Reveal in file explorer"));
  assert.ok(find(buildContextMenu(fileTarget, MAC, HASH_ALGOS), "Reveal in file explorer"));
  assert.ok(find(buildContextMenu(fileTarget, LINUX, HASH_ALGOS), "Open containing folder"));
  assert.equal(find(buildContextMenu(fileTarget, LINUX, HASH_ALGOS), "Reveal in file explorer"), undefined);
});

test("Delete says what it will actually do on this platform", () => {
  // The same honesty the confirm dialog already owes: never promise a Recycle Bin on a
  // platform that hasn't got one.
  assert.ok(find(buildContextMenu(fileTarget, WIN, HASH_ALGOS), "Delete (to Recycle Bin)"));
  assert.ok(find(buildContextMenu(fileTarget, MAC, HASH_ALGOS), "Delete permanently…"));
});

test("menuActions walks submenus, so nothing offered is unreachable", () => {
  const kinds = menuActions(buildContextMenu(fileTarget, WIN, HASH_ALGOS)).map((a) => a.kind);
  assert.deepEqual(new Set(kinds), new Set([
    "open",
    "open-with",
    "reveal",
    "rename",
    "delete",
    "hash",
    "new-folder",
    "new-file",
  ]));
});

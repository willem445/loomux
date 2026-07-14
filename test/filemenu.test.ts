// The pure context-menu model (#214) — filemenu.ts. The load-bearing test here is the
// first one: a context menu is the identity-vs-index trap with a longer fuse, since it
// is built at right-click and acted on seconds later, after the lists have had every
// chance to change underneath it.
import { test } from "node:test";
import assert from "node:assert/strict";
import { buildContextMenu, menuActions, type FmCaps, type MenuItem } from "../src/filemenu.ts";
import { HASH_ALGOS } from "../src/filehashmodel.ts";
import { ROW_AFFORDANCES, type OpTarget, type RowAffordance } from "../src/fileexplorermodel.ts";

const WIN: FmCaps = { delete_mode: "recycle", open_with: true, reveal: true, reveal_selects: true };
const MAC: FmCaps = { delete_mode: "permanent", open_with: false, reveal: true, reveal_selects: true };
const LINUX: FmCaps = { delete_mode: "permanent", open_with: false, reveal: true, reveal_selects: false };

const fileTarget: OpTarget = {
  rel: "deep/nested/report.pdf",
  name: "report.pdf",
  isDir: false,
  isSymlink: false,
  from: "results",
};
const dirTarget: OpTarget = {
  rel: "src",
  name: "src",
  isDir: true,
  isSymlink: false,
  from: "listing",
};
const linkTarget: OpTarget = {
  rel: "link",
  name: "link",
  isDir: false,
  isSymlink: true,
  from: "listing",
};

const labels = (items: MenuItem[]) => items.filter((i) => !i.separator).map((i) => i.label);
const find = (items: MenuItem[], label: string) => items.find((i) => i.label === label);
/** …and the same lookup where the item's absence is itself a failure. */
const must = (items: MenuItem[], label: string): MenuItem => {
  const item = find(items, label);
  assert.ok(item, `expected a "${label}" item`);
  return item;
};

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
    // The in-app counterpart to the OS hand-off above it (#217): loomux's own editor,
    // in a new pane. Sits with the other "open this somewhere" items, not with the
    // mutating ops below the separator.
    "Open in file editor pane",
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
  assert.ok(!find(menu, "Open")!.disabled);
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

// ---------- VIEW PARITY (rev-106) ----------
//
// Three rounds running, an affordance was built for the listing and forgotten for the
// Go-to-file results: rename bound the wrong row (round 4), the editor mounted into the
// hidden list (round 5), the context menu wasn't wired to result rows at all (round 6).
// Each fix was right; each time the NEXT affordance forgot again. These tests close the
// CLASS rather than the instance — a new row affordance cannot be added without answering
// "…and does it work in the results view?"

test("PARITY: every row affordance the MENU offers is declared in ROW_AFFORDANCES", () => {
  // The load-bearing one. Add an item to the context menu and forget the registry, and this
  // fails — which is the only moment anyone is going to think about the results view.
  const offered = new Set(
    menuActions(buildContextMenu(fileTarget, WIN, HASH_ALGOS))
      .map((a) => a.kind)
      // The creates act on the DIRECTORY, not on a row, so they are not row affordances.
      .filter((k) => k !== "new-folder" && k !== "new-file")
  );
  const declared = new Set(ROW_AFFORDANCES.map((a) => a.affordance));
  for (const kind of offered) {
    assert.ok(
      declared.has(kind as RowAffordance),
      `the menu offers "${kind}" but ROW_AFFORDANCES doesn't declare it — say whether it works on a Go-to-file result`
    );
  }
});

test("PARITY: every declared affordance either works on a RESULT, or says why not", () => {
  for (const a of ROW_AFFORDANCES) {
    if (a.results) continue;
    assert.ok(
      a.reason && a.reason.trim().length > 20,
      `"${a.affordance}" is declared listing-only with no real reason — "we forgot" is what this test exists to catch`
    );
  }
});

test("PARITY: a RESULT-sourced target gets the same menu as a LISTING-sourced one", () => {
  // Not "a menu" — the SAME menu. The model must not treat a result as a lesser row, which
  // is exactly the assumption that produced three rounds of bugs.
  const fromListing: OpTarget = { ...fileTarget, from: "listing" };
  const fromResults: OpTarget = { ...fileTarget, from: "results" };

  const kinds = (t: OpTarget) =>
    menuActions(buildContextMenu(t, WIN, HASH_ALGOS))
      .map((a) => a.kind)
      .sort();
  assert.deepEqual(kinds(fromResults), kinds(fromListing));

  // …and every action still carries the result's own path.
  for (const a of menuActions(buildContextMenu(fromResults, WIN, HASH_ALGOS))) {
    if ("target" in a) assert.equal(a.target.rel, fileTarget.rel);
  }
});

test("PARITY: the affordances declared results:true are all actually offered on a result", () => {
  // The registry could lie in the other direction — claiming parity it doesn't have. Cross-
  // check it against what the menu really builds for a result-sourced target.
  //
  // The sample is every row SHAPE the menu branches on, not one row: `workflow-pane` (#222)
  // is offered only on a .yml/.yaml row, so checking a lone .pdf would fail it for being
  // conditional rather than for lacking parity — and parity here is a claim about the
  // RESULTS VIEW ("does this work when the row came from Go-to-file?"), never about file
  // types. Both rows below are result-sourced, which is the thing under test.
  const rows: OpTarget[] = [fileTarget, { ...fileTarget, rel: "a/flow.yml", name: "flow.yml" }];
  const offered = new Set(
    rows.flatMap((t) => menuActions(buildContextMenu(t, WIN, HASH_ALGOS)).map((a) => a.kind))
  );
  for (const a of ROW_AFFORDANCES) {
    if (!a.results) continue;
    // Some affordances are row CHROME, not commands — the right-click itself, the inline
    // editor a command mounts, the busy marker (#216). The registry SAYS so (`menuItem`);
    // it is not a list of names buried in this test, which is where the next one would get
    // quietly buried too.
    if (!a.menuItem) continue;
    assert.ok(
      offered.has(a.affordance),
      `ROW_AFFORDANCES claims "${a.affordance}" works on a result, but the menu doesn't offer it`
    );
  }
});

test("PARITY: an affordance declared menuItem:false has to be one of the known chrome kinds", () => {
  // Otherwise `menuItem: false` becomes the new hiding place: declare it, skip the
  // cross-check, never wire it to a result. Chrome is a short, closed list — a NEW row
  // affordance is a command until someone argues otherwise, right here.
  const CHROME: RowAffordance[] = ["context-menu", "inline-edit", "busy-state"];
  for (const a of ROW_AFFORDANCES) {
    if (a.menuItem) continue;
    assert.ok(
      CHROME.includes(a.affordance),
      `"${a.affordance}" claims not to be a menu item — if it is a COMMAND, it must appear in the menu (and on a result); if it is chrome, say so here`
    );
  }
});

test("a SYMLINK's row actions are all greyed with a reason — it is shown, and inert", () => {
  // Every backend op refuses a link (ensure_no_symlink lstats the final component too), so
  // offering six items that all end in the same toast would be a menu that lies. Same
  // courtesy the folder already got: disabled, with the reason on the tooltip.
  const menu = buildContextMenu(linkTarget, WIN, HASH_ALGOS);
  for (const label of [
    "Open (default app)",
    "Open with…",
    "Reveal in file explorer",
    "Open in file editor pane",
    "Rename…",
    "Delete (to Recycle Bin)",
    "Hash",
  ]) {
    const item = find(menu, label)!;
    assert.ok(item, `${label} should still be listed`);
    assert.equal(item.disabled, true, `${label} must be disabled on a symlink`);
    assert.match(item.reason!, /symlink/i, `${label} must say WHY`);
  }
  // No Hash submenu to walk into, and New is still available (it acts on the directory).
  assert.equal(find(menu, "Hash")!.children, undefined);
  assert.ok(find(menu, "New"));
});

test("menuActions walks submenus, so nothing offered is unreachable", () => {
  const kinds = menuActions(buildContextMenu(fileTarget, WIN, HASH_ALGOS)).map((a) => a.kind);
  assert.deepEqual(new Set(kinds), new Set([
    "open",
    "open-with",
    "reveal",
    "edit-pane",
    "rename",
    "delete",
    "hash",
    "new-folder",
    "new-file",
  ]));
});

// ---------- open in an editor pane (#217) ----------

test("a FILE offers 'Open in file editor pane', bound to that file's path", () => {
  // The in-app answer to Open, which hands the file to the OS default app: a .png belongs
  // in an image viewer, a .ts belongs in loomux's editor. The action carries the row's
  // PATH (bound at menu-open), so the pane it opens edits the file that was clicked —
  // not whatever the list has re-ranked to by the time the item is chosen.
  const menu = buildContextMenu(fileTarget, WIN, HASH_ALGOS);
  const item = must(menu, "Open in file editor pane");
  assert.ok(!item.disabled);
  assert.deepEqual(item.action, { kind: "edit-pane", target: fileTarget });
});

test("a FOLDER offers it too, and the label says what it will actually do", () => {
  // An editor pane is rooted at a DIRECTORY, so "open this folder in an editor pane" is
  // the same action with nothing to open in it. The label admits that rather than
  // pretending a folder can be edited — and the item isn't disabled, because it works.
  const menu = buildContextMenu(dirTarget, WIN, HASH_ALGOS);
  assert.equal(find(menu, "Open in file editor pane"), undefined);
  const item = must(menu, "Open folder in editor pane");
  assert.ok(!item.disabled);
  assert.deepEqual(item.action, { kind: "edit-pane", target: dirTarget });
});

// ---------- open in a workflow pane (#222) ----------

test("a YAML file offers 'Open in workflow pane', bound to that file's path", () => {
  // The workflow pane reads a .yml as a WORKFLOW — blocks, edges, the merge gate — rather
  // than as text. Like every other command, the action carries the row's PATH (bound at
  // menu-open), so it opens the file that was clicked.
  const yaml: OpTarget = { rel: ".loomux/workflow.yml", name: "workflow.yml", isDir: false, isSymlink: false, from: "listing" };
  const item = must(buildContextMenu(yaml, WIN, HASH_ALGOS), "Open in workflow pane");
  assert.ok(!item.disabled);
  assert.deepEqual(item.action, { kind: "workflow-pane", target: yaml });

  // …and from a Go-to-file RESULT too — a path is a path wherever it was clicked from.
  const fromResults: OpTarget = { ...yaml, from: "results" };
  assert.deepEqual(must(buildContextMenu(fromResults, WIN, HASH_ALGOS), "Open in workflow pane").action, {
    kind: "workflow-pane",
    target: fromResults,
  });
});

test("only a YAML file offers it — no other row can have a workflow in it", () => {
  // An item that appeared on every file and then told you the file has no workflow in it
  // would be a menu that wastes a click to say no.
  assert.equal(find(buildContextMenu(fileTarget, WIN, HASH_ALGOS), "Open in workflow pane"), undefined);
  assert.equal(find(buildContextMenu(dirTarget, WIN, HASH_ALGOS), "Open in workflow pane"), undefined);
  const yml: OpTarget = { ...fileTarget, rel: "ci/deploy.yaml", name: "deploy.yaml" };
  assert.ok(find(buildContextMenu(yml, WIN, HASH_ALGOS), "Open in workflow pane"));
});

test("a SYMLINK's workflow item is offered but disabled, like every other op on it", () => {
  // Consistency with the symlink rule: every op is refused (the backend lstats the final
  // component), and the menu says why rather than offering six items that all end in the
  // same toast.
  const link: OpTarget = { rel: "link.yml", name: "link.yml", isDir: false, isSymlink: true, from: "listing" };
  const item = must(buildContextMenu(link, WIN, HASH_ALGOS), "Open in workflow pane");
  assert.equal(item.disabled, true);
  assert.match(item.reason!, /symlink/i);
});

test("PARITY: the workflow item is declared in ROW_AFFORDANCES", () => {
  // The generic parity test above walks a .pdf target, where this item correctly does not
  // appear — so the registry cross-check for it has to be made on a row that CAN offer it,
  // or the declaration requirement would quietly not apply to the one kind that is
  // conditional.
  const yaml: OpTarget = { ...fileTarget, rel: "a.yml", name: "a.yml" };
  const offered = new Set(menuActions(buildContextMenu(yaml, WIN, HASH_ALGOS)).map((a) => a.kind));
  assert.ok(offered.has("workflow-pane"));
  const declared = new Set(ROW_AFFORDANCES.map((a) => a.affordance));
  for (const kind of offered) {
    if (kind === "new-folder" || kind === "new-file") continue;
    assert.ok(declared.has(kind as RowAffordance), `the menu offers "${kind}" but ROW_AFFORDANCES doesn't declare it`);
  }
});

// ---------- a delete in flight (#216) ----------

test("while a delete is in flight, the menu greys the ops that would mutate the tree", () => {
  const busy = 'Deleting "node_modules" — wait for that to finish first.';
  const items = buildContextMenu(fileTarget, WIN, HASH_ALGOS, busy);
  for (const label of ["Rename…", "Delete (to Recycle Bin)"]) {
    const item = must(items, label);
    assert.ok(item.disabled, `${label} must be greyed while a delete runs`);
    assert.equal(item.reason, busy, "and say what it's waiting on");
  }
  const created = must(items, "New").children!;
  assert.ok(created.every((c) => c.disabled && c.reason === busy), "New → Folder/File too");
});

test("…but Open, Reveal, Hash and open-in-editor-pane stay live — they read, they don't write", () => {
  // The same rule the op-state machine enforces: freezing the read paths would be the
  // main-thread freeze all over again, just moved into the menu. Opening the file in an
  // editor pane (#217) reads it into a new pane — it mutates nothing here, so a delete
  // running elsewhere in the tree is no reason to withhold it.
  const items = buildContextMenu(fileTarget, WIN, HASH_ALGOS, "Deleting…");
  for (const label of [
    "Open (default app)",
    "Open with…",
    "Reveal in file explorer",
    "Open in file editor pane",
  ]) {
    assert.ok(!must(items, label).disabled, `${label} must stay usable during a delete`);
  }
  assert.ok(must(items, "Hash").children!.every((c) => !c.disabled), "hashing stays live");
});

test("a symlink's reason beats the busy reason — the permanent fact is the useful one", () => {
  const items = buildContextMenu(linkTarget, WIN, HASH_ALGOS, "Deleting…");
  assert.match(must(items, "Rename…").reason!, /symlink/);
});

test("the empty-space menu is greyed too — a delete blocks creates wherever they're invoked", () => {
  const items = buildContextMenu(null, WIN, HASH_ALGOS, "Deleting…");
  assert.ok(must(items, "New").children!.every((c) => c.disabled));
});

test("no busy reason means nothing is greyed by it — idle is the default", () => {
  for (const idle of [undefined, null, ""]) {
    const items = buildContextMenu(fileTarget, WIN, HASH_ALGOS, idle as string | null | undefined);
    assert.ok(!must(items, "Rename…").disabled);
  }
});

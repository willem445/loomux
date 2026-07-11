// Pure context-menu model for the file-manager pane (#214). DOM-free — the menu's
// SHAPE (what appears, what's enabled, and above all WHAT IT ACTS ON) is decided here
// and unit-tested; contextmenu.ts renders it and fileexplorer.ts executes the actions.
//
// THE RULE THIS MODULE EXISTS TO OBEY. A context menu is the identity-vs-index trap
// with a longer fuse: it is built at right-click and acted on some seconds later, after
// the user has read it, moved the mouse, maybe let a streaming index batch re-rank the
// list underneath. If it resolved its target when you CLICKED AN ITEM, it would be
// resolving against a list that has had every opportunity to change.
//
// So the menu binds an `OpTarget` — a row's PATH — at menu-OPEN, and every action it
// fires carries that value. It is the same discipline the toolbar buttons follow
// (`activeTarget`), and the ops it hands off to are the same ops (`editMountFor`,
// `mountBlocker`, the same fm_* calls). The menu is a second way to *reach* the op
// layer, never a second copy of it.

import type { OpTarget } from "./fileexplorermodel";
import type { HashAlgo } from "./filehashmodel";

/** One entry of the Hash → submenu. Passed IN rather than imported, because a
 *  node:test'd module cannot value-import another src module (Node's type-stripping
 *  loader won't resolve an extensionless relative TS import — see every other pure
 *  module here). It also leaves the menu model independent of the hash module, which is
 *  the right layering anyway: this decides the menu's SHAPE, not what hashing means. */
export interface AlgoChoice {
  algo: HashAlgo;
  label: string;
}

/** What the OS can actually do here — from `fm_capabilities`. Items that would always
 *  fail are HIDDEN rather than shown-and-broken. */
export interface FmCaps {
  /** "recycle" | "permanent" — what Delete will do. */
  delete_mode: "recycle" | "permanent";
  /** Is there an OS "Open with" chooser? (Windows only.) */
  open_with: boolean;
  /** Can we reveal in the OS file manager at all? */
  reveal: boolean;
  /** …and does that reveal actually SELECT the entry? (Not on Linux.) */
  reveal_selects: boolean;
}

/** Everything a menu item can do. Each carries the target it was BOUND to, so an action
 *  is a complete instruction — there is nothing left to look up when it fires. */
export type MenuAction =
  | { kind: "open"; target: OpTarget }
  | { kind: "open-with"; target: OpTarget }
  | { kind: "reveal"; target: OpTarget }
  /** Open an EDITOR pane beside this one (#217): rooted at the manager's root with
   *  the clicked FILE open, or rooted at the clicked FOLDER. The in-app counterpart
   *  to `open` — which hands the file to the OS default app and is deliberately not
   *  the same thing (a .png belongs in an image viewer; a .ts belongs here). */
  | { kind: "edit-pane"; target: OpTarget }
  | { kind: "rename"; target: OpTarget }
  | { kind: "delete"; target: OpTarget }
  | { kind: "hash"; target: OpTarget; algo: HashAlgo }
  /** The creates act on the DIRECTORY, not on a row — so they carry no target. */
  | { kind: "new-folder" }
  | { kind: "new-file" };

export interface MenuItem {
  label: string;
  /** Absent on a separator or a submenu parent. */
  action?: MenuAction;
  /** A submenu (Hash →, New →). */
  children?: MenuItem[];
  separator?: boolean;
  /** Disabled items are shown greyed with `reason` as a tooltip — an item that is
   *  *inapplicable* stays visible (so the menu doesn't reshuffle under the cursor),
   *  while an item that is *unsupported on this OS* is omitted entirely. */
  disabled?: boolean;
  reason?: string;
}

const sep: MenuItem = { label: "", separator: true };

/** The "New →" submenu. Present in every menu — on a row and on empty space alike,
 *  because "make something here" is always a sensible thing to want. */
function newSubmenu(busy?: string): MenuItem {
  return {
    label: "New",
    children: [
      { label: "Folder", action: { kind: "new-folder" }, disabled: !!busy, reason: busy },
      { label: "File", action: { kind: "new-file" }, disabled: !!busy, reason: busy },
    ],
  };
}

/** The "Hash →" submenu for `target`. */
function hashSubmenu(target: OpTarget, algos: readonly AlgoChoice[]): MenuItem {
  return {
    label: "Hash",
    children: algos.map(({ algo, label }) => ({
      label,
      action: { kind: "hash", target, algo } as MenuAction,
    })),
  };
}

/** Build the context menu for a right-click.
 *
 *  `target` is the row the user right-clicked, resolved (by `activeTarget`) from the
 *  view on screen and **bound here, at menu-open** — every action below carries it. A
 *  null target means empty space below the rows: no row-scoped item is offered, only
 *  the creates.
 *
 *  Directories get Open (which navigates) and Reveal, but no Open-with and no Hash: a
 *  folder has no default app and no digest, and offering either would be a lie. Those
 *  items stay *visible but disabled* with a reason, so the menu's shape doesn't shift
 *  depending on what you clicked — you learn where things are. */
export function buildContextMenu(
  target: OpTarget | null,
  caps: FmCaps,
  algos: readonly AlgoChoice[],
  /** Set while a long-running op (a delete) is in flight — from `opBlockedReason` (#216).
   *  The MUTATING items are greyed with it, so the menu can't offer a rename of a tree the
   *  shell is halfway through deleting. Everything else stays live: Open, Reveal and Hash
   *  read, they don't write, and blocking them would be the main-thread freeze all over
   *  again, just implemented in the menu instead. */
  busyReason?: string | null
): MenuItem[] {
  const busy = busyReason || undefined;
  if (!target) {
    // Empty space: nothing is selected, so only the directory-scoped actions apply.
    return [newSubmenu(busy)];
  }

  const items: MenuItem[] = [];
  const isDir = target.isDir;

  // A SYMLINK (or Windows junction) is shown and otherwise INERT: the backend refuses every
  // op on it, because `ensure_no_symlink` lstats the final component too. Rather than offer
  // six items that all end in the same toast, grey them all with the reason — the same
  // courtesy a folder already got. (Why the refusal is that broad: a junction pointing
  // outside the root is exactly the shape a recursive Recycle-Bin delete would escape
  // through, so we never hand the shell the chance to ask. See the design note.)
  const inert = target.isSymlink ? "Loomux doesn't follow or modify symlinks — this one is shown, but left alone." : undefined;

  items.push({
    label: isDir ? "Open" : "Open (default app)",
    action: { kind: "open", target },
    disabled: !!inert,
    reason: inert,
  });

  // Open-with is Windows-only: omitted entirely elsewhere rather than shown and broken.
  if (caps.open_with) {
    const why = inert ?? (isDir ? "A folder has no application to open it with." : undefined);
    items.push({
      label: "Open with…",
      action: { kind: "open-with", target },
      disabled: !!why,
      reason: why,
    });
  }

  if (caps.reveal) {
    // Say what it will actually do. On Linux it opens the containing folder and selects
    // nothing — the label admits that instead of promising a selection we can't make.
    items.push({
      label: caps.reveal_selects ? "Reveal in file explorer" : "Open containing folder",
      action: { kind: "reveal", target },
      disabled: !!inert,
      reason: inert,
    });
  }

  // Open it in loomux's OWN editor, in a new pane beside this one (#217). Offered for a
  // folder too — an editor pane is rooted at a directory, so "open this folder in an
  // editor pane" is the same action with nothing to open in it, and the label says so
  // rather than pretending a folder can be edited. Only a symlink is refused, for the
  // same reason everything else is: we don't follow them.
  items.push({
    label: isDir ? "Open folder in editor pane" : "Open in file editor pane",
    action: { kind: "edit-pane", target },
    disabled: !!inert,
    reason: inert,
  });

  items.push(sep);
  // `inert` (a symlink) wins over `busy`: it is a permanent property of the row, and the
  // more specific explanation is the more useful one.
  const mutBlock = inert ?? busy;
  items.push({
    label: "Rename…",
    action: { kind: "rename", target },
    disabled: !!mutBlock,
    reason: mutBlock,
  });
  items.push({
    label: caps.delete_mode === "recycle" ? "Delete (to Recycle Bin)" : "Delete permanently…",
    action: { kind: "delete", target },
    disabled: !!mutBlock,
    reason: mutBlock,
  });

  items.push(sep);
  if (isDir || inert) {
    items.push({
      label: "Hash",
      disabled: true,
      reason: inert ?? "A folder has no file contents to hash.",
    });
  } else {
    items.push(hashSubmenu(target, algos));
  }

  items.push(sep);
  items.push(newSubmenu(busy));
  return items;
}

/** Every action reachable in `items`, flattened through submenus. The menu's own
 *  keyboard/click handling walks the tree, but tests (and any future "is this action
 *  offered?" question) want the flat set. */
export function menuActions(items: readonly MenuItem[]): MenuAction[] {
  const out: MenuAction[] = [];
  for (const item of items) {
    if (item.action) out.push(item.action);
    if (item.children) out.push(...menuActions(item.children));
  }
  return out;
}

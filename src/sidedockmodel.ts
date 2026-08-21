// Pure decisions for the right-side dock (#1020 item 6, #934). DOM-free so
// `node --test` can import it directly — no jsdom, no bundler, no localStorage
// shim. The DOM half is src/sidedock.ts; see doc/design/side-dock.md.
//
// Three questions live here, and they are separate on purpose:
//
//   1. WHERE SHOULD THE DOCK BE POINTED?  (`decideFollow`) — the active pane
//      changed; does the dock re-root, remember for later, or do nothing?
//   2. WHAT SHOULD ONE TAB'S VIEW DO ABOUT IT?  (`decideViewSync`) — the dock's
//      root moved; does this particular view get built, rebuilt, or protected?
//   3. WHAT SURVIVES A RESTART?  (`decodeDockPrefs`/`encodeDockPrefs`).
//
// Splitting 1 from 2 is what keeps the unsaved-work rule in one place. The dock
// re-roots freely; a view decides for itself whether it can follow, and the
// editor is the one that sometimes cannot (see `decideViewSync`).

/** The dock's three tabs, in display order. */
export type DockTab = "git" | "files" | "editor";

export const DOCK_TABS: readonly DockTab[] = ["git", "files", "editor"];

export function isDockTab(v: unknown): v is DockTab {
  return v === "git" || v === "files" || v === "editor";
}

// ---------- geometry ----------

/** Narrowest useful dock: a git diff or a file tree below this is unreadable. */
export const DOCK_MIN_W = 280;

/** Widest the dock may be dragged, absent a window constraint. */
export const DOCK_MAX_W = 900;

/** How much of the workspace the terminal grid always keeps.
 *
 *  Since #1150 the dock DISPLACES the grid rather than covering it (it is a
 *  flex sibling of `#grid-area`, the mirror of `#sessions`), so this number
 *  changed meaning without changing value: it used to be how much of the grid
 *  the dock could never hide, and it is now how much of the row the grid never
 *  gives up. Either way it is the same promise — a panel that can consume its
 *  own host entirely is a way to lose the app, the same reason `TERM_RESERVE_H`
 *  exists in overlaysize.ts on the other axis. The value is duplicated rather
 *  than imported for the reason embedded-panels.md records: a pure module that
 *  imports another pure module has no import spelling that satisfies both `tsc`
 *  and bare `node --test`.
 *
 *  **THE RESERVE IS ENFORCED IN THREE PLACES, none of them redundant.** Two are
 *  CSS, because CSS holds at boot, on a restore from persistence and on every
 *  window resize with no listener to forget:
 *
 *   - `.sidedock`'s `max-width` bounds what the dock ASKS FOR. It used to be
 *     applied only on the drag path, so a width persisted on a wide monitor came
 *     back whole on a narrow one and covered every pane (#1097 rev-767 B2).
 *   - `#grid-area`'s `min-width` is the floor the grid KEEPS. It exists because
 *     the max-width above is measured against the whole workspace and cannot see
 *     `#sessions` taking 344px of the same row; with both panels open on a
 *     narrow window, the grid — and not the panel — is what would vanish.
 *
 *  The third is `dockBoxes`/`clampDockWidth` here, against the room the dock and
 *  the grid actually SHARE. CSS decides what is rendered; this decides what is
 *  persisted and how wide the panel inside the column is laid out, and neither
 *  of those is something a stylesheet can answer. Passing the workspace's width
 *  instead was a real defect (#1189 review, finding 2): every width between
 *  `room - 240` and `workspace - 240` is one the row can never seat, so a drag
 *  through that gap persisted a number the layout silently ignored.
 *
 *  `test/sidedockmodel.test.ts` pins both stylesheet copies against this
 *  constant, so the mirror cannot drift. */
export const DOCK_TERM_RESERVE_PX = 240;

/**
 * Clamp a dock width to something both readable and non-takeable.
 *
 * `roomPx` is **the width the dock and the terminal grid SHARE** — not the
 * workspace's own width, which is a different and larger number whenever
 * `#sessions` (344px, `flex: none`) is open beside them. That distinction is
 * the whole point of the parameter since #1150 made the dock displace: the
 * workspace-wide bound accepts widths the row can never seat, so a drag past
 * that point stops tracking the pointer while the number being persisted keeps
 * climbing, and the next boot restores a width the layout silently ignores
 * (review of #1189, finding 2). The caller measures it as
 * `#grid-area` + `.sidedock`, which is invariant to where the divider between
 * them sits and therefore stable for the length of a drag.
 *
 * The default leaves only the absolute bounds, for callers reasoning about a
 * persisted value before anything has been measured. A non-finite input (a
 * corrupt pref, a measurement taken while the window was hidden) falls back to
 * the default width rather than propagating `NaN` into a style property, where
 * it would silently collapse the dock to zero.
 */
export function clampDockWidth(px: number, roomPx = Number.POSITIVE_INFINITY): number {
  if (!Number.isFinite(px)) return DEFAULT_DOCK_PREFS.width;
  const roomy = Number.isFinite(roomPx)
    ? Math.max(DOCK_MIN_W, roomPx - DOCK_TERM_RESERVE_PX)
    : DOCK_MAX_W;
  const max = Math.min(DOCK_MAX_W, roomy);
  return Math.round(Math.max(DOCK_MIN_W, Math.min(max, px)));
}

/** The dock's two widths, plus the verdict that can zero the first of them. */
export interface DockBoxes {
  /** What the flex row gives the dock — `0` while it is closed OR starved. */
  columnPx: number;
  /** The absolutely-positioned inner panel's width, which the column clips. */
  contentPx: number;
  /** True when the row cannot seat a readable dock beside the grid's reserve. */
  starved: boolean;
}

/**
 * The widths the DOM sets when the dock opens, closes, is dragged, or the room
 * around it changes (#1150).
 *
 * Handed back together, the same way `resizeburst.ts`'s `FitPlan` returns its
 * delay and burst start together: they are parts of one decision, and a caller
 * that could set them from separate calls could set them inconsistently.
 *
 * **`columnPx` is 0 when closed, and that is the whole feature.** The dock is a
 * flex sibling of `#grid-area` now, so the panes get their space back only if
 * the dock's column actually reaches zero. It has to get there by ANIMATING —
 * `display: none` would return the space in one jump, with no burst for
 * `resizeburst.ts` to coalesce and no autosize for the human to see.
 *
 * **`contentPx` is NOT 0 when closed, and that is not an oversight.** The panel
 * inside the column keeps its width throughout, so a toggle slides a fixed-size
 * panel behind a moving clip instead of re-laying-out a git graph or a file tree
 * at fifteen intermediate widths. It is also what makes the reopen cheap: the
 * contents are already at the width they will be shown at.
 *
 * **`starved` is the floor, and it exists because the fixed-width panel above
 * cannot adapt to a column narrower than itself — it can only be cropped by
 * one** (review of #1189, finding 3). With `#sessions` open the row gives the
 * dock `room - 240`, which falls under `DOCK_MIN_W` for any room below 520 — a
 * half-screen window on a 1366 laptop. Rather than render the right-hand sliver
 * of a panel laid out for a width it does not have, the dock takes no column at
 * all: readable, or absent. The caller says so on the toggle button, because a
 * control that silently does nothing is the other way to look broken.
 *
 * The direction of the trade is not in question — the grid keeps its reserve and
 * the dock is what yields (doc/design/side-dock.md). What this adds is a point
 * at which yielding stops being a cropped strip.
 *
 * `roomPx` is the width the dock and the grid SHARE (see `clampDockWidth`). A
 * missing or unmeasurable one — boot, before the window has been laid out —
 * fails OPEN: an unmeasured room is not evidence of a small one, and hiding the
 * dock on it would be a worse failure than showing it and correcting on the
 * first real measurement.
 */
export function dockBoxes(open: boolean, width: number, roomPx?: number): DockBoxes {
  const room =
    typeof roomPx === "number" && Number.isFinite(roomPx) && roomPx > 0
      ? roomPx
      : Number.POSITIVE_INFINITY;
  const starved = room - DOCK_TERM_RESERVE_PX < DOCK_MIN_W;
  const contentPx = clampDockWidth(width, room);
  return { columnPx: open && !starved ? contentPx : 0, contentPx, starved };
}

// ---------- roots ----------

/**
 * Canonicalize a working directory for comparison and display, or null when
 * there is not one.
 *
 * Only trailing separators are touched, because that is the one difference the
 * dock's own sources genuinely produce: a pane's launch cwd and the OSC 7 path
 * a shell reports for the same folder can disagree by a trailing slash, and
 * treating those as two roots would rebuild every view for no reason. A
 * filesystem root keeps exactly one separator (`C:\`, `/`) — stripping it would
 * turn a real path into a drive letter or the empty string.
 *
 * Deliberately NOT case-folded: doing so would bake a Windows assumption into
 * product code (CLAUDE.md constraint 8), and the cost of being wrong is one
 * redundant rebuild, not a defect.
 */
export function normalizeDockRoot(raw: string | null | undefined): string | null {
  if (typeof raw !== "string") return null;
  const trimmed = raw.trim();
  if (trimmed === "") return null;
  let end = trimmed.length;
  while (end > 0 && (trimmed[end - 1] === "/" || trimmed[end - 1] === "\\")) end--;
  const stripped = trimmed.slice(0, end);
  if (stripped === "") return trimmed.slice(0, 1); // "/" or "\" — a POSIX/UNC root
  if (/^[A-Za-z]:$/.test(stripped)) return `${stripped}\\`; // "C:" is a drive, "C:\" is a root
  return stripped;
}

// ---------- 1. where the dock points ----------

export type FollowAction =
  /** Nothing to do: the dock is closed, there is no cwd, or it is already there. */
  | { kind: "none" }
  /** Re-root now. */
  | { kind: "adopt"; root: string };

export interface FollowInput {
  /** Is the dock currently open? */
  open: boolean;
  /** The root the dock is pointed at now, null before it has ever adopted one. */
  dockRoot: string | null;
  /** The active pane's working directory, read at the moment of the signal. */
  paneCwd: string | null;
}

/**
 * A follow signal arrived — should the dock re-root, and where?
 *
 * Three rules, and each is here rather than as an `if` in the DOM layer because
 * each is a promise the docs make:
 *
 * **A pane with no local cwd never blanks the dock.** An SSH pane reports no
 * local path at all (`Pane.isSshPane` refuses OSC 7 outright — the path names a
 * folder on the far end), and a welcome pane has none yet. Clicking one of
 * those is not a request to empty the sidebar; the dock keeps the last real
 * root it had.
 *
 * **A closed dock does nothing at all** — not even bookkeeping. It deliberately
 * remembers no "pending" root: opening the dock runs this same decision against
 * the *live* cwd, which is strictly more accurate than replaying a root that was
 * current several minutes ago. That also keeps every state this function can
 * return reachable from the real flow, which an earlier `park` action was not
 * (#1097 rev-767 N3: it recorded a root that made the next call a no-op, so the
 * `adopt` its own test witnessed could never actually occur).
 *
 * **The same folder is never re-adopted.** Re-rooting disposes and rebuilds a
 * view, so an equality check is the difference between a signal being free and
 * a signal costing the human their place in a file tree.
 */
export function decideFollow(i: FollowInput): FollowAction {
  if (!i.open) return { kind: "none" };
  const next = normalizeDockRoot(i.paneCwd);
  if (next === null) return { kind: "none" };
  if (next === normalizeDockRoot(i.dockRoot)) return { kind: "none" };
  return { kind: "adopt", root: next };
}

/**
 * Is a tab-manager notification an ACTIVE-TAB change — the only kind the dock
 * may follow?
 *
 * `TabManager.onChange` is a tab-**set** listener, not an active-tab one. Its
 * `emit()` fires from `addTab`, `closeTab`, `switchTo`, `renameTab`, `setColor`,
 * `moveTab`, `setTabAttention` (every time a background agent's attention state
 * flips) and `touch()` (orch-channel traffic). Subscribing to it directly and
 * re-rooting on every notification is a real defect, and it was this one
 * (#1097 rev-767 B1): the dock re-reads the active pane's *live* cwd, so a
 * `cd` the human typed minutes ago — correctly ignored at the time — would get
 * adopted later, at whatever unrelated moment some other tab flipped its
 * attention chip. That silently rebuilt the file explorer out from under them
 * and closed a clean editor file, at a moment nothing they did caused.
 *
 * The dock therefore follows a tab notification only when the active tab id
 * genuinely moved. Comparing ids (rather than trusting the event) is what makes
 * "switching project tabs" mean exactly that, and it is the entire fix: every
 * other emit source leaves the id alone.
 */
export function isActiveTabChange(prevTabId: string | null, nextTabId: string | null): boolean {
  return prevTabId !== nextTabId;
}

/**
 * Is a workspace's pane-active change one the dock may follow — i.e. did it
 * happen in the FOREGROUND tab?
 *
 * `Grid.setActive`'s `onActive` callback is wired for **every** workspace, one
 * per project tab, because every grid has one. Only the foreground tab's may
 * move the dock, and the reason is the same one that governs `isActiveTabChange`
 * above: a follow re-reads the active pane's **live** cwd, so *when* it runs is
 * as load-bearing as *what* it reads.
 *
 * This closed the second door on the same defect (#1097 rev-776). The first
 * revision took no workspace argument at all and justified it by saying the dock
 * "reads the active pane itself rather than trusting the pane the event fired
 * for". That is true and it is not sufficient — reading the *right* pane at the
 * *wrong* moment is the whole bug. A background tab opens or closes a pane (an
 * agent finishing, a delegate spawning, a group resuming), its grid calls
 * `setActive` on the survivor, and the dock — pointed at the foreground pane all
 * along — re-reads *that* pane's cwd and adopts a `cd` the human typed earlier
 * and had every reason to think was ignored. Explorer rebuilt, clean editor file
 * closed, at a moment nothing they did caused, dependent on whether some other
 * tab's agent happened to be busy.
 *
 * A null `activeTabId` (before the first tab exists) follows nothing, and that
 * falls out of the comparison rather than needing a guard of its own: no real
 * workspace id is null. An explicit `activeTabId !== null` clause was written
 * here first and then removed — mutating it away reddened nothing, which makes
 * it a guard that guards nothing and a claim the code does not keep.
 */
export function followsPaneChange(workspaceId: string, activeTabId: string | null): boolean {
  return workspaceId === activeTabId;
}

// ---------- 2. what one tab's view does about it ----------

export type ViewSync =
  /** Already correct, or there is no root to show yet. */
  | "none"
  /** Never constructed — construct it at the dock's root. */
  | "build"
  /** Constructed at a stale root — tear it down and rebuild at the new one. */
  | "rebuild"
  /** Stale, but rebuilding would destroy unsaved work. Leave it alone. */
  | "hold";

export interface ViewSyncInput {
  /** Where the dock is pointed. */
  dockRoot: string | null;
  /** The root this view was constructed at, or null if it never has been. */
  builtRoot: string | null;
  /** Does this view hold unsaved edits? Always false for git and files. */
  dirty: boolean;
}

/**
 * Whether the view behind one tab may follow the dock's root.
 *
 * **Why `rebuild` and not "re-root".** None of the three hosted views exposes a
 * public setter for its root — `GitView` and `FileExplorerView` pull it through
 * a `getCwd()`/`getRoot()` callback and re-read it on their next refresh, while
 * `FileEditView` latches it on first `show()` and never re-reads it. Dispose
 * and reconstruct is the one operation that is correct for all three, and it is
 * also the only one that drops the caches a re-root would otherwise strand
 * (`FileExplorerView`'s go-to-file index and content hashes are invalidated on
 * its OWN picker path only). So the dock uses it uniformly rather than three
 * per-view re-root recipes, two of which nothing would ever test.
 *
 * **`hold` is the price of that, and it is the point.** Reconstructing a
 * `FileEditView` throws its buffer away. Doing so because the human clicked a
 * different pane would destroy work they never agreed to lose — precisely the
 * rule #219 exists to state — so a dirty editor simply stops following, keeps
 * its file, and says so. It resumes on the next sync after it goes clean
 * (saving, or discarding, both reach that), which is why the dock re-asks this
 * question every time a tab is activated and not only when the root moves.
 *
 * **A missing root leaves the view alone rather than tearing it down.** `none`
 * on a null `dockRoot` means the app can boot with the dock open and no active
 * cwd yet without flickering a view into existence and back out.
 */
export function decideViewSync(i: ViewSyncInput): ViewSync {
  const want = normalizeDockRoot(i.dockRoot);
  if (want === null) return "none";
  if (i.builtRoot === null) return "build";
  if (normalizeDockRoot(i.builtRoot) === want) return "none";
  if (i.dirty) return "hold";
  return "rebuild";
}

// ---------- 3. what survives a restart ----------

export interface DockPrefs {
  open: boolean;
  tab: DockTab;
  width: number;
}

/** Closed by default. The reason survived #1150 changing what an open dock DOES
 *  to the grid — it used to cover panes, it now shrinks them — because the
 *  reason was never about which of the two it is: a first run should hand the
 *  human a full-width terminal, not a panel they have not asked for and a grid
 *  already narrowed to make room for it. Git first because it is the tab that
 *  most wants a folder it did not have to choose. */
export const DEFAULT_DOCK_PREFS: DockPrefs = { open: false, tab: "git", width: 420 };

/** Where the prefs live. UI chrome state is localStorage in this codebase (the
 *  `loomux.*` convention `agents.ts`, `editor.ts` and `gitlayout.ts` already
 *  use); the backend settings file is for durable app/session config. */
export const DOCK_PREFS_KEY = "loomux.sidedock";

/**
 * Read the persisted prefs back, tolerating anything.
 *
 * **Field-wise, not record-wise.** A malformed `tab` costs the human their tab
 * choice and nothing else — `open` and `width` still survive. That is the same
 * leniency `tabstore.decodePane` applies to a persisted embed (drop the bad
 * entry, keep the array), and the reason is the same: the alternative silently
 * discards a whole preference on the next boot after a stray hand-edit or a
 * version that wrote one extra field.
 *
 * Total by construction — it never throws and never returns a partial record,
 * so no caller needs a try/catch or a `??` chain around it.
 */
export function decodeDockPrefs(raw: string | null): DockPrefs {
  if (raw === null || raw === "") return { ...DEFAULT_DOCK_PREFS };
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return { ...DEFAULT_DOCK_PREFS };
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    return { ...DEFAULT_DOCK_PREFS };
  }
  const o = parsed as Record<string, unknown>;
  return {
    open: typeof o.open === "boolean" ? o.open : DEFAULT_DOCK_PREFS.open,
    tab: isDockTab(o.tab) ? o.tab : DEFAULT_DOCK_PREFS.tab,
    // No live window width is available here, so this bounds the value to
    // [DOCK_MIN_W, DOCK_MAX_W] only. The workspace reserve is NOT applied on
    // this path and cannot be — the stylesheet's `max-width` is what keeps a
    // restored width from covering the grid (see DOCK_TERM_RESERVE_PX).
    width: typeof o.width === "number" ? clampDockWidth(o.width) : DEFAULT_DOCK_PREFS.width,
  };
}

export function encodeDockPrefs(p: DockPrefs): string {
  return JSON.stringify({ open: p.open, tab: p.tab, width: clampDockWidth(p.width) });
}

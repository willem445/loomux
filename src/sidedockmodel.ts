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

/** How much of the workspace the dock must always leave uncovered.
 *
 *  The dock is an OVERLAY, not a flex sibling (CLAUDE.md constraint 1 — see
 *  doc/design/side-dock.md), so nothing about the terminal grid pushes back on
 *  it: a wide enough dock would simply hide every pane while the panes carried
 *  on rendering underneath at full size. This is the same reserve idea as
 *  `TERM_RESERVE_H` in overlaysize.ts, on the other axis, and for the same
 *  reason — an overlay that can cover its own host entirely is a way to lose
 *  the app. The value is duplicated rather than imported for the reason
 *  embedded-panels.md records: a pure module that imports another pure module
 *  has no import spelling that satisfies both `tsc` and bare `node --test`. */
export const DOCK_TERM_RESERVE_PX = 240;

/**
 * Clamp a dock width to something both readable and non-covering.
 *
 * `availablePx` is the workspace's own width when it is known; the default
 * leaves only the absolute bounds, for callers reasoning about a persisted
 * value before the window has been measured. A non-finite input (a corrupt
 * pref, a measurement taken while the window was hidden) falls back to the
 * default width rather than propagating `NaN` into a style property, where it
 * would silently collapse the dock to zero.
 */
export function clampDockWidth(px: number, availablePx = Number.POSITIVE_INFINITY): number {
  if (!Number.isFinite(px)) return DEFAULT_DOCK_PREFS.width;
  const roomy = Number.isFinite(availablePx)
    ? Math.max(DOCK_MIN_W, availablePx - DOCK_TERM_RESERVE_PX)
    : DOCK_MAX_W;
  const max = Math.min(DOCK_MAX_W, roomy);
  return Math.round(Math.max(DOCK_MIN_W, Math.min(max, px)));
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
  /** Nothing to do: no cwd to follow, or the dock is already there. */
  | { kind: "none" }
  /** The dock is CLOSED — record the root and build nothing. */
  | { kind: "park"; root: string }
  /** Re-root now. */
  | { kind: "adopt"; root: string };

export interface FollowInput {
  /** Is the dock currently open? */
  open: boolean;
  /** The root the dock is pointed at now (or the parked one), null at boot. */
  dockRoot: string | null;
  /** The newly-active pane's working directory. */
  paneCwd: string | null;
}

/**
 * The dock followed the active pane somewhere — now what?
 *
 * Two rules carry the weight, and both are the reason this is a function rather
 * than an `if` in the DOM layer:
 *
 * **A pane with no local cwd never blanks the dock.** An SSH pane reports no
 * local path at all (`Pane.isSshPane` refuses OSC 7 outright — the path names a
 * folder on the far end), and a welcome pane has none yet. Clicking one of
 * those is not a request to empty the sidebar; the dock keeps showing the last
 * real root it had.
 *
 * **A closed dock does no work.** `park` is the whole of the "only while the
 * dock is open" requirement: the root is remembered so opening the dock lands
 * on the right folder, and not one view is constructed or refreshed in the
 * meantime. Re-running this with `paneCwd = <the parked root>` at open time is
 * how the parked value is redeemed — there is no second entry point.
 */
export function decideFollow(i: FollowInput): FollowAction {
  const next = normalizeDockRoot(i.paneCwd);
  if (next === null) return { kind: "none" };
  if (next === normalizeDockRoot(i.dockRoot)) return { kind: "none" };
  return i.open ? { kind: "adopt", root: next } : { kind: "park", root: next };
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

/** Closed by default: the dock OCCLUDES the grid rather than shrinking it (the
 *  no-PTY-resize trade, doc/design/side-dock.md), so it may not cover a pane on
 *  first run before anyone has asked for it. Git first because it is the tab
 *  that most wants a folder it did not have to choose. */
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
    width: typeof o.width === "number" ? clampDockWidth(o.width) : DEFAULT_DOCK_PREFS.width,
  };
}

export function encodeDockPrefs(p: DockPrefs): string {
  return JSON.stringify({ open: p.open, tab: p.tab, width: clampDockWidth(p.width) });
}

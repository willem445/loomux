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
 *  has no import spelling that satisfies both `tsc` and bare `node --test`.
 *
 *  **THE RESERVE IS ENFORCED IN CSS, NOT HERE** — `.sidedock`'s `max-width`
 *  (styles.css), which holds at boot, on a restore from persistence, and on
 *  every window resize, none of which run this function. It used to be applied
 *  only on the drag path, so a width persisted on a wide monitor came back
 *  whole on a narrow one and covered every pane (#1097 rev-767 B2). A CSS
 *  bound closes all three at once with no JS and nothing that could reach a
 *  PTY. `clampDockWidth` below still applies it on the drag, so the number
 *  that gets PERSISTED is sane too; `test/sidedockmodel.test.ts` pins the
 *  stylesheet's copy of both constants against these, so the mirror cannot
 *  drift silently. */
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

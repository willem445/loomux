// Pure core of the file-MANAGER pane (issue #214). DOM-free, so the listing order,
// the breadcrumb, the navigation arithmetic, the display formatting and the
// inline-edit state machine are all unit-tested without a DOM
// (test/fileexplorermodel.test.ts). The DOM wiring lives in fileexplorer.ts and is
// hand-validated, per house convention.
//
// The backend (filemgr.rs) deliberately returns directory entries UNSORTED and
// UNFILTERED. Sorting and hiding are product decisions, not facts about the disk,
// so they live here where they can be pinned by tests.

/** One directory entry, exactly as `fm_list` returns it. */
export interface FmEntry {
  name: string;
  is_dir: boolean;
  is_symlink: boolean;
  /** Bytes; 0 for directories and symlinks. */
  size: number;
  /** Last-modified, ms since the Unix epoch; 0 when unknown. */
  modified_ms: number;
  /** Platform-correct hidden flag (Windows attribute bit, or a leading dot). */
  is_hidden: boolean;
}

// ---------- listing order + filter ----------

/** Compare two entries the way every file manager does: **folders first**, then by
 *  name, case-insensitively, with a case-sensitive tiebreak so the order is total
 *  and stable (`README` and `readme` can coexist and must not swap between
 *  listings). `localeCompare` with `numeric` gives `file2` before `file10`, which
 *  is what a human means by "in order".
 *
 *  A symlink sorts with FILES even when it points at a directory: we never follow
 *  it, so as far as this pane is concerned it isn't one. */
export function compareEntries(a: FmEntry, b: FmEntry): number {
  const aDir = a.is_dir && !a.is_symlink;
  const bDir = b.is_dir && !b.is_symlink;
  if (aDir !== bDir) return aDir ? -1 : 1;
  const byName = a.name.localeCompare(b.name, undefined, { sensitivity: "base", numeric: true });
  if (byName !== 0) return byName;
  return a.name < b.name ? -1 : a.name > b.name ? 1 : 0;
}

/** The rows to render: hidden entries dropped unless asked for, then ordered.
 *  Pure — it never mutates `entries`, so the caller's cached listing stays intact
 *  when the hidden toggle flips (no refetch needed). */
export function visibleEntries(entries: readonly FmEntry[], showHidden: boolean): FmEntry[] {
  return entries.filter((e) => showHidden || !e.is_hidden).sort(compareEntries);
}

// ---------- navigation ----------

/** Join a child name onto a `rel` directory, in the forward-slashed `rel`
 *  convention the whole file stack (and the backend) uses. */
export function joinRel(rel: string, name: string): string {
  const base = rel.replace(/^\/+|\/+$/g, "");
  return base ? `${base}/${name}` : name;
}

/** The parent of `rel`, or null when `rel` IS the root ("").
 *
 *  Null is what disables the Up button: this pane is rooted, and navigation is
 *  bounded by that root — you can't climb out of the folder the pane was opened on.
 *  That bound is also what makes the backend's `root` + `rel` containment model
 *  meaningful rather than decorative. */
export function parentRel(rel: string): string | null {
  const base = rel.replace(/^\/+|\/+$/g, "");
  if (base === "") return null;
  const cut = base.lastIndexOf("/");
  return cut < 0 ? "" : base.slice(0, cut);
}

/** One clickable crumb: what to show, and the `rel` it navigates to. */
export interface Crumb {
  label: string;
  rel: string;
}

/** The breadcrumb trail for `rel`, starting at the root (labelled `rootLabel` —
 *  the root folder's own short name, since "" would render as nothing). */
export function breadcrumbs(rootLabel: string, rel: string): Crumb[] {
  const crumbs: Crumb[] = [{ label: rootLabel, rel: "" }];
  const base = rel.replace(/^\/+|\/+$/g, "");
  if (base === "") return crumbs;
  let acc = "";
  for (const seg of base.split("/")) {
    acc = acc ? `${acc}/${seg}` : seg;
    crumbs.push({ label: seg, rel: acc });
  }
  return crumbs;
}

// ---------- display formatting ----------

const UNITS = ["B", "KB", "MB", "GB", "TB"];

/** A file's size, the way a file manager shows it. Directories get "" (a folder's
 *  "size" would mean walking it, which this pane will not do just to fill a
 *  column). */
export function formatSize(entry: FmEntry): string {
  if (entry.is_dir && !entry.is_symlink) return "";
  let n = entry.size;
  let u = 0;
  while (n >= 1024 && u < UNITS.length - 1) {
    n /= 1024;
    u++;
  }
  // Bytes are whole; everything above gets one decimal, dropped when it's .0 —
  // "1.5 MB" is useful, "1.0 MB" is just noise.
  const shown = u === 0 ? String(entry.size) : n.toFixed(1).replace(/\.0$/, "");
  return `${shown} ${UNITS[u]}`;
}

/** A last-modified stamp. `now` is injected rather than read from the clock so this
 *  is deterministic and testable (and because CLAUDE.md keeps `Date.now()` out of
 *  pure modules). 0 → "—": we don't know, and pretending it's 1970 is worse. */
export function formatModified(ms: number, now: number): string {
  if (!ms) return "—";
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  const hhmm = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  // Within the last ~24h, the time is what you actually want to compare on; older
  // than that, the date is.
  if (now - ms < 24 * 3600_000 && now >= ms) return hhmm;
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${hhmm}`;
}

// ---------- what an operation acts on ----------
//
// THE BUG THIS EXISTS TO KILL (human demo, #214). The explorer can be showing one of
// two different row sets: the directory LISTING, or the Go-to-file RESULTS (which
// replace it while a query is active). Ops used to resolve their target from the
// listing's selection index — unconditionally, even while the listing was hidden and
// the results were the thing on screen. So with a filter active, "rename" bound to an
// invisible row in a list nobody was looking at, rendered its editor into the hidden
// listing (hence "the click did nothing"), and then, when the filter cleared, appeared
// on a completely different file. Delete had the identical defect, and delete is not
// reversible with an "oh, that did nothing".
//
// The fix is structural, not a patch: an op resolves an `OpTarget` — a row's *identity*
// (its path) — from the view the user is ACTUALLY LOOKING AT, at the moment it is
// invoked. Once captured, that value is immune to anything that happens to the lists
// underneath it: a streaming index batch re-ranking the results, the filter clearing,
// a refresh reordering the listing. An index is a position in a list that may not even
// be on screen; a path is the file.

/** The row an operation acts on, captured by IDENTITY when the op is invoked. */
export interface OpTarget {
  /** Root-relative path — the identity. Valid no matter what the lists do next. */
  rel: string;
  /** Last path segment: what the confirm dialog and the rename editor show. */
  name: string;
  isDir: boolean;
  /** A symlink (or a Windows junction). Shown in the listing and otherwise INERT: every
   *  backend op refuses it (`ensure_no_symlink` lstats the final component too), so the
   *  menu greys its row actions with a reason rather than offering six things that will
   *  all toast. See the design note's symlink section for why the refusal is that broad. */
  isSymlink: boolean;
  /** Which view it came from. The caller needs this: an op invoked on a RESULTS row
   *  has to leave the filtered view to show the user what it is doing to that file. */
  from: "listing" | "results";
}

/** The two things the explorer can be showing. Ops resolve against whichever one the
 *  user is looking at — conflating them is precisely what went wrong.
 *
 *  BOTH carry `dir`: the directory being browsed is a fact about the pane, not about
 *  which list happens to be on screen. The results view still has one (it is what you
 *  return to when the filter clears), and `editMountFor` needs it. */
export type ExplorerView =
  | { kind: "listing"; dir: string; rows: readonly FmEntry[]; sel: number }
  /** Go-to-file hits. These are files from anywhere under the root, so a hit carries
   *  its own full `rel` — it is NOT relative to the directory being browsed, which is
   *  the other half of why resolving one against `dir` produced nonsense. */
  | { kind: "results"; dir: string; hits: readonly { rel: string }[]; sel: number };

/** The last segment of a root-relative path. */
export function baseName(rel: string): string {
  const cut = rel.lastIndexOf("/");
  return cut < 0 ? rel : rel.slice(cut + 1);
}

/** Resolve what an operation should act on, from the view currently on screen.
 *  Null when nothing is selected (or the selection is stale/out of range) — the
 *  caller disables its buttons on that, so an op can never fire at nothing. */
export function activeTarget(view: ExplorerView): OpTarget | null {
  if (view.kind === "listing") {
    const entry = view.rows[view.sel];
    if (!entry) return null;
    return {
      rel: joinRel(view.dir, entry.name),
      name: entry.name,
      isDir: entry.is_dir && !entry.is_symlink,
      isSymlink: entry.is_symlink,
      from: "listing",
    };
  }
  const hit = view.hits[view.sel];
  if (!hit) return null;
  // A Go-to-file hit is always a plain file: the enumeration lists files, never dirs, and
  // skips symlinks (it must not follow one to somewhere outside the root).
  return { rel: hit.rel, name: baseName(hit.rel), isDir: false, isSymlink: false, from: "results" };
}

// ---------- VIEW PARITY: the results view is not a second-class row list ----------
//
// THREE ROUNDS RUNNING, an affordance was built for the directory LISTING and quietly
// omitted from the Go-to-file RESULTS:
//
//   round 4 — rename resolved its target from the listing's index even while the results
//             were on screen, so it renamed a file the user could not see;
//   round 5 — the fix navigated correctly but mounted its editor into the still-hidden
//             listing, so "F2 does nothing" came back on the very path added to kill it;
//   round 6 — the context menu was wired to the listing's rows and not the results' at
//             all, so right-clicking a result got the webview's default menu.
//
// Each fix was correct. Each time, the NEXT affordance forgot again. That is not three
// bugs, it is one: the results view kept being an afterthought, and nothing in the code
// ever asked "…and does this work there?"
//
// Two guards now, one structural and one declarative:
//
//   * STRUCTURAL — `fileexplorer.wireRowAffordances` is the only place a row's behaviours
//     are attached, and both views call it. A new affordance lands in both by construction.
//   * DECLARATIVE — the registry below. Every row affordance must be listed, and must
//     either work in the results view or say WHY not. The parity test enforces both, and
//     cross-checks the registry against the context menu's own action set — so an
//     affordance added to the menu without an entry here FAILS THE BUILD until someone
//     answers the question for the results view.

/** Everything a ROW can offer. Add a row affordance → add it here (the parity test makes
 *  that non-optional) → state whether the results view has it. */
export type RowAffordance =
  | "open"
  | "open-with"
  | "reveal"
  | "rename"
  | "delete"
  | "hash"
  | "context-menu"
  | "inline-edit";

export interface AffordanceParity {
  affordance: RowAffordance;
  /** Does it work when the row is a Go-to-file RESULT? */
  results: boolean;
  /** REQUIRED when `results` is false. "We forgot" is not a reason; the test only checks
   *  that a reason exists, but a reviewer reads it. */
  reason?: string;
}

export const ROW_AFFORDANCES: readonly AffordanceParity[] = [
  { affordance: "open", results: true },
  { affordance: "open-with", results: true },
  { affordance: "reveal", results: true },
  // Rename from a result works, and is the one that cost two rounds to get right: it exits
  // the filter, navigates to the file's folder, and mounts the editor there (editMountFor).
  { affordance: "rename", results: true },
  { affordance: "delete", results: true },
  { affordance: "hash", results: true },
  { affordance: "context-menu", results: true },
  // The ONE genuine listing-only affordance, and the reason is structural rather than
  // neglect: the inline-edit ROW exists only in the listing. An op that opens one therefore
  // leaves the filter first (`editMountFor.exitFilter`) — so it is still *reachable* from a
  // result, it just can't be hosted there. Hosting it in the results list would also put a
  // focused text input in a list that re-renders on every streaming index batch.
  {
    affordance: "inline-edit",
    results: false,
    reason:
      "The inline-edit row exists only in the listing. Ops that open one leave the filter " +
      "first (editMountFor), so they remain reachable from a result — the editor simply " +
      "cannot be hosted in a list that re-renders on every streaming index batch.",
  },
];

// ---------- where an inline editor is allowed to mount ----------
//
// THE INVARIANT: the inline-edit row exists ONLY in the directory listing. Mounting it
// while the Go-to-file results are on screen puts it inside a `display:none` list — the
// row never appears, and the focus call no-ops on it. That is *exactly* the "F2 does
// nothing" symptom, and it has now been built twice: once by the original index-based
// targeting, and once again by the very code path added to fix that. Fixing the target
// resolution was not enough, because the target was never the whole bug: the other half
// is WHERE the editor lands.
//
// So the rule is stated here, in the pure model, where it can be asserted — instead of
// living as an easily-forgotten `exitFilter()` call at each call site.

/** The view changes an op must make BEFORE it can mount its inline editor. */
export interface EditMount {
  /** The directory whose listing must be showing (the target's own folder). */
  dir: string;
  /** Leave the Go-to-file results first. True whenever they are what's on screen —
   *  INCLUDING when the target already lives in the directory being browsed. That case
   *  is the trap: "only exit the filter if we also have to navigate" looks reasonable
   *  and is wrong, because the listing is hidden either way. */
  exitFilter: boolean;
  /** Fetch a different directory first. False when the target is already in view. */
  navigate: boolean;
}

export function editMountFor(target: OpTarget, view: ExplorerView): EditMount {
  const dir = parentRel(target.rel) ?? "";
  return {
    dir,
    exitFilter: view.kind === "results",
    navigate: dir !== view.dir,
  };
}

/** Why the target's row can't be rendered in the listing — or `ok` when it can.
 *
 *  `openRenameEditor` cannot just assume the row is there. The Go-to-file index reaches
 *  files the listing HIDES: on macOS/Linux that is every tracked dotfile (`.gitignore`,
 *  `.github/…`), on Windows every hidden-attribute file. Renaming `.gitignore` from a
 *  search with **Hidden** off is an ordinary thing to do, and it must not silently mount
 *  no editor — which would also leave `edit` set with no input to Escape from, deadening
 *  the listing's keyboard until some other path happened to reset it. */
export type MountBlock =
  /** The row is present and visible — go ahead. */
  | { kind: "ok" }
  /** It exists, but the Hidden toggle is hiding it. */
  | { kind: "hidden" }
  /** It is gone from disk since the target was captured (an agent deleted it). */
  | { kind: "missing" };

export function mountBlocker(
  target: OpTarget,
  entries: readonly FmEntry[],
  showHidden: boolean
): MountBlock {
  const entry = entries.find((e) => e.name === target.name);
  if (!entry) return { kind: "missing" };
  if (entry.is_hidden && !showHidden) return { kind: "hidden" };
  return { kind: "ok" };
}

// ---------- selection ----------

/** Move a selection index by `delta` within `count` rows, CLAMPING at both ends.
 *
 *  Clamped, not wrapped — unlike the Go-to-file result list (`filematch.moveSelection`,
 *  which wraps because a short result list is a menu you cycle). A directory listing
 *  is a place: holding Down must come to rest on the last row, not silently teleport
 *  you back to the top past a file you meant to land on. -1 means "nothing selected". */
export function clampSelection(current: number, delta: number, count: number): number {
  if (count <= 0) return -1;
  const next = (current < 0 ? (delta > 0 ? -1 : count) : current) + delta;
  return Math.max(0, Math.min(count - 1, next));
}

// ---------- inline edit (new folder / rename) ----------

/** The pane's inline-edit state. Exactly one edit can be in flight, and both kinds
 *  are the same interaction — an input row in the listing with a name in it — so
 *  they are one state machine rather than two flags that can disagree.
 *
 *  `rename` carries the entry's CURRENT name so the model can tell "unchanged" from
 *  "collides with a sibling" (renaming a file to its own name must be a no-op, not
 *  a duplicate-name error). */
export type EditState =
  | { kind: "none" }
  | { kind: "new-folder"; draft: string }
  | { kind: "new-file"; draft: string }
  | { kind: "rename"; rel: string; original: string; draft: string };

export const noEdit: EditState = { kind: "none" };

/** True for the two "make a new thing here" edits. They share every rule (same name
 *  validation, same sibling-collision check, same commit-and-select flow) and differ
 *  only in which backend command runs — so the code branches once, at the call, and
 *  nowhere else. */
export function isCreate(state: EditState): state is
  | { kind: "new-folder"; draft: string }
  | { kind: "new-file"; draft: string } {
  return state.kind === "new-folder" || state.kind === "new-file";
}

/** Why a draft name can't be committed, or null when it can.
 *
 *  This is a UI COURTESY, not a security boundary: the backend's `validate_name` is
 *  authoritative and re-checks everything on commit. What this adds is an answer
 *  *while the user types*, for the mistakes they are actually likely to make —
 *  empty, `.`/`..`, a path separator or other illegal character, a trailing dot —
 *  plus the one rule the backend CANNOT check, because it doesn't know the listing:
 *  a duplicate sibling name.
 *
 *  It is a SUBSET of the backend's rules, deliberately, and the gap is worth naming:
 *  the Windows RESERVED DEVICE NAMES (`con`, `nul`, `aux`, `com1`, `lpt9`, …) are
 *  not checked here. Nobody types `con` by accident, and the list is long, obscure,
 *  and would need a footnote to explain when it fired. So that one is left to the
 *  backend, which refuses it on commit with a toast that says exactly why. Adding it
 *  here would be three lines — the reason not to is that inline errors should cover
 *  the near-misses, not enumerate the trivia.
 *
 *  The duplicate check is case-INSENSITIVE, because that is how the Windows and
 *  macOS filesystems this runs on actually behave: offering to create `Foo` beside
 *  an existing `foo` would just fail at the syscall with a worse message. */
export function nameError(state: EditState, siblings: readonly string[]): string | null {
  if (state.kind === "none") return null;
  const name = state.draft.trim();
  if (name === "") return "Name cannot be empty.";
  if (name === "." || name === "..") return "Name cannot be '.' or '..'.";
  const bad = [...name].find((c) => '/\\:*?"<>|'.includes(c));
  if (bad) return `Name cannot contain '${bad}'.`;
  if (name.endsWith(".")) return "Name cannot end with a dot.";

  // A rename to the entry's own name is a no-op, NOT a collision with itself.
  const original = state.kind === "rename" ? state.original : null;
  if (original !== null && name.toLowerCase() === original.toLowerCase()) return null;
  if (siblings.some((s) => s.toLowerCase() === name.toLowerCase())) {
    return `'${name}' already exists here.`;
  }
  return null;
}

/** Can this edit be committed right now? */
export function canCommit(state: EditState, siblings: readonly string[]): boolean {
  return state.kind !== "none" && nameError(state, siblings) === null;
}

/** A rename whose name didn't actually change: the caller skips the round-trip and
 *  just closes the editor. (The backend tolerates this too — it returns the same
 *  rel rather than an "exists" error — but not making the call at all is better
 *  than making one we know is pointless.) */
export function isNoopRename(state: EditState): boolean {
  return state.kind === "rename" && state.draft.trim() === state.original;
}

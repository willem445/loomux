// DOM-free helpers for the git view's worktree selector (#208). The backend
// hands us the raw `git worktree list --porcelain` output; parsing it — and the
// logic that decides which worktree the view is pointed at (incl. failing soft
// back to the primary when a selected worktree was pruned) — lives here so it
// can be exercised by node:test without a webview. gitview.ts owns only the DOM.

export interface Worktree {
  /** Absolute path git reported for the worktree (native separators as given). */
  path: string;
  /** Short branch name checked out there, or null when detached / bare. */
  branch: string | null;
  /** HEAD commit sha, or null for a bare or unborn worktree. */
  head: string | null;
  /** Detached HEAD (no branch). */
  detached: boolean;
  /** The bare repository entry — has no working tree to inspect. */
  bare: boolean;
  /** Administratively locked (still viewable). */
  locked: boolean;
  /** Git thinks its working dir is gone (pruned on next `git worktree prune`). */
  prunable: boolean;
  /** The main working tree — git lists it first. This is the view's default. */
  primary: boolean;
}

/** Normalize a path for equality: unify separators, drop a trailing slash, and
 *  lowercase (this project's baseline is Windows, where paths are case-folded;
 *  a selected path is stored from git's own output, so this only guards casing
 *  and separator drift between enumerations). */
export function normalizePath(p: string): string {
  const unified = p.replace(/\\/g, "/").replace(/\/+$/, "");
  return unified.toLowerCase();
}

/** Parse `git worktree list --porcelain`. Records are blank-line separated;
 *  each opens with `worktree <path>` followed by attribute lines (`HEAD`,
 *  `branch`, `detached`, `bare`, `locked`, `prunable`). Unknown lines are
 *  ignored so newer git attributes never break the parse. The first record is
 *  the main worktree (`primary`). */
export function parseWorktrees(porcelain: string): Worktree[] {
  const out: Worktree[] = [];
  let cur: Worktree | null = null;
  const flush = () => {
    if (cur) out.push(cur);
    cur = null;
  };
  for (const raw of porcelain.split("\n")) {
    const line = raw.replace(/\r$/, "");
    if (line === "") {
      flush();
      continue;
    }
    if (line.startsWith("worktree ")) {
      flush();
      cur = {
        path: line.slice("worktree ".length),
        branch: null,
        head: null,
        detached: false,
        bare: false,
        locked: false,
        prunable: false,
        primary: out.length === 0,
      };
      continue;
    }
    if (!cur) continue; // attribute before any `worktree` line — malformed, skip
    if (line.startsWith("HEAD ")) {
      cur.head = line.slice("HEAD ".length);
    } else if (line.startsWith("branch ")) {
      // `branch refs/heads/<name>` → short name; keep the full ref if it isn't
      // under refs/heads (shouldn't happen for a worktree, but stay lossless).
      const ref = line.slice("branch ".length);
      cur.branch = ref.startsWith("refs/heads/") ? ref.slice("refs/heads/".length) : ref;
    } else if (line === "detached") {
      cur.detached = true;
    } else if (line === "bare") {
      cur.bare = true;
    } else if (line === "locked" || line.startsWith("locked ")) {
      cur.locked = true;
    } else if (line === "prunable" || line.startsWith("prunable ")) {
      cur.prunable = true;
    }
  }
  flush();
  return out;
}

/** The worktree the view should default to when nothing is selected: the
 *  primary if present, else the first entry, else null (empty list). */
export function primaryWorktree(worktrees: Worktree[]): Worktree | null {
  return worktrees.find((w) => w.primary) ?? worktrees[0] ?? null;
}

/** Find the worktree at `path` (separator/case-insensitive), or null. */
export function findWorktree(worktrees: Worktree[], path: string | null): Worktree | null {
  if (path === null) return null;
  const want = normalizePath(path);
  return worktrees.find((w) => normalizePath(w.path) === want) ?? null;
}

export interface Resolution {
  /** The worktree whose working dir the view runs git against. */
  active: Worktree | null;
  /** The selection to keep, normalized: null once we're on the primary, so a
   *  refresh doesn't keep re-reporting the same fall-back. */
  selected: string | null;
  /** True when the requested selection was gone and we dropped to primary — the
   *  caller surfaces a one-time message. */
  fellBack: boolean;
}

/** Decide which worktree the view is pointed at. `selectedPath` is the path the
 *  user last chose (null = primary). If that worktree is still listed we honor
 *  it; if it was pruned/removed we fail soft back to the primary and flag it.
 *  Selecting the primary explicitly collapses to `selected: null` so the state
 *  is always canonical. */
export function resolveSelection(
  worktrees: Worktree[],
  selectedPath: string | null
): Resolution {
  const primary = primaryWorktree(worktrees);
  if (selectedPath === null) {
    return { active: primary, selected: null, fellBack: false };
  }
  const match = findWorktree(worktrees, selectedPath);
  if (!match || match.primary) {
    // Gone, or the user re-picked the primary: canonicalize to primary.
    return { active: primary, selected: null, fellBack: match === null };
  }
  return { active: match, selected: match.path, fellBack: false };
}

/** A short label for the selector chip / menu: the worktree's folder name. */
export function worktreeLabel(w: Worktree): string {
  const parts = w.path.replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || w.path;
}

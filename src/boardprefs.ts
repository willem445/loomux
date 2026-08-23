// Durable task-board view preferences (#1270) — which containers the human has
// collapsed and which filters they have armed, per orchestration group,
// surviving a restart.
//
// Pure encode/decode + validation, DOM-free and unit-tested, modelled on
// tabstore.ts / settings.ts / sshprofile.ts. `uistate.rs` stores the blob
// atomically as `boardview.json` and never parses it beyond "is it JSON at
// all"; this module owns the schema.
//
// ---------------------------------------------------------------------------
// Why a sibling blob and not the task
// ---------------------------------------------------------------------------
//
// #1152 put the archive stamp (`cleared_ms`) ON the task, and argued the line
// this file sits on the other side of: "I have acknowledged this item and want
// it out of my working set" is a human-authored decision about the WORK ITEM,
// so it is board data by the same test `status` is. Collapse and filters are
// not that. They are this human's view of the board, and putting them on the
// task would make every chevron click an audited board write the orchestrator
// is handed. #1160's own design note names `collapsed` explicitly as
// frontend-only for that reason; what #1270 changes is only that "frontend-only"
// no longer has to mean "forgotten on restart".
//
// The drift objection #1160 raised against a task-id-keyed sidecar — delete the
// task and its id lives on — does not bite here, and the difference is
// structural rather than a promise. A stale id in a collapsed set is INERT: it
// names no container, so it collapses nothing. A stale `cleared_ms` sidecar
// entry would have HIDDEN A LIVE ROW. The board also already prunes the set to
// live rows on every refresh (`retainExisting`), so the next save writes the
// dead ids out on its own.
//
// It is a sibling of `settings.json` rather than a key inside it for the reason
// #887 gave for `sshprofiles.json`: a multi-entry keyed structure with its own
// lifecycle does not belong in a flat bag of app-wide scalars.
//
// The group id is a MAP KEY here and never a path — no `.join`, so hard
// constraint 6's surface is untouched by this file and by the two commands
// behind it.

import { NO_FILTER, type BoardFilter } from "./taskboard.ts";

/** Schema version of the persisted blob. Bumped only when an existing key
 *  changes MEANING — adding a filter family (#1272's sprint, #1273's typed
 *  links) is a new key inside `filters`, which older builds preserve and
 *  ignore, so it needs no bump and no migration. */
export const BOARD_PREFS_VERSION = 1;

/** How many groups' view state to keep. Keyed by group, so without a cap this
 *  file grows forever — one record per group ever opened, long after the group
 *  itself is gone (nothing here can know that it is: this module never touches
 *  disk, and asking the orchestration registry would make a view preference
 *  depend on a live backend read at save time).
 *
 *  Fifty is far past any plausible working set and still a small file, and the
 *  cost of falling off the end is that one board opens with its filters cleared
 *  and its tree expanded — the pre-#1270 behaviour, not a loss of anything the
 *  human authored. */
export const MAX_GROUPS = 50;

/** One group's persisted board view. */
export interface GroupBoardView {
  /** When this record was last written, ms — the LRU key `encodeBoardPrefs`
   *  evicts by. */
  touched: number;
  /** Container ids the human has collapsed. */
  collapsed: string[];
  /** The armed filter. */
  filter: BoardFilter;
  /** Filter families this build does not know about, kept verbatim so a round
   *  trip through an older build cannot silently delete a newer one's state.
   *  This is what makes "a new family is a key, not a migration" true in both
   *  directions rather than only forwards. */
  unknownFilters: Record<string, unknown>;
}

/** The whole store: group id → that group's view. A `Map` and not a plain
 *  object so a group id can never collide with `Object.prototype` (`toString`
 *  is a legal-looking key), and so iteration order is insertion order. */
export type BoardPrefs = Map<string, GroupBoardView>;

/** The view a group with nothing persisted opens with: everything expanded, no
 *  filter — exactly the pre-#1270 board. */
export function defaultGroupView(): GroupBoardView {
  return { touched: 0, collapsed: [], filter: { ...NO_FILTER }, unknownFilters: {} };
}

/** This group's persisted view, or the default. Always returns a FRESH object
 *  the caller may mutate: the store is long-lived and handing out its interior
 *  would let a caller's edit skip `writeGroupView` and never be saved. */
export function readGroupView(prefs: BoardPrefs, groupId: string): GroupBoardView {
  const found = prefs.get(groupId);
  if (!found) return defaultGroupView();
  return {
    touched: found.touched,
    collapsed: [...found.collapsed],
    filter: {
      kind: [...found.filter.kind],
      status: [...found.filter.status],
      text: found.filter.text,
      attention: found.filter.attention,
    },
    unknownFilters: { ...found.unknownFilters },
  };
}

/** Record this group's view, stamping `touched` so the LRU sees it as the most
 *  recently used. Returns a NEW map (the caller replaces its handle) rather
 *  than mutating in place, so a save that fails leaves nothing half-applied.
 *
 *  `nowMs` is passed in rather than read from the clock, so the whole module
 *  stays pure and the LRU is testable without faking `Date`. */
export function writeGroupView(
  prefs: BoardPrefs,
  groupId: string,
  view: Omit<GroupBoardView, "touched">,
  nowMs: number
): BoardPrefs {
  const next = new Map(prefs);
  next.set(groupId, {
    touched: nowMs,
    collapsed: [...view.collapsed],
    filter: {
      kind: [...view.filter.kind],
      status: [...view.filter.status],
      text: view.filter.text,
      attention: view.filter.attention,
    },
    unknownFilters: { ...view.unknownFilters },
  });
  return next;
}

/** Serialize for `saveBoardPrefs`, keeping the `MAX_GROUPS` most recently
 *  touched groups and dropping the rest.
 *
 *  The eviction happens HERE, at the write, rather than at the read: a build
 *  that only ever loaded would let the file grow unbounded on disk however
 *  small the in-memory map was, and this is the one function every write goes
 *  through. Ties keep insertion order (`sort` is stable since ES2019), so two
 *  groups saved in the same millisecond evict deterministically. */
export function encodeBoardPrefs(prefs: BoardPrefs): string {
  const kept = [...prefs.entries()]
    .sort((a, b) => b[1].touched - a[1].touched)
    .slice(0, MAX_GROUPS);
  const groups: Record<string, unknown> = {};
  for (const [id, v] of kept) {
    groups[id] = {
      touched: v.touched,
      collapsed: [...v.collapsed],
      filters: {
        // Unknown families first, so a future key can never shadow one this
        // build owns — a newer build writing `{"kind": "nonsense"}` into the
        // unknown bag must not be able to overwrite the validated `kind`.
        ...v.unknownFilters,
        kind: [...v.filter.kind],
        status: [...v.filter.status],
        text: v.filter.text,
        attention: v.filter.attention,
      },
    };
  }
  return JSON.stringify({ v: BOARD_PREFS_VERSION, groups }, null, 2);
}

/** Parse the persisted blob. Tolerant in exactly the way every other schema
 *  module here is: anything malformed at the top level yields an EMPTY store
 *  (the boards then open at their defaults), and a malformed individual group
 *  or field is dropped rather than invalidating the file — one hand-edit
 *  mistake must not cost every other group its view.
 *
 *  A future version number is read anyway rather than refused: every field is
 *  validated per-key regardless, so the worst a newer file can do is contribute
 *  keys this build ignores and preserves. Refusing it would throw away state a
 *  downgrade could otherwise hand straight back. */
export function decodeBoardPrefs(raw: string | null): BoardPrefs {
  const out: BoardPrefs = new Map();
  if (!raw) return out;
  let v: unknown;
  try {
    v = JSON.parse(raw);
  } catch {
    return out;
  }
  if (!v || typeof v !== "object" || Array.isArray(v)) return out;
  const groups = (v as Record<string, unknown>).groups;
  if (!groups || typeof groups !== "object" || Array.isArray(groups)) return out;
  for (const [id, entry] of Object.entries(groups as Record<string, unknown>)) {
    if (!id || !entry || typeof entry !== "object" || Array.isArray(entry)) continue;
    const rec = entry as Record<string, unknown>;
    const rawFilters =
      rec.filters && typeof rec.filters === "object" && !Array.isArray(rec.filters)
        ? (rec.filters as Record<string, unknown>)
        : {};
    const { kind, status, text, attention, ...unknownFilters } = rawFilters;
    out.set(id, {
      touched: typeof rec.touched === "number" && Number.isFinite(rec.touched) ? rec.touched : 0,
      collapsed: stringList(rec.collapsed),
      filter: {
        kind: stringList(kind),
        status: stringList(status),
        text: typeof text === "string" ? text : "",
        attention: attention === true,
      },
      unknownFilters,
    });
  }
  return out;
}

/** A list of non-empty strings, dropping anything else. Not an allowlist
 *  against `KINDS`/`STATUSES`: a hand-edited board can legitimately carry an
 *  out-of-vocabulary kind (`kindFilterChoices` offers one a chip for exactly
 *  that reason), and a stale status from a future build should survive a
 *  downgrade rather than being scrubbed. Nothing here reaches a path or a
 *  command — these values are compared against board rows and nothing else. */
function stringList(v: unknown): string[] {
  if (!Array.isArray(v)) return [];
  return v.filter((x): x is string => typeof x === "string" && x.length > 0);
}

// The launcher's recent-directories list (#2010): the pure ordering/dedup/cap
// decision behind `addRecentRepo`, extracted the same way `pickerSelection` was
// — DOM-free so the node tests can reach the one thing worth pinning about it.
//
// The list is what the new-pane dropdown shows (newest first) and what
// `addRecentRepo` persists, so one function owns both halves of the rule:
// what a launch/Browse… pick does to the list, and when a write is refused.
//
// A `null` list is NOT "empty" — it means the stored list could not be READ at
// all (localStorage.getItem threw). A write against it is declined: overwriting
// a list we never saw is how a transient storage failure would wipe the
// human's whole history (#2010; the single-tenant cousin of the BoardPrefsStore
// rule — #1299). A blob that parses to garbage is different: the data itself is
// gone and there is nothing to preserve, so that reads as an empty list.

/** How many recent directories are kept. The cap `addRecentRepo` has always
 *  enforced (#2010: the issue's "say 10" was an estimate; the existing cap
 *  stands). */
export const MAX_RECENT_REPOS = 8;

/** The list after recording `path`: deduplicated, newest first, capped. Returns
 *  `null` when the write must be declined (see the module comment). */
export function mergeRecentDir(recent: readonly string[] | null, path: string): string[] | null {
  if (recent === null) return null;
  const p = path.trim();
  // An empty path is a no-op, never a stored entry — a whitespace-only path is
  // a typing slip, and a "" option in the dropdown would render as a blank row.
  if (!p) return [...recent];
  return [p, ...recent.filter((x) => x !== p)].slice(0, MAX_RECENT_REPOS);
}

// Pure display model for search + replace results (issue #174). Turns the flat
// match list the backend returns into a per-file grouping the panel renders,
// tracks which files are selected for a replace, and derives the confirmed file
// set + count summaries. DOM-free and node:test-covered; the FileEditView binds
// it to checkboxes and the replace button.

import type { SearchMatch } from "./fileapi";

/** The inputs that define *what* a search matched: the query plus the two match
 *  modifiers. A replace must apply the SAME params it previewed, so the view
 *  snapshots these at search time and compares live inputs against the snapshot
 *  (`paramsEqual`) to invalidate a stale preview before it can be applied. */
export interface SearchParams {
  query: string;
  caseInsensitive: boolean;
  wholeWord: boolean;
}

/** Whether two search-parameter sets would match the same text. Used to detect
 *  that the query/options changed since the last search, so the preview→apply
 *  guarantee for cross-file replace can't be defeated by editing the query box
 *  (or toggling case) after searching. */
export function paramsEqual(a: SearchParams, b: SearchParams): boolean {
  return (
    a.query === b.query &&
    a.caseInsensitive === b.caseInsensitive &&
    a.wholeWord === b.wholeWord
  );
}

/** All matches in one file, plus whether the file is selected for replace.
 *  Selection is per-file in v1 (per-match is a documented future step). */
export interface FileGroup {
  rel: string;
  matches: SearchMatch[];
  selected: boolean;
}

/** Group flat matches by file, preserving first-seen file order (which is the
 *  walker's order — stable and predictable). Every group starts selected, so an
 *  unmodified replace targets everything the search found. */
export function groupMatches(matches: SearchMatch[]): FileGroup[] {
  const order: string[] = [];
  const byFile = new Map<string, SearchMatch[]>();
  for (const m of matches) {
    let bucket = byFile.get(m.rel);
    if (!bucket) {
      bucket = [];
      byFile.set(m.rel, bucket);
      order.push(m.rel);
    }
    bucket.push(m);
  }
  return order.map((rel) => ({ rel, matches: byFile.get(rel)!, selected: true }));
}

/** File and match totals for the "N matches in M files" summary line. */
export function countSummary(groups: FileGroup[]): { files: number; matches: number } {
  return {
    files: groups.length,
    matches: groups.reduce((sum, g) => sum + g.matches.length, 0),
  };
}

/** Toggle one file's selection, returning a new array (pure — the caller swaps
 *  its state and re-renders). Unknown `rel` is a no-op. */
export function toggleFile(groups: FileGroup[], rel: string): FileGroup[] {
  return groups.map((g) => (g.rel === rel ? { ...g, selected: !g.selected } : g));
}

/** Set every file's selection at once (the select-all / clear-all control). */
export function setAll(groups: FileGroup[], selected: boolean): FileGroup[] {
  return groups.map((g) => ({ ...g, selected }));
}

/** Root-relative paths of the selected files — exactly what `ftReplace` applies
 *  to. Empty when nothing is selected (the replace button is then disabled). */
export function selectedFiles(groups: FileGroup[]): string[] {
  return groups.filter((g) => g.selected).map((g) => g.rel);
}

/** Match count across only the selected files — drives the replace button label
 *  ("Replace N occurrences"). */
export function selectedMatchCount(groups: FileGroup[]): number {
  return groups.filter((g) => g.selected).reduce((sum, g) => sum + g.matches.length, 0);
}

/** Map of `rel` → match count, for highlighting files with hits in the tree
 *  (VS-Code-like) with a per-file count badge. Pure lookup the tree render
 *  consults per row. */
export function hitCounts(groups: FileGroup[]): Map<string, number> {
  return new Map(groups.map((g) => [g.rel, g.matches.length]));
}

/** The first match in a file (line/col), for jumping to it when the file is
 *  opened from the tree highlight. Null if the file has no matches. */
export function firstMatch(groups: FileGroup[], rel: string): SearchMatch | null {
  return groups.find((g) => g.rel === rel)?.matches[0] ?? null;
}

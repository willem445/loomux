// Pure file-NAME matching + ranking for the file explorer's "Go to file" box
// (issue #214). DOM-free so the whole matcher is unit-tested (test/filematch.test.ts);
// the input box, result list, and keyboard nav are DOM wiring in fileedit.ts.
//
// WHY THIS IS SEPARATE FROM THE #207 SEARCH. The owner's ask on #214 is explicit:
// "an optimized and fast file search … It does not need to search into files as we
// already have the file editor pane that can do that." So this matches PATHS, never
// contents. The backend enumerates the path list ONCE per root (`ft_files_start`,
// which reads no file), the view caches it, and every keystroke runs this function
// over that in-memory array — zero I/O per keystroke, which is what makes it feel
// instant on a big repo.
//
// SUBSTRING, NOT FUZZY. A subsequence ("fuzzy") matcher scores higher on demos and
// worse in practice: on a 20k-path repo it matches almost everything (`pnt` hits any
// path with a p, an n and a t in order), so the ranking function becomes the entire
// product and small changes to it reshuffle results unpredictably. Substring
// matching is the opposite trade: you can always predict what it will and won't
// find, which is the property you actually want when you're trying to *jump to a
// file you already know the name of*. Space-separated terms are AND-ed across the
// whole path, which recovers the useful part of fuzzy ("pane rest" → panerestore.ts,
// "src pane" → src/pane.ts) without the noise. v1 by choice, not by omission.

/** One ranked path, with the spans that matched so the view can highlight them. */
export interface FileNameHit {
  /** Root-relative, forward-slashed path (the backend's `rel` convention). */
  rel: string;
  /** Higher is better. Only meaningful relative to other hits for the same query. */
  score: number;
  /** Half-open `[start, end)` character ranges in `rel`, merged and ascending. */
  ranges: [number, number][];
}

/** Where the basename starts in `rel` (0 when the path has no directory part). */
export function basenameStart(rel: string): number {
  return rel.lastIndexOf("/") + 1;
}

/** A term matching inside the file NAME is worth far more than one matching only
 *  in the directory part: you type a name to find a file, not to find a folder. */
const IN_NAME = 100;
const IN_DIR = 20;
/** The name (or a path segment) *starts* with the term — a much stronger signal
 *  than a match buried mid-word. */
const AT_NAME_START = 60;
const AT_SEGMENT_START = 25;
/** The whole query is exactly the file name ("pane.ts" → src/pane.ts). The one
 *  case where the user has told us precisely what they want, so it outranks every
 *  accumulation of partial-term scores. */
const EXACT_NAME = 1000;

/** True when `rel[i]` begins a segment: start of string, or right after a `/`,
 *  `-`, `_`, or `.` — the separators paths and filenames actually use. */
function atSegmentStart(rel: string, i: number): boolean {
  if (i === 0) return true;
  return "/-_.".includes(rel[i - 1]);
}

/** Split a query into terms. Whitespace-separated, all AND-ed against the path;
 *  empty/blank query yields no terms (the caller treats that as "no filter"). */
export function queryTerms(query: string): string[] {
  return query.toLowerCase().split(/\s+/).filter(Boolean);
}

/** Score the BEST occurrence of `term` in `lower`, or null when it doesn't occur.
 *
 *  Every occurrence is considered, not just the first: `test/panesetup.test.ts`
 *  contains "test" in its directory AND in its name, and taking `indexOf`'s first
 *  hit would score it as a mere directory match — collapsing the name-beats-
 *  directory rule exactly on the paths where it matters most. */
function bestOccurrence(
  lower: string,
  rel: string,
  term: string,
  base: number
): { score: number; span: [number, number] } | null {
  let best: { score: number; span: [number, number] } | null = null;
  for (let idx = lower.indexOf(term); idx >= 0; idx = lower.indexOf(term, idx + 1)) {
    let score = idx >= base ? IN_NAME : IN_DIR;
    if (idx === base) score += AT_NAME_START;
    else if (atSegmentStart(rel, idx)) score += AT_SEGMENT_START;
    if (!best || score > best.score) best = { score, span: [idx, idx + term.length] };
  }
  return best;
}

/** Score one path against the (already lowercased, non-empty) terms. Null when any
 *  term is absent — matching is AND, so one miss disqualifies the path entirely. */
function scorePath(rel: string, terms: string[]): FileNameHit | null {
  const lower = rel.toLowerCase();
  const base = basenameStart(rel);
  const name = lower.slice(base);

  let score = 0;
  const spans: [number, number][] = [];
  for (const term of terms) {
    const hit = bestOccurrence(lower, rel, term, base);
    if (!hit) return null; // AND: this path is out
    score += hit.score;
    spans.push(hit.span);
  }
  // The query, rejoined, IS the file name — the user named the file outright.
  if (terms.length === 1 && name === terms[0]) score += EXACT_NAME;

  return { rel, score, ranges: mergeRanges(spans) };
}

/** Merge overlapping/adjacent spans into ascending, disjoint ranges, so the view
 *  can paint highlights without double-wrapping a character (two terms that
 *  overlap in the path would otherwise produce nested spans). */
export function mergeRanges(spans: [number, number][]): [number, number][] {
  if (spans.length === 0) return [];
  const sorted = [...spans].sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  const out: [number, number][] = [sorted[0]];
  for (const [start, end] of sorted.slice(1)) {
    const last = out[out.length - 1];
    if (start <= last[1]) last[1] = Math.max(last[1], end);
    else out.push([start, end]);
  }
  return out;
}

/** Rank `files` against `query`, best first, capped at `limit`.
 *
 *  Ties break on the SHORTER path, then alphabetically — so the result order is
 *  fully deterministic (no dependence on enumeration order, which differs between
 *  `git ls-files` and the walk) and `src/pane.ts` beats `src/deeply/nested/pane.ts`
 *  for the same score. A blank query returns nothing: the box is a filter, and an
 *  empty filter means "show me the tree", not "show me all 20,000 files". */
export function rankFileNames(
  files: readonly string[],
  query: string,
  limit: number
): FileNameHit[] {
  const terms = queryTerms(query);
  if (terms.length === 0 || limit <= 0) return [];
  const hits: FileNameHit[] = [];
  for (const rel of files) {
    const hit = scorePath(rel, terms);
    if (hit) hits.push(hit);
  }
  hits.sort((a, b) => b.score - a.score || a.rel.length - b.rel.length || (a.rel < b.rel ? -1 : 1));
  return hits.slice(0, limit);
}

/** Move a selection index by `delta` within `count` items, wrapping at both ends
 *  (Down from the last result goes to the first — the quick-open convention).
 *  Returns 0 for an empty list so the caller never holds a stale index. */
export function moveSelection(current: number, delta: number, count: number): number {
  if (count <= 0) return 0;
  return (((current + delta) % count) + count) % count;
}

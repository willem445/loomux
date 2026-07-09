// Pure, DOM-free helpers for the in-file find widget (issue #174). The widget
// DOM itself (a custom CodeMirror search panel) is human-validated; the logic
// that's worth pinning — building the match regex from the toggle state, and the
// live "n of m" match count + its formatting — lives here with node:test
// coverage. No DOM, no CodeMirror import, so it stays in the main bundle and
// never drags the lazy CM6 chunk eager.

export interface FindFlags {
  caseSensitive: boolean;
  wholeWord: boolean;
  regexp: boolean;
}

/** Build the global regex the find widget searches with, or null for an empty
 *  query / an invalid user regex. In literal mode the query's regex
 *  metacharacters are escaped (so "a.b" matches only "a.b"); in regexp mode the
 *  query is used verbatim. `wholeWord` wraps it in word boundaries; case follows
 *  `caseSensitive`. Always global so the counter can iterate all matches. */
export function buildSearchRegex(query: string, flags: FindFlags): RegExp | null {
  if (!query) return null;
  let body = flags.regexp ? query : query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  if (flags.wholeWord) body = `\\b${body}\\b`;
  try {
    return new RegExp(body, flags.caseSensitive ? "g" : "gi");
  } catch {
    return null; // invalid user regex → no matches, never throw into the editor
  }
}

/** Regex for the always-on WORKSPACE-query highlight inside the open file (the
 *  `.cm-wsMatch` decoration). Literal-only (no regexp mode) to match the backend
 *  search; `ci`/`ww` are the project-search options. A thin wrapper over
 *  `buildSearchRegex` so the two searches share one escaping/flag implementation. */
export function buildHighlightRegex(query: string, ci: boolean, ww: boolean): RegExp | null {
  return buildSearchRegex(query, { caseSensitive: !ci, wholeWord: ww, regexp: false });
}

export interface MatchInfo {
  /** Total matches in the document. */
  count: number;
  /** 1-based index of the match currently selected (start === `selFrom`), or 0
   *  when the selection isn't sitting on a match yet. */
  current: number;
}

/** Count matches of `re` in `text` and locate which one the selection is on.
 *  `selFrom` is the selection's start offset (after `findNext`, CodeMirror
 *  selects the match, so its start equals a match start → that becomes
 *  `current`). Pure over the document string. */
export function matchInfo(text: string, re: RegExp | null, selFrom: number): MatchInfo {
  if (!re) return { count: 0, current: 0 };
  // Work on a guaranteed-global clone so iteration terminates and we don't
  // mutate the caller's lastIndex.
  const g = new RegExp(re.source, re.flags.includes("g") ? re.flags : re.flags + "g");
  let m: RegExpExecArray | null;
  let count = 0;
  let current = 0;
  while ((m = g.exec(text)) !== null) {
    count++;
    if (m.index === selFrom) current = count;
    if (m.index === g.lastIndex) g.lastIndex++; // step past a zero-width match
  }
  return { count, current };
}

/** The "n of m" label. Empty query shows nothing; a query with no hits shows
 *  "No results"; before the caret is on a match, the plain total; once on a
 *  match, "current of total". */
export function formatMatchCount(query: string, info: MatchInfo): string {
  if (query === "") return "";
  if (info.count === 0) return "No results";
  if (info.current > 0) return `${info.current} of ${info.count}`;
  return `${info.count} found`;
}

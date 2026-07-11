// Unit tests for the streaming-search state machine (issue #207). Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  idle,
  begin,
  accept,
  isTruncated,
  isSearching,
  enumerationSource,
  RENDER_CAP,
  type SearchBatch,
} from "../src/searchsession.ts";
import type { SearchMatch } from "../src/fileapi.ts";

const m = (rel: string, line: number): SearchMatch => ({
  rel,
  line,
  col: 1,
  line_text: `line ${line}`,
});

const batch = (id: number, matches: SearchMatch[], extra: Partial<SearchBatch> = {}): SearchBatch => ({
  id,
  matches,
  done: false,
  truncated: false,
  ...extra,
});

test("accept folds matching-id batches and tracks done/truncated", () => {
  let s = begin(1);
  s = accept(s, batch(1, [m("a.ts", 1), m("a.ts", 2)]));
  assert.equal(s.matches.length, 2);
  assert.equal(s.done, false);
  s = accept(s, batch(1, [m("b.ts", 3)], { done: true, truncated: true }));
  assert.equal(s.matches.length, 3);
  assert.equal(s.done, true);
  assert.equal(s.truncated, true);
  assert.ok(isTruncated(s));
});

test("accept drops a batch whose id doesn't match the active session", () => {
  let s = begin(2);
  s = accept(s, batch(2, [m("a.ts", 1)]));
  const before = s;
  const after = accept(s, batch(99, [m("evil.ts", 1)]));
  assert.equal(after, before, "a foreign-id batch must be a no-op (same reference)");
  assert.equal(after.matches.length, 1);
});

test("cancellation race: results from a superseded search never land", () => {
  // Search #1 runs and delivers a batch...
  let s = begin(1);
  s = accept(s, batch(1, [m("old.ts", 1)]));
  // ...then the user types again → the view starts search #2 (new session).
  s = begin(2);
  // The now-stale search #1 finishes late: its remaining + terminal batches must
  // not resurrect its results into session #2.
  s = accept(s, batch(1, [m("old.ts", 2)]));
  s = accept(s, batch(1, [], { done: true, truncated: true }));
  assert.equal(s.matches.length, 0, "no matches from the cancelled search");
  assert.equal(s.done, false, "the stale done must not finish the new session");
  assert.equal(s.truncated, false);
  // Session #2's own results still accumulate normally.
  s = accept(s, batch(2, [m("new.ts", 1)]));
  assert.deepEqual(s.matches.map((x) => x.rel), ["new.ts"]);
});

test("going idle drops any further batches from the last search", () => {
  let s = begin(5);
  s = accept(s, batch(5, [m("a.ts", 1)]));
  s = idle();
  s = accept(s, batch(5, [m("a.ts", 2)], { done: true }));
  assert.equal(s.matches.length, 0);
  assert.equal(s.activeId, null);
});

test("accept caps accumulation at the render cap and latches overflow", () => {
  const cap = 5;
  let s = begin(1);
  s = accept(s, batch(1, [m("a", 1), m("a", 2), m("a", 3)]), cap);
  assert.equal(s.overflow, false);
  // This batch crosses the cap: it's sliced to fit and overflow latches.
  s = accept(s, batch(1, [m("b", 1), m("b", 2), m("b", 3), m("b", 4)]), cap);
  assert.equal(s.matches.length, cap, "never accumulates past the cap");
  assert.ok(s.overflow);
  assert.ok(isTruncated(s), "overflow reads as truncated in the summary");
  // Once overflowed, later batches add nothing.
  s = accept(s, batch(1, [m("c", 1)]), cap);
  assert.equal(s.matches.length, cap);
});

test("RENDER_CAP is a sane, DOM-safe bound", () => {
  assert.ok(RENDER_CAP > 0 && RENDER_CAP <= 5000);
});

test("isSearching drives Esc routing: true only while a search is actively running", () => {
  // Idle → Esc should close the overlay, not cancel.
  assert.equal(isSearching(idle()), false);
  // Running (batches still arriving) → Esc cancels.
  let s = begin(1);
  assert.equal(isSearching(s), true);
  s = accept(s, batch(1, [m("a.ts", 1)]));
  assert.equal(isSearching(s), true);
  // Finished → activeId is still set, but Esc must fall through to close in one
  // press (the two-presses-to-close nit), so this must read false.
  s = accept(s, batch(1, [], { done: true }));
  assert.equal(isSearching(s), false);
});

test("enumerationSource: ignored-by-default respects .gitignore in a git repo", () => {
  // The default (toggle off) in a git repo must use the gitignore-aware source.
  assert.equal(enumerationSource(true, false), "git");
  // Toggle on → full walk, even in a git repo.
  assert.equal(enumerationSource(true, true), "walk");
  // Non-git root has no .gitignore to respect → always the full walk.
  assert.equal(enumerationSource(false, false), "walk");
  assert.equal(enumerationSource(false, true), "walk");
});

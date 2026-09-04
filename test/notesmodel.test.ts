// Unit tests for the pure note rules (#2116) — src/notesmodel.ts.
// Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  MAX_NOTE_LEN,
  normalizeNoteText,
  notesEmptyState,
  orderedNotes,
  type SessionNote,
} from "../src/notesmodel.ts";

const note = (id: string, created_ms: number, text = id): SessionNote => ({
  id,
  text,
  created_ms,
  unknown: {},
});

test("a note is trimmed, so surrounding whitespace never reaches the store", () => {
  assert.equal(normalizeNoteText("  rebasing onto main  \n"), "rebasing onto main");
});

test("a note that is only whitespace is not a note", () => {
  // The textarea submits on Enter, so a stray newline is the commonest input
  // here — and it must not create a blank row the human then has to delete.
  assert.equal(normalizeNoteText("   \n\t "), null);
  assert.equal(normalizeNoteText(""), null);
});

test("an over-long note is truncated, not refused", () => {
  // Refusing would lose a long paste outright; truncating keeps its opening,
  // which is the part a human wrote first.
  const long = "x".repeat(MAX_NOTE_LEN + 500);
  const out = normalizeNoteText(long);
  assert.equal(out?.length, MAX_NOTE_LEN);
  assert.equal(out, "x".repeat(MAX_NOTE_LEN));
});

test("a note exactly at the cap survives whole", () => {
  // The boundary in the other direction, so the cap cannot pass by truncating
  // everything.
  const exact = "y".repeat(MAX_NOTE_LEN);
  assert.equal(normalizeNoteText(exact), exact);
});

test("the trim happens BEFORE the cap, so padding cannot cost a note its tail", () => {
  const padded = `   ${"z".repeat(MAX_NOTE_LEN)}   `;
  assert.equal(normalizeNoteText(padded), "z".repeat(MAX_NOTE_LEN));
});

test("notes read oldest-first — the order they were written", () => {
  const out = orderedNotes([note("c", 300), note("a", 100), note("b", 200)]);
  assert.deepEqual(
    out.map((n) => n.id),
    ["a", "b", "c"]
  );
});

test("ordering never mutates its input", () => {
  const input = [note("c", 300), note("a", 100)];
  orderedNotes(input);
  assert.deepEqual(
    input.map((n) => n.id),
    ["c", "a"]
  );
});

test("two notes written in the same millisecond keep the order they arrived in", () => {
  const out = orderedNotes([note("first", 500), note("second", 500)]);
  assert.deepEqual(
    out.map((n) => n.id),
    ["first", "second"]
  );
});

test("the empty state for a known session and for a pending pane say different things", () => {
  const known = notesEmptyState("sess-1");
  const pending = notesEmptyState(null);
  assert.notEqual(known, pending);
  // The pending wording must actually disclose the residual it exists for —
  // an app restart before the id is learned loses these notes. Asserting only
  // "the two differ" would pass on any two sentences.
  assert.match(pending, /restarts/);
  assert.match(pending, /session id/);
  assert.doesNotMatch(known, /restarts/);
});

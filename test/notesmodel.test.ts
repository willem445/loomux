// Unit tests for the pure note rules (#2116) — src/notesmodel.ts.
// Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  MAX_NOTE_LEN,
  NoteDrafts,
  noteDraftIsPristine,
  noteTargetFor,
  noteWriteFeedback,
  normalizeNoteText,
  notesApplyToPane,
  notesEmptyState,
  orderedNotes,
  targetKey,
  type NoteWriteOutcome,
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

// ---- the draft book (the in-list-editor rule) ----

test("a draft is pristine exactly when it would not become a note", () => {
  // ONE predicate, and it must agree with the store's own answer — otherwise
  // the Add button is enabled for a box the store then refuses, or disabled
  // for one it would have accepted.
  for (const raw of ["", "   ", "\n\t "]) {
    assert.equal(noteDraftIsPristine(raw), true, JSON.stringify(raw));
    assert.equal(normalizeNoteText(raw), null, `${JSON.stringify(raw)} — and the store agrees`);
  }
  for (const raw of ["a", "  a  ", "0"]) {
    assert.equal(noteDraftIsPristine(raw), false, JSON.stringify(raw));
    assert.notEqual(normalizeNoteText(raw), null, JSON.stringify(raw));
  }
});

test("a whitespace-only box is pristine — which `!raw` would get wrong", () => {
  // The specific miss the predicate exists for: three call sites ask "is there
  // anything here", and the obvious spelling disagrees with the store on
  // exactly this input.
  assert.equal(noteDraftIsPristine("   "), true);
  assert.notEqual(noteDraftIsPristine("   "), !"   ");
});

test("a session target and a pane target never share a draft key", () => {
  // The two id spaces are unrelated strings and can spell the same thing; a
  // shared key would show one pane's half-typed note in another's editor.
  assert.notEqual(targetKey({ sessionId: "x" }), targetKey({ paneKey: "x" }));
});

test("a draft survives a close and reopen on the same target", () => {
  const drafts = new NoteDrafts();
  drafts.set({ sessionId: "s-1" }, "half a thou");
  assert.equal(drafts.get({ sessionId: "s-1" }), "half a thou");
});

test("two targets' drafts do not mix", () => {
  const drafts = new NoteDrafts();
  drafts.set({ sessionId: "s-1" }, "mine");
  drafts.set({ paneKey: "pane-2" }, "theirs");
  assert.equal(drafts.get({ sessionId: "s-1" }), "mine");
  assert.equal(drafts.get({ paneKey: "pane-2" }), "theirs");
  assert.equal(drafts.get({ sessionId: "never-typed-in" }), "");
});

test("a draft that goes pristine is pruned, not stored as an empty string", () => {
  // Otherwise the book grows one entry per session the human ever opened the
  // dialog on, and `size` stops meaning "things part-way through".
  const drafts = new NoteDrafts();
  drafts.set({ sessionId: "s-1" }, "typing…");
  assert.equal(drafts.size, 1);
  drafts.set({ sessionId: "s-1" }, "   ");
  assert.equal(drafts.size, 0);
  assert.equal(drafts.get({ sessionId: "s-1" }), "");
});

test("a submitted draft is cleared", () => {
  const drafts = new NoteDrafts();
  drafts.set({ paneKey: "pane-3" }, "submitted");
  drafts.clear({ paneKey: "pane-3" });
  assert.equal(drafts.size, 0);
  assert.equal(drafts.get({ paneKey: "pane-3" }), "");
});

test("the editor's seed and the pristine predicate answer the same question", () => {
  // CLAUDE.md: "the renderer's seed and the predicate's default are one
  // question asked twice". A target with nothing held opens with `""`, and
  // `""` is pristine — so a freshly opened editor can never start with its Add
  // button enabled.
  const drafts = new NoteDrafts();
  assert.equal(noteDraftIsPristine(drafts.get({ sessionId: "fresh" })), true);
});

// ---- where the Notes control applies (#2116 slice D) ----

test("an agent pane running a local CLI gets the Notes control", () => {
  assert.equal(notesApplyToPane("claude", false), true);
});

test("a pane with no harness does not — there would be no session to key to", () => {
  assert.equal(notesApplyToPane(null, false), false);
});

test("an SSH pane does NOT, even though it reports a harness", () => {
  // The gate that is easy to get wrong, because `facts().harness` is non-null
  // here: an SSH pane's CLI runs on the FAR END, so its session is not on this
  // machine and `sessionlog.json` is per-local-machine. Same reason the store
  // is not in a group dir.
  assert.equal(notesApplyToPane("claude", true), false);
});

test("a CLI orrerix does not recognise reports null and is refused, not guessed", () => {
  // The rule reads the harness, never branches on a CLI NAME (#722/#841): a
  // fourth CLI shows up as itself or as null, and null is refused rather than
  // inheriting some other CLI's answer.
  assert.equal(notesApplyToPane("opencode", false), true);
  assert.equal(notesApplyToPane("some-cli-nobody-has-written-yet", false), true);
  assert.equal(notesApplyToPane(null, true), false);
});

test("a pane with no session id yet is keyed on its pane key, not on nothing", () => {
  assert.deepEqual(noteTargetFor(null, "pane-7"), { paneKey: "pane-7" });
  assert.deepEqual(noteTargetFor("sess-9", "pane-7"), { sessionId: "sess-9" });
});

test("the target a pane key produces is the one rekey later moves", () => {
  // The two ends of the pending path have to agree on the key, and this is the
  // assertion that fails if either side starts spelling it differently.
  const pending = noteTargetFor(null, "pane-7");
  assert.equal(targetKey(pending), targetKey({ paneKey: "pane-7" }));
  assert.notEqual(targetKey(pending), targetKey(noteTargetFor("pane-7", "pane-7")));
});

// ---- what a write outcome owes the human ----

test("a declined write hands the text back — nothing was recorded anywhere", () => {
  // The data-loss case. The store returns before it mutates anything, so the
  // note reaches neither disk nor the list; the box has already been cleared,
  // so without this the human's text is simply gone, silently.
  const { message, restoreDraft } = noteWriteFeedback("declined-unread");
  assert.equal(restoreDraft, true);
  assert.notEqual(message, null);
  assert.match(message!, /nothing was saved/i);
});

test("a failed SAVE says so but does NOT hand the text back", () => {
  // The opposite shape, and the reason these are two branches rather than one
  // "it went wrong": the note IS in memory and on screen here, so restoring
  // the box would leave the human looking at a note plus a copy of its text,
  // and re-submitting would duplicate it.
  const { message, restoreDraft } = noteWriteFeedback("failed");
  assert.equal(restoreDraft, false);
  assert.notEqual(message, null);
  assert.match(message!, /not saved yet/i);
});

test("the two failure messages are different — they ask for different things", () => {
  assert.notEqual(noteWriteFeedback("declined-unread").message, noteWriteFeedback("failed").message);
});

test("success is silent, and no outcome is left to a default", () => {
  // Exhaustive over the union rather than over the two happy values: a new
  // outcome added to `NoteWriteOutcome` must be given a branch, and this list
  // is what makes forgetting one visible.
  const all: NoteWriteOutcome[] = ["saved", "unchanged", "pending", "declined-unread", "failed"];
  const silent = all.filter((o) => noteWriteFeedback(o).message === null);
  assert.deepEqual(silent, ["saved", "unchanged", "pending"]);
  for (const o of all) {
    // Non-vacuity: every outcome must actually reach a branch and return an
    // object, so this cannot pass by the function throwing or returning
    // undefined for the ones it was never taught.
    assert.equal(typeof noteWriteFeedback(o).restoreDraft, "boolean", o);
  }
});

test("only the declined outcome asks for the text back", () => {
  const all: NoteWriteOutcome[] = ["saved", "unchanged", "pending", "declined-unread", "failed"];
  assert.deepEqual(
    all.filter((o) => noteWriteFeedback(o).restoreDraft),
    ["declined-unread"]
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

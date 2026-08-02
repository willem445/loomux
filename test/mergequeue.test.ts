// Merge-queue visibility model (#581 slice F) — the pure logic behind the
// lifecycle chrome's queue row.
//
// What these tests defend, all three from the design note
// (doc/design/merge-queue.md) and the brief that slices it:
//
// 1. **Eight states and no ninth** (§4). Every state the core defines renders
//    as itself — a distinct, non-empty label and a tone — so no queue entry can
//    reach a human as blank or as "unknown".
// 2. **A ninth state fails loud.** A state word this build does not know throws
//    a named error rather than getting a made-up label. Same for a `status`
//    word: a vocabulary drift is a bug to surface, not a shape to smooth over.
// 3. **Truncation is surfaced, never silent** (#608's convention). Neither cut
//    — the backend's wire cap nor the row cap here — can make a short list read
//    as a complete one.
//
// And the case that motivates the whole shape: an unreadable queue must not
// look like an empty one. "Nothing is queued" and "loomux cannot read the
// queue" are the same picture to a human unless the model keeps them apart.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  ENTRY_STATES,
  MERGE_QUEUE_HELP,
  QUEUE_STATUSES,
  ROW_LIMIT,
  UnknownMergeQueueState,
  UnknownMergeQueueStatus,
  batchLine,
  entryStateLabel,
  entryStateTone,
  isEntryState,
  mergeQueueView,
  truncationNote,
} from "../src/mergequeue.ts";
import type { MergeQueueBatch, MergeQueueEntry, MergeQueueStatus } from "../src/orchestration.ts";

const entry = (o: Partial<MergeQueueEntry> = {}): MergeQueueEntry => ({
  pr: 612,
  state: "queued",
  blocked_reason: null,
  head: "deadbeef",
  enqueued_ms: 0,
  batch: null,
  ...o,
});

const batch = (o: Partial<MergeQueueBatch> = {}): MergeQueueBatch => ({
  id: "mq-7f3a",
  prs: [612, 613],
  state: "ci-wait",
  draft_pr: 640,
  scratch_sha: "cafebabe",
  started_ms: 0,
  ...o,
});

const status = (o: Partial<MergeQueueStatus> = {}): MergeQueueStatus => {
  const entries = o.entries ?? [entry()];
  return {
    status: "ok",
    detail: null,
    version: 1,
    target: "feat/integration-batch-2",
    entries,
    entries_total: entries.length,
    truncated: false,
    batch: null,
    ...o,
  };
};

// --- rule 1: eight states, none of them blank ------------------------------

test("every state the core defines has its own label and tone — none blank, none shared", () => {
  assert.equal(ENTRY_STATES.length, 8, "§4: eight states and no ninth");
  const labels = ENTRY_STATES.map(entryStateLabel);
  for (const [i, label] of labels.entries()) {
    assert.ok(label.trim().length > 0, `${ENTRY_STATES[i]} rendered blank`);
    assert.ok(
      !/unknown|\?\?/i.test(label),
      `${ENTRY_STATES[i]} rendered as an unknown-ish placeholder: ${label}`
    );
  }
  assert.equal(new Set(labels).size, 8, "two states sharing a label is two states a human can't tell apart");
  for (const s of ENTRY_STATES) assert.ok(entryStateTone(s), `${s} has no tone`);
});

test("every state reaches a rendered row, and terminal/blocked states read as trouble", () => {
  for (const s of ENTRY_STATES) {
    const v = mergeQueueView(status({ entries: [entry({ state: s })] }));
    assert.equal(v.kind, "queue");
    if (v.kind !== "queue") return;
    assert.equal(v.rows.length, 1);
    assert.match(v.rows[0].text, /^#612 /, `${s} lost its PR number`);
    assert.ok(v.rows[0].text.length > "#612 ".length, `${s} rendered an empty row`);
  }
  // A culprit and a kicked-back entry are not "progress" — the tone says so.
  assert.equal(entryStateTone("kicked-back"), "bad");
  assert.equal(entryStateTone("bisecting"), "bad");
  assert.equal(entryStateTone("landed"), "good");
});

test("a blocked queued entry says WHY — §4's live predicate is the only explanation there is", () => {
  const v = mergeQueueView(
    status({ entries: [entry({ state: "queued", blocked_reason: "head moved; verdicts stale" })] })
  );
  assert.equal(v.kind, "queue");
  if (v.kind !== "queue") return;
  assert.equal(v.rows[0].blockedReason, "head moved; verdicts stale");
  assert.match(v.rows[0].text, /blocked: head moved; verdicts stale/);
  assert.equal(v.rows[0].tone, "bad", "a blocked entry is not moving; it must not read as waiting");
});

// --- rule 2: a ninth state, and an unknown status, fail LOUD ---------------

test("a ninth state throws a named error instead of getting a made-up label", () => {
  assert.equal(isEntryState("frobnicating"), false);
  assert.throws(() => entryStateLabel("frobnicating"), UnknownMergeQueueState);
  assert.throws(() => entryStateTone("frobnicating"), UnknownMergeQueueState);
  assert.throws(
    () => mergeQueueView(status({ entries: [entry({ state: "frobnicating" })] })),
    (err: unknown) => {
      assert.ok(err instanceof UnknownMergeQueueState);
      assert.equal(err.value, "frobnicating");
      assert.match(err.message, /frobnicating/, "the error must name the state it refused");
      return true;
    }
  );
});

test("a state that is merely mis-cased or padded is still a ninth state — never coerced", () => {
  // The backend spells these kebab-case and exactly; anything else is drift,
  // and quietly normalizing it would hide the drift rather than report it.
  for (const bad of ["Queued", "ci_wait", " queued", "kicked back", ""]) {
    assert.throws(() => entryStateLabel(bad), UnknownMergeQueueState, `${JSON.stringify(bad)} was accepted`);
  }
});

test("an unknown status word throws too — a payload this build can't interpret is not an empty queue", () => {
  assert.equal(QUEUE_STATUSES.length, 4);
  assert.throws(() => mergeQueueView(status({ status: "quarantined" })), UnknownMergeQueueStatus);
});

// --- an unreadable queue is not an empty queue -----------------------------

test("no merge_queue.json at all is the ONLY silent case", () => {
  const v = mergeQueueView(status({ status: "absent", entries: [], entries_total: 0, target: "" }));
  assert.equal(v.kind, "hidden");
});

test("a file that exists and can't be read is LOUD, never hidden and never an empty queue", () => {
  const v = mergeQueueView(
    status({
      status: "unreadable",
      detail: "merge_queue.json did not parse: unknown variant `frobnicating`",
      entries: [],
      entries_total: 0,
      target: "",
    })
  );
  assert.equal(v.kind, "problem", "an unreadable queue rendered as hidden — indistinguishable from no queue");
  if (v.kind !== "problem") return;
  assert.match(v.line, /unreadable/);
  assert.match(v.detail ?? "", /frobnicating/, "the parser's own reason must survive to the human");
});

test("a schema this build doesn't understand is refused, and says which version", () => {
  const v = mergeQueueView(
    status({ status: "unsupported-version", version: 2, detail: "…", entries: [], entries_total: 0 })
  );
  assert.equal(v.kind, "problem");
  if (v.kind !== "problem") return;
  assert.match(v.line, /version 2/);
});

// --- rule 3: truncation surfaced, never silent -----------------------------

test("the row cap surfaces its own cut", () => {
  const entries = Array.from({ length: ROW_LIMIT + 4 }, (_, i) => entry({ pr: 600 + i }));
  const v = mergeQueueView(status({ entries }));
  assert.equal(v.kind, "queue");
  if (v.kind !== "queue") return;
  assert.equal(v.rows.length, ROW_LIMIT);
  assert.equal(v.note, `showing ${ROW_LIMIT} of ${ROW_LIMIT + 4} entries`);
});

test("the backend's wire cap surfaces too — entries_total is the file's count, not the payload's", () => {
  const entries = Array.from({ length: 3 }, (_, i) => entry({ pr: 600 + i }));
  const v = mergeQueueView(status({ entries, entries_total: 70, truncated: true }));
  assert.equal(v.kind, "queue");
  if (v.kind !== "queue") return;
  assert.equal(v.rows.length, 3);
  assert.equal(v.note, "showing 3 of 70 entries");
  assert.match(v.line, /70 entries/, "the header counts the file, not the slice that reached the webview");
});

test("both cuts compose into one honest pair of numbers", () => {
  const entries = Array.from({ length: 64 }, (_, i) => entry({ pr: 600 + i }));
  const v = mergeQueueView(status({ entries, entries_total: 71, truncated: true }));
  assert.equal(v.kind, "queue");
  if (v.kind !== "queue") return;
  assert.equal(v.note, `showing ${ROW_LIMIT} of 71 entries`);
});

test("nothing cut, no note — the convention doesn't cry wolf", () => {
  const entries = Array.from({ length: ROW_LIMIT }, (_, i) => entry({ pr: 600 + i }));
  assert.equal(truncationNote(ROW_LIMIT, status({ entries })), null);
});

test("a `truncated` flag the arithmetic can't demonstrate is still honored", () => {
  // The reader that did the cutting is the authority (the audit_log_windowed
  // precedent): an exactly-full list is indistinguishable from a cut one by
  // counting, which is why the flag exists and why it wins here.
  const entries = Array.from({ length: 2 }, (_, i) => entry({ pr: 600 + i }));
  assert.equal(truncationNote(2, status({ entries, entries_total: 2, truncated: true })), "showing 2 of 2+ entries");
});

// --- the header line -------------------------------------------------------

test("the header names the target and the in-flight batch — the two things a human is watching", () => {
  const v = mergeQueueView(status({ entries: [entry({ state: "ci-wait" })], batch: batch() }));
  assert.equal(v.kind, "queue");
  if (v.kind !== "queue") return;
  assert.match(v.line, /merge queue · → feat\/integration-batch-2 · 1 entry · batch mq-7f3a \(#640\)/);
  assert.match(v.line, /#612 \+ #613/);
});

test("a drained queue has no target and says so by omission, not by an empty arrow", () => {
  const v = mergeQueueView(status({ target: "", entries: [], entries_total: 0 }));
  assert.equal(v.kind, "queue");
  if (v.kind !== "queue") return;
  assert.equal(v.line, "merge queue · 0 entries");
  assert.equal(v.rows.length, 0);
  assert.equal(v.note, null);
});

test("batchLine refuses a ninth batch state as hard as an entry's", () => {
  assert.throws(() => batchLine(batch({ state: "warping" })), UnknownMergeQueueState);
});

test("the help text never promises the queue can touch the default branch", () => {
  assert.match(MERGE_QUEUE_HELP, /never the default branch/);
});

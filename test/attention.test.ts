// Unit tests for the pure attention-routing presentation mapping shared by the
// pane header chip and the minimize-dock chip. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  attentionPresentation,
  dockChipAttention,
  attentionDismiss,
  attentionChanged,
} from "../src/attention.ts";

test("each known reason maps to its label", () => {
  assert.equal(attentionPresentation("blocked").label, "⚠ blocked");
  assert.equal(attentionPresentation("stranded").label, "⚠ stuck prompt");
  assert.equal(attentionPresentation("waiting").label, "⚠ waiting");
  assert.equal(attentionPresentation("report").label, "✓ reported");
  // #1091 slice D: a pending `ask_human` row on this pane's own asker.
  assert.equal(attentionPresentation("question").label, "❓ question");
  assert.equal(attentionPresentation("gate").label, "⚑ your call");
});

test("'blocked' and 'stranded' are the urgent reasons", () => {
  assert.equal(attentionPresentation("blocked").urgent, true);
  // #496 PR-C: a prompt that was delivered but never submitted wedges the
  // pane until an Enter lands — red, not the amber of a pane that is merely
  // parked on a question it is happy to keep asking.
  assert.equal(attentionPresentation("stranded").urgent, true);
  for (const reason of ["waiting", "report", "question", "gate"]) {
    assert.equal(attentionPresentation(reason).urgent, false, `${reason} not urgent`);
  }
});

test("a stranded pane's dock chip is red and keeps the backend's instruction", () => {
  // #496 PR-C: minimizing a wedged pane must not hide it — the dock chip is
  // the only surface left, and its tooltip carries the badge detail verbatim
  // (which names what the human has to clear).
  const stranded = attentionPresentation("stranded");
  const chip = dockChipAttention("orch", {
    label: stranded.label,
    urgent: stranded.urgent,
    detail: "orch's prompt is stuck behind text you typed — press Enter or clear the box",
  });
  assert.equal(chip.needsAttention, true);
  assert.equal(chip.urgent, true, "a wedged pane is red on the dock too");
  assert.match(chip.title, /stuck prompt/);
  assert.match(chip.title, /press Enter or clear the box/);
});

test("an unknown reason falls back to a generic, non-urgent badge", () => {
  const p = attentionPresentation("some-future-reason");
  assert.equal(p.label, "⚠ attention");
  assert.equal(p.urgent, false);
});

// The dock-dot path (#40): once detection sets an attention reason, a minimized
// pane's dock chip must mirror it — the dot is how attention survives docking.
test("a docked pane with attention shows the dot and mirrors urgency", () => {
  // An agent parked on an interactive question surfaces as reason "waiting".
  const waiting = attentionPresentation("waiting");
  const chip = dockChipAttention("copilot", {
    label: waiting.label,
    urgent: waiting.urgent,
    detail: "copilot is waiting on a prompt",
  });
  assert.equal(chip.needsAttention, true, "waiting must light the dock dot");
  assert.equal(chip.urgent, false, "waiting is amber, not urgent red");
  assert.match(chip.title, /waiting/);
  assert.match(chip.title, /restore copilot/);

  // A blocked report is the urgent (red) variant.
  const blocked = attentionPresentation("blocked");
  const urgentChip = dockChipAttention("w", {
    label: blocked.label,
    urgent: blocked.urgent,
    detail: null,
  });
  assert.equal(urgentChip.needsAttention, true);
  assert.equal(urgentChip.urgent, true);
  assert.match(urgentChip.title, /needs you/);
});

test("a docked pane with no attention shows no dot, only a restore hint", () => {
  const chip = dockChipAttention("editor", null);
  assert.equal(chip.needsAttention, false);
  assert.equal(chip.urgent, false);
  assert.equal(chip.title, "Restore editor");
});

// #825 M1: the explicit dismiss. `stranded` is the one LATCHED reason — it
// stays up until something removes it backend-side, and for several blocker
// classes nothing ever does — so it is the one that needs a gesture of its own.
test("the latched stranded chip is the one that offers an explicit dismiss", () => {
  const d = attentionDismiss("stranded", "w-3");
  assert.equal(d.dismissible, true);
  assert.notEqual(d.label, "", "a dismissible chip needs something to click");
});

test("the live-recomputed reasons offer no dismiss control", () => {
  // These are re-derived by every 3-second attention scan (waiting/gate) or
  // already released by the focus ack (report/blocked). A dismiss control on
  // them would be a button that visibly does nothing — the chip is back on the
  // next tick — which teaches the human that dismissing does not work, the
  // exact complaint #825 exists to fix.
  for (const reason of ["waiting", "report", "question", "gate", "blocked"]) {
    assert.equal(
      attentionDismiss(reason, "w-3").dismissible,
      false,
      `${reason} is not dismissible`,
    );
  }
  assert.equal(attentionDismiss(null, "w-3").dismissible, false, "no chip, nothing to dismiss");
});

test("a stranded chip with no agent identity offers no dismiss", () => {
  // The backend releases the badge by agent id (`orch_dismiss_stranded`), so a
  // plain pane — which has no orchestration identity — has nothing to send.
  // Offering the control anyway would be a click that silently fails.
  assert.equal(attentionDismiss("stranded", null).dismissible, false);
  assert.equal(attentionDismiss("stranded", "").dismissible, false, "an empty id is no id");
});

test("the dismiss tooltip promises only what the dismiss actually does", () => {
  // It takes the CHIP down; it does not unstick the pane. A tooltip that
  // implied otherwise would be the false claim this whole issue is about —
  // a human who reads "resolve" and walks away from a genuinely wedged pane.
  //
  // The disclaimer is pinned as a phrase rather than a vibe because it IS the
  // guarantee: a chip the human can take down on their own say-so is only
  // honest while the control says what it settles and what it leaves alone.
  const { title } = attentionDismiss("stranded", "w-3");
  assert.match(title, /dismiss/i);
  assert.match(
    title,
    /does not unstick the pane/i,
    `the tooltip must say what it does NOT do: ${title}`,
  );
});

// #1091 slice D review: `Pane.setAttention` used to be idempotent on `reason`
// ALONE, which meant a pending-question count going from 1 to 2 — same
// `reason: "question"`, different `detail` — never reached the chip's
// tooltip: the docs claimed "hover it for the question count" while the code
// silently kept showing whatever count first raised the badge. Same defect
// shape for `gate` (detail carries the task's status, which can change
// without the task leaving the gate-status set). `attentionChanged` is the
// extracted, pure identity check `setAttention` now gates on — this pins
// that a detail-only change is still a change.
test("attentionChanged fires on a detail change even when the reason stays the same", () => {
  assert.equal(
    attentionChanged("question", "1 pending question — needs your answer", "question", "1 pending question — needs your answer"),
    false,
    "an identical repeat is a no-op",
  );
  assert.equal(
    attentionChanged("question", "1 pending question — needs your answer", "question", "2 pending questions — needs your answer"),
    true,
    "a growing count must not be swallowed by same-reason idempotency",
  );
  assert.equal(
    attentionChanged("gate", "task is pr — awaiting your call", "gate", "task is human-testing — awaiting your call"),
    true,
    "gate's status text is the same live-detail shape as question's count",
  );
});

test("attentionChanged treats a fresh reason and a clear as changes too", () => {
  assert.equal(attentionChanged(null, null, "question", "1 pending question — needs your answer"), true);
  assert.equal(attentionChanged("question", "1 pending question — needs your answer", null, null), true);
  assert.equal(attentionChanged(null, null, null, null), false, "clear-on-clear is still a no-op");
});

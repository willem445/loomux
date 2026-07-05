// Unit tests for the pure attention-routing presentation mapping shared by the
// pane header chip and the minimize-dock chip. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { attentionPresentation, dockChipAttention } from "../src/attention.ts";

test("each known reason maps to its label", () => {
  assert.equal(attentionPresentation("blocked").label, "⚠ blocked");
  assert.equal(attentionPresentation("waiting").label, "⚠ waiting");
  assert.equal(attentionPresentation("report").label, "✓ reported");
  assert.equal(attentionPresentation("gate").label, "⚑ your call");
});

test("only 'blocked' is urgent", () => {
  assert.equal(attentionPresentation("blocked").urgent, true);
  for (const reason of ["waiting", "report", "gate"]) {
    assert.equal(attentionPresentation(reason).urgent, false, `${reason} not urgent`);
  }
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

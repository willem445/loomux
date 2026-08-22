import test from "node:test";
import assert from "node:assert/strict";

import { wipChips } from "../src/wipchips.ts";
import type { WipCap } from "../src/orchestration.ts";

const cap = (status: string, count: number, limit: number, enforce = false): WipCap => ({
  status,
  count,
  limit,
  enforce,
});

test("no declared caps renders nothing at all — the header a repo without the feature has", () => {
  assert.deepEqual(wipChips([]), []);
  assert.deepEqual(wipChips(undefined), []);
});

test("a chip reads count/limit and its fill is the three states, not two", () => {
  const [under, full, over] = wipChips([
    cap("queued", 2, 8),
    cap("in-progress", 4, 4),
    cap("review", 5, 3),
  ]);
  assert.equal(under!.text, "queued 2/8");
  assert.equal(under!.fill, "under");
  assert.equal(full!.text, "in-progress 4/4");
  // AT the limit is already "full": the practice is "start nothing new", which
  // begins at the cap and not one past it. A two-state fill would say nothing
  // until the board was already over.
  assert.equal(full!.fill, "full");
  assert.equal(over!.text, "review 5/3");
  assert.equal(over!.fill, "over");
});

test("chips render in BOARD order, never in the order the backend listed them", () => {
  // The backend's caps come out of a BTreeMap, so they arrive alphabetically.
  const chips = wipChips([cap("review", 1, 3), cap("in-progress", 1, 4), cap("blocked", 1, 2)]);
  assert.deepEqual(
    chips.map((c) => c.status),
    ["in-progress", "review", "blocked"]
  );
});

test("a status this build does not know is shown LAST, never dropped", () => {
  // A newer backend's ninth status is still a real limit the human is subject
  // to; hiding it would be the board lying by omission.
  const chips = wipChips([cap("triage", 2, 2), cap("review", 1, 3)]);
  assert.deepEqual(
    chips.map((c) => c.status),
    ["review", "triage"]
  );
  assert.equal(chips[1]!.text, "triage 2/2");
});

test("the tooltip says what to DO, and whose writes an enforced cap stops", () => {
  const [warned] = wipChips([cap("review", 3, 3, false)]);
  assert.match(warned!.title, /3 of a declared limit of 3/);
  assert.match(warned!.title, /Finish or re-status something in review/);
  // Warn mode must not claim anything is refused — that is the whole
  // difference between the two postures.
  assert.match(warned!.title, /does not refuse anything/);
  assert.equal(warned!.enforce, false);

  const [enforced] = wipChips([cap("review", 3, 3, true)]);
  assert.equal(enforced!.enforce, true);
  // The human's own board edits are never refused, and the tooltip is the only
  // place the human is told so.
  assert.match(enforced!.title, /your edits here are not/);
  assert.doesNotMatch(enforced!.title, /does not refuse anything/);
});

test("an under-cap chip offers headroom rather than an instruction", () => {
  const [c] = wipChips([cap("in-progress", 1, 4)]);
  assert.match(c!.title, /Room for 3 more/);
  assert.doesNotMatch(c!.title, /Finish or re-status/);
});

test("being over the cap says by how much — a board at 5/3 is not the same as 4/3", () => {
  const [c] = wipChips([cap("pr", 5, 3)]);
  assert.match(c!.title, /over by 2/);
});

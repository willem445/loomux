// Unit tests for the pure delivery-hold badge presentation mapping (#246).
// Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { heldPresentation } from "../src/heldbadge.ts";

test("each known reason maps to a distinct, descriptive label", () => {
  assert.equal(heldPresentation("typing").label, "⏸ held: typing");
  assert.equal(heldPresentation("box-occupied").label, "⏸ held: unsubmitted text");
});

test("an unknown reason falls back to a generic held badge, not a blank one", () => {
  assert.equal(heldPresentation("some-future-reason").label, "⏸ held");
});

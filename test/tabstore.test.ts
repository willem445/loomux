// Persistence round-trip + validation for the project-tab set (#63 phase 5).
// Pure (tabstore.ts) — the localStorage wiring is validated by hand. `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { encodeTabs, decodeTabs, type PersistedTabs } from "../src/tabstore.ts";

test("encode → decode round-trips name / color / group / active index", () => {
  const state: PersistedTabs = {
    tabs: [
      { name: "loomux", color: "#9ece6a", groupId: "grp-1" },
      { name: "scratch", color: null, groupId: null },
    ],
    activeIndex: 1,
  };
  const back = decodeTabs(encodeTabs(state));
  assert.deepEqual(back, state);
});

test("decode returns null for missing / non-JSON / shapeless input", () => {
  assert.equal(decodeTabs(null), null);
  assert.equal(decodeTabs(""), null);
  assert.equal(decodeTabs("not json {"), null);
  assert.equal(decodeTabs(JSON.stringify({ nope: 1 })), null, "no tabs array");
  assert.equal(decodeTabs(JSON.stringify({ tabs: [] })), null, "empty tab set → null (seed a fresh tab)");
});

test("decode drops malformed tab entries and coerces bad fields", () => {
  const raw = JSON.stringify({
    tabs: [
      { name: "keep", color: 123, groupId: {} }, // bad color/group → null
      { color: "#fff" }, // no name → dropped
      { name: "  " }, // blank name → dropped
      { name: "second", color: "#7aa2f7", groupId: "g" },
    ],
    activeIndex: 0,
  });
  const back = decodeTabs(raw);
  assert.deepEqual(back, {
    tabs: [
      { name: "keep", color: null, groupId: null },
      { name: "second", color: "#7aa2f7", groupId: "g" },
    ],
    activeIndex: 0,
  });
});

test("decode clamps an out-of-range or missing activeIndex to 0", () => {
  const mk = (activeIndex: unknown) =>
    JSON.stringify({ tabs: [{ name: "a", color: null, groupId: null }], activeIndex });
  assert.equal(decodeTabs(mk(9))?.activeIndex, 0, "beyond range → 0");
  assert.equal(decodeTabs(mk(-1))?.activeIndex, 0, "negative → 0");
  assert.equal(decodeTabs(mk("x"))?.activeIndex, 0, "non-number → 0");
  assert.equal(decodeTabs(mk(1.5))?.activeIndex, 0, "non-integer → 0");
});

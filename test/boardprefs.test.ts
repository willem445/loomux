// Unit tests for the durable task-board view store (#1270) — src/boardprefs.ts.
// Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  BOARD_PREFS_VERSION,
  decodeBoardPrefs,
  defaultGroupView,
  encodeBoardPrefs,
  MAX_GROUPS,
  readGroupView,
  writeGroupView,
  type BoardPrefs,
} from "../src/boardprefs.ts";
import { NO_FILTER } from "../src/taskboard.ts";

const view = (over: Partial<ReturnType<typeof defaultGroupView>> = {}) => ({
  ...defaultGroupView(),
  ...over,
});

test("a group with nothing persisted opens at the pre-#1270 board", () => {
  const empty: BoardPrefs = new Map();
  const v = readGroupView(empty, "g-1");
  assert.deepEqual(v.collapsed, []);
  assert.deepEqual(v.filter, { ...NO_FILTER });
  assert.deepEqual(v.unknownFilters, {});
});

test("a write survives an encode/decode round trip, field for field", () => {
  const prefs = writeGroupView(
    new Map(),
    "loomux-abc",
    view({
      collapsed: ["e-1", "f-2"],
      filter: { kind: ["story", "unlabelled"], status: ["blocked"], text: "auth", attention: true },
    }),
    1_700_000_000_000
  );
  const back = readGroupView(decodeBoardPrefs(encodeBoardPrefs(prefs)), "loomux-abc");
  assert.deepEqual(back.collapsed, ["e-1", "f-2"]);
  assert.deepEqual(back.filter, {
    kind: ["story", "unlabelled"],
    status: ["blocked"],
    text: "auth",
    attention: true,
  });
  assert.equal(back.touched, 1_700_000_000_000);
  assert.equal(JSON.parse(encodeBoardPrefs(prefs)).v, BOARD_PREFS_VERSION);
});

test("a filter family this build does not know is preserved verbatim", () => {
  // The forward-compat claim the schema is built on (#1272's sprint filter,
  // #1273's typed links): a new family is a KEY, not a migration. That is only
  // true in both directions if a build that does not know the key hands it back
  // unchanged — otherwise opening the board once on an older build silently
  // deletes the newer one's state.
  const stored = JSON.stringify({
    v: 1,
    groups: {
      "g-1": {
        touched: 7,
        collapsed: [],
        filters: { kind: ["epic"], sprint: ["s-4"], someFutureFlag: true },
      },
    },
  });
  const prefs = decodeBoardPrefs(stored);
  assert.deepEqual(readGroupView(prefs, "g-1").unknownFilters, {
    sprint: ["s-4"],
    someFutureFlag: true,
  });
  const filters = JSON.parse(encodeBoardPrefs(prefs)).groups["g-1"].filters;
  assert.deepEqual(filters.sprint, ["s-4"]);
  assert.equal(filters.someFutureFlag, true);
  assert.deepEqual(filters.kind, ["epic"], "the known family still round-trips beside it");
});

test("an unknown family cannot shadow a known one", () => {
  // The spread order in encodeBoardPrefs. A newer build (or a hand edit)
  // putting a bad `kind` into the unknown bag must not be able to overwrite the
  // validated one — the value the board actually filters on has to be the one
  // this build validated.
  const prefs: BoardPrefs = new Map([
    [
      "g-1",
      view({
        filter: { kind: ["epic"], status: [], text: "", attention: false },
        unknownFilters: { kind: "nonsense", status: 42 },
      }),
    ],
  ]);
  const filters = JSON.parse(encodeBoardPrefs(prefs)).groups["g-1"].filters;
  assert.deepEqual(filters.kind, ["epic"]);
  assert.deepEqual(filters.status, []);
});

test("the store keeps the most recently touched groups and evicts the rest", () => {
  let prefs: BoardPrefs = new Map();
  // MAX_GROUPS + 3 groups, each touched later than the last.
  for (let i = 0; i < MAX_GROUPS + 3; i++) {
    prefs = writeGroupView(prefs, `g-${i}`, view({ collapsed: [`c-${i}`] }), 1000 + i);
  }
  const kept = decodeBoardPrefs(encodeBoardPrefs(prefs));
  assert.equal(kept.size, MAX_GROUPS, "the file is bounded, not the in-memory map");
  assert.equal(kept.has(`g-${MAX_GROUPS + 2}`), true, "the newest survives");
  assert.equal(kept.has("g-0"), false, "the three oldest are evicted");
  assert.equal(kept.has("g-1"), false);
  assert.equal(kept.has("g-2"), false);
  assert.equal(kept.has("g-3"), true, "…and nothing beyond the three");
});

test("re-saving an old group lifts it clear of eviction", () => {
  // The whole point of LRU over insertion order: the board you actually use
  // must not age out because you opened it first.
  let prefs: BoardPrefs = new Map();
  for (let i = 0; i < MAX_GROUPS; i++) {
    prefs = writeGroupView(prefs, `g-${i}`, view(), 1000 + i);
  }
  prefs = writeGroupView(prefs, "g-0", view({ collapsed: ["still-here"] }), 9999);
  prefs = writeGroupView(prefs, "newcomer", view(), 10_000);
  const kept = decodeBoardPrefs(encodeBoardPrefs(prefs));
  assert.equal(kept.size, MAX_GROUPS);
  assert.deepEqual(readGroupView(kept, "g-0").collapsed, ["still-here"]);
  assert.equal(kept.has("g-1"), false, "the next-oldest went instead");
});

test("a corrupt or absent blob degrades to an empty store, never a throw", () => {
  for (const bad of [null, "", "not json", "[1,2,3]", '"a string"', "42", "{}"]) {
    assert.deepEqual([...decodeBoardPrefs(bad).keys()], [], `input: ${JSON.stringify(bad)}`);
  }
});

test("one malformed group does not cost the others their view", () => {
  const stored = JSON.stringify({
    v: 1,
    groups: {
      good: { touched: 5, collapsed: ["e-1"], filters: { text: "auth" } },
      broken: [1, 2, 3],
      alsoBroken: "nope",
      "": { touched: 1, collapsed: ["x"] },
    },
  });
  const prefs = decodeBoardPrefs(stored);
  assert.deepEqual([...prefs.keys()], ["good"], "an empty group id is dropped too");
  assert.equal(readGroupView(prefs, "good").filter.text, "auth");
});

// One malformed-field specimen, read by three tests rather than one. Each
// fallback gets its own, so a mutation that removes exactly one of them
// reddens exactly one test — a single test asserting all three would abort at
// its first failure and leave the other two unproven.
const MALFORMED = JSON.stringify({
  v: 1,
  groups: {
    "g-1": {
      touched: "yesterday",
      collapsed: ["e-1", 7, null, "", "f-2"],
      filters: { kind: "epic", status: ["blocked"], text: 12, attention: "yes" },
    },
  },
});

test("a non-numeric touched stamp reads as oldest, so a broken record evicts first", () => {
  assert.equal(readGroupView(decodeBoardPrefs(MALFORMED), "g-1").touched, 0);
});

test("a list field drops what is not a usable string, and is never coerced from one", () => {
  const v = readGroupView(decodeBoardPrefs(MALFORMED), "g-1");
  assert.deepEqual(v.collapsed, ["e-1", "f-2"], "non-string and empty ids are dropped");
  assert.deepEqual(v.filter.kind, [], "a bare string where a list belongs is not coerced");
  assert.deepEqual(v.filter.status, ["blocked"], "…and the sibling family is untouched");
  assert.equal(v.filter.text, "", "a number where a string belongs falls back too");
});

test("only a real `true` arms the attention toggle", () => {
  // `"yes"` is truthy, and a filter that silently armed itself off a
  // hand-edited string would empty the board on the next launch with no
  // gesture behind it.
  assert.equal(readGroupView(decodeBoardPrefs(MALFORMED), "g-1").filter.attention, false);
});

test("a group id that names an Object.prototype member is stored, not swallowed", () => {
  // Why the store is a Map. With a plain object, `prefs["toString"]` reads a
  // function on every group that has no record, and `"constructor" in prefs` is
  // true for a group nobody ever saved — either of which turns a normal-looking
  // group id into a board that cannot persist anything.
  const prefs = writeGroupView(new Map(), "toString", view({ collapsed: ["e-1"] }), 1);
  assert.deepEqual(readGroupView(prefs, "toString").collapsed, ["e-1"]);
  const back = decodeBoardPrefs(encodeBoardPrefs(prefs));
  assert.deepEqual(readGroupView(back, "toString").collapsed, ["e-1"]);
  assert.deepEqual(readGroupView(back, "constructor").collapsed, [], "and an absent one is empty");
});

test("readGroupView and writeGroupView hand out copies, never the store's interior", () => {
  // A caller mutating what it read would skip writeGroupView entirely and its
  // change would never be saved — silently, since the in-memory board would
  // look right until the next launch.
  const prefs = writeGroupView(new Map(), "g-1", view({ collapsed: ["e-1"] }), 1);
  const got = readGroupView(prefs, "g-1");
  got.collapsed.push("sneaky");
  got.filter.kind = ["epic"];
  assert.deepEqual(readGroupView(prefs, "g-1").collapsed, ["e-1"]);
  assert.deepEqual(readGroupView(prefs, "g-1").filter.kind, []);
  // …and the source object handed to writeGroupView is likewise not retained.
  const source = view({ collapsed: ["a"] });
  const stored = writeGroupView(new Map(), "g-2", source, 1);
  source.collapsed.push("b");
  assert.deepEqual(readGroupView(stored, "g-2").collapsed, ["a"]);
});

test("writeGroupView returns a new map and leaves the old one alone", () => {
  const before: BoardPrefs = new Map();
  const after = writeGroupView(before, "g-1", view(), 1);
  assert.equal(before.size, 0, "a failed save must leave nothing half-applied");
  assert.equal(after.size, 1);
});

test("a future version number is read rather than refused", () => {
  // Every field is validated per key regardless of the version, so the worst a
  // newer file can do is contribute keys this build ignores and preserves.
  // Refusing it would throw away state a downgrade could hand straight back.
  const stored = JSON.stringify({
    v: 99,
    groups: { "g-1": { touched: 3, collapsed: ["e-1"], filters: { text: "auth" } } },
  });
  assert.deepEqual(readGroupView(decodeBoardPrefs(stored), "g-1").collapsed, ["e-1"]);
});

// Unit tests for the durable task-board view store (#1270) — src/boardprefs.ts.
// Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  BOARD_PREFS_VERSION,
  BoardPrefsStore,
  decodeBoardPrefs,
  defaultGroupView,
  encodeBoardPrefs,
  MAX_GROUPS,
  readGroupView,
  writeGroupView,
  type BoardPrefs,
  type BoardPrefsIo,
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
    "orrerix-abc",
    view({
      collapsed: ["e-1", "f-2"],
      filter: {
        kind: ["story", "unlabelled"],
        status: ["blocked"],
        // Both spellings the sprint family can hold — a decimal number and the
        // backlog sentinel — so the round trip is exercised on the sentinel
        // too, which is the one value a numeric decoder would have eaten.
        sprint: ["2", "backlog"],
        text: "auth",
        attention: true,
      },
    }),
    1_700_000_000_000
  );
  const back = readGroupView(decodeBoardPrefs(encodeBoardPrefs(prefs)), "orrerix-abc");
  assert.deepEqual(back.collapsed, ["e-1", "f-2"]);
  assert.deepEqual(back.filter, {
    kind: ["story", "unlabelled"],
    status: ["blocked"],
    sprint: ["2", "backlog"],
    text: "auth",
    attention: true,
  });
  assert.equal(back.touched, 1_700_000_000_000);
  assert.equal(JSON.parse(encodeBoardPrefs(prefs)).v, BOARD_PREFS_VERSION);
});

/** The witness for "a family this build does not know".
 *
 *  Deliberately a name no `BoardFilter` key can ever take — the real ones are
 *  single lowercase identifiers — rather than a plausible future family. These
 *  tests were originally written with `sprint` as the specimen, and #1272
 *  shipped that family: the specimen left the class it was witnessing, and the
 *  assertions went red on a build where nothing about forward compatibility had
 *  changed. Relocating onto a name that can never be adopted is what keeps the
 *  property pinned instead of decaying into a list of what has not landed yet. */
const FUTURE_FAMILY = "family-from-a-newer-build";

test("a filter family this build does not know is preserved verbatim", () => {
  // The forward-compat claim the schema is built on: a new family is a KEY, not
  // a migration. That is only true in both directions if a build that does not
  // know the key hands it back unchanged — otherwise opening the board once on
  // an older build silently deletes the newer one's state.
  const stored = JSON.stringify({
    v: 1,
    groups: {
      "g-1": {
        touched: 7,
        collapsed: [],
        filters: { kind: ["epic"], [FUTURE_FAMILY]: ["s-4"], someFutureFlag: true },
      },
    },
  });
  const prefs = decodeBoardPrefs(stored);
  assert.deepEqual(readGroupView(prefs, "g-1").unknownFilters, {
    [FUTURE_FAMILY]: ["s-4"],
    someFutureFlag: true,
  });
  const filters = JSON.parse(encodeBoardPrefs(prefs)).groups["g-1"].filters;
  assert.deepEqual(filters[FUTURE_FAMILY], ["s-4"]);
  assert.equal(filters.someFutureFlag, true);
  assert.deepEqual(filters.kind, ["epic"], "the known family still round-trips beside it");
  // The other half of the same guarantee, and the half #1272 actually
  // exercised: a family this build DOES know is decoded into `filter`, never
  // left in the unknown bag where a later write would treat it as passthrough.
  const known = decodeBoardPrefs(
    JSON.stringify({
      v: 1,
      groups: { "g-2": { touched: 7, collapsed: [], filters: { sprint: ["3", "backlog"] } } },
    })
  );
  assert.deepEqual(readGroupView(known, "g-2").filter.sprint, ["3", "backlog"]);
  assert.deepEqual(readGroupView(known, "g-2").unknownFilters, {});
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
        filter: { kind: ["epic"], status: [], sprint: ["2"], text: "", attention: false },
        unknownFilters: { kind: "nonsense", status: 42, sprint: ["99"] },
      }),
    ],
  ]);
  const filters = JSON.parse(encodeBoardPrefs(prefs)).groups["g-1"].filters;
  assert.deepEqual(filters.kind, ["epic"]);
  assert.deepEqual(filters.status, []);
  // Every family this build owns, not just the two that existed when the rule
  // was written: the spread order has to cover the whole set, and a family
  // added below it in the literal would be the one that could be shadowed.
  assert.deepEqual(filters.sprint, ["2"]);
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
  // Positive control FIRST (CLAUDE.md, #1209): "every one of these decodes to
  // nothing" is satisfied just as well by a decoder that returns nothing for
  // everything, which would make the whole loop below vacuous.
  assert.deepEqual(
    [...decodeBoardPrefs(JSON.stringify({ v: 1, groups: { "g-1": { touched: 1 } } })).keys()],
    ["g-1"],
    "the decoder does read a good blob — without this the loop below proves nothing"
  );
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

test("a group id that names an Object.prototype member survives a round trip", () => {
  // Why the store is a Map, and why the ENCODER's accumulator is
  // `Object.create(null)`. With a plain object, `prefs["toString"]` reads a
  // function on every group that has no record, and `"constructor" in prefs` is
  // true for a group nobody ever saved — either of which turns a normal-looking
  // group id into a board that cannot persist anything.
  //
  // `__proto__` leads, because it is the member of this class that actually
  // BROKE (#1270 review N3): assigning it on an object literal reaches the
  // prototype setter instead of creating an own property, so it was the one id
  // silently dropped at encode time while `toString` and `constructor` — which
  // this test used to be written on — round-tripped fine. A specimen that
  // cannot distinguish is not a witness.
  for (const id of ["__proto__", "toString", "constructor"]) {
    const prefs = writeGroupView(new Map(), id, view({ collapsed: ["e-1"] }), 1);
    assert.deepEqual(readGroupView(prefs, id).collapsed, ["e-1"], `in-memory: ${id}`);
    const encoded = encodeBoardPrefs(prefs);
    assert.deepEqual(
      Object.keys(JSON.parse(encoded).groups),
      [id],
      `the encoded blob must carry ${id} as its own key`
    );
    assert.deepEqual(
      readGroupView(decodeBoardPrefs(encoded), id).collapsed,
      ["e-1"],
      `round trip: ${id}`
    );
  }
  const empty = decodeBoardPrefs(encodeBoardPrefs(new Map()));
  assert.deepEqual(readGroupView(empty, "constructor").collapsed, [], "an absent id is empty");
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

// ---------------------------------------------------------------------------
// BoardPrefsStore — the read-before-publish ordering (#1270 review B1).
//
// The blob is ONE file for every group, so a save built from a store that was
// never read publishes an empty map as the whole truth and destroys every other
// group's view. These are the tests that fail on the racy order.
// ---------------------------------------------------------------------------

/** A view payload for `write`. */
const wrote = (collapsed: string[]) => ({
  collapsed,
  filter: { ...NO_FILTER },
  unknownFilters: {},
});

/** A blob holding one OTHER group's view — the thing a premature save destroys. */
const OTHERS = JSON.stringify({
  v: 1,
  groups: { "g-other": { touched: 1, collapsed: ["e-9"], filters: { text: "auth" } } },
});

/** Records what reached the backend, and lets the test decide when the read
 *  resolves. */
function fakeIo(load: () => Promise<string | null>): BoardPrefsIo & { saved: string[] } {
  const saved: string[] = [];
  return {
    saved,
    load,
    save: async (contents: string) => {
      saved.push(contents);
    },
  };
}

/** Let every already-scheduled microtask/timer run. */
const settle = () => new Promise((r) => setTimeout(r, 0));

test("a write that beats the read does not publish a store nobody has read", async () => {
  // THE B1 PIN. The human folds a container before the load has come back. A
  // store that writes straight from its empty map publishes a blob containing
  // only this group — silently deleting up to MAX_GROUPS other records.
  let release: (v: string | null) => void = () => {};
  let reads = 0;
  const io = fakeIo(() => {
    reads += 1;
    return new Promise<string | null>((res) => (release = res));
  });
  const store = new BoardPrefsStore(io);

  const writing = store.write("g-mine", wrote(["e-1"]), 5);
  await settle();
  // Positive control before the absence (CLAUDE.md, #1209): the write really is
  // in flight and really did ask for the file. Without this, "nothing reached
  // disk" is also what a write that threw on entry, or a broken fake, looks
  // like — and the interesting assertion below would be vacuous.
  assert.equal(reads, 1, "the write asked for the file");
  assert.deepEqual(io.saved, [], "…and published nothing while that read was outstanding");

  release(OTHERS); // the file finally arrives, holding someone else's view
  assert.equal(await writing, "saved");
  assert.equal(io.saved.length, 1);

  const back = decodeBoardPrefs(io.saved[0]);
  assert.deepEqual(
    readGroupView(back, "g-other").collapsed,
    ["e-9"],
    "the other group's collapse set survived the write that raced it"
  );
  assert.equal(readGroupView(back, "g-other").filter.text, "auth", "…and its filter");
  assert.deepEqual(readGroupView(back, "g-mine").collapsed, ["e-1"], "…beside the new one");
});

test("a write is declined outright when the file could not be read", async () => {
  // "I could not look" is not "there was nothing there". Publishing here would
  // turn one transient IPC rejection into permanent data loss.
  const io = fakeIo(() => Promise.reject(new Error("ipc rejected")));
  const store = new BoardPrefsStore(io);
  assert.equal(await store.write("g-mine", wrote(["e-1"]), 5), "declined-unread");
  assert.deepEqual(io.saved, [], "a store that was never read is never published");
});

test("a failed read is retried by the next gesture, not latched for the session", async () => {
  // The other direction, so the guard cannot pass by refusing everything: one
  // rejection must not disable persistence for as long as the board is open.
  let attempt = 0;
  const io = fakeIo(() => {
    attempt += 1;
    return attempt === 1 ? Promise.reject(new Error("transient")) : Promise.resolve(OTHERS);
  });
  const store = new BoardPrefsStore(io);

  assert.equal(await store.write("g-mine", wrote(["e-1"]), 5), "declined-unread");
  assert.equal(await store.write("g-mine", wrote(["e-1"]), 6), "saved", "the retry lands");
  assert.equal(attempt, 2);
  const back = decodeBoardPrefs(io.saved[0]);
  assert.deepEqual(readGroupView(back, "g-other").collapsed, ["e-9"]);
  assert.deepEqual(readGroupView(back, "g-mine").collapsed, ["e-1"]);
});

test("an unreadable file reads as null, never as a group with nothing stored", async () => {
  // The caller must be able to tell "no record for this group" (adopt defaults)
  // from "I cannot see the file" (do not adopt, and retry later). This pins the
  // DISTINCTION only — what the caller does with it, and what protects the
  // record in the meantime, is the write-side behaviour pinned below (#1270
  // review N5).
  const bad = new BoardPrefsStore(fakeIo(() => Promise.reject(new Error("nope"))));
  assert.equal(await bad.read("g-1"), null);

  const absent = new BoardPrefsStore(fakeIo(() => Promise.resolve(null)));
  const view = await absent.read("g-1");
  assert.notEqual(view, null, "an ABSENT file is a complete answer, not a failure");
  assert.deepEqual(view?.collapsed, []);
});

test("the file is read once however many gestures arrive", async () => {
  let reads = 0;
  const io = fakeIo(() => {
    reads += 1;
    return Promise.resolve(OTHERS);
  });
  const store = new BoardPrefsStore(io);
  // Concurrent, then sequential — one shared in-flight read, then the memo.
  await Promise.all([store.read("a"), store.read("b"), store.write("c", wrote([]), 1)]);
  await store.write("d", wrote([]), 2);
  assert.equal(reads, 1, "a burst of gestures must not each re-read the blob");
});

test("a save that fails leaves the newer value in memory for the next gesture", async () => {
  const io: BoardPrefsIo = {
    load: () => Promise.resolve(OTHERS),
    save: () => Promise.reject(new Error("disk full")),
  };
  const store = new BoardPrefsStore(io);
  assert.equal(await store.write("g-mine", wrote(["e-1"]), 5), "failed");
  // Not "declined-unread": the read succeeded, so the failure is the write's.
  // The store keeps the value, which is what makes the next gesture re-offer it.
  assert.deepEqual((await store.read("g-mine"))?.collapsed, ["e-1"]);
});

test("a write carries over unknown families the caller never saw", async () => {
  // #1270 review N5, and the sharpest edge of it: `unknownFilters` is a NEWER
  // build's state (#1272's `sprint`), and the round-trip guarantee this schema
  // sells is that an older build preserves it. A caller supplying the families
  // could hand back an empty set — which is exactly what a view whose boot read
  // failed holds — and delete them. The store reads them off the record instead,
  // so the caller has nothing to get wrong.
  const io = fakeIo(() =>
    Promise.resolve(
      JSON.stringify({
        v: 1,
        groups: {
          "g-mine": { touched: 1, collapsed: ["e-1"], filters: { [FUTURE_FAMILY]: ["s-7"], kind: ["story"] } },
        },
      })
    )
  );
  const store = new BoardPrefsStore(io);
  assert.equal(await store.write("g-mine", { collapsed: ["e-9"], filter: { ...NO_FILTER } }, 5), "saved");
  const back = readGroupView(decodeBoardPrefs(io.saved[0]), "g-mine");
  assert.deepEqual(back.collapsed, ["e-9"], "the caller's own fields are what it set");
  assert.deepEqual(back.filter.kind, [], "…including clearing one");
  assert.deepEqual(
    back.unknownFilters,
    { [FUTURE_FAMILY]: ["s-7"] },
    "…but a newer build's family survives a write that never mentioned it"
  );
});

test("a first gesture after a FAILED boot read does not delete this group's unknown families", async () => {
  // The end-to-end shape of N5: boot read rejects, the view is left at its
  // defaults, then the human folds something. The write retries the read (by
  // design) and publishes — and the question is what it publishes over.
  let attempt = 0;
  const io = fakeIo(() => {
    attempt += 1;
    return attempt === 1
      ? Promise.reject(new Error("transient at boot"))
      : Promise.resolve(
          JSON.stringify({
            v: 1,
            groups: {
              "g-mine": { touched: 1, collapsed: ["e-1"], filters: { [FUTURE_FAMILY]: ["s-7"] } },
              "g-other": { touched: 2, collapsed: ["e-9"], filters: {} },
            },
          })
        );
  });
  const store = new BoardPrefsStore(io);

  assert.equal(await store.read("g-mine"), null, "the boot read failed");
  assert.deepEqual(io.saved, [], "and nothing was published on the strength of it");

  // The human's gesture, made against defaults because nothing could be adopted.
  assert.equal(await store.write("g-mine", { collapsed: ["e-5"], filter: { ...NO_FILTER } }, 9), "saved");
  const back = decodeBoardPrefs(io.saved[0]);
  assert.deepEqual(
    readGroupView(back, "g-mine").unknownFilters,
    { [FUTURE_FAMILY]: ["s-7"] },
    "the newer build's family survived the unhappy path, which is the guarantee #1272 is told to build on"
  );
  assert.deepEqual(
    readGroupView(back, "g-mine").collapsed,
    ["e-5"],
    "the human's own gesture wins over a file nobody could read — the accepted residue"
  );
  assert.deepEqual(
    readGroupView(back, "g-other").collapsed,
    ["e-9"],
    "and B1's guarantee still holds: other groups are untouched"
  );
});

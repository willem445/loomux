// Persistence round-trip + validation for the project-tab set (#63 phase 5),
// extended for the #194 session-restore schema (layout tree, restorePref,
// schemaVersion). Pure (tabstore.ts) — the localStorage/backend wiring is
// validated by hand. `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  encodeTabs,
  decodeTabs,
  SCHEMA_VERSION,
  type PersistedTabs,
  type PersistedLayoutNode,
} from "../src/tabstore.ts";

test("encode → decode round-trips name / color / group / active index", () => {
  const state: PersistedTabs = {
    tabs: [
      { name: "loomux", color: "#9ece6a", groupId: "grp-1" },
      { name: "scratch", color: null, groupId: null },
    ],
    activeIndex: 1,
    restorePref: "restore",
  };
  const back = decodeTabs(encodeTabs(state));
  // Decode always resolves restorePref + schemaVersion (they drive Phase 4 boot).
  assert.deepEqual(back, { ...state, schemaVersion: SCHEMA_VERSION });
});

test("encode stamps the current schema version and defaults restorePref to ask", () => {
  // A pre-#194 snapshot object (no restorePref/schemaVersion) must still encode —
  // this is what lets main.ts keep calling encodeTabs(tabs.snapshot()) unchanged.
  const encoded = encodeTabs({ tabs: [{ name: "a", color: null, groupId: null }], activeIndex: 0 });
  const parsed = JSON.parse(encoded);
  assert.equal(parsed.schemaVersion, SCHEMA_VERSION);
  assert.equal(parsed.restorePref, "ask");
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
    restorePref: "ask",
    schemaVersion: 1, // no version present → the pre-#194 v1 blob
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

// ---------- #194 migration: old files load cleanly ----------

test("an old (pre-#194) file decodes shells-only — no layout key, defaults applied", () => {
  // Exactly what a v1 file looks like: no schemaVersion, no restorePref, no layout.
  const raw = JSON.stringify({
    tabs: [{ name: "loomux", color: "#9ece6a", groupId: null }],
    activeIndex: 0,
  });
  const back = decodeTabs(raw);
  assert.deepEqual(back, {
    tabs: [{ name: "loomux", color: "#9ece6a", groupId: null }],
    activeIndex: 0,
    restorePref: "ask",
    schemaVersion: 1,
  });
  // Migration contract: no `layout` property is invented on an old tab.
  assert.ok(!("layout" in back!.tabs[0]), "old tab has no layout key");
});

// ---------- #194 layout tree ----------

const NESTED_LAYOUT: PersistedLayoutNode = {
  kind: "split",
  dir: "row",
  weight: 1,
  children: [
    {
      kind: "leaf",
      weight: 1,
      pane: {
        paneKind: "terminal",
        name: "shell",
        cwd: "/repo",
        command: null,
        argv: null,
        shellKind: "gitbash",
        sessionId: null,
      },
    },
    {
      kind: "split",
      dir: "column",
      weight: 2,
      children: [
        {
          kind: "leaf",
          weight: 1,
          pane: {
            paneKind: "agent",
            name: "claude",
            cwd: "/repo",
            command: "claude",
            argv: ["claude", "--resume", "abc-123"],
            shellKind: null,
            sessionId: "abc-123",
          },
        },
        {
          kind: "leaf",
          weight: 1,
          pane: {
            paneKind: "orch",
            name: "orchestrator",
            cwd: "/repo",
            command: null,
            argv: null,
            shellKind: null,
            sessionId: null,
          },
        },
      ],
    },
  ],
};

test("layout tree round-trips exactly (nested split, weights, all pane kinds)", () => {
  const state: PersistedTabs = {
    tabs: [{ name: "loomux", color: null, groupId: "g", layout: NESTED_LAYOUT }],
    activeIndex: 0,
    restorePref: "restore",
  };
  const back = decodeTabs(encodeTabs(state));
  assert.deepEqual(back?.tabs[0].layout, NESTED_LAYOUT);
});

test("a malformed layout node degrades that tab's whole layout to null (not a throw)", () => {
  const raw = JSON.stringify({
    tabs: [
      {
        name: "loomux",
        color: null,
        groupId: null,
        layout: {
          kind: "split",
          dir: "row",
          weight: 1,
          children: [
            { kind: "leaf", weight: 1, pane: { paneKind: "terminal", name: "ok" } },
            { kind: "leaf", weight: 1, pane: { paneKind: "bogus", name: "bad" } }, // invalid kind
          ],
        },
      },
    ],
    activeIndex: 0,
  });
  const back = decodeTabs(raw);
  // The tab survives — only its layout drops to null (restores as a fresh shell).
  assert.equal(back?.tabs.length, 1);
  assert.equal(back?.tabs[0].layout, null);
});

test("a leaf with no pane, an empty split, and a bad root all degrade to null", () => {
  const mk = (layout: unknown) =>
    decodeTabs(JSON.stringify({ tabs: [{ name: "t", color: null, groupId: null, layout }], activeIndex: 0 }))
      ?.tabs[0].layout;
  assert.equal(mk({ kind: "leaf", weight: 1 }), null, "leaf missing pane");
  assert.equal(mk({ kind: "split", dir: "row", weight: 1, children: [] }), null, "empty split");
  assert.equal(mk({ kind: "split", dir: "sideways", weight: 1, children: [] }), null, "bad dir");
  assert.equal(mk({ kind: "nonsense" }), null, "unknown node kind");
});

test("malformed pane fields inside a valid leaf coerce to null, not a drop", () => {
  const layout = {
    kind: "leaf",
    weight: "heavy", // bad weight → default 1
    pane: {
      paneKind: "agent",
      name: "claude",
      cwd: 42, // bad → null
      command: "claude",
      argv: ["ok", 7], // non-string element → whole argv null
      shellKind: "fish", // unknown → null
      sessionId: null,
    },
  };
  const back = decodeTabs(
    JSON.stringify({ tabs: [{ name: "t", color: null, groupId: null, layout }], activeIndex: 0 })
  );
  assert.deepEqual(back?.tabs[0].layout, {
    kind: "leaf",
    weight: 1,
    pane: {
      paneKind: "agent",
      name: "claude",
      cwd: null,
      command: "claude",
      argv: null,
      shellKind: null,
      sessionId: null,
    },
  });
});

test("restorePref and schemaVersion coerce unknown values to safe defaults", () => {
  const mk = (extra: object) =>
    decodeTabs(JSON.stringify({ tabs: [{ name: "t", color: null, groupId: null }], activeIndex: 0, ...extra }));
  assert.equal(mk({ restorePref: "restore" })?.restorePref, "restore", "valid pref kept");
  assert.equal(mk({ restorePref: "fresh" })?.restorePref, "fresh", "valid pref kept");
  assert.equal(mk({ restorePref: "maybe" })?.restorePref, "ask", "unknown pref → ask");
  assert.equal(mk({ restorePref: 7 })?.restorePref, "ask", "non-string pref → ask");
  assert.equal(mk({ schemaVersion: 2 })?.schemaVersion, 2, "valid version kept");
  assert.equal(mk({ schemaVersion: "two" })?.schemaVersion, 1, "non-number version → 1");
});

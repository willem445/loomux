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

test("docked panes round-trip (captured outside the layout tree, #194 P4)", () => {
  const state: PersistedTabs = {
    tabs: [
      {
        name: "loomux",
        color: null,
        groupId: null,
        docked: [
          {
            paneKind: "agent",
            name: "claude · fix",
            cwd: "/repo",
            command: "claude --session-id abc",
            argv: null,
            shellKind: null,
            sessionId: "abc",
            role: null,
            file: null,
            embeds: [],
          },
        ],
      },
    ],
    activeIndex: 0,
    restorePref: "restore",
  };
  const back = decodeTabs(encodeTabs(state));
  assert.deepEqual(back?.tabs[0].docked, state.tabs[0].docked, "docked pane survives the round-trip");
});

test("an empty docked list is omitted (old-file shape preserved)", () => {
  const encoded = encodeTabs({
    tabs: [{ name: "a", color: null, groupId: null, docked: [] }],
    activeIndex: 0,
  });
  assert.equal(encoded.includes("docked"), false, "no docked key written for an empty list");
  assert.equal(decodeTabs(encoded)?.tabs[0].docked, undefined);
});

test("a malformed docked entry is dropped, not fatal to the tab", () => {
  const raw = JSON.stringify({
    tabs: [{ name: "a", color: null, groupId: null, docked: [{ paneKind: "bogus" }, { nope: 1 }] }],
    activeIndex: 0,
  });
  const back = decodeTabs(raw);
  assert.equal(back?.tabs.length, 1, "the tab survives");
  assert.equal(back?.tabs[0].docked, undefined, "all-malformed docked entries drop to no dock");
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
        role: null,
        file: null,
        embeds: [],
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
            role: null,
            file: null,
            embeds: [],
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
            sessionId: "orch-sess-9",
            role: "orchestrator",
            file: null,
            embeds: [],
          },
        },
        {
          // A file-explorer pane (#214): its root rides in `cwd`, and every
          // spawn-shaped field is null — it has no process to describe.
          kind: "leaf",
          weight: 1,
          pane: {
            paneKind: "files",
            name: "loomux",
            cwd: "C:/Projects/loomux",
            command: null,
            argv: null,
            shellKind: null,
            sessionId: null,
            role: null,
            file: null,
            embeds: [],
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

// ---------- #214 file-explorer leaves ----------

test("a files leaf round-trips its root — and needed NO new field or schema bump", () => {
  const files: PersistedPane = {
    paneKind: "files",
    name: "loomux",
    cwd: "C:/Projects/loomux",
    command: null,
    argv: null,
    shellKind: null,
    sessionId: null,
    role: null,
    file: null,
    embeds: [],
  };
  const state: PersistedTabs = {
    tabs: [
      { name: "t", color: null, groupId: null, layout: { kind: "leaf", weight: 1, pane: files } },
    ],
    activeIndex: 0,
  };
  const back = decodeTabs(encodeTabs(state));
  const leaf = back?.tabs[0].layout;
  assert.ok(leaf?.kind === "leaf");
  assert.deepEqual(leaf.pane, files);
  // The root rides in the EXISTING `cwd`, exactly as `role` rode in for orch panes —
  // so the decoder stays shape-driven and v2 files (which simply never contain a
  // "files" leaf) still decode unchanged. A bump here would be a false signal.
  assert.equal(back?.schemaVersion, 2);
});

test("a files leaf with no root decodes (null) rather than dropping the whole tab layout", () => {
  // The strict whole-tree fail-safe is for MALFORMED data. A rootless files leaf is
  // well-formed but unrestorable, and killing the entire tab's layout over it would
  // punish every sibling pane. It decodes, and restore fails soft in that ONE slot
  // (planPaneRestore → open-files with root null → main.ts opens the welcome form).
  const raw = JSON.stringify({
    tabs: [
      {
        name: "t",
        color: null,
        groupId: null,
        layout: {
          kind: "split",
          dir: "row",
          weight: 1,
          children: [
            { kind: "leaf", weight: 1, pane: { paneKind: "terminal", name: "shell" } },
            { kind: "leaf", weight: 1, pane: { paneKind: "files", name: "files" } }, // no cwd
          ],
        },
      },
    ],
    activeIndex: 0,
  });
  const layout = decodeTabs(raw)?.tabs[0].layout;
  assert.ok(layout?.kind === "split");
  assert.equal(layout.children.length, 2, "the sibling terminal survives");
  const filesLeaf = layout.children[1];
  assert.ok(filesLeaf.kind === "leaf");
  assert.equal(filesLeaf.pane.paneKind, "files");
  assert.equal(filesLeaf.pane.cwd, null);
});

// ---------- #217 editor + git leaves ----------

test("editor and git leaves round-trip their root — and the editor's open FILE", () => {
  // The third and fourth members of the same family (#214's files was the first). The
  // editor's FOLDER and the git pane's REPO both ride in the existing `cwd`; the editor
  // also carries the file it was showing — a PATH, never a buffer (#217). Both are
  // additive in exactly the way `role` and the files root were: a decoder that has never
  // heard of them is not needed, because old snapshots simply never carry them.
  const mk = (paneKind: PersistedPane["paneKind"], file: string | null = null): PersistedPane => ({
    paneKind,
    name: "loomux",
    cwd: "C:/Projects/loomux",
    command: null,
    argv: null,
    shellKind: null,
    sessionId: null,
    role: null,
    file,
    embeds: [],
  });
  const state: PersistedTabs = {
    tabs: [
      {
        name: "t",
        color: null,
        groupId: null,
        layout: {
          kind: "split",
          dir: "row",
          weight: 1,
          children: [
            { kind: "leaf", weight: 1, pane: mk("editor", "src/pane.ts") },
            { kind: "leaf", weight: 1, pane: mk("git") },
          ],
        },
      },
    ],
    activeIndex: 0,
  };
  const back = decodeTabs(encodeTabs(state));
  const layout = back?.tabs[0].layout;
  assert.ok(layout?.kind === "split");
  assert.deepEqual(layout.children[0].kind === "leaf" && layout.children[0].pane, mk("editor", "src/pane.ts"));
  assert.deepEqual(layout.children[1].kind === "leaf" && layout.children[1].pane, mk("git"));
  assert.equal(back?.schemaVersion, 2, "additive — a bump here would be a false signal");
});

test("a rootless editor/git leaf decodes (null) rather than dropping the whole tab layout", () => {
  // Same fail-soft as the files leaf: well-formed but unrestorable is NOT malformed, and
  // killing the tab's whole layout over one such leaf would punish every sibling pane.
  const raw = JSON.stringify({
    tabs: [
      {
        name: "t",
        color: null,
        groupId: null,
        layout: {
          kind: "split",
          dir: "row",
          weight: 1,
          children: [
            { kind: "leaf", weight: 1, pane: { paneKind: "terminal", name: "shell" } },
            { kind: "leaf", weight: 1, pane: { paneKind: "editor", name: "editor" } }, // no cwd
            { kind: "leaf", weight: 1, pane: { paneKind: "git", name: "git" } }, // no cwd
          ],
        },
      },
    ],
    activeIndex: 0,
  });
  const layout = decodeTabs(raw)?.tabs[0].layout;
  assert.ok(layout?.kind === "split");
  assert.equal(layout.children.length, 3, "the sibling terminal survives");
  for (const [i, kind] of [
    [1, "editor"],
    [2, "git"],
  ] as const) {
    const leaf = layout.children[i];
    assert.ok(leaf.kind === "leaf");
    assert.equal(leaf.pane.paneKind, kind);
    assert.equal(leaf.pane.cwd, null);
  }
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
      role: 99, // non-string → null
      file: 7, // non-string → null (#217)
      embeds: "0.4", // not an array → [] (#361)
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
      role: null,
      file: null,
      embeds: [],
    },
  });
});

// ---------- #361 embedded views (multi-slot: left/right/bottom) ----------

test("embed preferences ({view, side, share}), one per docked edge, round-trip through encode/decode", () => {
  const orch: PersistedPane = {
    paneKind: "orch",
    name: "orchestrator",
    cwd: "/repo",
    command: null,
    argv: null,
    shellKind: null,
    sessionId: "orch-1",
    role: "orchestrator",
    file: null,
    embeds: [
      { view: "group", side: "bottom", share: 0.42 },
      { view: "tasks", side: "left", share: 0.3 },
    ],
  };
  const state: PersistedTabs = {
    tabs: [
      { name: "t", color: null, groupId: "g", layout: { kind: "leaf", weight: 1, pane: orch } },
    ],
    activeIndex: 0,
  };
  const back = decodeTabs(encodeTabs(state));
  const leaf = back?.tabs[0].layout;
  assert.ok(leaf?.kind === "leaf");
  assert.deepEqual(leaf.pane.embeds, [
    { view: "group", side: "bottom", share: 0.42 },
    { view: "tasks", side: "left", share: 0.3 },
  ]);
});

test("an old snapshot with no embeds key decodes it as [] (overlay mode, unchanged)", () => {
  // A pre-#361 file never wrote the key at all — additive, like `role` and the
  // files root before it: no schema bump, no decoder branch needed.
  const raw = JSON.stringify({
    tabs: [
      {
        name: "t",
        color: null,
        groupId: null,
        layout: {
          kind: "leaf",
          weight: 1,
          pane: { paneKind: "orch", name: "orchestrator", role: "orchestrator" },
        },
      },
    ],
    activeIndex: 0,
  });
  const leaf = decodeTabs(raw)?.tabs[0].layout;
  assert.ok(leaf?.kind === "leaf");
  assert.deepEqual(leaf.pane.embeds, []);
});

test("a malformed entry inside a valid embeds array is dropped, not the whole array", () => {
  const raw = JSON.stringify({
    tabs: [
      {
        name: "t",
        color: null,
        groupId: null,
        layout: {
          kind: "leaf",
          weight: 1,
          pane: {
            paneKind: "orch",
            name: "orchestrator",
            role: "orchestrator",
            embeds: [
              { view: "group", side: "bottom", share: 0.4 }, // valid
              { view: "git", side: "left", share: 0.3 }, // not a RESTORABLE kind
              { view: "tasks", side: "sideways", share: 0.3 }, // bad side
              { view: "audit", side: "right", share: "big" }, // bad share
              "not even an object",
            ],
          },
        },
      },
    ],
    activeIndex: 0,
  });
  const leaf = decodeTabs(raw)?.tabs[0].layout;
  assert.ok(leaf?.kind === "leaf");
  assert.deepEqual(leaf.pane.embeds, [{ view: "group", side: "bottom", share: 0.4 }]);
});

test("two entries claiming the SAME side: the first wins, the second is dropped", () => {
  const raw = JSON.stringify({
    tabs: [
      {
        name: "t",
        color: null,
        groupId: null,
        layout: {
          kind: "leaf",
          weight: 1,
          pane: {
            paneKind: "orch",
            name: "orchestrator",
            role: "orchestrator",
            embeds: [
              { view: "tasks", side: "left", share: 0.3 },
              { view: "audit", side: "left", share: 0.5 }, // same side, stale/malformed data
            ],
          },
        },
      },
    ],
    activeIndex: 0,
  });
  const leaf = decodeTabs(raw)?.tabs[0].layout;
  assert.ok(leaf?.kind === "leaf");
  assert.deepEqual(leaf.pane.embeds, [{ view: "tasks", side: "left", share: 0.3 }]);
});

test("a legacy single-slot embed:{view,share} (pre-multi-slot #361 shape) migrates to bottom", () => {
  // The single-embed-slot shape never shipped in a release (generalized to
  // multiple sides within the same PR) — but decode stays lenient
  // regardless: bottom was the only side that shape could ever mean.
  const raw = JSON.stringify({
    tabs: [
      {
        name: "t",
        color: null,
        groupId: null,
        layout: {
          kind: "leaf",
          weight: 1,
          pane: {
            paneKind: "orch",
            name: "orchestrator",
            role: "orchestrator",
            embed: { view: "audit", share: 0.5 },
          },
        },
      },
    ],
    activeIndex: 0,
  });
  const leaf = decodeTabs(raw)?.tabs[0].layout;
  assert.ok(leaf?.kind === "leaf");
  assert.deepEqual(leaf.pane.embeds, [{ view: "audit", side: "bottom", share: 0.5 }]);
});

test("a legacy taskEmbed:number (pre-generalization #361 shape, oldest of the three) migrates to [{tasks, bottom}]", () => {
  // taskEmbed never shipped in a release either (renamed, then generalized,
  // within the same PR, #404's review rounds) — decode stays lenient for
  // the same reason: the cost of tolerating an old shape is a few lines,
  // the cost of not is a silently dropped preference on the next boot after
  // a stray hand-edited or pre-rebase tabs.json.
  const raw = JSON.stringify({
    tabs: [
      {
        name: "t",
        color: null,
        groupId: null,
        layout: {
          kind: "leaf",
          weight: 1,
          pane: { paneKind: "orch", name: "orchestrator", role: "orchestrator", taskEmbed: 0.3 },
        },
      },
    ],
    activeIndex: 0,
  });
  const leaf = decodeTabs(raw)?.tabs[0].layout;
  assert.ok(leaf?.kind === "leaf");
  assert.deepEqual(leaf.pane.embeds, [{ view: "tasks", side: "bottom", share: 0.3 }]);
});

test("the newest present shape wins when a pane somehow carries more than one", () => {
  const raw = JSON.stringify({
    tabs: [
      {
        name: "t",
        color: null,
        groupId: null,
        layout: {
          kind: "leaf",
          weight: 1,
          pane: {
            paneKind: "orch",
            name: "orchestrator",
            role: "orchestrator",
            embeds: [{ view: "group", side: "right", share: 0.6 }],
            embed: { view: "audit", share: 0.5 }, // stale — ignored when `embeds` is present
            taskEmbed: 0.9, // stalest — ignored too
          },
        },
      },
    ],
    activeIndex: 0,
  });
  const leaf = decodeTabs(raw)?.tabs[0].layout;
  assert.ok(leaf?.kind === "leaf");
  assert.deepEqual(leaf.pane.embeds, [{ view: "group", side: "right", share: 0.6 }]);
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

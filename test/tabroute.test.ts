// Routing/preview decision tests for project tabs phases 3–4 (#63): which tab a
// cross-tab attention scan badges, focus-switches-tab, and the preview throttle.
// Pure (tabroute.ts) — no DOM/Tauri. Run `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  tabAttention,
  sameAttention,
  findPaneByPty,
  safeStyleDeclarations,
  SAFE_STYLE_PROPS,
  PreviewBudget,
  type TabAttn,
} from "../src/tabroute.ts";

const ptyMap = (pairs: [number, string][]) => new Map<number, string>(pairs);

test("tabAttention badges the tab owning a needs-attention pty", () => {
  // pty 7 lives in the hidden tab ws-b; its blocked agent must badge ws-b.
  const out = tabAttention(
    [{ pty_id: 7, reason: "waiting" }],
    ptyMap([
      [7, "ws-b"],
      [3, "ws-a"],
    ])
  );
  assert.equal(out.size, 1);
  assert.deepEqual(out.get("ws-b"), { urgent: false, reason: "waiting" });
  assert.equal(out.has("ws-a"), false, "a tab with no attention item is not badged");
});

test("tabAttention marks a tab urgent if ANY of its ptys is blocked", () => {
  const out = tabAttention(
    [
      { pty_id: 1, reason: "report" }, // not urgent
      { pty_id: 2, reason: "blocked" }, // urgent
    ],
    ptyMap([
      [1, "ws-a"],
      [2, "ws-a"],
    ])
  );
  assert.deepEqual(
    out.get("ws-a"),
    { urgent: true, reason: "blocked" },
    "urgency reuses attention.ts (blocked = urgent) and shows the most urgent reason"
  );
});

test("tabAttention keeps the highest-priority reason when a tab has several", () => {
  const out = tabAttention(
    [
      { pty_id: 1, reason: "report" },
      { pty_id: 2, reason: "waiting" },
    ],
    ptyMap([
      [1, "ws-a"],
      [2, "ws-a"],
    ])
  );
  assert.deepEqual(out.get("ws-a"), { urgent: false, reason: "waiting" }, "waiting outranks report");
});

test("every attention class badges the tab (blocked/waiting/report/gate), urgent only for blocked", () => {
  for (const reason of ["blocked", "waiting", "report", "gate"]) {
    const out = tabAttention([{ pty_id: 1, reason }], ptyMap([[1, "ws-a"]]));
    assert.deepEqual(
      out.get("ws-a"),
      { urgent: reason === "blocked", reason },
      `${reason} must badge the tab`
    );
  }
});

test("tabAttention ignores null-pty items and ptys not mapped to a tab", () => {
  const out = tabAttention(
    [
      { pty_id: null, reason: "gate" },
      { pty_id: 99, reason: "blocked" }, // no tab owns pty 99
    ],
    ptyMap([[1, "ws-a"]])
  );
  assert.equal(out.size, 0);
});

test("sameAttention detects equal and changed sets (skips needless re-renders)", () => {
  const a = new Map<string, TabAttn>([["ws-a", { urgent: false, reason: "waiting" }]]);
  const b = new Map<string, TabAttn>([["ws-a", { urgent: false, reason: "waiting" }]]);
  const c = new Map<string, TabAttn>([["ws-a", { urgent: true, reason: "blocked" }]]);
  const e = new Map<string, TabAttn>([["ws-a", { urgent: false, reason: "report" }]]);
  const d = new Map<string, TabAttn>();
  assert.equal(sameAttention(a, b), true);
  assert.equal(sameAttention(a, c), false, "urgency flip is a change");
  assert.equal(sameAttention(a, e), false, "reason change is a change");
  assert.equal(sameAttention(a, d), false, "size change is a change");
});

// findPaneByPty is the core of the LIVE cross-tab lookup main.ts uses for
// orch-focus / pty-exit / rename (findPaneAcrossTabs). Fakes stand in for the
// Grid (findByPtyId) and the Workspace, exactly as production wires them.
test("findPaneByPty locates the workspace + pane owning a pty, scanning in order", () => {
  const paneA1 = { pty: 5 };
  const paneB1 = { pty: 6 };
  const gridOf = (ws: { panes: { pty: number }[] }) => ({
    findByPtyId: (id: number) => ws.panes.find((p) => p.pty === id),
  });
  const wsA = { id: "ws-a", panes: [paneA1] };
  const wsB = { id: "ws-b", panes: [paneB1] };
  const tabs = [wsA, wsB];

  // pty 6 lives in the (possibly hidden) second tab → returns ws-b + its pane.
  assert.deepEqual(findPaneByPty(tabs, gridOf, 6), { ws: wsB, pane: paneB1 });
  // pty 5 in the first tab.
  assert.deepEqual(findPaneByPty(tabs, gridOf, 5), { ws: wsA, pane: paneA1 });
  // no open pane has pty 999 → null (caller no-ops). This is why the scan beats
  // a maintained map: a closed pane simply isn't found, never a stale hit.
  assert.equal(findPaneByPty(tabs, gridOf, 999), null);
});

test("findPaneByPty returns the FIRST match when two tabs report the same pty", () => {
  // Defensive: pty ids shouldn't collide across tabs, but the scan is
  // deterministic (display order) rather than surfacing an arbitrary one.
  const gridOf = (ws: { has: number[] }) => ({
    findByPtyId: (id: number) => (ws.has.includes(id) ? { pty: id, ws } : undefined),
  });
  const first = { id: "ws-a", has: [7] };
  const second = { id: "ws-b", has: [7] };
  assert.equal(findPaneByPty([first, second], gridOf, 7)?.ws, first);
});

// ---- preview HTML sanitizer (#63 finding 3): the security-critical rule ----

test("safeStyleDeclarations keeps whitelisted visual props, drops the rest", () => {
  assert.deepEqual(
    safeStyleDeclarations("color:#f00;background-color:#001;font-weight:bold"),
    [
      ["color", "#f00"],
      ["background-color", "#001"],
      ["font-weight", "bold"],
    ]
  );
  // Layout / positioning / sizing props a serialized span has no business
  // carrying are dropped, even with innocent values.
  assert.deepEqual(
    safeStyleDeclarations("color:#0f0;position:fixed;top:0;width:100vw;z-index:9999"),
    [["color", "#0f0"]]
  );
});

test("safeStyleDeclarations rejects values that could load a resource or run code", () => {
  // Even on a whitelisted property, a value reaching outside pure styling is
  // dropped: url() resource loads, IE expression(), javascript: schemes, and
  // any markup delimiters (which could matter if the value were ever reflected).
  const attacks = [
    "background-color:url(http://evil/x)",
    "background-color:URL('data:...')",
    "color:expression(alert(1))",
    "color:javascript:alert(1)",
    "color:</style><script>alert(1)</script>",
    "color:#fff{}",
  ];
  for (const a of attacks) {
    assert.deepEqual(safeStyleDeclarations(a), [], `must reject: ${a}`);
  }
});

test("safeStyleDeclarations tolerates malformed / empty declarations", () => {
  assert.deepEqual(safeStyleDeclarations(null), []);
  assert.deepEqual(safeStyleDeclarations(undefined), []);
  assert.deepEqual(safeStyleDeclarations(""), []);
  assert.deepEqual(safeStyleDeclarations("garbage-without-a-colon"), []);
  assert.deepEqual(safeStyleDeclarations("color:;font-style:"), [], "blank values dropped");
  // A good declaration survives alongside junk ones.
  assert.deepEqual(safeStyleDeclarations(";;color:red;;nonsense;;"), [["color", "red"]]);
});

test("safeStyleDeclarations lowercases and trims property names before matching", () => {
  assert.deepEqual(safeStyleDeclarations("  COLOR : #abc "), [["color", "#abc"]]);
  // Every advertised safe prop is actually accepted (guards against the set and
  // the parser drifting apart).
  for (const prop of SAFE_STYLE_PROPS) {
    assert.deepEqual(safeStyleDeclarations(`${prop}: inherit`), [[prop, "inherit"]]);
  }
});

// ---- preview pane cap edge (#63): exactly N serialized, the rest degraded ----

test("PreviewBudget serializes exactly `cap` panes then caps the rest", () => {
  const budget = new PreviewBudget(3);
  // First three panes render; every pane after the cap is degraded.
  assert.deepEqual(
    [budget.take(), budget.take(), budget.take(), budget.take(), budget.take()],
    [true, true, true, false, false],
    "cap=3 → 3 rendered, then capped (no off-by-one)"
  );
});

test("PreviewBudget with a zero/negative cap caps everything", () => {
  const zero = new PreviewBudget(0);
  assert.equal(zero.take(), false);
  const neg = new PreviewBudget(-1);
  assert.equal(neg.take(), false, "a nonsensical cap never renders rather than looping");
});

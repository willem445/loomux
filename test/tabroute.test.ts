// Routing/preview decision tests for project tabs phases 3–4 (#63): which tab a
// cross-tab attention scan badges, focus-switches-tab, and the preview throttle.
// Pure (tabroute.ts) — no DOM/Tauri. Run `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  tabAttention,
  sameAttention,
  revealPlan,
  shouldRefreshPreview,
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
  assert.deepEqual(out.get("ws-b"), { urgent: false });
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
  assert.deepEqual(out.get("ws-a"), { urgent: true }, "urgency reuses attention.ts (blocked = urgent)");
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
  const a = new Map<string, TabAttn>([["ws-a", { urgent: false }]]);
  const b = new Map<string, TabAttn>([["ws-a", { urgent: false }]]);
  const c = new Map<string, TabAttn>([["ws-a", { urgent: true }]]);
  const d = new Map<string, TabAttn>();
  assert.equal(sameAttention(a, b), true);
  assert.equal(sameAttention(a, c), false, "urgency flip is a change");
  assert.equal(sameAttention(a, d), false, "size change is a change");
});

test("revealPlan: a pty in a hidden tab switches tabs; in the active tab it doesn't", () => {
  const map = ptyMap([
    [5, "ws-a"],
    [6, "ws-b"],
  ]);
  // pty 6 is in ws-b while ws-a is active → switch to ws-b, then focus.
  assert.deepEqual(revealPlan(map, "ws-a", 6), { switchTo: "ws-b", known: true });
  // pty 5 is already in the active tab → no tab switch, just focus in place.
  assert.deepEqual(revealPlan(map, "ws-a", 5), { switchTo: null, known: true });
  // unknown pty → caller falls back to a cross-tab search.
  assert.deepEqual(revealPlan(map, "ws-a", 999), { switchTo: null, known: false });
});

test("shouldRefreshPreview gates on the throttle interval", () => {
  assert.equal(shouldRefreshPreview(1000, 5000, 4000), true, "interval elapsed → refresh");
  assert.equal(shouldRefreshPreview(1000, 4999, 4000), false, "just under → skip");
  assert.equal(shouldRefreshPreview(1000, 5001, 4000), true);
});

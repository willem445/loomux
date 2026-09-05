// Unit tests for the single-pane autopilot toggle's persisted default-ON
// semantics (#101), the standalone channel-tools toggle (#271 W3
// addendum / PR #289 review round 2, N1) which shares the identical
// default-ON/explicit-"0"-off shape, and the orrerix-subagents toggle (#2519
// C1) whose polarity is the inverse of both — default OFF. Run with `npm
// test`. The pure `*FromStored` functions are tested directly so the default
// rules need no localStorage shim; `getSubagents`/`setSubagents`' own
// try/catch contract is tested over a throwing shim, because a read/write
// that raises must degrade to OFF / a silent no-op, not crash the launcher.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  autopilotFromStored,
  channelToolsFromStored,
  subagentsFromStored,
  getSubagents,
  setSubagents,
} from "../src/agents.ts";

test("autopilot defaults ON when nothing is stored", () => {
  // A brand-new user (no key yet) launches with autopilot on.
  assert.equal(autopilotFromStored(null), true);
});

test('only an explicit "0" turns autopilot off', () => {
  assert.equal(autopilotFromStored("0"), false);
});

test('a stored "1" keeps autopilot on', () => {
  assert.equal(autopilotFromStored("1"), true);
});

test("an empty or unrecognized value stays ON (fail-safe to the default)", () => {
  // A corrupted value must not silently disable autopilot — default wins.
  assert.equal(autopilotFromStored(""), true);
  assert.equal(autopilotFromStored("yes"), true);
  assert.equal(autopilotFromStored("false"), true);
});

test("channel tools default ON when nothing is stored", () => {
  // A brand-new user (no key yet) launches claude/copilot agent panes with
  // eager solo-prepare on — the addendum's stated "full membership at spawn"
  // default (#271 W3, N1 fix).
  assert.equal(channelToolsFromStored(null), true);
});

test('only an explicit "0" turns channel tools off', () => {
  assert.equal(channelToolsFromStored("0"), false);
});

test('a stored "1" keeps channel tools on', () => {
  assert.equal(channelToolsFromStored("1"), true);
});

test("an empty or unrecognized channel-tools value stays ON (fail-safe to the default)", () => {
  assert.equal(channelToolsFromStored(""), true);
  assert.equal(channelToolsFromStored("yes"), true);
  assert.equal(channelToolsFromStored("false"), true);
});

// ---------- the orrerix-subagents toggle (#2519 C1) ----------
// The polarity is deliberately the INVERSE of the two toggles above. Autopilot
// and channel tools default ON because doing nothing should not silently
// downgrade a feature the user already has; spawning a fleet of worker panes
// is the opposite kind of gesture — it mints real groups and real processes —
// so an absent, stale, or corrupted value must read OFF, and only an explicit
// "1" (what `setSubagents(true)` writes) turns it on.

test("orrerix subagents default OFF when nothing is stored", () => {
  // A brand-new user (no key yet) must not find fleet-spawning enabled.
  assert.equal(subagentsFromStored(null), false);
});

test('a stored "1" turns orrerix subagents on', () => {
  assert.equal(subagentsFromStored("1"), true);
});

test('only the exact "1" reads on — "0" and every garbage value read off', () => {
  assert.equal(subagentsFromStored("0"), false);
  assert.equal(subagentsFromStored(""), false);
  assert.equal(subagentsFromStored("yes"), false);
  assert.equal(subagentsFromStored("false"), false);
  assert.equal(subagentsFromStored("on"), false);
  assert.equal(subagentsFromStored(" 1"), false);
});

/** Swap in a localStorage shim and restore the previous global afterwards, so
 *  a throwing test cannot poison the suite's own storage (the lock_safe rule:
 *  a global overridden by a harness is restored from a cleanup, not leaked). */
function withStorage(
  shim: { getItem(key: string): string | null; setItem(key: string, value: string): void },
  fn: () => void,
): void {
  const g = globalThis as { localStorage?: unknown };
  const prev = g.localStorage;
  g.localStorage = shim;
  try {
    fn();
  } finally {
    g.localStorage = prev;
  }
}

test("getSubagents/setSubagents persist through the one key and round-trip", () => {
  const store = new Map<string, string>();
  const seenKeys: string[] = [];
  withStorage(
    {
      getItem: (k) => {
        seenKeys.push(k);
        return store.get(k) ?? null;
      },
      setItem: (k, v) => {
        store.set(k, v);
        seenKeys.push(k);
      },
    },
    () => {
      assert.equal(getSubagents(), false, "an empty store reads off");
      setSubagents(true);
      assert.equal(getSubagents(), true);
      setSubagents(false);
      assert.equal(getSubagents(), false);
      // ONE key: the toggle must not scatter state across the profile.
      assert.deepEqual([...new Set(seenKeys)], ["loomux.orrerixSubagents"]);
      assert.equal(store.get("loomux.orrerixSubagents"), "0", "OFF is written as an explicit 0");
    },
  );
});

test("a throwing read degrades to OFF; a throwing write is swallowed", () => {
  withStorage(
    {
      getItem: () => {
        throw new Error("quota / security / no storage");
      },
      setItem: () => {
        throw new Error("quota / security / no storage");
      },
    },
    () => {
      assert.equal(getSubagents(), false, "a refused read reads off, never throws");
      assert.doesNotThrow(() => setSubagents(true), "a refused write must not crash the caller");
    },
  );
});

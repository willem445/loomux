// The Agents tab's remembered group order (#2371). The decision is pure and is
// tested here; the two `localStorage` accessors around it are exercised against
// a stub for the three things that can go wrong — no storage at all, a storage
// that throws, and a storage holding something this build does not know.
//
// Red arm (mechanically): make `agentOrderFromStored` return its input
// unchecked and `an unknown stored value reads as the default` reddens alone;
// drop the try/catch from `getAgentOrder` and `a storage that throws is not a
// crash` reddens alone.
import { test } from "node:test";
import assert from "node:assert/strict";

import { DEFAULT_AGENT_ORDER, type AgentOrder } from "../src/agentrows.ts";
import {
  AGENT_ORDER_KEY,
  agentOrderFromStored,
  getAgentOrder,
  setAgentOrder,
} from "../src/agentorder.ts";

/** Install a `localStorage` stand-in for the duration of one test and take it
 *  away again — restored from a `try/finally` rather than left behind, so one
 *  test's stub cannot decide another's result (the `SERIAL`/`Drop` discipline
 *  `CLAUDE.md` states for the Rust side, in the shape this runtime allows). */
function withStorage<T>(impl: Partial<Storage>, body: () => T): T {
  const g = globalThis as { localStorage?: Storage };
  const had = Object.prototype.hasOwnProperty.call(g, "localStorage");
  const before = g.localStorage;
  g.localStorage = impl as Storage;
  try {
    return body();
  } finally {
    if (had) g.localStorage = before;
    else delete g.localStorage;
  }
}

/** A working in-memory storage, so a test can assert what was WRITTEN. */
function memoryStorage(seed: Record<string, string> = {}) {
  const map = new Map(Object.entries(seed));
  return {
    store: map,
    impl: {
      getItem: (k: string) => map.get(k) ?? null,
      setItem: (k: string, v: string) => void map.set(k, v),
    } as Partial<Storage>,
  };
}

test("an unknown stored value reads as the default, not as a third order", () => {
  // An absent key (first run), a key a NEWER build wrote a fourth order into,
  // and a corrupted one all mean the same thing to this build.
  assert.equal(agentOrderFromStored(null), DEFAULT_AGENT_ORDER);
  assert.equal(agentOrderFromStored(""), DEFAULT_AGENT_ORDER);
  assert.equal(agentOrderFromStored("by-project"), DEFAULT_AGENT_ORDER);
  assert.equal(agentOrderFromStored("TAB"), DEFAULT_AGENT_ORDER, "the match is exact, not case-folded");
  // The positive control: the two words this build DOES know come back as
  // themselves, so the assertions above are about the values and not about a
  // function that returns the default for everything.
  assert.equal(agentOrderFromStored("tab"), "tab");
  assert.equal(agentOrderFromStored("state"), "state");
});

test("the default is the pre-#2371 reading", () => {
  // A viewer who has never touched the control gets most-wants-you, which is
  // the order the tab already had. Pinned because changing it silently would
  // move every existing viewer's list.
  assert.equal(DEFAULT_AGENT_ORDER, "state");
});

test("a stored choice round-trips through the one key", () => {
  const { store, impl } = memoryStorage();
  withStorage(impl, () => {
    setAgentOrder("tab");
    assert.equal(store.get(AGENT_ORDER_KEY), "tab");
    assert.equal(getAgentOrder(), "tab");
    setAgentOrder("state");
    assert.equal(getAgentOrder(), "state");
  });
  // ONE key, and nothing else in it. This is the whole reason a
  // `BoardPrefsStore` is not needed here: the write cannot reach another
  // tenant's record because the key holds no other tenant.
  assert.deepEqual([...store.keys()], [AGENT_ORDER_KEY]);
});

test("a storage that throws is not a crash, in either direction", () => {
  const boom: Partial<Storage> = {
    getItem: () => {
      throw new Error("site data blocked");
    },
    setItem: () => {
      throw new Error("site data blocked");
    },
  };
  withStorage(boom, () => {
    assert.equal(getAgentOrder(), DEFAULT_AGENT_ORDER, "a read that cannot happen answers the default");
    assert.doesNotThrow(() => setAgentOrder("tab"), "a write that cannot happen is dropped, not raised");
  });
});

test("no localStorage at all answers the default", () => {
  // The unit-test context, and a host that has none. `getAgentOrder` is a bare
  // `localStorage.getItem`, which is a ReferenceError rather than a rejected
  // call — a different throw from the one above, and the same answer.
  const g = globalThis as { localStorage?: Storage };
  const had = Object.prototype.hasOwnProperty.call(g, "localStorage");
  const before = g.localStorage;
  if (had) delete g.localStorage;
  try {
    assert.equal(getAgentOrder(), DEFAULT_AGENT_ORDER);
    assert.doesNotThrow(() => setAgentOrder("tab"));
  } finally {
    if (had) g.localStorage = before;
  }
});

test("a value a newer build stored is left alone rather than scrubbed", () => {
  // Reading an unknown value as the default must not REWRITE it: a downgrade
  // that erased a newer build's choice would lose the human's decision the
  // moment they opened the old build, without them touching the control.
  const { store, impl } = memoryStorage({ [AGENT_ORDER_KEY]: "by-project" });
  withStorage(impl, () => {
    const read: AgentOrder = getAgentOrder();
    assert.equal(read, DEFAULT_AGENT_ORDER);
  });
  assert.equal(store.get(AGENT_ORDER_KEY), "by-project", "the read scrubbed a value it did not understand");
});

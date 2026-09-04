// The Agents tab's group order, remembered per viewer (#2371).
//
// One `localStorage` key holding one word, and the module is this small on
// purpose — it exists so the ambient dependency has exactly one home and
// `agentsviewmodel.ts` stays purely computational.
//
// ---------------------------------------------------------------------------
// Why not a `BoardPrefsStore`
// ---------------------------------------------------------------------------
//
// #1299's rule is that a multi-tenant whole-file store must never publish from
// a handle it has not read: an in-memory map seeded empty and serialised whole
// erases every OTHER tenant's record the first time a gesture beats the load.
// `BoardPrefsStore` (`src/boardprefs.ts`) is the class that holds that
// invariant, because `boardprefs.json` is ONE file shared by every group and a
// save publishes all of it.
//
// This key has no other tenants and no handle. It holds one scalar, this
// viewer's choice, and `setAgentOrder` writes it straight through — there is no
// in-memory copy that could be stale, and nothing else lives in the key to
// lose. The rule is satisfied structurally rather than by ceremony: wrapping a
// synchronous single-value API in an async read-before-write store would add
// the shape of the protection without any of the hazard it protects against,
// which is the kind of claim `CLAUDE.md` calls a defect.
//
// What DOES apply, and is handled below: every access is guarded. A private
// window, cleared site data, or a browser set to refuse storage makes the
// accessor itself throw, and the unit tests run under `node --test` where
// `localStorage` does not exist at all. So a read that cannot happen answers
// the default and a write that cannot happen is dropped — the tab renders
// correctly with nothing stored, which is also the first-run case.

import { DEFAULT_AGENT_ORDER, type AgentOrder } from "./agentrows.ts";

/** The stored key. `loomux.`-prefixed like every other preference this app
 *  keeps in `localStorage` (`agents.ts`'s bundle). */
export const AGENT_ORDER_KEY = "loomux.agentsOrder";

/** Interpret a stored value. Total and pure — the whole decision, testable
 *  without a storage shim, the same split `autopilotFromStored` uses.
 *
 *  Anything that is not a word this build knows reads as the default rather
 *  than as a third state: an absent key (first run), a key a NEWER build wrote
 *  a fourth order into, and a corrupted one all mean "show them the default
 *  order", and there is no reading of any of them that should show something
 *  else. */
export function agentOrderFromStored(raw: string | null): AgentOrder {
  return raw === "tab" || raw === "state" ? raw : DEFAULT_AGENT_ORDER;
}

/** This viewer's remembered group order, or the default. Never throws. */
export function getAgentOrder(): AgentOrder {
  try {
    return agentOrderFromStored(localStorage.getItem(AGENT_ORDER_KEY));
  } catch {
    // No localStorage (unit test), or a browser refusing site data. The default
    // is a complete answer, not a degraded one.
    return DEFAULT_AGENT_ORDER;
  }
}

/** Remember this viewer's group order. Best-effort: a storage that refuses the
 *  write leaves the view working and the choice un-remembered, which is the
 *  same outcome as never having chosen. */
export function setAgentOrder(order: AgentOrder): void {
  try {
    localStorage.setItem(AGENT_ORDER_KEY, order);
  } catch {
    /* storage unavailable — the choice applies to this session and is not kept */
  }
}

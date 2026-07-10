// Pure encode/decode + validation for the persisted tab set (#63), split out so
// the round-trip and the corrupt-input fail-safe are unit-testable under
// `node --test` (CLAUDE.md: pure logic here, DOM/IPC wiring validated by hand).
//
// tabstore.ts is the SINGLE SOURCE of the tab schema. The bytes live in durable
// backend storage — an atomic, corrupt-quarantining tabs.json in AppData
// (src-tauri/src/uistate.rs), reached through the typed loadUiTabs/saveUiTabs
// wrappers in pty.ts (main.ts does the load→decode / snapshot→encode→save). The
// backend guarantees "valid JSON text or nothing"; decodeTabs below adds the
// SCHEMA-level guard, returning null for anything malformed so a hand-edited or
// partially-written blob degrades to a fresh tab instead of crashing boot.
//
// What persists: each tab's name, color, order, active index, and bound
// orchestration group id (so a restored group's session rehydrates into the
// right tab — see restoreSession). What does NOT: the live panes/PTYs — agents
// revive only as far as the existing per-session restore allows, a deliberate
// cost-safety decision documented in doc/design/project-tabs.md.

export interface PersistedTab {
  name: string;
  color: string | null;
  /** The orchestration group this tab owns, or null for a plain tab. */
  groupId: string | null;
}

export interface PersistedTabs {
  tabs: PersistedTab[];
  /** Index of the tab that was active, clamped into range on decode. */
  activeIndex: number;
}

export function encodeTabs(state: PersistedTabs): string {
  return JSON.stringify(state);
}

/** Parse persisted tab state, tolerating anything malformed by returning null
 *  (the caller then boots with a single fresh tab). Every field is validated
 *  and coerced so a hand-edited or partially-written blob can't crash boot. */
export function decodeTabs(raw: string | null): PersistedTabs | null {
  if (!raw) return null;
  let v: unknown;
  try {
    v = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!v || typeof v !== "object") return null;
  const obj = v as { tabs?: unknown; activeIndex?: unknown };
  if (!Array.isArray(obj.tabs)) return null;

  const tabs: PersistedTab[] = [];
  for (const t of obj.tabs) {
    if (!t || typeof t !== "object") continue;
    const rec = t as { name?: unknown; color?: unknown; groupId?: unknown };
    if (typeof rec.name !== "string" || !rec.name.trim()) continue;
    tabs.push({
      name: rec.name,
      color: typeof rec.color === "string" ? rec.color : null,
      groupId: typeof rec.groupId === "string" ? rec.groupId : null,
    });
  }
  if (tabs.length === 0) return null;

  const idx = obj.activeIndex;
  const activeIndex =
    typeof idx === "number" && Number.isInteger(idx) && idx >= 0 && idx < tabs.length ? idx : 0;
  return { tabs, activeIndex };
}

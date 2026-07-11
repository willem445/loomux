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
// right tab — see restoreSession). From #194 the schema ALSO carries a per-tab
// pane LAYOUT tree, a top-level restore PREFERENCE, and a schemaVersion — the
// data layer for full session restore (doc/design/session-restore.md). The
// live panes/PTYs are still never captured; a persisted leaf records only what
// is needed to re-spawn or resume a pane (kind, cwd, command/argv, shell kind,
// agent session id). Group panes are revived by the group-resume path, not from
// these leaves — see panerestore.ts for the per-pane restore policy.
//
// MIGRATION CONTRACT: old files (schemaVersion absent, no per-tab layout) decode
// exactly as before — shells-only. Every #194 field is optional and additive, and
// a malformed layout node degrades that tab's whole layout to null (the tab then
// restores as a single fresh shell) rather than throwing. `encodeTabs` accepts a
// pre-#194 snapshot object (no restorePref/schemaVersion/layout) unchanged, so
// main.ts's `tabs.snapshot()` needs no change to keep writing a valid blob.

import type { ShellKind } from "./panesetup";

/** Bump when the persisted shape changes in a way decode must branch on. v1 was
 *  the pre-#194 {tabs,activeIndex} blob; v2 adds layout + restorePref. */
export const SCHEMA_VERSION = 2;

/** Restore preference: first-run "ask" (show the splash, then remember the
 *  choice), or a remembered "restore" / "fresh". Consumed by restoredecision.ts. */
export type RestorePref = "ask" | "restore" | "fresh";

/** The kind of a persisted pane leaf. Distinct from panesetup's setup-time
 *  `PaneKind` ("orchestrator" spawns a whole tab): here "orch" tags any
 *  orchestration pane (orchestrator / worker / reviewer) so restore keeps the
 *  whole group DORMANT and lets the group-resume path revive it.
 *
 *  "files" (#214) is the PTY-less file-explorer pane. It needs NO new persisted
 *  field: its root rides in the existing `cwd`, exactly as `role` rode into the
 *  schema for orch panes — decode is shape-driven, so old files (which simply
 *  never carry a "files" leaf) are unaffected and SCHEMA_VERSION stays at 2. */
export type PersistedPaneKind = "terminal" | "agent" | "orch" | "files";

/** One pane at a layout leaf, reduced to what restore needs. Never the live
 *  PTY/buffer — those are deliberately not captured (cost/#78 process-storm and
 *  the no-resize invariant; see the design note). */
export interface PersistedPane {
  paneKind: PersistedPaneKind;
  name: string;
  /** Directory to restore into — the pane's live cwd when captured; null = home.
   *  For kind "files" this is the tree's ROOT (#214), and it is the one thing
   *  that pane needs; a null root there is unrestorable (the slot fails soft to
   *  the welcome form) rather than a decode failure. */
  cwd: string | null;
  /** Agent/command spawn line (kind "agent"); null for terminals. */
  command: string | null;
  /** Structured agent argv, if the pane was spawned with one (kind "agent"). */
  argv: string[] | null;
  /** Terminal shell kind (kind "terminal"); null when unknown / not a terminal. */
  shellKind: ShellKind | null;
  /** Recorded resumable session id — enables --resume into the prior context.
   *  Captured for kind "agent" AND kind "orch" (an orchestration pane's own
   *  session, so a group resume restores exactly the captured members). Absent
   *  for terminals and best-effort CLIs. */
  sessionId: string | null;
  /** Orchestration role for kind "orch" ("orchestrator" | "worker" | "reviewer"
   *  | "planner"), so a whole-group resume can tell the orchestrator (resume
   *  first, relaunches the group) from its delegates. Null for agent/terminal
   *  panes and for pre-#194.5 files. */
  role: string | null;
}

/** A tab's pane layout: the split tree with PersistedPane leaves. Mirrors grid's
 *  `GridLayoutNode` but serializable (live `Pane` objects replaced by records).
 *  `weight` is the flex-grow the node held in its parent split. */
export type PersistedLayoutNode =
  | { kind: "leaf"; weight: number; pane: PersistedPane }
  | { kind: "split"; dir: "row" | "column"; weight: number; children: PersistedLayoutNode[] };

export interface PersistedTab {
  name: string;
  color: string | null;
  /** The orchestration group this tab owns, or null for a plain tab. */
  groupId: string | null;
  /** Pane layout tree (#194). Absent/null = old file or a group-only tab →
   *  restore falls back to a single fresh shell. */
  layout?: PersistedLayoutNode | null;
  /** Minimized (docked) panes (#194 P4). These live OUTSIDE the layout tree, so
   *  they're captured separately and restored back into the dock — otherwise a
   *  docked agent session would be silently dropped on restore. Absent/empty when
   *  the tab has no docked panes. */
  docked?: PersistedPane[];
}

export interface PersistedTabs {
  tabs: PersistedTab[];
  /** Index of the tab that was active, clamped into range on decode. */
  activeIndex: number;
  /** #194 restore preference; defaults to "ask" (first run, then remembered).
   *  Optional on input so a pre-#194 snapshot object still encodes. */
  restorePref?: RestorePref;
  /** Persisted schema version; encode always stamps SCHEMA_VERSION. Optional on
   *  input; absent on read means a pre-#194 (v1) file. */
  schemaVersion?: number;
}

const SHELL_KINDS: readonly ShellKind[] = ["powershell", "gitbash", "cmd"];
const RESTORE_PREFS: readonly RestorePref[] = ["ask", "restore", "fresh"];

function isShellKind(v: unknown): v is ShellKind {
  return typeof v === "string" && (SHELL_KINDS as readonly string[]).includes(v);
}
function isRestorePref(v: unknown): v is RestorePref {
  return typeof v === "string" && (RESTORE_PREFS as readonly string[]).includes(v);
}

export function encodeTabs(state: PersistedTabs): string {
  // Stamp the current version and default the preference so a pre-#194 snapshot
  // object (no restorePref/schemaVersion) still writes a valid v2 blob — this is
  // what lets main.ts keep calling encodeTabs(tabs.snapshot()) unchanged.
  return JSON.stringify({
    schemaVersion: SCHEMA_VERSION,
    restorePref: state.restorePref ?? "ask",
    activeIndex: state.activeIndex,
    tabs: state.tabs.map((t) => ({
      name: t.name,
      color: t.color,
      groupId: t.groupId,
      // Only serialize a layout when present; an absent one keeps old-file shape.
      ...(t.layout ? { layout: t.layout } : {}),
      // Same for docked panes: omit the key entirely when there are none.
      ...(t.docked && t.docked.length ? { docked: t.docked } : {}),
    })),
  });
}

/** Validate one persisted pane leaf, returning null on any malformation so its
 *  whole layout tree degrades (see decodeLayout). */
function decodePane(v: unknown): PersistedPane | null {
  if (!v || typeof v !== "object") return null;
  const r = v as Record<string, unknown>;
  const kind = r.paneKind;
  if (kind !== "terminal" && kind !== "agent" && kind !== "orch" && kind !== "files") return null;
  if (typeof r.name !== "string" || !r.name.trim()) return null;
  const argvOk = Array.isArray(r.argv) && r.argv.every((a) => typeof a === "string");
  return {
    paneKind: kind,
    name: r.name,
    cwd: typeof r.cwd === "string" ? r.cwd : null,
    command: typeof r.command === "string" ? r.command : null,
    argv: argvOk ? (r.argv as string[]) : null,
    shellKind: isShellKind(r.shellKind) ? r.shellKind : null,
    sessionId: typeof r.sessionId === "string" ? r.sessionId : null,
    role: typeof r.role === "string" ? r.role : null,
  };
}

/** Validate a layout tree. STRICT whole-tree fail-safe: any malformed node
 *  (bad pane, unknown kind, empty/invalid split) collapses the ENTIRE tab layout
 *  to null, so the tab restores as one fresh shell rather than a half-built,
 *  possibly-misleading tree. Never throws. */
function decodeLayout(v: unknown): PersistedLayoutNode | null {
  if (!v || typeof v !== "object") return null;
  const r = v as Record<string, unknown>;
  const weight = typeof r.weight === "number" && Number.isFinite(r.weight) && r.weight > 0 ? r.weight : 1;
  if (r.kind === "leaf") {
    const pane = decodePane(r.pane);
    return pane ? { kind: "leaf", weight, pane } : null;
  }
  if (r.kind === "split") {
    if (r.dir !== "row" && r.dir !== "column") return null;
    if (!Array.isArray(r.children) || r.children.length === 0) return null;
    const children: PersistedLayoutNode[] = [];
    for (const c of r.children) {
      const node = decodeLayout(c);
      if (!node) return null; // one bad descendant drops the whole tab layout
      children.push(node);
    }
    return { kind: "split", dir: r.dir, weight, children };
  }
  return null;
}

/** Parse persisted tab state, tolerating anything malformed by returning null
 *  (the caller then boots with a single fresh tab). Every field is validated
 *  and coerced so a hand-edited or partially-written blob can't crash boot.
 *  Old (pre-#194) files decode exactly as before — shells-only — with
 *  restorePref defaulted to "ask" and schemaVersion to 1. */
export function decodeTabs(raw: string | null): PersistedTabs | null {
  if (!raw) return null;
  let v: unknown;
  try {
    v = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!v || typeof v !== "object") return null;
  const obj = v as {
    tabs?: unknown;
    activeIndex?: unknown;
    restorePref?: unknown;
    schemaVersion?: unknown;
  };
  if (!Array.isArray(obj.tabs)) return null;

  const tabs: PersistedTab[] = [];
  for (const t of obj.tabs) {
    if (!t || typeof t !== "object") continue;
    const rec = t as {
      name?: unknown;
      color?: unknown;
      groupId?: unknown;
      layout?: unknown;
      docked?: unknown;
    };
    if (typeof rec.name !== "string" || !rec.name.trim()) continue;
    const tab: PersistedTab = {
      name: rec.name,
      color: typeof rec.color === "string" ? rec.color : null,
      groupId: typeof rec.groupId === "string" ? rec.groupId : null,
    };
    // Only attach `layout` when the source had one: an absent layout stays absent
    // (old-file shape, so the round-trip is exact), while a present-but-malformed
    // layout degrades to null → the tab restores as a single fresh shell.
    if (rec.layout !== undefined) tab.layout = decodeLayout(rec.layout);
    // Docked panes: drop any malformed entry rather than failing the whole tab
    // (a lost dock chip is a smaller degradation than a lost tab).
    if (Array.isArray(rec.docked)) {
      const docked = rec.docked.map(decodePane).filter((p): p is PersistedPane => p !== null);
      if (docked.length) tab.docked = docked;
    }
    tabs.push(tab);
  }
  if (tabs.length === 0) return null;

  const idx = obj.activeIndex;
  const activeIndex =
    typeof idx === "number" && Number.isInteger(idx) && idx >= 0 && idx < tabs.length ? idx : 0;
  const restorePref = isRestorePref(obj.restorePref) ? obj.restorePref : "ask";
  const schemaVersion =
    typeof obj.schemaVersion === "number" && Number.isInteger(obj.schemaVersion)
      ? obj.schemaVersion
      : 1; // no version → the pre-#194 v1 blob
  return { tabs, activeIndex, restorePref, schemaVersion };
}

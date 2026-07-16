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
 *  "files" (#214), "editor" and "git" (#217) and "workflow" (#222) are the PTY-less
 *  CONTENT panes. None needs a new persisted field: the kind's root — the folder a
 *  tree/listing is rooted at, the repo a git view or a workflow pane is pointed at —
 *  rides in the existing `cwd`, exactly as `role` rode into the schema for orch panes,
 *  and the workflow pane's file rides in the `file` field the editor already added.
 *  Decode is shape-driven, so old files (which simply never carry these leaves) are
 *  unaffected and SCHEMA_VERSION stays at 2.
 *
 *  "plugin" (#360 Slice D) is a fifth CONTENT pane, and the first whose identity
 *  ISN'T a path — it's WHICH plugin. `cwd` has no meaning for it (a plugin pane
 *  has no folder/repo root of its own; the plugin's own install root, if any, is
 *  re-derived live from its CURRENT manifest on restore, never persisted — a
 *  plugin can be reinstalled at a different path between sessions and the pane
 *  must follow it, not a stale snapshot). So this is the one content kind that
 *  DOES need a new field: `pluginId` below, additive like every other #194 field
 *  (an old snapshot simply never contains a "plugin" leaf). */
export type PersistedPaneKind =
  | "terminal"
  | "agent"
  | "orch"
  | "files"
  | "editor"
  | "git"
  | "workflow"
  | "plugin";

/** The PTY-less content kinds, in one place — what `cwd` means for them is a ROOT,
 *  not a shell's directory (except "plugin", which uses neither — see `pluginId`). */
const CONTENT_KINDS: readonly PersistedPaneKind[] = ["files", "editor", "git", "workflow", "plugin"];

/** One pane at a layout leaf, reduced to what restore needs. Never the live
 *  PTY/buffer — those are deliberately not captured (cost/#78 process-storm and
 *  the no-resize invariant; see the design note). */
export interface PersistedPane {
  paneKind: PersistedPaneKind;
  name: string;
  /** Directory to restore into — the pane's live cwd when captured; null = home.
   *  For a CONTENT kind ("files" #214, "editor"/"git" #217) this is instead the
   *  pane's ROOT (the folder browsed/edited, the repo viewed), and it is the one
   *  thing that pane needs; a null root there is unrestorable (the slot fails soft
   *  to the welcome form) rather than a decode failure. */
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
  /** The file an EDITOR pane (#217) had open, root-relative — a PATH, never a buffer.
   *  A pane opened on a file is titled after it, so without this a restore would show a
   *  bare tree under a title naming a file it isn't showing. The content is re-read from
   *  disk; unsaved edits are deliberately NOT persisted (see doc/design/content-panes.md
   *  — the close guard's whole point is that the human was asked).
   *
   *  A WORKFLOW pane (#222) rides the same field for the same reason: the workflow file
   *  it is editing (`.loomux/workflow.yml` by default, or whichever YAML it was opened
   *  on from the file browser). Null for every other kind, and absent from any snapshot
   *  written before #217. */
  file: string | null;
  /** A PLUGIN pane's (#360 Slice D) identity: the manifest `id` of the installed
   *  plugin it hosts. This is the one content kind whose root doesn't fit `cwd` —
   *  see the PersistedPaneKind doc comment — so it gets its own field, additive
   *  like `file` was for #217: absent from any snapshot written before Slice D,
   *  and null for every other pane kind. A `pluginId` naming a plugin that has
   *  since been uninstalled is not a decode failure — decode doesn't (and can't)
   *  check installation state, that's I/O — it fails soft on RESTORE instead
   *  (panerestore.ts's open-plugin action / main.ts's probe). */
  pluginId: string | null;
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

const PANE_KINDS: readonly PersistedPaneKind[] = [
  "terminal",
  "agent",
  "orch",
  ...CONTENT_KINDS,
];
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
  if (!PANE_KINDS.includes(kind as PersistedPaneKind)) return null;
  if (typeof r.name !== "string" || !r.name.trim()) return null;
  const argvOk = Array.isArray(r.argv) && r.argv.every((a) => typeof a === "string");
  return {
    paneKind: kind as PersistedPaneKind,
    name: r.name,
    cwd: typeof r.cwd === "string" ? r.cwd : null,
    command: typeof r.command === "string" ? r.command : null,
    argv: argvOk ? (r.argv as string[]) : null,
    shellKind: isShellKind(r.shellKind) ? r.shellKind : null,
    sessionId: typeof r.sessionId === "string" ? r.sessionId : null,
    role: typeof r.role === "string" ? r.role : null,
    file: typeof r.file === "string" ? r.file : null,
    pluginId: typeof r.pluginId === "string" ? r.pluginId : null,
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

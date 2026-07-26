// Project tabs (#63, Option A) — the tab model, deliberately DOM- and Grid-free.
//
// Each tab is a `Workspace`: one `Grid` + its own minimize dock (the DOM/Grid
// half lives in workspace.ts). `TabManager` owns the ordered tab list, the
// active tab, the "never zero tabs" invariant, and the routing maps that send an
// orchestration group / pty to the right tab.
//
// This file has NO Tauri/xterm/Grid imports so it runs under `node --test`
// (CLAUDE.md: pure logic here, DOM wiring validated by hand). TabManager is
// generic over the minimal `ManagedWorkspace` surface, so tests drive it with a
// lightweight fake and production plugs in the real Grid-backed Workspace.

import type { TabAttn, PreviewNode } from "./tabroute";
import type { PersistedTabs, PersistedLayoutNode, PersistedPane, RestorePref } from "./tabstore";
import type { TabPaneInfo } from "./tabcounts";

/** The minimal surface TabManager needs from a workspace. `Workspace`
 *  (workspace.ts) implements this and adds the concrete Grid/DOM. */
export interface ManagedWorkspace {
  readonly id: string;
  /** Human-facing tab name. Persisted across launches via TabManager.snapshot →
   *  the durable backend store (tabstore.ts, uistate.rs). */
  name: string;
  /** Tab accent color, or null for the default palette slot / no custom color. */
  color: string | null;
  /** Show or hide the whole workspace. Hiding is `display:none`, which drops
   *  every pane in the tab to zero width so none of them resizes its PTY — the
   *  maximize no-resize precedent (Grid.toggleMaximize / styles.css
   *  `.has-maximized`, panefit.ts `shouldResizePty`). THIS is the load-bearing
   *  invariant for tab switching (#63, CLAUDE.md constraint 1). */
  setVisible(visible: boolean): void;
  /** Focus the workspace's active pane — called when a tab becomes active. */
  focus(): void;
  /** Snapshot the tab's whole split layout for a live hover thumbnail (#63
   *  the split tree with EVERY pane's serialized-HTML viewport at
   *  the leaves. Reads the in-memory xterm buffers (which keep updating while
   *  hidden), so it works with zero layout and no PTY resize — the tab bar
   *  re-calls it on a short interval while hovered for a live view, and renders
   *  it SAFELY (spans → textContent). Null when the tab has no panes. */
  previewLayout(): PreviewNode | null;
  /** Capture the tab's whole pane layout for session restore (#194): the split
   *  tree with each pane reduced to a serializable record, or null when there's
   *  nothing worth restoring (an empty tab / only a welcome pane). */
  captureLayout(): PersistedLayoutNode | null;
  /** Capture the tab's minimized (docked) panes — outside the layout tree, so
   *  restored separately rather than silently dropped (#194 P4). */
  captureDocked(): PersistedPane[];
  /** Classify every pane in the tab for the live per-tab agent counter and the
   *  orchestration markers (#194 P4). */
  paneInfos(): TabPaneInfo[];
  /** Does any pane in this tab hold unsaved editor edits (#217)? Closing the tab
   *  disposes every pane in it, which would destroy them — so the tab bar treats such
   *  a close as DESTRUCTIVE and makes the human confirm, the same arm-and-confirm an
   *  orchestration tab already gets. Reports; never prompts (the teardown is
   *  synchronous, so a question that pops its own modal is no use here). */
  hasUnsavedWork(): boolean;
  /** Tear the workspace down and kill its panes' PTYs (tab closed). */
  dispose(): void;
}

/** Called on any change to the tab set (add/remove/switch/rename/color) so the
 *  tab bar can re-render. */
export type TabsListener = () => void;

/** Compute the `moveTab` target index for a drag-drop (#379): dropping a
 *  dragged tab on `overId`'s leading edge (`before=true`) or trailing edge
 *  (`before=false`). `ids` is the CURRENT tab order (as shown, before the
 *  drop). `moveTab(id, toIndex)` places `id` at index `toIndex` of the
 *  RESULTING list — so when the dragged tab starts BEFORE the target, the
 *  target (and everything between them) shifts left by one as the drag
 *  vacates its old slot, and the target index must shift with it; dragging
 *  from AFTER the target needs no such adjustment. Isolated from the tab
 *  strip's pointer/rect math so the off-by-one is unit-tested on its own. */
export function dropTargetIndex(
  ids: readonly string[],
  draggedId: string,
  overId: string,
  before: boolean
): number {
  const from = ids.indexOf(draggedId);
  const overIdx = ids.indexOf(overId);
  if (from < 0 || overIdx < 0) return from;
  let to = before ? overIdx : overIdx + 1;
  if (from < to) to -= 1;
  return to;
}

export class TabManager<T extends ManagedWorkspace> {
  private workspaces: T[] = [];
  private activeId: string | null = null;
  /** Monotonic id/name counter — no getrandom, no Date.now (CLAUDE.md; and it
   *  keeps ids deterministic for the unit tests). */
  private seq = 0;
  private listeners = new Set<TabsListener>();

  // Group→tab routing: spawn / focus / group-ended events resolve their target
  // tab through this. The router (main.ts's OrchWiring, fed by orchestration.ts's
  // backend listeners) populates and reads it; maintained here so add/close keep
  // it consistent in one place. (There is deliberately no pty→tab side-map: the
  // focus/exit/rename paths scan live panes via findPaneByPty, which can't go
  // stale on a pane close the way a maintained map would — see tabroute.ts.)
  private groupToWs = new Map<string, string>();
  /** Per-tab attention badge state, refreshed from the backend attention scan by
   *  the router; read by the tab bar. Absent = no attention. */
  private attn = new Map<string, TabAttn>();

  /** The remembered restore preference (#194 P4): "ask" until the human answers
   *  the boot splash, then "restore"/"fresh". Persisted in snapshot() and read
   *  back on boot to decide whether to prompt. */
  private restorePref: RestorePref = "ask";

  /** Builds a workspace for a freshly minted id (real Workspace in production;
   *  a fake in tests). Not a constructor parameter property — Node's strip-only
   *  TS loader (which runs the unit tests) rejects those. */
  private factory: (id: string) => T;

  constructor(factory: (id: string) => T) {
    this.factory = factory;
  }

  // ---------- read side ----------

  /** All tabs in display order. */
  get tabs(): readonly T[] {
    return this.workspaces;
  }

  get count(): number {
    return this.workspaces.length;
  }

  /** The active workspace. Throws if called before the first tab is created —
   *  callers (main.ts) always seed one default tab at startup. */
  get activeWorkspace(): T {
    const ws = this.workspaces.find((w) => w.id === this.activeId);
    if (!ws) throw new Error("TabManager: no active workspace (create a tab first)");
    return ws;
  }

  get activeTabId(): string | null {
    return this.activeId;
  }

  get(id: string): T | undefined {
    return this.workspaces.find((w) => w.id === id);
  }

  // ---------- mutations ----------

  /** Create a tab. Activates it (showing it, hiding the rest) unless
   *  `activate` is false, in which case it opens hidden in the background — but
   *  the very first tab always becomes active so there is always a visible one. */
  newTab(activate = true): T {
    const id = `ws-${++this.seq}`;
    const ws = this.factory(id);
    if (!ws.name) ws.name = `Tab ${this.seq}`;
    this.workspaces.push(ws);
    if (activate || this.activeId === null) {
      this.applyActive(id);
    } else {
      ws.setVisible(false);
    }
    this.emit();
    return ws;
  }

  /** Switch to a tab: mark it active, show it, hide every other. A no-op (bar a
   *  refocus) if it is already active or unknown. */
  switchTo(id: string): void {
    if (!this.get(id)) return;
    if (this.activeId === id) {
      this.activeWorkspace.focus();
      return;
    }
    this.applyActive(id);
    this.emit();
  }

  /** Close a tab. Refuses to remove the last one (the "never zero tabs"
   *  invariant — the app must always have a visible workspace with a focusable
   *  pane). Returns whether a tab was actually closed. Disposing kills the
   *  tab's panes' PTYs. */
  closeTab(id: string): boolean {
    if (this.workspaces.length <= 1) return false; // never zero tabs
    const idx = this.workspaces.findIndex((w) => w.id === id);
    if (idx < 0) return false;
    const [ws] = this.workspaces.splice(idx, 1);
    this.forgetRoutes(id);
    const wasActive = this.activeId === id;
    if (wasActive) {
      // Prefer the tab that shifted into this slot, else the new last tab.
      const next = this.workspaces[idx] ?? this.workspaces[this.workspaces.length - 1];
      this.applyActive(next.id);
    }
    ws.dispose();
    this.emit();
    return true;
  }

  /** Activate the next/previous tab, wrapping around. No-op with <2 tabs. */
  nextTab(): void {
    this.cycle(1);
  }
  prevTab(): void {
    this.cycle(-1);
  }

  /** Rename a tab. Blank/whitespace names are rejected (parity with pane
   *  rename, panerename.ts). */
  renameTab(id: string, name: string): void {
    const ws = this.get(id);
    if (!ws) return;
    const trimmed = name.trim();
    if (!trimmed) return;
    ws.name = trimmed;
    this.emit();
  }

  /** Set (or clear, with null) a tab's accent color. */
  setColor(id: string, color: string | null): void {
    const ws = this.get(id);
    if (!ws) return;
    ws.color = color;
    this.emit();
  }

  /** Reorder a tab (#379): move it to `toIndex` in the display order, shifting
   *  the tabs between its old and new slot. `tabs`'s array order IS the display
   *  and persisted order (see `snapshot`), so this is the entire model — the
   *  tab strip's drag handling and the keyboard alternative both call it with
   *  an already-clamped index. A no-op for an unknown id or a no-move
   *  (`toIndex` at or past the current slot after removal collapses to the
   *  same position). */
  moveTab(id: string, toIndex: number): void {
    const from = this.workspaces.findIndex((w) => w.id === id);
    if (from < 0) return;
    const clamped = Math.max(0, Math.min(toIndex, this.workspaces.length - 1));
    if (clamped === from) return;
    const [ws] = this.workspaces.splice(from, 1);
    this.workspaces.splice(clamped, 0, ws);
    this.emit();
  }

  /** Move the active tab one slot left/right (#379's keyboard alternative to
   *  dragging — Ctrl+Alt+Shift+BracketLeft/Right, main.ts). A no-op at either
   *  end, same as `moveTab` clamping past the array bounds. */
  moveActiveTab(delta: -1 | 1): void {
    if (!this.activeId) return;
    const from = this.workspaces.findIndex((w) => w.id === this.activeId);
    if (from < 0) return;
    this.moveTab(this.activeId, from + delta);
  }

  // ---------- orchestration routing ----------

  /** Bind an orchestration group to the tab that owns it. The router calls this
   *  on first sight of a group (spawn request), on launching an orchestrator into
   *  a fresh tab, and on restoring a group's session (main.ts). */
  bindGroup(groupId: string, workspaceId: string): void {
    this.groupToWs.set(groupId, workspaceId);
  }
  workspaceForGroup(groupId: string): T | undefined {
    const id = this.groupToWs.get(groupId);
    return id ? this.get(id) : undefined;
  }
  /** The group a tab owns (inverse of bindGroup), or null for a plain tab. */
  groupForWorkspace(workspaceId: string): string | null {
    for (const [g, wid] of this.groupToWs) if (wid === workspaceId) return g;
    return null;
  }

  // ---------- per-tab attention ----------

  /** Replace the whole per-tab attention set from an attention scan and emit.
   *  The caller (main.ts) dedups with tabroute.sameAttention so the 3-second
   *  re-emits don't reach here unchanged. */
  setTabAttention(next: Map<string, TabAttn>): void {
    this.attn = next;
    this.emit();
  }
  /** The current per-tab attention set, for the caller's change-detection. */
  get tabAttention(): ReadonlyMap<string, TabAttn> {
    return this.attn;
  }
  attentionFor(workspaceId: string): TabAttn | undefined {
    return this.attn.get(workspaceId);
  }

  // ---------- persistence ----------

  /** The remembered restore preference (read on boot; set from the splash). */
  get restorePreference(): RestorePref {
    return this.restorePref;
  }
  /** Record the human's restore choice so future boots honor it (#194 P4). Emits
   *  so it lands in the next persisted snapshot. */
  setRestorePreference(pref: RestorePref): void {
    if (this.restorePref === pref) return;
    this.restorePref = pref;
    this.emit();
  }

  /** Force a change notification when the pane set / layout changed inside a tab
   *  (a pane opened, closed, or converted) — the tab list itself is unchanged, so
   *  nothing else would emit. Drives the tab strip's live agent counter and the
   *  layout re-persist (#194 P4). */
  notifyLayoutChanged(): void {
    this.emit();
  }

  /** Snapshot the tab set for persistence: each tab's name/color/owning group and
   *  its captured pane LAYOUT (#194), which tab was active, and the remembered
   *  restore preference. Live PTY/buffer contents are NOT captured (tabstore.ts). */
  snapshot(): PersistedTabs {
    const tabs = this.workspaces.map((w) => ({
      name: w.name,
      color: w.color,
      groupId: this.groupForWorkspace(w.id),
      layout: w.captureLayout(),
      docked: w.captureDocked(),
    }));
    const activeIndex = Math.max(
      0,
      this.workspaces.findIndex((w) => w.id === this.activeId)
    );
    return { tabs, activeIndex, restorePref: this.restorePref };
  }

  // ---------- subscription ----------

  /** Subscribe to tab-set changes; returns an unsubscribe fn. */
  onChange(fn: TabsListener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  // ---------- internals ----------

  private applyActive(id: string): void {
    this.activeId = id;
    for (const w of this.workspaces) w.setVisible(w.id === id);
    this.activeWorkspace.focus();
  }

  private cycle(delta: 1 | -1): void {
    if (this.workspaces.length <= 1) return;
    const idx = this.workspaces.findIndex((w) => w.id === this.activeId);
    const n = this.workspaces.length;
    const next = this.workspaces[(idx + delta + n) % n];
    this.switchTo(next.id);
  }

  private forgetRoutes(workspaceId: string): void {
    for (const [g, wid] of this.groupToWs) if (wid === workspaceId) this.groupToWs.delete(g);
    this.attn.delete(workspaceId);
  }

  private emit(): void {
    for (const fn of this.listeners) fn();
  }

  /** Force a tab-bar re-render for state that lives OUTSIDE TabManager — cross-
   *  workspace channel membership (#271) is derived live from `ws.paneInfos()`
   *  (tabcounts.ts), not tracked here the way `attn` is, so there is no setter to
   *  trigger the usual `emit()`. The `orch-channel` event handler calls this after
   *  updating pane state so the tab-strip dot doesn't wait for the next 4s poll. */
  touch(): void {
    this.emit();
  }
}

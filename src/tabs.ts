// Project tabs (#63, Option A) — the tab model, deliberately DOM- and Grid-free.
//
// Each tab is a `Workspace`: one `Grid` + its own minimize dock (the DOM/Grid
// half lives in workspace.ts). `TabManager` owns the ordered tab list, the
// active tab, the "never zero tabs" invariant, and the routing maps that later
// phases use to send an orchestration group / pty to the right tab.
//
// This file has NO Tauri/xterm/Grid imports so it runs under `node --test`
// (CLAUDE.md: pure logic here, DOM wiring validated by hand). TabManager is
// generic over the minimal `ManagedWorkspace` surface, so tests drive it with a
// lightweight fake and production plugs in the real Grid-backed Workspace.

import type { TabAttn } from "./tabroute";
import type { PersistedTabs } from "./tabstore";

/** The minimal surface TabManager needs from a workspace. `Workspace`
 *  (workspace.ts) implements this and adds the concrete Grid/DOM. */
export interface ManagedWorkspace {
  readonly id: string;
  /** Human-facing tab name (in-memory for the prototype; phase 5 persists it). */
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
  /** Serialize the preview pane's FULL viewport right now, for a live hover
   *  thumbnail (#63 finding 2). Reads the in-memory xterm buffer (which keeps
   *  updating while hidden), so it works with zero layout and no PTY resize —
   *  the tab bar re-calls it on a short interval while hovered for a live view.
   *  May contain ANSI escapes the tab bar strips. "" when there's no pane. */
  livePreview(): string;
  /** Tear the workspace down and kill its panes' PTYs (tab closed). */
  dispose(): void;
}

/** Called on any change to the tab set (add/remove/switch/rename/color) so the
 *  tab bar can re-render. */
export type TabsListener = () => void;

export class TabManager<T extends ManagedWorkspace> {
  private workspaces: T[] = [];
  private activeId: string | null = null;
  /** Monotonic id/name counter — no getrandom, no Date.now (CLAUDE.md; and it
   *  keeps ids deterministic for the unit tests). */
  private seq = 0;
  private listeners = new Set<TabsListener>();

  // Routing seams for phase 3 (worker B): orchestration events (spawn/focus/
  // attention/group-ended) resolve their target tab through these. Kept here so
  // add/remove maintain them in one place; the tab-aware router that populates
  // and reads them is TODO(#63 phase 3).
  private groupToWs = new Map<string, string>();
  private ptyToWs = new Map<number, string>();
  /** Per-tab attention badge state (phase 3), refreshed from the attention scan
   *  by the router; read by the tab bar. Absent = no attention. */
  private attn = new Map<string, TabAttn>();

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

  // ---------- routing seams (phase 3, worker B) ----------

  /** Bind an orchestration group to the tab that owns it. TODO(#63 phase 3):
   *  the tab-aware orchestration router calls this on first sight of a group. */
  bindGroup(groupId: string, workspaceId: string): void {
    this.groupToWs.set(groupId, workspaceId);
  }
  /** Bind a pty to its owning tab (derived from the group). TODO(#63 phase 3). */
  bindPty(ptyId: number, workspaceId: string): void {
    this.ptyToWs.set(ptyId, workspaceId);
  }
  workspaceForGroup(groupId: string): T | undefined {
    const id = this.groupToWs.get(groupId);
    return id ? this.get(id) : undefined;
  }
  workspaceForPty(ptyId: number): T | undefined {
    const id = this.ptyToWs.get(ptyId);
    return id ? this.get(id) : undefined;
  }
  /** The group a tab owns (inverse of bindGroup), or null for a plain tab. */
  groupForWorkspace(workspaceId: string): string | null {
    for (const [g, wid] of this.groupToWs) if (wid === workspaceId) return g;
    return null;
  }
  /** Read-only view of the pty→tab routing, for the attention/focus routers. */
  get ptyRoutes(): ReadonlyMap<number, string> {
    return this.ptyToWs;
  }

  // ---------- per-tab attention (phase 3) ----------

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

  // ---------- persistence (phase 5) ----------

  /** Snapshot the tab set for persistence: each tab's name/color/owning group,
   *  plus which tab was active. Pane/PTY contents are NOT captured (see
   *  tabstore.ts). */
  snapshot(): PersistedTabs {
    const tabs = this.workspaces.map((w) => ({
      name: w.name,
      color: w.color,
      groupId: this.groupForWorkspace(w.id),
    }));
    const activeIndex = Math.max(
      0,
      this.workspaces.findIndex((w) => w.id === this.activeId)
    );
    return { tabs, activeIndex };
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
    for (const [p, wid] of this.ptyToWs) if (wid === workspaceId) this.ptyToWs.delete(p);
    this.attn.delete(workspaceId);
  }

  private emit(): void {
    for (const fn of this.listeners) fn();
  }
}

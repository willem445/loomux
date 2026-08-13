// The project-tab strip (#63): switch, close, new (+), double-click rename,
// color swatch; a labelled attention chip on a tab whose (possibly hidden) pane
// needs the human; a live status chip (agent count + cost); a hover thumbnail
// compositing the tab's whole layout; and a right-click menu that pauses/resumes
// the tab's orchestration group.
//
// Pure tab state lives in TabManager (tabs.ts); this module renders it and turns
// interactions into TabManager / backend calls, re-rendering on onChange.

import { dropTargetIndex, type TabManager, type ManagedWorkspace } from "./tabs";
import { safeStyleDeclarations, compositeScale, type PreviewNode, type PreviewFit } from "./tabroute";
import { makeRenameCommit } from "./panerename";
import { swapEditor } from "./domutil";
import { attentionPresentation } from "./attention";
import { pauseGroup, resumeGroup, groupSummary, groupUsage } from "./orchestration";
import { tabCounts } from "./tabcounts";
import { PollGate } from "./pollgate";

// Reuse the orchestration group palette (orchbadge.ts GROUP_COLORS) so a tab's
// color vocabulary matches the group-accent colors the panes already use.
const TAB_COLORS = ["#7aa2f7", "#9ece6a", "#e0af68", "#bb9af7", "#7dcfff", "#f7768e"];

// Preview cost, formalized (#63). Re-serializing every pane on a fast timer is
// the preview's whole expense, so both levers are named + bounded here and in
// workspace.ts (PREVIEW_PANE_CAP = 8 panes/refresh). See the design doc.
//
// PREVIEW_REFRESH_MS: how often a hovered background tab re-serializes its
// viewport so a running prompt streams in. ~700ms is well below a human's sense
// of "live" yet coarse enough that even an 8-pane composite is a trivial slice
// of one frame budget; it only runs WHILE a background tab is hovered (one tab
// at a time), and stops the instant the pointer leaves. Degradation if a grid is
// enormous: panes past the cap render as a titled placeholder, never unbounded
// work (workspace.ts).
const PREVIEW_REFRESH_MS = 700;

// Every mini-pane in a composite renders at ONE shared scale (compositeScale),
// so text is a consistent, readable size across panes regardless of each pane's
// serialized terminal dims (#63 review). PREVIEW_MIN_SCALE floors it off the
// sub-pixel range where a heavily-downscaled pane's background rows smear into
// bars — below it, an oversized pane crops to its cell rather than shrinking
// further. 1 is the ceiling (never enlarge past the source glyphs).
const PREVIEW_MIN_SCALE = 0.16;
const PREVIEW_MAX_SCALE = 1;

/** Pixels the pointer must travel from the mousedown before a tab-strip drag
 *  starts (#379's pointer-based rework) — matches grid.ts's own
 *  DRAG_THRESHOLD_PX, keeping a plain click (switch tab, open the rename
 *  input) from turning into a reorder attempt at the slightest hand tremor. */
const DRAG_THRESHOLD_PX = 6;

/** Live per-tab status pulled from the backend for a bound group. */
interface TabStatus {
  agents: number;
  cost: number | null;
  paused: boolean;
}

export class TabBar<T extends ManagedWorkspace = ManagedWorkspace> {
  /** The id currently being renamed, so a re-render doesn't clobber the open
   *  input mid-edit. */
  private renamingId: string | null = null;
  private palette: HTMLElement | null = null;
  private menu: HTMLElement | null = null;
  private preview: HTMLElement | null = null;
  private previewTimer: number | null = null;
  /** The open preview's repaint closure, held so the visibility gate can
   *  re-arm (and catch up) the timer it owns without reopening the popup. */
  private previewPaint: (() => void) | null = null;
  private previewWsId: string | null = null;
  private status = new Map<string, TabStatus>();
  private statusTimer: number | null = null;
  /** Visibility gates for this strip's two timers (#743 S6, pollgate.ts). */
  private statusGate: PollGate;
  private previewGate: PollGate;
  /** The tab whose close is armed for a two-step confirm (destructive close of a
   *  group-owning tab), and its auto-disarm timer. Mirrors the group view's
   *  "End orchestration" arm/confirm (groupview.ts) so ending a project's agents
   *  takes a deliberate second action. */
  private closeArmedId: string | null = null;
  private closeArmTimer: number | null = null;
  /** The tab id mid-drag (#379), or null. Held on the strip rather than
   *  per-tab DOM state so `render()` (and any other tab's own hit-testing,
   *  see `wireDrag`) can tell a drag is in progress without threading a
   *  reference through every tab element. */
  private draggingId: string | null = null;

  private el: HTMLElement;
  private tabs: TabManager<T>;
  /** Opens a new tab WITH its starting pane . Falls back to a bare
   *  TabManager.newTab (blank) only if the host didn't wire one. */
  private onNewTab: () => void;

  constructor(el: HTMLElement, tabs: TabManager<T>, onNewTab?: () => void) {
    this.el = el;
    this.tabs = tabs;
    this.onNewTab = onNewTab ?? (() => void tabs.newTab());
    tabs.onChange(() => this.render());
    // Poll bound groups for agent count / cost / paused (#63). Cheap; the
    // strip only re-renders when a value actually differs.
    //
    // Visibility-gated (#743 S6). This is the app's one app-lifetime poll —
    // armed here and never cleared — and every tick invokes groupSummary AND
    // groupUsage for EVERY group-bound tab, so behind a minimized window it was
    // the largest standing IPC cost in the app while feeding a strip nobody
    // could see. The gate stops the timer outright while the window is hidden
    // and runs one catch-up poll when it comes back, so the strip is current by
    // the time it is looked at instead of up to 4 s stale.
    //
    // What is deliberately NOT done here: per-tab cadence tiers. The plan (#743
    // S6) left open whether background tabs should stretch or skip groupUsage,
    // and the answer is neither — a background tab's badge IS what the strip is
    // for, so polling only the active tab would remove the feature to save its
    // cost, and S4 already collapsed the expensive half (groupUsage now shares
    // one ~1 s backend snapshot across every caller). Window visibility is the
    // axis where the work is worth nothing; tab position is not.
    this.statusGate = new PollGate({
      arm: () => {
        this.statusTimer = window.setInterval(() => void this.pollStatus(), 4000);
      },
      disarm: () => {
        if (this.statusTimer !== null) clearInterval(this.statusTimer);
        this.statusTimer = null;
      },
      refresh: () => void this.pollStatus(),
    });
    this.statusGate.enable();

    // The hover preview's own repaint timer (below) is gated too: it issues no
    // IPC, but re-serializing up to eight panes every 700 ms is real webview-
    // thread work, and a window minimized with a preview open keeps a pointer
    // "over" the strip as far as the DOM is concerned — nothing synthesizes the
    // mouseleave that would otherwise close it.
    this.previewGate = new PollGate({
      arm: () => {
        if (this.previewPaint !== null) {
          this.previewTimer = window.setInterval(this.previewPaint, PREVIEW_REFRESH_MS);
        }
      },
      disarm: () => {
        if (this.previewTimer !== null) clearInterval(this.previewTimer);
        this.previewTimer = null;
      },
      refresh: () => this.previewPaint?.(),
    });

    // Preview lifecycle (#63 — no stuck preview). These live on the
    // stable strip element (not per-tab), so a re-render never orphans them and
    // the popup dismisses reliably. Delegation drives open/switch by whichever
    // non-active tab the pointer is over; anything else closes it.
    this.el.addEventListener("mousemove", (e) => this.onStripHover(e));
    this.el.addEventListener("mouseleave", () => this.closePreview()); // leaving the strip
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape") this.closePreview();
    });

    this.render();
  }

  /** Open/switch/close the hover preview from a pointer position over the strip. */
  private onStripHover(e: MouseEvent): void {
    const tabEl = (e.target as HTMLElement).closest<HTMLElement>(".tab");
    const wsId = tabEl?.dataset.wsId ?? null;
    // No tab under the pointer (gap / the + button), or the ACTIVE tab (it's
    // right there) → close. A different non-active tab → (re)open for it.
    if (!wsId || wsId === this.tabs.activeTabId) {
      this.closePreview();
      return;
    }
    if (wsId === this.previewWsId) return; // already previewing this tab; the timer keeps it live
    const ws = this.tabs.get(wsId);
    if (ws && tabEl) this.openPreview(ws, tabEl);
  }

  /** Close a tab, requiring a two-step confirm when the close is DESTRUCTIVE. Two
   *  things make it so:
   *
   *   - the tab owns an orchestration group, so closing it KILLS that project's live
   *     agents (unrecoverable, and it's what the human is really asking for when they
   *     hit ✕ / Ctrl+Shift+K);
   *   - a pane in it holds UNSAVED editor edits (#217) — closing the tab disposes every
   *     pane, and those edits are gone. A single-pane close asks about them with a
   *     modal (Pane.requestClose); a tab close tears down N panes synchronously, so it
   *     asks the way this bar already asks about something irreversible: arm, then
   *     confirm. Cheap, sync, and it reuses the affordance the human has already met.
   *
   *  An ordinary tab still closes immediately: its panes are shells, on par with
   *  close-pane (itself unconfirmed), so a confirm there would be heavier than closing
   *  the same panes directly. Both the ✕ button and Ctrl+Shift+K route here (main.ts). */
  requestClose(id: string): void {
    const ws = this.tabs.get(id);
    if (!ws || this.tabs.count <= 1) return; // never-zero-tabs floor
    if (!this.destructiveClose(id)) {
      this.tabs.closeTab(id); // non-destructive — no confirm
      return;
    }
    if (this.closeArmedId === id) {
      this.disarmClose();
      this.tabs.closeTab(id); // second action within the window — do it
      return;
    }
    // Arm this tab; a 4s window (matching groupview's End) then auto-disarms.
    this.disarmClose();
    this.closeArmedId = id;
    this.closeArmTimer = window.setTimeout(() => {
      this.closeArmedId = null;
      this.render();
    }, 4000);
    this.render();
  }

  /** Would closing this tab destroy something the human can't get back — its group's
   *  live agents, or unsaved editor edits in one of its panes (#217)? Asked fresh on
   *  every close attempt and on every render, so saving the file (or ending the group)
   *  disarms the confirm without anything having to remember to. */
  private destructiveClose(id: string): boolean {
    const ws = this.tabs.get(id);
    if (!ws) return false;
    return !!this.tabs.groupForWorkspace(id) || ws.hasUnsavedWork();
  }

  private disarmClose(): void {
    if (this.closeArmTimer !== null) {
      clearTimeout(this.closeArmTimer);
      this.closeArmTimer = null;
    }
    this.closeArmedId = null;
  }

  private render(): void {
    // Don't stomp an in-flight rename input.
    if (this.renamingId) return;
    // Don't rebuild out from under an in-flight drag (#379) — replacing the
    // dragged tab's DOM node mid-gesture (e.g. a status poll's re-render
    // landing between pointerdown and pointerup) would detach the element the
    // live pointermove/pointerup closures still reference, silently killing
    // the CSS feedback (dimming, drop indicators) with no visible error.
    // `onTabPointerDown`'s own `finish()` clears `draggingId` itself, before
    // calling `moveTab`, so the reorder's own resulting render still goes
    // through.
    if (this.draggingId) return;
    this.el.replaceChildren();

    for (const ws of this.tabs.tabs) {
      const active = ws.id === this.tabs.activeTabId;
      const attn = this.tabs.attentionFor(ws.id);
      const st = this.status.get(ws.id);
      const groupId = this.tabs.groupForWorkspace(ws.id);

      const tab = document.createElement("div");
      tab.className = "tab" + (active ? " active" : "");
      tab.dataset.wsId = ws.id;
      if (ws.color) tab.style.setProperty("--tab-color", ws.color);
      tab.classList.toggle("colored", !!ws.color);
      if (attn) {
        tab.classList.add("needs-attention");
        tab.classList.toggle("urgent", attn.urgent);
      }

      // Color swatch: click opens the palette; stops the click from switching.
      const swatch = document.createElement("button");
      swatch.className = "tab-swatch";
      swatch.title = "Tab color";
      swatch.addEventListener("click", (e) => {
        e.stopPropagation();
        this.openPalette(ws.id, swatch);
      });

      const name = document.createElement("span");
      name.className = "tab-name";
      name.textContent = ws.name;
      name.addEventListener("dblclick", (e) => {
        e.stopPropagation();
        this.startRename(ws, name);
      });

      tab.append(swatch, name);

      // Unmistakable alert (#63): a pane in this tab is
      // blocked/waiting or otherwise needs the human. Render the SAME label the
      // pane header chip shows (attention.ts), red for the urgent `blocked`
      // class, amber otherwise, pulsing — so a blocked agent in a background tab
      // can't be missed. Covers every attention class (blocked/waiting/report/
      // gate) and both agent and plain (#40) panes.
      if (attn) {
        const chip = document.createElement("span");
        chip.className = "tab-attn";
        chip.dataset.reason = attn.reason;
        chip.textContent = attentionPresentation(attn.reason).label;
        chip.title = `A pane in "${ws.name}" needs you (${attn.reason}) — click to switch`;
        tab.appendChild(chip);
      }

      // Live agent counter + orchestration markers (#194 P4). The count is now
      // DETERMINISTIC — derived from the panes actually open in this tab
      // (tabcounts.ts), not the flaky 4s backend poll that made it render
      // sometimes and flash a stray "+0". Cost/paused still come from the poll
      // (backend-only facts), shown alongside.
      const counts = tabCounts(ws.paneInfos(), !!groupId);
      if (counts.agents > 0) {
        const status = document.createElement("span");
        status.className = "tab-status";
        const cost = st?.cost != null ? ` · $${st.cost.toFixed(2)}` : "";
        status.textContent = `✦${counts.agents}${cost}`;
        status.title = `${counts.agents} live agent(s)${cost ? `, ${cost.slice(3)} so far` : ""}`;
        tab.appendChild(status);
      } else if (st?.cost != null && (groupId || counts.dormantOrch)) {
        // No live agents, but the group's accrued cost is still worth showing —
        // don't let it vanish when the last agent exits/idle-kills (#194 P4 LOW-8).
        const status = document.createElement("span");
        status.className = "tab-status";
        status.textContent = `$${st.cost.toFixed(2)}`;
        status.title = `$${st.cost.toFixed(2)} accrued (no live agents)`;
        tab.appendChild(status);
      }
      // Orchestration marker: a live icon when a group is running in this tab, or
      // the static ORCH chip for a dormant (restored-but-not-resumed) group — a
      // tab can mix normal agents with orchestration, so this is independent of
      // the agent count. Never both at once (tabCounts guarantees it).
      if (counts.liveOrch) {
        const orch = document.createElement("span");
        orch.className = "tab-orch live";
        orch.textContent = "⛓";
        orch.title = "Orchestration active in this tab";
        tab.appendChild(orch);
      } else if (counts.dormantOrch) {
        const orch = document.createElement("span");
        orch.className = "tab-orch dormant";
        orch.textContent = "ORCH";
        orch.title = "Dormant orchestration group — open the tab and Resume it";
        tab.appendChild(orch);
      }
      // Cross-workspace channel dot (#271): a pane in this tab is connected, so
      // a hidden/background tab still surfaces it — the per-pane header chip
      // alone only shows once you switch to the tab. The count (not just a
      // boolean) distinguishes one channel from the multi-channel case the
      // whole feature exists to make legible.
      if (counts.connectedChannels > 0) {
        const chan = document.createElement("span");
        chan.className = "tab-channel";
        chan.textContent = counts.connectedChannels > 1 ? `⇄${counts.connectedChannels}` : "⇄";
        chan.title =
          counts.connectedChannels === 1
            ? `A pane in "${ws.name}" is connected to a cross-workspace channel`
            : `Panes in "${ws.name}" are connected to ${counts.connectedChannels} cross-workspace channels`;
        tab.appendChild(chan);
      }
      if (st?.paused) {
        const pause = document.createElement("span");
        pause.className = "tab-paused";
        pause.textContent = "⏸";
        pause.title = "Project paused — prompts/kickoffs held";
        tab.appendChild(pause);
      }

      const close = document.createElement("button");
      close.className = "tab-close";
      const armed = this.closeArmedId === ws.id;
      const ownsGroup = !!groupId;
      // Say what will actually be lost. A tab can be destructive for two different
      // reasons now (live agents, unsaved edits — #217) and it can be both at once;
      // naming only the group would let a human confirm a close believing their edits
      // were safe.
      const stake = [ownsGroup ? "end its agents" : null, ws.hasUnsavedWork() ? "discard unsaved edits" : null]
        .filter(Boolean)
        .join(" and ");
      close.classList.toggle("confirm", armed);
      close.textContent = armed ? "✕?" : "✕";
      close.title = armed
        ? `Click again to close "${ws.name}" — this will ${stake}`
        : stake
          ? `Close tab — will ${stake} (confirm, Ctrl+Shift+K)`
          : "Close tab (Ctrl+Shift+K)";
      close.hidden = this.tabs.count <= 1; // never zero tabs
      close.addEventListener("click", (e) => {
        e.stopPropagation();
        this.requestClose(ws.id);
      });
      tab.appendChild(close);

      tab.addEventListener("click", () => {
        // Activating a tab must dismiss the preview immediately — else it lingers
        // over the now-active tab (#63). Hover delegation handles the
        // rest (the newly-active tab won't re-open a preview).
        this.closePreview();
        this.tabs.switchTo(ws.id);
      });
      tab.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        this.closePreview();
        this.openMenu(ws, e.clientX, e.clientY);
      });
      this.wireDrag(tab, ws.id);
      this.el.appendChild(tab);
    }

    const add = document.createElement("button");
    add.className = "tab-add";
    add.textContent = "+";
    add.title = "New tab (Ctrl+Shift+T)";
    add.addEventListener("click", () => this.onNewTab());
    this.el.appendChild(add);
  }

  /** Wire drag-to-reorder for one tab element via POINTER events (#379,
   *  reworked in #402's fourth live-demo round). The original mechanism used
   *  native HTML5 `draggable="true"` drag-and-drop; in real WebView2 usage it
   *  "grabbed" (dragstart fired, the tab dimmed) but never accepted a drop
   *  anywhere — a failure that neither manual `DragEvent` dispatch nor
   *  Playwright's CDP-driven `locator.dragTo()` could reproduce in
   *  `e2e/tests/tab-reorder.spec.ts`, tried against the code both with and
   *  without the dragenter/dropEffect fix from the prior review round. Both
   *  techniques succeeded every time regardless, which points at the gap
   *  living in how native HTML5 DnD initiates against a REAL OS-level mouse
   *  gesture inside a WebView2-hosted Tauri window — a layer CDP's synthetic
   *  input never touches, so it could never have caught this. `src/grid.ts`'s
   *  own pane-reorder drag already avoids that whole layer (raw
   *  `pointerdown`/`pointermove`/`pointerup`, no native DnD at all) and IS
   *  E2E-verified reliable (`e2e/tests/pane-reorder.spec.ts`); this mirrors
   *  that exact mechanism — a drag threshold, hit-testing by iterating live
   *  element rects, `Escape` to cancel — instead of native
   *  draggable/dragstart/dragover/drop, which is removed entirely. The pure
   *  index math (`dropTargetIndex`/`moveTab`, tabs.ts) is untouched — only
   *  the DOM trigger mechanism changed, so `test/tabs.test.ts`'s coverage of
   *  it still stands. The dragged tab dims and the hovered tab shows a thin
   *  accent line on whichever edge the drop would land on (CSS
   *  `.dragging`/`.drag-before`/`.drag-after`, unchanged from the native-DnD
   *  version) — same "target shows where it lands" convention as the
   *  split-drag drop zones (layout.ts). */
  private wireDrag(tab: HTMLElement, wsId: string): void {
    tab.addEventListener("pointerdown", (e) => this.onTabPointerDown(e, tab, wsId));
  }

  private onTabPointerDown(down: PointerEvent, tab: HTMLElement, wsId: string): void {
    if (down.button !== 0) return;
    // The swatch/close buttons keep their own click behavior — never start a
    // drag from them (mirrors grid.ts's own onPointerDown button exclusion).
    if ((down.target as HTMLElement).closest("button")) return;
    if (this.tabs.count < 2) return; // nothing to reorder into

    const startX = down.clientX;
    const startY = down.clientY;
    let started = false;
    let hover: { id: string; before: boolean } | null = null;

    const hitTest = (x: number, y: number): { id: string; before: boolean } | null => {
      for (const el of Array.from(this.el.querySelectorAll<HTMLElement>(".tab"))) {
        const id = el.dataset.wsId;
        if (!id || id === wsId) continue;
        const r = el.getBoundingClientRect();
        if (x < r.left || x > r.right || y < r.top || y > r.bottom) continue;
        return { id, before: x < r.left + r.width / 2 };
      }
      return null;
    };

    const clearIndicators = (): void => {
      for (const el of this.el.querySelectorAll(".drag-before, .drag-after")) {
        el.classList.remove("drag-before", "drag-after");
      }
    };

    const start = (): void => {
      started = true;
      this.closePreview();
      this.draggingId = wsId;
      tab.classList.add("dragging");
      // Native HTML5 drag gave a "grabbing" OS cursor for free; a pointer-
      // based drag has to supply its own — same body-class convention
      // grid.ts's pane-drag uses (`dragging-pane`/styles.css).
      document.body.classList.add("dragging-tab");
    };

    const move = (ev: PointerEvent): void => {
      if (!started) {
        if (Math.hypot(ev.clientX - startX, ev.clientY - startY) < DRAG_THRESHOLD_PX) return;
        start();
      }
      hover = hitTest(ev.clientX, ev.clientY);
      clearIndicators();
      if (hover) {
        const target = this.el.querySelector<HTMLElement>(`.tab[data-ws-id="${hover.id}"]`);
        target?.classList.toggle("drag-before", hover.before);
        target?.classList.toggle("drag-after", !hover.before);
      }
    };

    const finish = (commit: boolean): void => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("keydown", onKey, true);
      // Clear BEFORE moveTab: its emit() → render() checks draggingId and
      // skips the rebuild while a drag is in flight (see render()) — the
      // commit itself must not be mistaken for still-dragging, or the
      // reorder never paints until some unrelated later render fires.
      this.draggingId = null;
      tab.classList.remove("dragging");
      document.body.classList.remove("dragging-tab");
      clearIndicators();
      if (commit && started && hover) {
        const ids = this.tabs.tabs.map((w) => w.id);
        this.tabs.moveTab(wsId, dropTargetIndex(ids, wsId, hover.id, hover.before));
      }
    };
    const up = (): void => finish(true);
    const onKey = (ev: KeyboardEvent): void => {
      if (ev.key === "Escape") {
        ev.preventDefault();
        ev.stopPropagation();
        finish(false);
      }
    };

    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("keydown", onKey, true);
  }

  /** Poll each group-bound tab for its live status; re-render if anything moved. */
  private async pollStatus(): Promise<void> {
    let changed = false;
    const seen = new Set<string>();
    for (const ws of this.tabs.tabs) {
      const groupId = this.tabs.groupForWorkspace(ws.id);
      if (!groupId) continue;
      seen.add(ws.id);
      try {
        const [summary, usage] = await Promise.all([groupSummary(groupId), groupUsage(groupId)]);
        // #904: the backend answers `null` if it refuses the group id. A tab
        // badge has no way to say "unknown", and a stale-but-true count beats
        // a fabricated zero — so leave the previous status in place rather
        // than writing one built from a refusal.
        if (!summary || !usage) continue;
        const next: TabStatus = {
          agents: summary.live_agents,
          cost: usage.live_cost_usd,
          paused: summary.paused,
        };
        const prev = this.status.get(ws.id);
        if (!prev || prev.agents !== next.agents || prev.cost !== next.cost || prev.paused !== next.paused) {
          this.status.set(ws.id, next);
          changed = true;
        }
      } catch {
        /* a group not yet known to the backend — skip this tick */
      }
    }
    // Drop status for tabs that lost their group / closed.
    for (const id of [...this.status.keys()]) {
      if (!seen.has(id)) {
        this.status.delete(id);
        changed = true;
      }
    }
    if (changed) this.render();
  }

  /** Inline rename, mirroring the pane title rename (makeRenameCommit +
   *  swapEditor) so the double-commit crash guards (#75/#113) apply here too. */
  private startRename(ws: ManagedWorkspace, nameEl: HTMLElement): void {
    this.renamingId = ws.id;
    const input = document.createElement("input");
    input.className = "tab-name-input";
    input.value = ws.name;
    nameEl.replaceWith(input);
    input.focus();
    input.select();

    const commit = makeRenameCommit({
      value: () => input.value,
      isConnected: () => input.isConnected,
      save: (name) => this.tabs.renameTab(ws.id, name),
      restore: () => {
        nameEl.textContent = ws.name;
        swapEditor(input, nameEl);
        this.renamingId = null;
        this.render(); // reflect the new name (and re-enable close etc.)
      },
    });
    input.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Enter") commit(true);
      if (e.key === "Escape") commit(false);
    });
    input.addEventListener("blur", () => commit(true));
  }

  /** A small color palette anchored under the swatch: the shared group colors
   *  plus a native custom picker and a "default" (clear) option. */
  private openPalette(wsId: string, anchor: HTMLElement): void {
    this.closePreview();
    this.closePalette();
    const pop = document.createElement("div");
    pop.className = "tab-palette";

    for (const color of TAB_COLORS) {
      const dot = document.createElement("button");
      dot.className = "tab-palette-dot";
      dot.style.background = color;
      dot.title = color;
      dot.addEventListener("click", () => {
        this.tabs.setColor(wsId, color);
        this.closePalette();
      });
      pop.appendChild(dot);
    }

    const custom = document.createElement("input");
    custom.type = "color";
    custom.className = "tab-palette-custom";
    custom.title = "Custom color";
    custom.addEventListener("input", () => this.tabs.setColor(wsId, custom.value));
    pop.appendChild(custom);

    const clear = document.createElement("button");
    clear.className = "tab-palette-clear";
    clear.textContent = "default";
    clear.addEventListener("click", () => {
      this.tabs.setColor(wsId, null);
      this.closePalette();
    });
    pop.appendChild(clear);

    this.anchorPopover(pop, anchor.getBoundingClientRect());
    this.palette = pop;
    this.dismissOnOutside(pop, () => this.closePalette());
  }

  private closePalette(): void {
    this.palette?.remove();
    this.palette = null;
  }

  /** Right-click context menu: pause/resume the tab's orchestration group
   *  (contains unattended spend, #63/#78), plus rename/close. Pause is only
   *  offered when the tab owns a group. */
  private openMenu(ws: ManagedWorkspace, x: number, y: number): void {
    this.closeMenu();
    const menu = document.createElement("div");
    menu.className = "tab-menu";

    const groupId = this.tabs.groupForWorkspace(ws.id);
    if (groupId) {
      const paused = this.status.get(ws.id)?.paused ?? false;
      const item = document.createElement("button");
      item.className = "tab-menu-item";
      item.textContent = paused ? "Resume project" : "Pause project";
      item.title = paused
        ? "Resume prompt/kickoff delivery to this project's agents"
        : "Hold prompt/kickoff delivery so this project's agents idle out (contains spend)";
      item.addEventListener("click", () => {
        void (paused ? resumeGroup(groupId) : pauseGroup(groupId)).finally(() => {
          void this.pollStatus();
        });
        this.closeMenu();
      });
      menu.appendChild(item);
    }

    const rename = document.createElement("button");
    rename.className = "tab-menu-item";
    rename.textContent = "Rename tab";
    rename.addEventListener("click", () => {
      this.closeMenu();
      const nameEl = this.el.querySelector<HTMLElement>(`.tab[data-ws-id="${ws.id}"] .tab-name`);
      if (nameEl) this.startRename(ws, nameEl);
    });
    menu.appendChild(rename);

    if (this.tabs.count > 1) {
      const close = document.createElement("button");
      close.className = "tab-menu-item danger";
      close.textContent = "Close tab";
      close.addEventListener("click", () => {
        this.closeMenu();
        this.requestClose(ws.id); // same confirm as the ✕ / shortcut
      });
      menu.appendChild(close);
    }

    document.body.appendChild(menu);
    menu.style.left = `${x}px`;
    menu.style.top = `${y}px`;
    this.menu = menu;
    this.dismissOnOutside(menu, () => this.closeMenu());
  }

  private closeMenu(): void {
    this.menu?.remove();
    this.menu = null;
  }

  /** Live hover thumbnail (#63): a composite of the tab's FULL
   *  layout — every pane serialized (with color + correct spacing) and arranged
   *  like its split tree — re-serialized every ~700ms so a running prompt streams
   *  in. It's a serialized snapshot of the in-memory buffers, NEVER a laid-out
   *  pane, so it can't re-arm a hidden pane's fit / PTY resize. */
  private openPreview(ws: ManagedWorkspace, anchor: HTMLElement): void {
    this.closePreview();
    const pop = document.createElement("div");
    pop.className = "tab-preview";
    document.body.appendChild(pop);
    this.preview = pop;
    this.previewWsId = ws.id;

    const anchorRect = anchor.getBoundingClientRect();
    const paint = () => {
      const layout = ws.previewLayout();
      if (!layout) {
        this.closePreview();
        return;
      }
      // Fixed content box; each pane's content is scaled to fit its own cell.
      const cw = Math.min(window.innerWidth * 0.92, 860);
      const ch = Math.min(window.innerHeight * 0.72, 540);
      const root = this.buildPreviewNode(layout);
      root.style.width = `${cw}px`;
      root.style.height = `${ch}px`;
      pop.replaceChildren(root);
      // Scale ALL mini-panes at one shared factor now that the tree is laid out,
      // so text is a consistent size across the composite (#63 review).
      this.scaleComposite(pop);
      this.positionPreview(pop, anchorRect, cw + 16, ch + 16);
    };
    paint();
    this.previewPaint = paint;
    this.previewGate.enable();
  }

  /** Build the preview composite: nested flex boxes mirroring the split tree,
   *  each leaf a titled mini-pane holding the safely-rebuilt serialized viewport. */
  private buildPreviewNode(node: PreviewNode): HTMLElement {
    if (node.kind === "split") {
      const box = document.createElement("div");
      box.className = `mini-split ${node.dir}`;
      for (const child of node.children) {
        const el = this.buildPreviewNode(child);
        el.style.flex = `${child.weight} 1 0`;
        box.appendChild(el);
      }
      return box;
    }
    const leaf = document.createElement("div");
    leaf.className = "mini-pane";
    const title = document.createElement("div");
    title.className = "mini-pane-title";
    title.textContent = node.title;
    const body = document.createElement("div");
    body.className = "mini-pane-body";
    const scaler = document.createElement("div");
    scaler.className = "mini-pane-scaler";
    if (node.capped) {
      const note = document.createElement("div");
      note.className = "mini-pane-note";
      note.textContent = "(preview capped)";
      scaler.appendChild(note);
    } else {
      scaler.appendChild(this.renderSerializedHtml(node.html));
    }
    body.appendChild(scaler);
    leaf.append(title, body);
    return leaf;
  }

  /** Rebuild @xterm/addon-serialize HTML SAFELY : parse it detached,
   *  then re-emit each cell run as a <span> with textContent (auto-escaped) and
   *  a whitelisted, value-sanitized inline style — never innerHTML of the raw
   *  string, which the addon does not escape. */
  private renderSerializedHtml(html: string): HTMLElement {
    const term = document.createElement("div");
    term.className = "mini-term";
    let container: Element | null = null;
    try {
      const doc = new DOMParser().parseFromString(html, "text/html");
      container = doc.querySelector("pre > div") ?? doc.querySelector("pre") ?? doc.body;
    } catch {
      container = null;
    }
    if (!container) return term;
    for (const rowEl of Array.from(container.children)) {
      const row = document.createElement("div");
      row.className = "mini-term-row";
      const spans = rowEl.querySelectorAll(":scope > span");
      let any = false;
      for (const span of Array.from(spans)) {
        const text = span.textContent ?? "";
        if (!text) continue;
        const s = document.createElement("span");
        s.textContent = text; // auto-escapes — no injection from raw cell chars
        this.applySafeStyle(s, span.getAttribute("style"));
        row.appendChild(s);
        any = true;
      }
      if (!any) row.textContent = " "; // keep a blank row's height
      term.appendChild(row);
    }
    return term;
  }

  /** Apply only whitelisted CSS props with sanitized values to a preview span.
   *  The whitelist + value guards live in the pure `safeStyleDeclarations`
   *  (tabroute.ts), unit-tested against injection attempts. */
  private applySafeStyle(el: HTMLElement, style: string | null): void {
    for (const [prop, value] of safeStyleDeclarations(style)) {
      el.style.setProperty(prop, value);
    }
  }

  /** Scale every mini-pane in the composite at ONE shared factor, so glyphs are
   *  a consistent, readable size across panes — instead of fitting each pane to
   *  its own cell, which made panes serialized at different terminal dims render
   *  at wildly different font sizes (#63 review). Two passes: measure each pane's
   *  natural content + cell size (transform reset so scrollWidth/Height are the
   *  unscaled content), compute the shared scale (the pure, tested
   *  `compositeScale` — median of per-pane fits, clamped), then apply it to all.
   *  A pane whose content overflows its cell at that scale crops (cells are
   *  `overflow:hidden`) — crop, never squish; the glyph aspect is always
   *  preserved by the uniform `scale()`. */
  private scaleComposite(root: HTMLElement): void {
    const scalers: HTMLElement[] = [];
    const fits: PreviewFit[] = [];
    for (const pane of Array.from(root.querySelectorAll<HTMLElement>(".mini-pane"))) {
      const body = pane.querySelector<HTMLElement>(".mini-pane-body");
      const scaler = pane.querySelector<HTMLElement>(".mini-pane-scaler");
      if (!body || !scaler) continue;
      scaler.style.transform = "none"; // measure natural (unscaled) content size
      scalers.push(scaler);
      fits.push({
        contentW: Math.max(1, scaler.scrollWidth),
        contentH: Math.max(1, scaler.scrollHeight),
        cellW: body.clientWidth,
        cellH: body.clientHeight,
      });
    }
    const scale = compositeScale(fits, PREVIEW_MIN_SCALE, PREVIEW_MAX_SCALE);
    for (const scaler of scalers) scaler.style.transform = `scale(${scale})`;
  }

  /** Place the popup below the tab, clamped into view (flip above if needed). */
  private positionPreview(pop: HTMLElement, anchor: DOMRect, w: number, h: number): void {
    const left = Math.max(8, Math.min(Math.round(anchor.left), window.innerWidth - w - 8));
    let top = Math.round(anchor.bottom + 4);
    if (top + h > window.innerHeight - 8) top = Math.max(8, Math.round(anchor.top - h - 4));
    pop.style.left = `${left}px`;
    pop.style.top = `${top}px`;
  }

  private closePreview(): void {
    // `disable()` clears the timer through the gate's own disarm, so the
    // handle is cleared on every close path exactly as it was before.
    this.previewGate.disable();
    this.previewPaint = null;
    this.preview?.remove();
    this.preview = null;
    this.previewWsId = null;
  }

  private anchorPopover(pop: HTMLElement, r: DOMRect): void {
    document.body.appendChild(pop);
    pop.style.left = `${Math.round(r.left)}px`;
    pop.style.top = `${Math.round(r.bottom + 4)}px`;
  }

  /** Dismiss a popover on the next outside mousedown / Escape. */
  private dismissOnOutside(pop: HTMLElement, close: () => void): void {
    const onDoc = (e: Event) => {
      if (e instanceof KeyboardEvent && e.key !== "Escape") return;
      if (e instanceof MouseEvent && pop.contains(e.target as Node)) return;
      close();
    };
    setTimeout(() => {
      document.addEventListener("mousedown", onDoc, { once: true });
      document.addEventListener("keydown", onDoc, { once: true });
    }, 0);
  }
}

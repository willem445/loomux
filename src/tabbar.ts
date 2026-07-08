// The project-tab strip (#63). Phases 1–2: switch, close, new (+), double-click
// rename, color swatch. Phase 3–4: an attention dot on a tab whose (possibly
// hidden) pane needs the human, a live status chip (agent count + cost), and a
// hover thumbnail of the tab's viewport. Phase 5: a right-click context menu
// that pauses/resumes the tab's orchestration group.
//
// Pure tab state lives in TabManager (tabs.ts); this module renders it and turns
// interactions into TabManager / backend calls, re-rendering on onChange.

import type { TabManager, ManagedWorkspace } from "./tabs";
import { makeRenameCommit } from "./panerename";
import { swapEditor } from "./domutil";
import { attentionPresentation } from "./attention";
import { pauseGroup, resumeGroup, groupSummary, groupUsage } from "./orchestration";

// Reuse the orchestration group palette (orchbadge.ts GROUP_COLORS) so a tab's
// color vocabulary matches the group-accent colors the panes already use.
const TAB_COLORS = ["#7aa2f7", "#9ece6a", "#e0af68", "#bb9af7", "#7dcfff", "#f7768e"];

/** Live per-tab status pulled from the backend for a bound group. */
interface TabStatus {
  agents: number;
  cost: number | null;
  paused: boolean;
}

/** Strip ANSI/CSI escapes so a serialized viewport renders as plain text in the
 *  hover thumbnail (a real mini-terminal would be far heavier for a prototype). */
function stripAnsi(s: string): string {
  return (
    s
      // CSI sequences (colors, cursor moves): ESC [ … final byte.
      .replace(/\x1b\[[0-9;?]*[ -/]*[@-~]/g, "")
      // OSC sequences (window title, etc.): ESC ] … BEL or ST.
      .replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, "")
      // Any other lone two-char escape.
      .replace(/\x1b[@-Z\\-_]/g, "")
  );
}

export class TabBar<T extends ManagedWorkspace = ManagedWorkspace> {
  /** The id currently being renamed, so a re-render doesn't clobber the open
   *  input mid-edit. */
  private renamingId: string | null = null;
  private palette: HTMLElement | null = null;
  private menu: HTMLElement | null = null;
  private preview: HTMLElement | null = null;
  private previewTimer: number | null = null;
  private status = new Map<string, TabStatus>();

  private el: HTMLElement;
  private tabs: TabManager<T>;
  /** Opens a new tab WITH its starting pane (finding 3). Falls back to a bare
   *  TabManager.newTab (blank) only if the host didn't wire one. */
  private onNewTab: () => void;

  constructor(el: HTMLElement, tabs: TabManager<T>, onNewTab?: () => void) {
    this.el = el;
    this.tabs = tabs;
    this.onNewTab = onNewTab ?? (() => void tabs.newTab());
    tabs.onChange(() => this.render());
    // Poll bound groups for agent count / cost / paused (phase 4). Cheap; the
    // strip only re-renders when a value actually differs.
    window.setInterval(() => void this.pollStatus(), 4000);
    this.render();
  }

  private render(): void {
    // Don't stomp an in-flight rename input.
    if (this.renamingId) return;
    this.el.replaceChildren();

    for (const ws of this.tabs.tabs) {
      const active = ws.id === this.tabs.activeTabId;
      const attn = this.tabs.attentionFor(ws.id);
      const st = this.status.get(ws.id);

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

      // Unmistakable alert (#63 round 2, finding 1): a pane in this tab is
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

      // Live status chip: agent count + cost, when the tab owns a group.
      if (st) {
        const status = document.createElement("span");
        status.className = "tab-status";
        const cost = st.cost != null ? ` · $${st.cost.toFixed(2)}` : "";
        status.textContent = `✦${st.agents}${cost}`;
        status.title = `${st.agents} live agent(s)${cost ? `, ${cost.slice(3)} so far` : ""}`;
        tab.appendChild(status);
        if (st.paused) {
          const pause = document.createElement("span");
          pause.className = "tab-paused";
          pause.textContent = "⏸";
          pause.title = "Project paused — prompts/kickoffs held";
          tab.appendChild(pause);
        }
      }

      const close = document.createElement("button");
      close.className = "tab-close";
      close.textContent = "✕";
      close.title = "Close tab (Ctrl+Shift+K)";
      close.hidden = this.tabs.count <= 1; // never zero tabs
      close.addEventListener("click", (e) => {
        e.stopPropagation();
        this.tabs.closeTab(ws.id);
      });
      tab.appendChild(close);

      tab.addEventListener("click", () => this.tabs.switchTo(ws.id));
      tab.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        this.openMenu(ws, e.clientX, e.clientY);
      });
      // Hover thumbnail of the tab's viewport (phase 4) — only useful for a
      // background tab (the active one is right there).
      if (!active) {
        tab.addEventListener("mouseenter", () => this.openPreview(ws, tab));
        tab.addEventListener("mouseleave", () => this.closePreview());
      }
      this.el.appendChild(tab);
    }

    const add = document.createElement("button");
    add.className = "tab-add";
    add.textContent = "+";
    add.title = "New tab (Ctrl+Shift+T)";
    add.addEventListener("click", () => this.onNewTab());
    this.el.appendChild(add);
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
   *  (phase 5), plus rename/close. Pause is only offered when the tab owns a
   *  group. */
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
        this.tabs.closeTab(ws.id);
        this.closeMenu();
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

  /** Live hover thumbnail (#63 finding 2): the tab's FULL current viewport,
   *  re-serialized on a short interval so a running prompt streams in. It's a
   *  serialized text SNAPSHOT of the in-memory buffer — never a laid-out pane —
   *  so it can't re-arm a hidden pane's fit / PTY resize. The whole viewport is
   *  rendered and CSS-scaled to fit, so nothing is clipped. */
  private openPreview(ws: ManagedWorkspace, anchor: HTMLElement): void {
    this.closePreview();
    const pop = document.createElement("div");
    pop.className = "tab-preview";
    const scaler = document.createElement("div");
    scaler.className = "tab-preview-scaler";
    const pre = document.createElement("pre");
    scaler.appendChild(pre);
    pop.appendChild(scaler);
    document.body.appendChild(pop);
    this.preview = pop;

    const anchorRect = anchor.getBoundingClientRect();
    const paint = () => {
      // Trim only trailing blank rows (an agent screen is mostly empty below its
      // prompt) — keep the full viewport above intact.
      const text = stripAnsi(ws.livePreview()).replace(/[ \t]+$/gm, "").replace(/\n+$/, "");
      pre.textContent = text || "(no output yet)";
      this.layoutPreview(pop, scaler, pre, anchorRect);
    };
    paint();
    // Re-serialize while hovered → effectively live.
    this.previewTimer = window.setInterval(paint, 700);
  }

  /** Size the popup to the full viewport scaled to fit the screen, and clamp it
   *  into view (flip above the tab if it would run off the bottom). */
  private layoutPreview(pop: HTMLElement, scaler: HTMLElement, pre: HTMLElement, anchor: DOMRect): void {
    scaler.style.transform = "none";
    const naturalW = Math.max(1, pre.scrollWidth);
    const naturalH = Math.max(1, pre.scrollHeight);
    const maxW = Math.min(window.innerWidth * 0.9, 760);
    const maxH = window.innerHeight * 0.72;
    const scale = Math.min(1, maxW / naturalW, maxH / naturalH);
    scaler.style.transform = `scale(${scale})`;
    // +padding to match .tab-preview-scaler's 8px/6px inset so nothing clips.
    const w = Math.ceil(naturalW * scale) + 16;
    const h = Math.ceil(naturalH * scale) + 12;
    pop.style.width = `${w}px`;
    pop.style.height = `${h}px`;
    const left = Math.max(8, Math.min(Math.round(anchor.left), window.innerWidth - w - 8));
    let top = Math.round(anchor.bottom + 4);
    if (top + h > window.innerHeight - 8) top = Math.max(8, Math.round(anchor.top - h - 4));
    pop.style.left = `${left}px`;
    pop.style.top = `${top}px`;
  }

  private closePreview(): void {
    if (this.previewTimer !== null) {
      clearInterval(this.previewTimer);
      this.previewTimer = null;
    }
    this.preview?.remove();
    this.preview = null;
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

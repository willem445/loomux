// The project-tab strip (#63, phases 1–2): one chip per tab, an active marker,
// close ✕, a "+" new-tab button, double-click rename (parity with pane rename,
// pane.ts:1093), and a color swatch that opens a small palette popover.
//
// Pure state lives in TabManager (tabs.ts); this module only renders it and
// turns clicks into TabManager calls, then re-renders on its onChange.

import type { TabManager, ManagedWorkspace } from "./tabs";
import { makeRenameCommit } from "./panerename";
import { swapEditor } from "./domutil";

// Reuse the orchestration group palette (orchbadge.ts GROUP_COLORS) so a tab's
// color vocabulary matches the group-accent colors the panes already use.
const TAB_COLORS = ["#7aa2f7", "#9ece6a", "#e0af68", "#bb9af7", "#7dcfff", "#f7768e"];

export class TabBar<T extends ManagedWorkspace = ManagedWorkspace> {
  /** The id currently being renamed, so a re-render doesn't clobber the open
   *  input mid-edit. */
  private renamingId: string | null = null;
  private palette: HTMLElement | null = null;

  constructor(
    private el: HTMLElement,
    private tabs: TabManager<T>
  ) {
    tabs.onChange(() => this.render());
    this.render();
  }

  private render(): void {
    // Don't stomp an in-flight rename input.
    if (this.renamingId) return;
    this.el.replaceChildren();

    for (const ws of this.tabs.tabs) {
      const active = ws.id === this.tabs.activeTabId;
      const tab = document.createElement("div");
      tab.className = "tab" + (active ? " active" : "");
      tab.dataset.wsId = ws.id;
      if (ws.color) tab.style.setProperty("--tab-color", ws.color);
      tab.classList.toggle("colored", !!ws.color);

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

      const close = document.createElement("button");
      close.className = "tab-close";
      close.textContent = "✕";
      close.title = "Close tab (Ctrl+Shift+K)";
      // Only offer close when it's allowed — never zero tabs.
      close.hidden = this.tabs.count <= 1;
      close.addEventListener("click", (e) => {
        e.stopPropagation();
        this.tabs.closeTab(ws.id);
      });

      tab.append(swatch, name, close);
      tab.addEventListener("click", () => this.tabs.switchTo(ws.id));
      this.el.appendChild(tab);
    }

    const add = document.createElement("button");
    add.className = "tab-add";
    add.textContent = "+";
    add.title = "New tab (Ctrl+Shift+T)";
    add.addEventListener("click", () => this.tabs.newTab());
    this.el.appendChild(add);
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

    // Custom color via the native picker.
    const custom = document.createElement("input");
    custom.type = "color";
    custom.className = "tab-palette-custom";
    custom.title = "Custom color";
    custom.addEventListener("input", () => this.tabs.setColor(wsId, custom.value));
    pop.appendChild(custom);

    // Clear back to the default (no accent).
    const clear = document.createElement("button");
    clear.className = "tab-palette-clear";
    clear.textContent = "default";
    clear.addEventListener("click", () => {
      this.tabs.setColor(wsId, null);
      this.closePalette();
    });
    pop.appendChild(clear);

    document.body.appendChild(pop);
    const r = anchor.getBoundingClientRect();
    pop.style.left = `${Math.round(r.left)}px`;
    pop.style.top = `${Math.round(r.bottom + 4)}px`;
    this.palette = pop;

    // Dismiss on the next outside click / Escape.
    const onDoc = (e: Event) => {
      if (e instanceof KeyboardEvent && e.key !== "Escape") return;
      if (e instanceof MouseEvent && pop.contains(e.target as Node)) return;
      this.closePalette();
    };
    // Defer so the opening click doesn't immediately close it.
    setTimeout(() => {
      document.addEventListener("mousedown", onDoc, { once: true });
      document.addEventListener("keydown", onDoc, { once: true });
    }, 0);
  }

  private closePalette(): void {
    this.palette?.remove();
    this.palette = null;
  }
}

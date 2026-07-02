// A single terminal pane: xterm.js instance wired to a backend PTY,
// with a slim header for naming, splitting, and closing.

import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { spawnPty, writePty, resizePty, killPty, onPtyOutput } from "./pty";
import { isAppShortcut } from "./shortcuts";

export interface PaneOptions {
  name?: string;
  cwd?: string;
  command?: string;
}

const TERM_THEME = {
  background: "#0e0e12",
  foreground: "#c9d1e3",
  cursor: "#7aa2f7",
  cursorAccent: "#0e0e12",
  selectionBackground: "#2d3450",
  black: "#15161e",
  red: "#f7768e",
  green: "#9ece6a",
  yellow: "#e0af68",
  blue: "#7aa2f7",
  magenta: "#bb9af7",
  cyan: "#7dcfff",
  white: "#a9b1d6",
  brightBlack: "#414868",
  brightRed: "#ff899d",
  brightGreen: "#b4e878",
  brightYellow: "#faba4a",
  brightBlue: "#8db0ff",
  brightMagenta: "#c7a9ff",
  brightCyan: "#a4daff",
  brightWhite: "#c0caf5",
};

export interface PaneEvents {
  onFocus: (pane: Pane) => void;
  onCloseRequest: (pane: Pane) => void;
  onSplit: (pane: Pane, dir: "row" | "column") => void;
}

export class Pane {
  readonly el: HTMLElement;
  readonly term: Terminal;
  ptyId: number | null = null;
  name = "shell";

  private titleEl: HTMLElement;
  private termEl: HTMLElement;
  private fit = new FitAddon();
  private unlisten: (() => void) | null = null;
  private resizeObs: ResizeObserver;
  private disposed = false;

  constructor(private events: PaneEvents) {
    this.el = document.createElement("div");
    this.el.className = "pane";

    const header = document.createElement("div");
    header.className = "pane-header";
    this.titleEl = document.createElement("span");
    this.titleEl.className = "pane-title";
    this.titleEl.title = "Double-click to rename (F2)";
    this.titleEl.addEventListener("dblclick", () => this.startRename());
    header.appendChild(this.titleEl);

    for (const [glyph, cls, tip, fn] of [
      ["◫", "", "Split right", () => this.events.onSplit(this, "row")],
      ["⬓", "", "Split down", () => this.events.onSplit(this, "column")],
      ["✕", "close", "Close pane", () => this.events.onCloseRequest(this)],
    ] as const) {
      const btn = document.createElement("button");
      btn.className = `pane-btn ${cls}`;
      btn.textContent = glyph;
      btn.title = tip;
      btn.addEventListener("click", (e) => {
        e.stopPropagation();
        fn();
      });
      header.appendChild(btn);
    }
    this.el.appendChild(header);

    this.termEl = document.createElement("div");
    this.termEl.className = "pane-term";
    this.el.appendChild(this.termEl);

    this.term = new Terminal({
      allowProposedApi: true,
      cursorBlink: true,
      fontFamily: '"Cascadia Code", "Cascadia Mono", Consolas, "Courier New", monospace',
      fontSize: 14,
      lineHeight: 1.1,
      scrollback: 10000,
      theme: TERM_THEME,
    });
    this.term.loadAddon(this.fit);
    this.term.loadAddon(new WebLinksAddon());
    this.term.loadAddon(new Unicode11Addon());
    this.term.unicode.activeVersion = "11";

    // Let app-level shortcuts pass through xterm untouched; handle
    // clipboard combos here (Windows Terminal conventions).
    this.term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown") return true;
      if (isAppShortcut(e)) return false;
      if (e.ctrlKey && e.shiftKey && e.code === "KeyC") {
        const sel = this.term.getSelection();
        if (sel) navigator.clipboard.writeText(sel).catch(() => {});
        return false;
      }
      if (e.ctrlKey && e.shiftKey && e.code === "KeyV") {
        navigator.clipboard
          .readText()
          .then((t) => t && this.term.paste(t))
          .catch(() => {});
        return false;
      }
      return true;
    });

    this.el.addEventListener("mousedown", () => this.events.onFocus(this));

    this.resizeObs = new ResizeObserver(() => this.applyFit());
    this.setName("shell");
  }

  /** Open the terminal in the DOM and spawn its PTY. Call after `el` is attached. */
  async start(opts: PaneOptions = {}): Promise<void> {
    this.setName(opts.name ?? "shell");
    this.term.open(this.termEl);
    this.term.textarea?.addEventListener("focus", () => this.events.onFocus(this));
    this.tryWebgl();
    this.fit.fit();

    const ptyId = await spawnPty({
      cols: this.term.cols,
      rows: this.term.rows,
      cwd: opts.cwd,
      command: opts.command,
    });
    if (this.disposed) {
      killPty(ptyId).catch(() => {});
      return;
    }
    this.ptyId = ptyId;

    this.unlisten = await onPtyOutput(ptyId, (bytes) => this.term.write(bytes));
    this.term.onData((data) => {
      if (this.ptyId !== null) writePty(this.ptyId, data).catch(() => {});
    });
    this.resizeObs.observe(this.termEl);
    this.focus();
  }

  private tryWebgl(): void {
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose()); // falls back to DOM renderer
      this.term.loadAddon(webgl);
    } catch {
      // WebGL unavailable — xterm's DOM renderer still works fine.
    }
  }

  private fitTimer: number | undefined;
  private applyFit(): void {
    // Debounce: divider drags fire many resize events per frame.
    clearTimeout(this.fitTimer);
    this.fitTimer = window.setTimeout(() => {
      if (this.disposed || !this.termEl.isConnected) return;
      this.fit.fit();
      if (this.ptyId !== null) {
        resizePty(this.ptyId, this.term.cols, this.term.rows).catch(() => {});
      }
    }, 16);
  }

  setName(name: string): void {
    this.name = name;
    this.titleEl.textContent = name;
  }

  startRename(): void {
    const input = document.createElement("input");
    input.className = "pane-title-input";
    input.value = this.name;
    this.titleEl.replaceWith(input);
    input.focus();
    input.select();
    const commit = (save: boolean) => {
      if (save && input.value.trim()) this.name = input.value.trim();
      input.replaceWith(this.titleEl);
      this.titleEl.textContent = this.name;
      this.focus();
    };
    input.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Enter") commit(true);
      if (e.key === "Escape") commit(false);
    });
    input.addEventListener("blur", () => commit(true));
  }

  setActive(active: boolean): void {
    this.el.classList.toggle("active", active);
  }

  focus(): void {
    this.term.focus();
  }

  /** Tear down DOM + terminal. Kills the PTY unless it already exited. */
  dispose(killBackend = true): void {
    if (this.disposed) return;
    this.disposed = true;
    this.resizeObs.disconnect();
    clearTimeout(this.fitTimer);
    this.unlisten?.();
    if (killBackend && this.ptyId !== null) killPty(this.ptyId).catch(() => {});
    this.term.dispose();
    this.el.remove();
  }
}

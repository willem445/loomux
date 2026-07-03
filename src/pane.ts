// A single terminal pane: xterm.js instance wired to a backend PTY,
// with a slim header for naming, splitting, and closing.

import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { open } from "@tauri-apps/plugin-dialog";
import {
  spawnPty,
  writePty,
  resizePty,
  killPty,
  dirInfo,
  changeDir,
  ensureOutputRouter,
  attachOutput,
  detachOutput,
} from "./pty";
import { isAppShortcut } from "./shortcuts";
import { GitView } from "./gitview";

// Inline icons so the toolbar renders identically regardless of installed
// fonts; they inherit color via `currentColor`.
const FOLDER_ICON = `<svg viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"><path d="M1.9 4.3c0-.6.5-1.1 1.1-1.1h3l1.4 1.5h5.6c.6 0 1.1.5 1.1 1.1v5.4c0 .6-.5 1.1-1.1 1.1H3c-.6 0-1.1-.5-1.1-1.1z"/></svg>`;
const BRANCH_ICON = `<svg viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><circle cx="4.5" cy="3.6" r="1.7"/><circle cx="4.5" cy="12.4" r="1.7"/><circle cx="11.5" cy="5.4" r="1.7"/><path d="M4.5 5.3v5.4M11.5 7.1c0 2.4-1.9 3.1-4 3.6"/></svg>`;
const GIT_ICON = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"><circle cx="8" cy="2.8" r="1.6"/><circle cx="4" cy="13.2" r="1.6"/><circle cx="12" cy="13.2" r="1.6"/><path d="M8 4.4v2.2M8 6.6c0 2.6-4 2.4-4 5M8 6.6c0 2.6 4 2.4 4 5"/></svg>`;

/** Extract a filesystem path from an OSC 7 payload, which may be a raw path
 *  or a `file://host/path` URL. Returns "" if nothing usable. */
function normalizeOscPath(payload: string): string {
  const raw = payload.trim();
  if (!raw.startsWith("file://")) return raw;
  try {
    // Strip scheme + host, then percent-decode. On Windows a URL path looks
    // like `/C:/Users/...`; drop the leading slash before a drive letter.
    let p = decodeURIComponent(new URL(raw).pathname);
    if (/^\/[A-Za-z]:/.test(p)) p = p.slice(1);
    return p;
  } catch {
    return "";
  }
}

/** Trim a path to its last two segments for a compact toolbar label. */
function shortCwd(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  if (parts.length <= 2) return path;
  return "…/" + parts.slice(-2).join("/");
}

/** A hidden-by-default toolbar chip: an icon plus a text span. */
function makeMetaItem(cls: string, icon: string): [HTMLElement, HTMLElement] {
  const wrap = document.createElement("span");
  wrap.className = `pane-meta-item ${cls}`;
  wrap.hidden = true;
  const iconEl = document.createElement("span");
  iconEl.className = "pane-meta-icon";
  iconEl.innerHTML = icon;
  const text = document.createElement("span");
  text.className = "pane-meta-text";
  wrap.append(iconEl, text);
  return [wrap, text];
}

export interface PaneOptions {
  name?: string;
  cwd?: string;
  command?: string;
}

const TERM_THEME = {
  background: "#0b0b10",
  foreground: "#c9d1e3",
  cursor: "#7aa2f7",
  cursorAccent: "#0b0b10",
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
  private cwdEl: HTMLElement;
  private cwdTextEl: HTMLElement;
  private branchEl: HTMLElement;
  private branchTextEl: HTMLElement;
  /** Latest un-abbreviated directory the shell reported, for the picker. */
  private cwdRaw: string | null = null;
  /** Lazily created git view; null until the first toggle. */
  private gitView: GitView | null = null;
  private gitDivider: HTMLElement | null = null;
  private fit = new FitAddon();
  private resizeObs: ResizeObserver;
  private disposed = false;
  /** Keystrokes typed before the PTY is ready; flushed once it is. */
  private inputQueue: string[] = [];

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

    // Live metadata: current folder + git branch, reported by the shell.
    // The folder chip picks a folder to cd into; the branch chip opens the
    // git view.
    const meta = document.createElement("div");
    meta.className = "pane-meta";
    [this.cwdEl, this.cwdTextEl] = makeMetaItem("pane-cwd", FOLDER_ICON);
    [this.branchEl, this.branchTextEl] = makeMetaItem("pane-branch", BRANCH_ICON);
    this.cwdEl.setAttribute("role", "button");
    this.cwdEl.tabIndex = 0;
    this.cwdEl.addEventListener("click", (e) => {
      e.stopPropagation();
      void this.pickFolder();
    });
    this.branchEl.setAttribute("role", "button");
    this.branchEl.tabIndex = 0;
    this.branchEl.addEventListener("click", (e) => {
      e.stopPropagation();
      this.toggleGitView();
    });
    meta.append(this.cwdEl, this.branchEl);
    header.appendChild(meta);

    const gitBtn = document.createElement("button");
    gitBtn.className = "pane-btn";
    gitBtn.innerHTML = GIT_ICON;
    gitBtn.title = "Git view (Alt+G)";
    gitBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.toggleGitView();
    });
    header.appendChild(gitBtn);

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
      cursorStyle: "bar",
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

    // Shell integration: the shell emits OSC 7 with its working directory on
    // every prompt (see PWSH_CWD_HOOK / PROMPT_COMMAND in the backend). The
    // payload is the raw path; consume it and refresh the toolbar.
    this.term.parser.registerOscHandler(7, (payload) => {
      this.onCwdReported(payload);
      return true;
    });

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
    // Seed the toolbar from the startup directory. Interactive shells refine
    // this via OSC 7; command panes (agents) keep this initial value since
    // they have no prompt to report from.
    if (opts.cwd) {
      this.cwdRaw = opts.cwd;
      void this.refreshDir(opts.cwd);
    }
    this.term.open(this.termEl);
    this.term.textarea?.addEventListener("focus", () => this.events.onFocus(this));
    this.tryWebgl();
    this.fit.fit();

    // Everything is wired before the process exists: input queues until
    // the PTY is ready, and the output router buffers until we attach.
    this.term.onData((data) => {
      if (this.ptyId !== null) writePty(this.ptyId, data).catch(() => {});
      else this.inputQueue.push(data);
    });
    this.resizeObs.observe(this.termEl);
    this.focus();

    try {
      await ensureOutputRouter();
      const ptyId = await spawnPty({
        cols: Number.isFinite(this.term.cols) && this.term.cols > 1 ? this.term.cols : 80,
        rows: Number.isFinite(this.term.rows) && this.term.rows > 1 ? this.term.rows : 24,
        cwd: opts.cwd,
        command: opts.command,
      });
      if (this.disposed) {
        killPty(ptyId).catch(() => {});
        return;
      }
      this.ptyId = ptyId;
      attachOutput(ptyId, (bytes) => this.term.write(bytes));
      for (const data of this.inputQueue.splice(0)) {
        writePty(ptyId, data).catch(() => {});
      }
    } catch (err) {
      // Never leave a dead black pane: surface the failure in-terminal.
      this.term.writeln(`\x1b[91mloomux: failed to start shell\x1b[0m`);
      this.term.writeln(`\x1b[90m${String(err)}\x1b[0m`);
    }
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
      if (this.termEl.clientWidth === 0) return; // hidden behind git view
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

  /** Handle an OSC 7 working-directory report from the shell. Payloads are
   *  usually a raw path, but tolerate a `file://host/path` URL too. */
  private onCwdReported(payload: string): void {
    // Every prompt is a "something may have happened" signal for the git
    // view, even when the directory itself didn't change.
    this.gitView?.notifyPrompt();
    const path = normalizeOscPath(payload);
    if (!path) return;
    this.cwdRaw = path;
    // Refresh even when the path is unchanged: the *branch* can change
    // without a cd (git checkout), and dir_info is cheap.
    void this.refreshDir(path);
  }

  /** Toggle the git view. It stacks ABOVE the terminal — the shell stays
   *  visible and usable, with a draggable divider between the two. */
  toggleGitView(): void {
    if (!this.gitView) {
      this.gitView = new GitView({
        getCwd: () => this.cwdRaw,
        onClose: () => this.toggleGitView(),
        onRepoAction: () => {
          if (this.cwdRaw) void this.refreshDir(this.cwdRaw);
        },
      });
      this.gitDivider = this.makeGitDivider();
      // Order: header, git view, divider, terminal.
      this.termEl.before(this.gitView.el, this.gitDivider);
    }
    try {
      if (this.gitView.visible) {
        this.gitView.hide();
        this.gitDivider!.hidden = true;
        this.termEl.style.flex = "";
        this.termEl.style.height = "";
        this.focus();
      } else {
        // Terminal keeps a fixed share at the bottom; the git view takes the
        // rest. The ResizeObserver re-fits the terminal automatically.
        const share = Math.max(140, Math.round(this.el.clientHeight * 0.35));
        this.termEl.style.flex = "0 0 auto";
        this.termEl.style.height = `${share}px`;
        this.gitDivider!.hidden = false;
        this.gitView.show();
      }
    } catch (err) {
      // Never leave the pane half-toggled: give the terminal back its
      // space, then let the error surface (global handler shows a banner).
      this.gitView?.hide();
      if (this.gitDivider) this.gitDivider.hidden = true;
      this.termEl.style.flex = "";
      this.termEl.style.height = "";
      throw err;
    }
  }

  /** Horizontal drag handle between the git view and the terminal. */
  private makeGitDivider(): HTMLElement {
    const div = document.createElement("div");
    div.className = "git-divider";
    div.hidden = true;
    div.addEventListener("mousedown", (e) => {
      e.preventDefault();
      const startY = e.clientY;
      const startH = this.termEl.offsetHeight;
      div.classList.add("dragging");
      const move = (ev: MouseEvent) => {
        const max = this.el.clientHeight - 160; // keep the git view usable
        const h = Math.max(100, Math.min(max, startH + (startY - ev.clientY)));
        this.termEl.style.height = `${h}px`;
      };
      const up = () => {
        div.classList.remove("dragging");
        window.removeEventListener("mousemove", move);
        window.removeEventListener("mouseup", up);
      };
      window.addEventListener("mousemove", move);
      window.addEventListener("mouseup", up);
    });
    return div;
  }

  private async refreshDir(path: string): Promise<void> {
    let info;
    try {
      info = await dirInfo(path);
    } catch {
      return;
    }
    if (this.disposed || this.cwdRaw !== path) return; // superseded
    this.setMeta(this.cwdEl, this.cwdTextEl, shortCwd(info.cwd), info.cwd);
    this.setMeta(this.branchEl, this.branchTextEl, info.branch, info.branch);
  }

  /** Open a native folder picker and cd the shell into the chosen directory. */
  private async pickFolder(): Promise<void> {
    if (this.ptyId === null) return;
    const picked = await open({
      directory: true,
      title: "Change folder",
      defaultPath: this.cwdRaw ?? undefined,
    });
    if (typeof picked === "string" && this.ptyId !== null) {
      await changeDir(this.ptyId, picked);
      this.focus(); // return focus to the terminal after the dialog
    }
  }

  private setMeta(
    wrap: HTMLElement,
    text: HTMLElement,
    label: string | null | undefined,
    tip: string | null
  ): void {
    if (label) {
      text.textContent = label;
      wrap.title = tip ?? label;
      wrap.hidden = false;
    } else {
      wrap.hidden = true;
    }
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
    this.gitView?.dispose();
    if (this.ptyId !== null) {
      detachOutput(this.ptyId);
      if (killBackend) killPty(this.ptyId).catch(() => {});
    }
    this.term.dispose();
    this.el.remove();
  }
}

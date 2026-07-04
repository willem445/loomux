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
  ptyBackendInfo,
} from "./pty";
import { invoke } from "@tauri-apps/api/core";
import { isAppShortcut } from "./shortcuts";
import { openInEditor, editorConfigDialog } from "./editor";
import { GitView } from "./gitview";
import { TasksView } from "./tasksview";
import { AuditView } from "./auditview";
import { GroupView } from "./groupview";

// Inline icons so the toolbar renders identically regardless of installed
// fonts; they inherit color via `currentColor`.
const FOLDER_ICON = `<svg viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"><path d="M1.9 4.3c0-.6.5-1.1 1.1-1.1h3l1.4 1.5h5.6c.6 0 1.1.5 1.1 1.1v5.4c0 .6-.5 1.1-1.1 1.1H3c-.6 0-1.1-.5-1.1-1.1z"/></svg>`;
const BRANCH_ICON = `<svg viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><circle cx="4.5" cy="3.6" r="1.7"/><circle cx="4.5" cy="12.4" r="1.7"/><circle cx="11.5" cy="5.4" r="1.7"/><path d="M4.5 5.3v5.4M11.5 7.1c0 2.4-1.9 3.1-4 3.6"/></svg>`;
const TASKS_ICON = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"><path d="M5.5 4h8M5.5 8h8M5.5 12h8"/><circle cx="2.3" cy="4" r="0.9" fill="currentColor" stroke="none"/><circle cx="2.3" cy="8" r="0.9" fill="currentColor" stroke="none"/><circle cx="2.3" cy="12" r="0.9" fill="currentColor" stroke="none"/></svg>`;
const GIT_ICON = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"><circle cx="8" cy="2.8" r="1.6"/><circle cx="4" cy="13.2" r="1.6"/><circle cx="12" cy="13.2" r="1.6"/><path d="M8 4.4v2.2M8 6.6c0 2.6-4 2.4-4 5M8 6.6c0 2.6 4 2.4 4 5"/></svg>`;
// Audit viewer: a clock/history glyph for the group's audit-log timeline.
const AUDIT_ICON = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M2.2 8a5.8 5.8 0 1 1 1.7 4.1"/><path d="M2.2 12.2V8.6H5.8"/><path d="M8 5.2V8l2 1.4"/></svg>`;
const GROUP_ICON = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="3.4" r="1.7"/><circle cx="3.4" cy="11" r="1.7"/><circle cx="12.6" cy="11" r="1.7"/><path d="M8 5.1v3M6.7 9.6 4.5 9.9M9.3 9.6l2.2.3"/></svg>`;
// "Open in editor": code-brackets glyph. Opens the pane's workspace folder in
// the user's configured external editor (VS Code, Zed, …).
const EDITOR_ICON = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M6 4.5 2.5 8 6 11.5M10 4.5 13.5 8 10 11.5"/></svg>`;

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

/** Role/group chip shown before the pane title (orchestration panes). */
export interface PaneBadge {
  /** Short uppercase label, e.g. "ORCH", "W", "REV". */
  label: string;
  /** Group accent color; also tints the pane header. */
  color: string;
  title?: string;
}

export interface PaneOptions {
  name?: string;
  cwd?: string;
  command?: string;
  badge?: PaneBadge;
  /** Orchestration group this pane belongs to (enables the task board). */
  orchGroup?: string;
  /** "orchestrator" | "worker" | "reviewer". */
  orchRole?: string;
  /** Agent id, for attention acks (clearing a "needs attention" badge). */
  orchAgent?: string;
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
  /** Park this pane in the dock (out of the grid, still running). */
  onMinimize: (pane: Pane) => void;
  /** Toggle this pane to/from fullscreen over the grid. */
  onMaximize: (pane: Pane) => void;
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
  /** Floating container for the git view + divider. It overlays the top of
   *  the terminal instead of shrinking it: resizing the PTY makes ConPTY and
   *  full-screen TUIs repaint from scratch, flooding scrollback with
   *  duplicate frames. */
  private gitOverlay: HTMLElement | null = null;
  /** Task board (orchestrator panes only), same overlay mechanics. */
  private tasksView: TasksView | null = null;
  private tasksOverlay: HTMLElement | null = null;
  private tasksBtn: HTMLButtonElement;
  /** Audit-log viewer (any orchestration pane), same overlay mechanics. */
  private auditView: AuditView | null = null;
  private auditOverlay: HTMLElement | null = null;
  private auditBtn: HTMLButtonElement;
  /** Group lifecycle panel (orchestrator panes only), same mechanics. */
  private groupView: GroupView | null = null;
  private groupOverlay: HTMLElement | null = null;
  private groupBtn: HTMLButtonElement;
  /** Fullscreen toggle; its glyph flips to a restore affordance when active. */
  private maximizeBtn: HTMLButtonElement;
  private orchGroup: string | null = null;
  private orchAgent: string | null = null;
  /** "needs attention" chip in the header (attention routing #6); hidden until
   *  the backend flags this pane. */
  private attnChip: HTMLButtonElement;
  private attentionReason: string | null = null;
  /** True for agent/command panes (vs plain shells). */
  private launchedCommand = false;
  private shiftTimer: number | undefined;
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

    // "Needs attention" chip: clicking it focuses the pane and acknowledges
    // the signal (clears a latched report backend-side). Hidden until flagged.
    this.attnChip = document.createElement("button");
    this.attnChip.className = "pane-attn";
    this.attnChip.hidden = true;
    this.attnChip.addEventListener("click", (e) => {
      e.stopPropagation();
      this.events.onFocus(this);
      this.focus();
      this.acknowledgeAttention();
    });
    header.appendChild(this.attnChip);

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

    this.tasksBtn = document.createElement("button");
    this.tasksBtn.className = "pane-btn";
    this.tasksBtn.innerHTML = TASKS_ICON;
    this.tasksBtn.title = "Task board (Alt+T)";
    this.tasksBtn.hidden = true; // shown for orchestrator panes in start()
    this.tasksBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.toggleTasksView();
    });
    header.appendChild(this.tasksBtn);

    this.auditBtn = document.createElement("button");
    this.auditBtn.className = "pane-btn";
    this.auditBtn.innerHTML = AUDIT_ICON;
    this.auditBtn.title = "Audit log (Alt+A)";
    this.auditBtn.hidden = true; // shown for orchestration panes in start()
    this.auditBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.toggleAuditView();
    });
    header.appendChild(this.auditBtn);

    this.groupBtn = document.createElement("button");
    this.groupBtn.className = "pane-btn";
    this.groupBtn.innerHTML = GROUP_ICON;
    this.groupBtn.title = "Group lifecycle (Alt+O)";
    this.groupBtn.hidden = true; // shown for orchestrator panes in start()
    this.groupBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.toggleGroupView();
    });
    header.appendChild(this.groupBtn);

    // Open the pane's workspace folder in the configured external editor.
    // Left-click opens (prompting for the editor on first use); right-click
    // reconfigures the editor command.
    const editorBtn = document.createElement("button");
    editorBtn.className = "pane-btn";
    editorBtn.innerHTML = EDITOR_ICON;
    editorBtn.title = "Open in editor (Alt+E) · right-click to configure";
    editorBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      void this.openInEditor();
    });
    editorBtn.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      void editorConfigDialog().then(() => this.focus());
    });
    header.appendChild(editorBtn);

    const gitBtn = document.createElement("button");
    gitBtn.className = "pane-btn";
    gitBtn.innerHTML = GIT_ICON;
    gitBtn.title = "Git view (Alt+G)";
    gitBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.toggleGitView();
    });
    header.appendChild(gitBtn);

    // Minimize / maximize live next to close: the same window-control cluster
    // users expect. Maximize keeps a stored ref so its glyph can flip to a
    // "restore" affordance while fullscreen.
    this.maximizeBtn = document.createElement("button");
    this.maximizeBtn.className = "pane-btn";
    this.maximizeBtn.textContent = "⤢";
    this.maximizeBtn.title = "Maximize (Ctrl+Shift+M)";
    this.maximizeBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.events.onMaximize(this);
    });

    for (const [glyph, cls, tip, fn] of [
      ["◫", "", "Split right", () => this.events.onSplit(this, "row")],
      ["⬓", "", "Split down", () => this.events.onSplit(this, "column")],
      ["—", "", "Minimize to dock (Alt+M)", () => this.events.onMinimize(this)],
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
    header.appendChild(this.maximizeBtn);

    const closeBtn = document.createElement("button");
    closeBtn.className = "pane-btn close";
    closeBtn.textContent = "✕";
    closeBtn.title = "Close pane";
    closeBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.events.onCloseRequest(this);
    });
    header.appendChild(closeBtn);
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

    this.el.addEventListener("mousedown", () => {
      this.events.onFocus(this);
      // Turning to a flagged pane acknowledges it (clears a latched report).
      this.acknowledgeAttention();
    });

    // Keep the cursor row visible under the git overlay as output arrives.
    this.term.onCursorMove(() => this.scheduleShift());

    this.resizeObs = new ResizeObserver(() => this.applyFit());
    this.setName("shell");
  }

  /** Open the terminal in the DOM and spawn its PTY. Call after `el` is attached. */
  async start(opts: PaneOptions = {}): Promise<void> {
    this.setName(opts.name ?? "shell");
    this.launchedCommand = !!opts.command?.trim();
    if (opts.badge) this.setBadge(opts.badge);
    if (opts.orchAgent) this.orchAgent = opts.orchAgent;
    if (opts.orchGroup) {
      this.orchGroup = opts.orchGroup;
      // The board lives on the orchestrator's pane; workers report there.
      this.tasksBtn.hidden = opts.orchRole !== "orchestrator";
      // The audit log is per-group and read-only, so it's useful from any
      // agent pane in the group, not just the orchestrator's.
      this.auditBtn.hidden = false;
      // Group lifecycle controls (pause / end orchestration) live on the
      // orchestrator's pane, alongside the task board.
      this.groupBtn.hidden = opts.orchRole !== "orchestrator";
    }
    // Seed the toolbar from the startup directory. Interactive shells refine
    // this via OSC 7; command panes (agents) keep this initial value since
    // they have no prompt to report from.
    if (opts.cwd) {
      this.cwdRaw = opts.cwd;
      void this.refreshDir(opts.cwd);
    }
    // Tell xterm which ConPTY it is talking to. This drives its resize
    // heuristics: against a modern conhost (sideloaded, honors the
    // resize-quirk flag and emits nothing on resize) xterm keeps its own
    // buffer reflow; against the inbox Win10 conhost (full repaint on every
    // resize) xterm disables reflow so the two don't fight and duplicate
    // content into scrollback.
    try {
      const backend = await ptyBackendInfo();
      if (backend.conpty_build > 0) {
        this.term.options.windowsPty = {
          backend: "conpty",
          buildNumber: backend.conpty_build,
        };
      }
    } catch {
      // Backend info is a tuning hint only — never block the terminal on it.
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
      const cols = Number.isFinite(this.term.cols) && this.term.cols > 1 ? this.term.cols : 80;
      const rows = Number.isFinite(this.term.rows) && this.term.rows > 1 ? this.term.rows : 24;
      const ptyId = await spawnPty({ cols, rows, cwd: opts.cwd, command: opts.command });
      if (this.disposed) {
        killPty(ptyId).catch(() => {});
        return;
      }
      this.ptyId = ptyId;
      this.sentSize = `${cols}x${rows}`;
      // Reconcile: if the pane was resized while the spawn was in flight,
      // the debounced fit will notice the size drifted and resend once.
      this.applyFit();
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
  /** Last grid size sent to the PTY, as `cols x rows`. Resizing ConPTY is
   *  never free (the inbox Win10 conhost repaints the whole screen, which
   *  TUIs then duplicate into scrollback), so same-size calls are skipped. */
  private sentSize = "";
  private applyFit(): void {
    // Debounce: divider drags fire many resize events per frame.
    clearTimeout(this.fitTimer);
    this.fitTimer = window.setTimeout(() => {
      if (this.disposed || !this.termEl.isConnected) return;
      if (this.termEl.clientWidth === 0) return; // not laid out yet
      this.fit.fit();
      const size = `${this.term.cols}x${this.term.rows}`;
      if (this.ptyId !== null && size !== this.sentSize) {
        this.sentSize = size;
        resizePty(this.ptyId, this.term.cols, this.term.rows).catch(() => {});
      }
      // The pane itself changed size: keep the overlay within bounds and
      // re-anchor the visible strip on the cursor.
      const overlay = this.activeOverlay();
      if (overlay) {
        overlay.style.height = `${this.overlayClamp(overlay.offsetHeight)}px`;
        this.updateTermShift();
      }
    }, 16);
  }

  setName(name: string): void {
    this.name = name;
    this.titleEl.textContent = name;
  }

  /** Mark this pane as part of an orchestration group: role chip before the
   *  title plus a group-colored accent on the header. */
  setBadge(badge: PaneBadge): void {
    const chip = document.createElement("span");
    chip.className = "pane-badge";
    chip.textContent = badge.label;
    if (badge.title) chip.title = badge.title;
    this.el.style.setProperty("--group-color", badge.color);
    this.el.classList.add("grouped");
    this.titleEl.before(chip);
  }

  /** Short label per attention reason (see the backend `AttentionItem`). */
  private static ATTN_LABEL: Record<string, string> = {
    blocked: "⚠ blocked",
    waiting: "⚠ waiting",
    report: "✓ reported",
    gate: "⚑ your call",
  };

  /** Flag (or clear) this pane as needing the human — driven by the backend
   *  attention scan. Idempotent: a same-reason repeat is a no-op, so the 3-second
   *  re-emits don't thrash the DOM. `null` clears the badge. */
  setAttention(reason: string | null, detail?: string): void {
    if (reason === this.attentionReason) return;
    this.attentionReason = reason;
    if (!reason) {
      this.attnChip.hidden = true;
      this.el.classList.remove("needs-attention");
      delete this.attnChip.dataset.reason;
      return;
    }
    this.attnChip.textContent = Pane.ATTN_LABEL[reason] ?? "⚠ attention";
    this.attnChip.title = detail ?? "This pane needs you";
    this.attnChip.dataset.reason = reason;
    this.attnChip.hidden = false;
    this.el.classList.add("needs-attention");
  }

  /** The human is now on this pane: clear a latched report backend-side so its
   *  badge drops. Live reasons (waiting/gate) are recomputed and reappear only
   *  if still true. */
  private acknowledgeAttention(): void {
    if (!this.attentionReason || !this.orchAgent) return;
    invoke("orch_ack_attention", { agentId: this.orchAgent }).catch(() => {});
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

  /** Toggle the git view. It FLOATS over the top of the terminal — the
   *  terminal keeps its full size and PTY dimensions, so toggling never
   *  triggers a resize repaint (which would push duplicate TUI frames into
   *  scrollback). The bottom strip of the terminal stays visible and usable,
   *  with a draggable divider on the overlay's lower edge. */
  toggleGitView(): void {
    if (!this.gitView) {
      this.gitView = new GitView({
        getCwd: () => this.cwdRaw,
        onClose: () => this.toggleGitView(),
        onRepoAction: () => {
          if (this.cwdRaw) void this.refreshDir(this.cwdRaw);
        },
      });
      this.gitDivider = this.makeOverlayDivider(() => this.gitOverlay!);
      this.gitOverlay = document.createElement("div");
      this.gitOverlay.className = "git-overlay";
      this.gitOverlay.hidden = true;
      this.gitOverlay.append(this.gitView.el, this.gitDivider);
      this.el.appendChild(this.gitOverlay);
    }
    try {
      if (this.gitView.visible) {
        this.gitView.hide();
        this.gitOverlay!.hidden = true;
        this.updateTermShift();
        this.focus();
      } else {
        if (this.tasksOverlay && !this.tasksOverlay.hidden) this.toggleTasksView();
        if (this.auditOverlay && !this.auditOverlay.hidden) this.toggleAuditView();
        if (this.groupOverlay && !this.groupOverlay.hidden) this.toggleGroupView();
        // Terminal keeps a fixed visible share at the bottom; the overlay
        // covers the rest.
        const strip = Math.max(140, Math.round(this.el.clientHeight * 0.35));
        this.gitOverlay!.style.height = `${this.overlayClamp(this.termEl.clientHeight - strip)}px`;
        this.gitOverlay!.hidden = false;
        this.gitView.show();
        this.updateTermShift();
      }
    } catch (err) {
      // Never leave the pane half-toggled: retract the overlay fully, then
      // let the error surface (global handler shows a banner).
      this.gitView?.hide();
      if (this.gitOverlay) this.gitOverlay.hidden = true;
      this.termEl.style.transform = "";
      throw err;
    }
  }

  /** Keep the overlay tall enough to be usable but always leave a terminal
   *  strip visible at the bottom. */
  private overlayClamp(h: number): number {
    const max = Math.max(160, this.termEl.clientHeight - 100);
    return Math.max(160, Math.min(max, h));
  }

  /** Horizontal drag handle on an overlay's bottom edge. */
  private makeOverlayDivider(overlay: () => HTMLElement): HTMLElement {
    const div = document.createElement("div");
    div.className = "git-divider";
    div.addEventListener("mousedown", (e) => {
      e.preventDefault();
      const startY = e.clientY;
      const startH = overlay().offsetHeight;
      div.classList.add("dragging");
      const move = (ev: MouseEvent) => {
        const h = this.overlayClamp(startH + (ev.clientY - startY));
        overlay().style.height = `${h}px`;
        this.updateTermShift();
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

  /** Toggle the task board overlay (orchestrator panes). Same no-resize
   *  overlay mechanics as the git view; only one overlay is open at a time. */
  toggleTasksView(): void {
    if (!this.orchGroup || this.tasksBtn.hidden) return;
    if (!this.tasksView) {
      this.tasksView = new TasksView(this.orchGroup, { onClose: () => this.toggleTasksView() });
      this.tasksOverlay = document.createElement("div");
      this.tasksOverlay.className = "git-overlay";
      this.tasksOverlay.hidden = true;
      this.tasksOverlay.append(this.tasksView.el, this.makeOverlayDivider(() => this.tasksOverlay!));
      this.el.appendChild(this.tasksOverlay);
    }
    if (!this.tasksOverlay!.hidden) {
      this.tasksOverlay!.hidden = true;
      this.updateTermShift();
      this.focus();
    } else {
      if (this.gitView?.visible) this.toggleGitView();
      if (this.auditOverlay && !this.auditOverlay.hidden) this.toggleAuditView();
      if (this.groupOverlay && !this.groupOverlay.hidden) this.toggleGroupView();
      const strip = Math.max(140, Math.round(this.el.clientHeight * 0.35));
      this.tasksOverlay!.style.height = `${this.overlayClamp(this.termEl.clientHeight - strip)}px`;
      this.tasksOverlay!.hidden = false;
      this.tasksView.show();
      this.updateTermShift();
    }
  }

  /** Toggle the audit-log viewer overlay (any orchestration pane). Same
   *  no-resize overlay mechanics as the git/task views; only one overlay is
   *  open at a time. */
  toggleAuditView(): void {
    if (!this.orchGroup || this.auditBtn.hidden) return;
    if (!this.auditView) {
      this.auditView = new AuditView(this.orchGroup, { onClose: () => this.toggleAuditView() });
      this.auditOverlay = document.createElement("div");
      this.auditOverlay.className = "git-overlay";
      this.auditOverlay.hidden = true;
      this.auditOverlay.append(this.auditView.el, this.makeOverlayDivider(() => this.auditOverlay!));
      this.el.appendChild(this.auditOverlay);
    }
    if (!this.auditOverlay!.hidden) {
      this.auditOverlay!.hidden = true;
      this.updateTermShift();
      this.focus();
    } else {
      if (this.gitView?.visible) this.toggleGitView();
      if (this.tasksOverlay && !this.tasksOverlay.hidden) this.toggleTasksView();
      if (this.groupOverlay && !this.groupOverlay.hidden) this.toggleGroupView();
      const strip = Math.max(140, Math.round(this.el.clientHeight * 0.35));
      this.auditOverlay!.style.height = `${this.overlayClamp(this.termEl.clientHeight - strip)}px`;
      this.auditOverlay!.hidden = false;
      this.auditView.show();
      this.updateTermShift();
    }
  }

  /** Toggle the group lifecycle panel overlay (orchestrator panes). Same
   *  no-resize overlay mechanics as the other views; only one is open. */
  toggleGroupView(): void {
    if (!this.orchGroup || this.groupBtn.hidden) return;
    if (!this.groupView) {
      this.groupView = new GroupView(this.orchGroup, { onClose: () => this.toggleGroupView() });
      this.groupOverlay = document.createElement("div");
      this.groupOverlay.className = "git-overlay";
      this.groupOverlay.hidden = true;
      this.groupOverlay.append(this.groupView.el, this.makeOverlayDivider(() => this.groupOverlay!));
      this.el.appendChild(this.groupOverlay);
    }
    if (!this.groupOverlay!.hidden) {
      this.groupOverlay!.hidden = true;
      this.updateTermShift();
      this.focus();
    } else {
      if (this.gitView?.visible) this.toggleGitView();
      if (this.tasksOverlay && !this.tasksOverlay.hidden) this.toggleTasksView();
      if (this.auditOverlay && !this.auditOverlay.hidden) this.toggleAuditView();
      const strip = Math.max(140, Math.round(this.el.clientHeight * 0.35));
      this.groupOverlay!.style.height = `${this.overlayClamp(this.termEl.clientHeight - strip)}px`;
      this.groupOverlay!.hidden = false;
      this.groupView.show();
      this.updateTermShift();
    }
  }

  /** Open this pane's workspace folder in the configured external editor.
   *  Prompts for the editor command on first use; errors surface as a toast.
   *  Uses the shell-reported cwd, falling back to the startup directory. */
  async openInEditor(): Promise<void> {
    await openInEditor(this.cwdRaw);
    this.focus(); // return focus to the terminal after any dialog
  }

  /** The orchestration group this pane belongs to, if any (for group-wide
   *  actions like end-orchestration closing every pane in the group). */
  get orchGroupId(): string | null {
    return this.orchGroup;
  }

  /** Whichever overlay (git / tasks / audit / group) is currently covering
   *  the terminal. */
  private activeOverlay(): HTMLElement | null {
    if (this.gitOverlay && !this.gitOverlay.hidden) return this.gitOverlay;
    if (this.tasksOverlay && !this.tasksOverlay.hidden) return this.tasksOverlay;
    if (this.auditOverlay && !this.auditOverlay.hidden) return this.auditOverlay;
    if (this.groupOverlay && !this.groupOverlay.hidden) return this.groupOverlay;
    return null;
  }

  /** Debounced cursor-follow for the overlay: TUIs sweep the cursor around
   *  while repainting, so settle before measuring. */
  private scheduleShift(): void {
    if (!this.activeOverlay()) return;
    clearTimeout(this.shiftTimer);
    this.shiftTimer = window.setTimeout(() => this.updateTermShift(), 80);
  }

  /** With the git overlay covering the top of the terminal, shift the
   *  terminal down (visually only — the grid/PTY size is untouched) just
   *  enough to keep the cursor's row inside the visible bottom strip.
   *  Full-screen TUIs write at the bottom and need no shift; a fresh shell
   *  writes at the top, which the overlay would otherwise hide. */
  private updateTermShift(): void {
    if (this.disposed) return;
    const overlay = this.activeOverlay();
    if (!overlay) {
      this.termEl.style.transform = "";
      return;
    }
    const screen = this.termEl.querySelector<HTMLElement>(".xterm-screen");
    const xtermEl = this.termEl.querySelector<HTMLElement>(".xterm");
    if (!screen || !xtermEl || !this.term.rows) return;
    const cell = screen.offsetHeight / this.term.rows;
    if (!cell) return;
    const covered = overlay.offsetHeight;
    const padTop = parseFloat(getComputedStyle(xtermEl).paddingTop) || 0;
    const cursorTop = padTop + this.term.buffer.active.cursorY * cell;
    // One extra row of context above the cursor when shifted.
    const shift = Math.max(0, Math.min(covered, Math.round(covered - cursorTop + cell)));
    this.termEl.style.transform = shift > 0 ? `translateY(${shift}px)` : "";
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

  /** Command panes that die unexpectedly stay open so the human can read
   *  the error (a crashing CLI's output would otherwise vanish with the
   *  pane). Clean exits and loomux-initiated kills close as usual. */
  keepOpenOnExit(exit: { exit_code: number | null; expected: boolean }): boolean {
    return this.launchedCommand && !exit.expected && exit.exit_code !== 0;
  }

  /** Announce a kept-open pane's exit inside its terminal. */
  notifyExited(code: number | null): void {
    const codeTxt = code === null ? "" : ` (code ${code})`;
    this.term.writeln(
      `
[91mprocess exited${codeTxt}[0m [90m— pane kept open so you can read the output; close it with Ctrl+Shift+W[0m`
    );
    this.setName(`${this.name} · exited`);
  }

  setActive(active: boolean): void {
    this.el.classList.toggle("active", active);
  }

  /** Reflect fullscreen state: the `.maximized` class drives the CSS overlay
   *  (no PTY resize is forced — the pane genuinely changes size, so its own
   *  ResizeObserver issues at most one debounced fit) and the button glyph
   *  flips between maximize and restore. */
  setMaximized(on: boolean): void {
    this.el.classList.toggle("maximized", on);
    this.maximizeBtn.textContent = on ? "⤡" : "⤢";
    this.maximizeBtn.title = on ? "Restore (Ctrl+Shift+M)" : "Maximize (Ctrl+Shift+M)";
  }

  /** Group accent color, if this pane carries an orchestration badge — used to
   *  tint its chip in the minimize dock. */
  get accentColor(): string | null {
    return this.el.style.getPropertyValue("--group-color").trim() || null;
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
    clearTimeout(this.shiftTimer);
    this.gitView?.dispose();
    this.tasksView?.dispose();
    this.auditView?.dispose();
    this.groupView?.dispose();
    if (this.ptyId !== null) {
      detachOutput(this.ptyId);
      if (killBackend) killPty(this.ptyId).catch(() => {});
    }
    this.term.dispose();
    this.el.remove();
  }
}

// A single terminal pane: xterm.js instance wired to a backend PTY,
// with a slim header for naming, splitting, and closing.

import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { SerializeAddon } from "@xterm/addon-serialize";
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
  attachGitWatch,
  setGitWatch,
  detachGitWatch,
  ptyBackendInfo,
} from "./pty";
import { voiceController, type VoiceTargetPane, type VoicePhase } from "./voicecontrol";
import { invoke } from "@tauri-apps/api/core";
import { parseOsc52, writeClipboard } from "./clipboard";
import {
  checkAttachment,
  attachRejectMessage,
  composeSteerText,
  bytesToBase64,
  steerKeyAction,
  steerBoxHeight,
} from "./steer";
import { createOrderedWriter } from "./ptywrite";
import { showToast } from "./toast";
import { isAppShortcut } from "./shortcuts";
import { attentionPresentation } from "./attention";
import { makeRenameCommit } from "./panerename";
import { shouldResizePty } from "./panefit";
import { swapEditor } from "./domutil";
import { openInEditor, editorConfigDialog } from "./editor";
import { GitView } from "./gitview";
import { IssuesView } from "./issuesview";
import { TasksView } from "./tasksview";
import { AuditView } from "./auditview";
import { GroupView } from "./groupview";
import { clampOverlayHeight, OVERLAY_MIN_H } from "./overlaysize";
import { FileEditView } from "./fileedit";

// Inline icons so the toolbar renders identically regardless of installed
// fonts; they inherit color via `currentColor`.
const FOLDER_ICON = `<svg viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"><path d="M1.9 4.3c0-.6.5-1.1 1.1-1.1h3l1.4 1.5h5.6c.6 0 1.1.5 1.1 1.1v5.4c0 .6-.5 1.1-1.1 1.1H3c-.6 0-1.1-.5-1.1-1.1z"/></svg>`;
const BRANCH_ICON = `<svg viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><circle cx="4.5" cy="3.6" r="1.7"/><circle cx="4.5" cy="12.4" r="1.7"/><circle cx="11.5" cy="5.4" r="1.7"/><path d="M4.5 5.3v5.4M11.5 7.1c0 2.4-1.9 3.1-4 3.6"/></svg>`;
const TASKS_ICON = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"><path d="M5.5 4h8M5.5 8h8M5.5 12h8"/><circle cx="2.3" cy="4" r="0.9" fill="currentColor" stroke="none"/><circle cx="2.3" cy="8" r="0.9" fill="currentColor" stroke="none"/><circle cx="2.3" cy="12" r="0.9" fill="currentColor" stroke="none"/></svg>`;
const GIT_ICON = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"><circle cx="8" cy="2.8" r="1.6"/><circle cx="4" cy="13.2" r="1.6"/><circle cx="12" cy="13.2" r="1.6"/><path d="M8 4.4v2.2M8 6.6c0 2.6-4 2.4-4 5M8 6.6c0 2.6 4 2.4 4 5"/></svg>`;
// Issues view (Alt+I): a dot inside a circle — GitHub's open-issue glyph.
const ISSUES_ICON = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3"><circle cx="8" cy="8" r="5.4"/><circle cx="8" cy="8" r="1.5" fill="currentColor" stroke="none"/></svg>`;
// Audit viewer: a clock/history glyph for the group's audit-log timeline.
const AUDIT_ICON = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M2.2 8a5.8 5.8 0 1 1 1.7 4.1"/><path d="M2.2 12.2V8.6H5.8"/><path d="M8 5.2V8l2 1.4"/></svg>`;
const GROUP_ICON = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="3.4" r="1.7"/><circle cx="3.4" cy="11" r="1.7"/><circle cx="12.6" cy="11" r="1.7"/><path d="M8 5.1v3M6.7 9.6 4.5 9.9M9.3 9.6l2.2.3"/></svg>`;
// Fold-group toggle (#46): stacked panes collapsing toward a baseline —
// signals "minimize every worker/reviewer pane to the dock at once".
const GROUP_MIN_ICON = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="2.4" width="10" height="3.2" rx="0.8"/><rect x="4.6" y="7" width="6.8" height="2.6" rx="0.7"/><path d="M4.2 13h7.6"/></svg>`;
// "Open in editor": code-brackets glyph. Opens the pane's workspace folder in
// the user's configured external editor (VS Code, Zed, …).
const EDITOR_ICON = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M6 4.5 2.5 8 6 11.5M10 4.5 13.5 8 10 11.5"/></svg>`;
// File-editor overlay (#174): a page with a fold + a small pencil, to read as
// "edit files" distinct from the external-editor </> glyph above.
const FILES_ICON = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M3.5 2.2h5l3.5 3.5v5.1"/><path d="M8.2 2.2v3.3h3.3"/><path d="M2.2 8.2h5.1v5.1H2.2z"/></svg>`;
// Attach affordance on the steering strip (#72): a paperclip.
const PAPERCLIP_ICON = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M12.5 6.6 7.1 12a2.4 2.4 0 0 1-3.4-3.4l5.6-5.6a1.5 1.5 0 0 1 2.1 2.1l-5.4 5.4a.6.6 0 0 1-.9-.9l4.9-4.9"/></svg>`;
// Voice-prompt push-to-talk button (#58): a simple microphone glyph.
const MIC_ICON = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><rect x="6" y="1.8" width="4" height="7.4" rx="2"/><path d="M3.8 7.2a4.2 4.2 0 0 0 8.4 0M8 11.4v2.8M6 14.2h4"/></svg>`;

/** Pull image files out of a paste/drag `DataTransfer`. Returns only entries
 *  the browser tags as images, so a text or mixed paste yields []. */
function imagesFromDataTransfer(dt: DataTransfer | null): File[] {
  if (!dt) return [];
  const out: File[] = [];
  for (const item of Array.from(dt.items)) {
    if (item.kind === "file" && item.type.startsWith("image/")) {
      const f = item.getAsFile();
      if (f) out.push(f);
    }
  }
  return out;
}

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
  /** Structured agent invocation for direct-CLI spawn (issue #78); the backend
   *  falls back to `command` (shell wrapper) when it can't apply. */
  argv?: string[];
  /** Extra per-pane env (#83): agent panes carry the gh-shim PATH +
   *  `LOOMUX_GROUP_DIR` here so the merge gate is enforced. Omitted for plain
   *  shells. Wire form: `[key, value]` pairs (the backend's `Vec<(String,String)>`). */
  env?: [string, string][];
  badge?: PaneBadge;
  /** Orchestration group this pane belongs to (enables the task board). */
  orchGroup?: string;
  /** "orchestrator" | "worker" | "reviewer". */
  orchRole?: string;
  /** Agent id, for attention acks (clearing a "needs attention" badge). */
  orchAgent?: string;
  /** Open without stealing keyboard focus (issue #117): an orchestrator-driven
   *  spawn must not yank the cursor from the pane the human is typing in. The
   *  human-initiated paths leave this unset (focus the new pane); only the
   *  orch-spawn-request path sets it. Grid.openPane resolves the actual
   *  decision — an empty grid still focuses regardless (see panefocus.ts). */
  background?: boolean;
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
  /** Minimize (or restore) this pane's whole orchestration group's
   *  worker/reviewer panes at once (#46). No-op off an orchestrator pane. */
  onToggleGroupMinimize: (pane: Pane) => void;
}

export class Pane implements VoiceTargetPane {
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
  /** Directory the external-change git watch is currently pointed at (#36),
   *  so we only re-issue the backend call when the pane actually changes dir. */
  private watchedPath: string | null = null;
  /** Lazily created git view; null until the first toggle. */
  private gitView: GitView | null = null;
  private gitDivider: HTMLElement | null = null;
  /** Floating container for the git view + divider. It overlays the top of
   *  the terminal instead of shrinking it: resizing the PTY makes ConPTY and
   *  full-screen TUIs repaint from scratch, flooding scrollback with
   *  duplicate frames. */
  private gitOverlay: HTMLElement | null = null;
  /** GitHub issues view (any pane in a git repo), same overlay mechanics. */
  private issuesView: IssuesView | null = null;
  private issuesOverlay: HTMLElement | null = null;
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
  /** File-editor overlay (#174): file tree + code editor + search/replace.
   *  Unlike the others it is UNGATED — present in every pane type, plain
   *  terminals included. Same no-resize overlay mechanics. */
  private fileEditView: FileEditView | null = null;
  private fileEditOverlay: HTMLElement | null = null;
  private fileEditBtn: HTMLButtonElement;
  /** Fold-group toggle (orchestrator panes only, #46): minimizes every
   *  worker/reviewer pane in the group to the dock, or restores them all. */
  private groupMinBtn: HTMLButtonElement;
  /** Fullscreen toggle; its glyph flips to a restore affordance when active. */
  private maximizeBtn: HTMLButtonElement;
  private orchGroup: string | null = null;
  private orchRoleName: string | null = null;
  private orchAgent: string | null = null;
  /** Loomux-owned steering strip docked under orchestrator panes (#43): the
   *  human types here and loomux enqueues it through the same serialized
   *  delivery path as worker reports, so the pane's stdin has one writer. */
  private composeInput: HTMLTextAreaElement | null = null;
  private composeStatus: HTMLElement | null = null;
  private composeStatusTimer: number | undefined;
  /** Thumbnail-chip row for images pasted/attached into the strip (#72); hidden
   *  until the first image is queued. */
  private composeChips: HTMLElement | null = null;
  /** Images queued for the next steer, in send order. `path` is the on-disk
   *  scratch file (from `orch_save_attachment`); `url` is a blob: object URL for
   *  the chip thumbnail and must be revoked when the chip goes away. */
  private attachments: { path: string; url: string; name: string }[] = [];
  /** The orchestrator's CLI, learned from the save-attachment response; decides
   *  how image paths are referenced in the steer text (#72). Defaults to the
   *  Claude form until a save reports otherwise. */
  private orchCli = "claude";
  /** Voice-prompt push-to-talk button on the steer strip (#58). Only present on
   *  orchestrator panes; the hotkey (Alt+S) works on any pane regardless. */
  private micBtn: HTMLButtonElement | null = null;
  /** Overlay badge shown while a voice capture targets THIS pane's terminal
   *  (#58). Overlay chrome — floats over `.xterm`, never resizes the PTY. */
  private voiceIndicator: HTMLElement | null = null;
  /** "needs attention" chip in the header (attention routing #6); hidden until
   *  the backend flags this pane. */
  private attnChip: HTMLButtonElement;
  private attentionReason: string | null = null;
  private attentionDetail: string | null = null;
  /** Notified when something the dock chip shows changes (attention state or
   *  the pane name); the grid uses it to keep a minimized pane's chip in sync,
   *  since a docked pane's header is out of the DOM (#6, #95r). */
  private dockSyncListener: (() => void) | null = null;
  /** True for agent/command panes (vs plain shells). */
  private launchedCommand = false;
  private shiftTimer: number | undefined;
  private fit = new FitAddon();
  private resizeObs: ResizeObserver;
  private disposed = false;
  /** Ordered input pipe to the PTY: serializes every keystroke/paste so the
   *  async IPC writes can't reorder (a bracketed-paste terminator overtaking
   *  its body wedges the target app — #65). Buffers input produced before the
   *  PTY exists and flushes it in order once ready. */
  private writer = createOrderedWriter();

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

    // Fold the whole group's worker/reviewer panes to the dock in one click
    // (or restore them). Orchestrator panes only; the group's real-estate
    // control when it grows large (#46).
    this.groupMinBtn = document.createElement("button");
    this.groupMinBtn.className = "pane-btn";
    this.groupMinBtn.innerHTML = GROUP_MIN_ICON;
    this.groupMinBtn.title = "Minimize / restore all group panes";
    this.groupMinBtn.hidden = true; // shown for orchestrator panes in start()
    this.groupMinBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.events.onToggleGroupMinimize(this);
    });
    header.appendChild(this.groupMinBtn);

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

    const issuesBtn = document.createElement("button");
    issuesBtn.className = "pane-btn";
    issuesBtn.innerHTML = ISSUES_ICON;
    issuesBtn.title = "GitHub issues (Alt+I)";
    issuesBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.toggleIssuesView();
    });
    header.appendChild(issuesBtn);

    const gitBtn = document.createElement("button");
    gitBtn.className = "pane-btn";
    gitBtn.innerHTML = GIT_ICON;
    gitBtn.title = "Git view (Alt+G)";
    gitBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.toggleGitView();
    });
    header.appendChild(gitBtn);

    // File-editor overlay (#174). Unconditional — every pane type gets it,
    // including plain terminals (unlike the orchestration-gated buttons above).
    this.fileEditBtn = document.createElement("button");
    this.fileEditBtn.className = "pane-btn";
    this.fileEditBtn.innerHTML = FILES_ICON;
    this.fileEditBtn.title = "File editor (Alt+F)";
    this.fileEditBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.toggleFileEditView();
    });
    header.appendChild(this.fileEditBtn);

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

    // Clipboard integration: a CLI (e.g. claude code) copies by emitting
    // OSC 52. xterm.js doesn't implement it, so without this handler the
    // sequence is dropped — the CLI says "copied" but the system clipboard
    // stays empty (#65). Decode the base64 payload and write it out; ignore
    // read requests (`?`) so we never leak the clipboard back to the process,
    // and refuse an oversized payload rather than balloon memory decoding it.
    this.term.parser.registerOscHandler(52, (payload) => {
      const parsed = parseOsc52(payload);
      if (parsed.ok) {
        void this.copyToClipboard(parsed.text);
      } else if (parsed.reason === "oversize") {
        showToast("Ignored an oversized copy request from the terminal.");
      }
      return true;
    });

    // Let app-level shortcuts pass through xterm untouched; handle
    // clipboard combos here (Windows Terminal conventions).
    this.term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown") return true;
      if (isAppShortcut(e)) return false;
      if (e.ctrlKey && e.shiftKey && e.code === "KeyC") {
        const sel = this.term.getSelection();
        if (sel) void this.copyToClipboard(sel);
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
  async start(opts: PaneOptions = {}, takeFocus = true): Promise<void> {
    this.setName(opts.name ?? "shell");
    this.launchedCommand = !!opts.command?.trim();
    if (opts.badge) this.setBadge(opts.badge);
    if (opts.orchAgent) this.orchAgent = opts.orchAgent;
    if (opts.orchGroup) {
      this.orchGroup = opts.orchGroup;
      this.orchRoleName = opts.orchRole ?? null;
      // The board lives on the orchestrator's pane; workers report there.
      this.tasksBtn.hidden = opts.orchRole !== "orchestrator";
      // The audit log is per-group and read-only, so it's useful from any
      // agent pane in the group, not just the orchestrator's.
      this.auditBtn.hidden = false;
      // Group lifecycle controls (pause / end orchestration) live on the
      // orchestrator's pane, alongside the task board.
      this.groupBtn.hidden = opts.orchRole !== "orchestrator";
      // Same for the fold-group toggle (#46): it acts on the orchestrator's
      // own worker/reviewer panes.
      this.groupMinBtn.hidden = opts.orchRole !== "orchestrator";
      // Steering strip (#43): only the orchestrator pane gets one. Build it
      // BEFORE term.open/fit below so the terminal sizes to the reduced
      // height once, avoiding a later resize repaint into scrollback.
      if (opts.orchRole === "orchestrator") this.buildComposeStrip();
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

    // Everything is wired before the process exists: input queues in the
    // ordered writer until the PTY is ready, and the output router buffers
    // until we attach.
    this.term.onData((data) => this.writer.write(data));
    this.resizeObs.observe(this.termEl);
    // A background (orchestrator-driven) spawn must not pull focus from the
    // pane the human is typing in (#117); grid.openPane decides takeFocus.
    if (takeFocus) this.focus();

    try {
      await ensureOutputRouter();
      const cols = Number.isFinite(this.term.cols) && this.term.cols > 1 ? this.term.cols : 80;
      const rows = Number.isFinite(this.term.rows) && this.term.rows > 1 ? this.term.rows : 24;
      const ptyId = await spawnPty({
        cols,
        rows,
        cwd: opts.cwd,
        command: opts.command,
        argv: opts.argv,
        env: opts.env,
      });
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
      // React to repo changes made outside this pane's shell (#36): the
      // backend watch is pointed at the repo on each cwd report below.
      attachGitWatch(ptyId, () => this.onExternalGitChange());
      if (this.cwdRaw) {
        this.watchedPath = this.cwdRaw;
        setGitWatch(ptyId, this.cwdRaw);
      }
      // Bind the ordered writer to this PTY and flush anything typed/pasted
      // while it was starting, in arrival order.
      this.writer.ready((data) => writePty(ptyId, data));
    } catch (err) {
      // Never leave a dead black pane: surface the failure in-terminal.
      this.term.writeln(`\x1b[91mloomux: failed to start shell\x1b[0m`);
      this.term.writeln(`\x1b[90m${String(err)}\x1b[0m`);
    }
  }

  /** The live WebGL renderer addon, if loaded. Held so hidden tabs can drop it
   *  (browsers cap live GL contexts, and N mounted-but-hidden tabs would each
   *  hold one) and reload it on show — the onContextLoss→DOM fallback path. */
  private webgl: WebglAddon | null = null;
  private serializer: SerializeAddon | null = null;
  /** True while this pane's project tab is hidden (#63). Held so `tryWebgl`
   *  refuses to create a context for a hidden pane — start() calls tryWebgl
   *  unconditionally, so a pane opened INTO a hidden tab (a background
   *  orchestrator spawn) would otherwise take a GL context it isn't showing. */
  private hiddenTab = false;

  private tryWebgl(): void {
    if (this.webgl || this.hiddenTab) return;
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => {
        webgl.dispose(); // falls back to DOM renderer
        if (this.webgl === webgl) this.webgl = null;
      });
      this.term.loadAddon(webgl);
      this.webgl = webgl;
    } catch {
      // WebGL unavailable — xterm's DOM renderer still works fine.
    }
  }

  /** Show/hide bookkeeping for a project-tab switch (#63). Hiding drops the
   *  WebGL context (freeing it for the active tab and cutting idle VRAM) and
   *  latches `hiddenTab` so start()/tryWebgl won't re-create one while hidden;
   *  showing clears the latch and reloads it (via the onContextLoss→DOM fallback
   *  if the GPU is out of contexts). Purely a rendering concern — the PTY and
   *  buffer are untouched, so no resize and no scrollback loss. Safe to call
   *  before the terminal is even open (tryWebgl no-ops until start opens it). */
  setHidden(hidden: boolean): void {
    if (this.disposed) return;
    this.hiddenTab = hidden;
    if (hidden) {
      this.webgl?.dispose();
      this.webgl = null;
    } else if (this.termEl.isConnected) {
      this.tryWebgl();
    }
  }

  /** An HTML snapshot of the terminal viewport, for a background tab's preview
   *  thumbnail (#63). Serializes the in-memory buffer (NOT the DOM),
   *  so it works while the pane is hidden/zero-width — the whole point: a preview
   *  must never require a laid-out element, which would re-arm applyFit and fire
   *  a PTY resize.
   *
   *  serializeAsHTML (not serialize): the string serializer emits cursor-forward
   *  escapes (`ESC[nC`) to skip blank cells, which stripping collapses runs of
   *  spaces ("Please count" → "Pleasecount", #63). The HTML serializer
   *  emits a literal space per blank cell and per-run `<span style='color:…'>`,
   *  so the preview keeps spacing AND color. The caller parses this SAFELY (spans
   *  → textContent + whitelisted styles), never innerHTML — the addon does not
   *  escape cell text. Returns "" if serialization isn't available. */
  serializeViewportHtml(): string {
    if (this.disposed) return "";
    try {
      if (!this.serializer) {
        this.serializer = new SerializeAddon();
        this.term.loadAddon(this.serializer);
      }
      // scrollback: 0 → just the visible screen, which is all a thumbnail shows.
      return this.serializer.serializeAsHTML({ scrollback: 0 });
    } catch {
      return "";
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
      if (this.termEl.clientWidth === 0) return; // hidden (inactive tab / maximized-behind) or unlaid — fit.fit() needs a laid-out element
      this.fit.fit();
      const size = `${this.term.cols}x${this.term.rows}`;
      // The zero-width / same-size / no-pty skips live in the pure, tested
      // shouldResizePty (panefit.ts) — THE invariant that keeps tab switches and
      // maximize free of ConPTY repaints (#63, CLAUDE.md constraint 1).
      if (shouldResizePty({ clientWidth: this.termEl.clientWidth, size, sentSize: this.sentSize, ptyId: this.ptyId })) {
        this.sentSize = size;
        resizePty(this.ptyId!, this.term.cols, this.term.rows).catch(() => {});
      }
      // The pane itself changed size: keep the overlay within bounds and
      // re-anchor the visible strip on the cursor.
      const overlay = this.activeOverlay();
      if (overlay) {
        overlay.style.height = `${this.overlayClamp(overlay.offsetHeight)}px`;
        this.updateTermShift();
      }
      // The steer box wraps to the strip's width, so a width change alters how
      // many lines the placeholder/draft occupies. growCompose only ran on input
      // events, so a widened pane never re-measured and the box stayed tall
      // (#163). Re-measure here; it's a no-op on panes without a compose strip.
      this.growCompose();
    }, 16);
  }

  setName(name: string): void {
    this.name = name;
    this.titleEl.textContent = name;
    // A docked pane's header is detached, so refresh its dock chip too — else an
    // orchestrator/human rename leaves the chip showing the stale name (#95r).
    this.dockSyncListener?.();
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

  /** Flag (or clear) this pane as needing the human — driven by the backend
   *  attention scan. Idempotent: a same-reason repeat is a no-op, so the 3-second
   *  re-emits don't thrash the DOM. `null` clears the badge. */
  setAttention(reason: string | null, detail?: string): void {
    if (reason === this.attentionReason) return;
    this.attentionReason = reason;
    this.attentionDetail = reason ? detail ?? null : null;
    if (!reason) {
      this.attnChip.hidden = true;
      this.el.classList.remove("needs-attention");
      delete this.attnChip.dataset.reason;
    } else {
      const { label } = attentionPresentation(reason);
      this.attnChip.textContent = label;
      this.attnChip.title = detail ?? "This pane needs you";
      this.attnChip.dataset.reason = reason;
      this.attnChip.hidden = false;
      this.el.classList.add("needs-attention");
    }
    // A minimized pane's element is detached, so its header chip is invisible;
    // the listener lets the grid mirror this state onto the dock chip.
    this.dockSyncListener?.();
  }

  /** Current needs-attention state, or null. Lets the grid render an equivalent
   *  badge on the dock chip while this pane is minimized (its header is out of
   *  the DOM). */
  get attention(): { reason: string; label: string; urgent: boolean; detail: string | null } | null {
    if (!this.attentionReason) return null;
    const { label, urgent } = attentionPresentation(this.attentionReason);
    return { reason: this.attentionReason, label, urgent, detail: this.attentionDetail };
  }

  /** Register a callback fired whenever the dock chip's content changes
   *  (attention state or name) — used by the grid to refresh the chip of a
   *  minimized pane, whose header is out of the DOM. */
  setDockSyncListener(fn: (() => void) | null): void {
    this.dockSyncListener = fn;
  }

  /** The human is now on this pane: acknowledge its attention backend-side so
   *  the badge drops and (for `waiting`) stays down until the prompt changes.
   *  Agent panes ack by agent id; a plain pane (no agent identity) acks by its
   *  pty id (#40). Public so restoring a docked pane clears it the same way
   *  turning to a pane does. */
  acknowledgeAttention(): void {
    if (!this.attentionReason) return;
    if (this.orchAgent) {
      invoke("orch_ack_attention", { agentId: this.orchAgent }).catch(() => {});
    } else if (this.ptyId !== null) {
      invoke("orch_ack_attention_pty", { ptyId: this.ptyId }).catch(() => {});
    }
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
    // Repoint the external-change watch when the directory changes (#36); the
    // backend dedupes same-repo calls so cd-within-a-repo is a no-op there.
    if (path !== this.watchedPath && this.ptyId !== null) {
      this.watchedPath = path;
      setGitWatch(this.ptyId, path);
    }
    // Refresh even when the path is unchanged: the *branch* can change
    // without a cd (git checkout), and dir_info is cheap.
    void this.refreshDir(path);
  }

  /** The backend saw this pane's repo change on disk (an external checkout /
   *  commit / stage). Drive the same refresh a shell prompt would: the git
   *  view (throttled) and the header branch chip. */
  private onExternalGitChange(): void {
    this.gitView?.notifyPrompt();
    if (this.cwdRaw) void this.refreshDir(this.cwdRaw);
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
        if (this.issuesView?.visible) this.toggleIssuesView();
        if (this.tasksOverlay && !this.tasksOverlay.hidden) this.toggleTasksView();
        if (this.auditOverlay && !this.auditOverlay.hidden) this.toggleAuditView();
        if (this.groupOverlay && !this.groupOverlay.hidden) this.toggleGroupView();
        if (this.fileEditView?.visible) this.toggleFileEditView();
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

  /** Keep the overlay tall enough that its bottom drag bar stays grabbable and
   *  no control clips, but always leave a terminal strip visible at the bottom.
   *  `floor` overrides the baseline minimum with a panel-specific one (the group
   *  panel measures its fixed chrome so the footer can't collapse — #83 rev-58).
   *  Pure math + tests in overlaysize.ts. */
  private overlayClamp(h: number, floor?: number): number {
    return clampOverlayHeight(h, this.termEl.clientHeight, floor ?? OVERLAY_MIN_H);
  }

  /** The group panel's minimum content height — its measured fixed chrome so
   *  every control (footer End/Pause, suspended banner) stays on-screen — never
   *  below the shared baseline, and never so tall it can't fit the pane. */
  private groupFloor(): number {
    const measured = this.groupView?.minChromeHeight() ?? 0;
    return Math.max(OVERLAY_MIN_H, measured);
  }

  /** Re-apply the height clamp to the open group overlay (its content height may
   *  have changed since it was sized). Only touches the height when the clamp
   *  actually moves it — typically a bump UP when the measured floor grew (the
   *  suspended banner appeared) — so it never fights the user's chosen size. */
  private reclampGroupOverlay(): void {
    if (!this.groupOverlay || this.groupOverlay.hidden) return;
    const cur = this.groupOverlay.offsetHeight;
    const clamped = this.overlayClamp(cur, this.groupFloor());
    if (clamped !== cur) {
      this.groupOverlay.style.height = `${clamped}px`;
      this.updateTermShift();
    }
  }

  /** Horizontal drag handle on an overlay's bottom edge. `floor` (optional) is a
   *  panel-specific minimum height provider passed to the clamp on each drag. */
  private makeOverlayDivider(overlay: () => HTMLElement, floor?: () => number): HTMLElement {
    const div = document.createElement("div");
    div.className = "git-divider";
    div.addEventListener("mousedown", (e) => {
      e.preventDefault();
      const startY = e.clientY;
      const startH = overlay().offsetHeight;
      div.classList.add("dragging");
      const move = (ev: MouseEvent) => {
        const h = this.overlayClamp(startH + (ev.clientY - startY), floor?.());
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

  /** Toggle the GitHub issues overlay. Available on any pane whose cwd is a git
   *  repo (the view resolves the repo root itself). Same no-resize overlay
   *  mechanics as the git view — it FLOATS over the terminal and never resizes
   *  the PTY; only one overlay is open at a time. */
  toggleIssuesView(): void {
    if (!this.issuesView) {
      this.issuesView = new IssuesView({
        getCwd: () => this.cwdRaw,
        onClose: () => this.toggleIssuesView(),
      });
      this.issuesOverlay = document.createElement("div");
      this.issuesOverlay.className = "git-overlay";
      this.issuesOverlay.hidden = true;
      this.issuesOverlay.append(
        this.issuesView.el,
        this.makeOverlayDivider(() => this.issuesOverlay!)
      );
      this.el.appendChild(this.issuesOverlay);
    }
    if (this.issuesView.visible) {
      this.issuesView.hide();
      this.issuesOverlay!.hidden = true;
      this.updateTermShift();
      this.focus();
    } else {
      if (this.gitView?.visible) this.toggleGitView();
      if (this.tasksOverlay && !this.tasksOverlay.hidden) this.toggleTasksView();
      if (this.auditOverlay && !this.auditOverlay.hidden) this.toggleAuditView();
      if (this.groupOverlay && !this.groupOverlay.hidden) this.toggleGroupView();
      if (this.fileEditView?.visible) this.toggleFileEditView();
      const strip = Math.max(140, Math.round(this.el.clientHeight * 0.35));
      this.issuesOverlay!.style.height = `${this.overlayClamp(this.termEl.clientHeight - strip)}px`;
      this.issuesOverlay!.hidden = false;
      this.issuesView.show();
      this.updateTermShift();
    }
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
      if (this.issuesView?.visible) this.toggleIssuesView();
      if (this.gitView?.visible) this.toggleGitView();
      if (this.auditOverlay && !this.auditOverlay.hidden) this.toggleAuditView();
      if (this.groupOverlay && !this.groupOverlay.hidden) this.toggleGroupView();
      if (this.fileEditView?.visible) this.toggleFileEditView();
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
      if (this.issuesView?.visible) this.toggleIssuesView();
      if (this.gitView?.visible) this.toggleGitView();
      if (this.tasksOverlay && !this.tasksOverlay.hidden) this.toggleTasksView();
      if (this.groupOverlay && !this.groupOverlay.hidden) this.toggleGroupView();
      if (this.fileEditView?.visible) this.toggleFileEditView();
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
      this.groupView = new GroupView(this.orchGroup, {
        onClose: () => this.toggleGroupView(),
        // Mirror the header's fold-group toggle inside the lifecycle panel (#46).
        onToggleMinimize: () => this.events.onToggleGroupMinimize(this),
        // Content grew (e.g. the suspended banner appeared) — re-clamp so the
        // footer never slides under overflow:hidden (#83 rev-58).
        onResize: () => this.reclampGroupOverlay(),
      });
      this.groupOverlay = document.createElement("div");
      this.groupOverlay.className = "git-overlay";
      this.groupOverlay.hidden = true;
      this.groupOverlay.append(
        this.groupView.el,
        this.makeOverlayDivider(() => this.groupOverlay!, () => this.groupFloor())
      );
      this.el.appendChild(this.groupOverlay);
    }
    if (!this.groupOverlay!.hidden) {
      this.groupOverlay!.hidden = true;
      this.updateTermShift();
      this.focus();
    } else {
      if (this.issuesView?.visible) this.toggleIssuesView();
      if (this.gitView?.visible) this.toggleGitView();
      if (this.tasksOverlay && !this.tasksOverlay.hidden) this.toggleTasksView();
      if (this.auditOverlay && !this.auditOverlay.hidden) this.toggleAuditView();
      if (this.fileEditView?.visible) this.toggleFileEditView();
      const strip = Math.max(140, Math.round(this.el.clientHeight * 0.35));
      this.groupOverlay!.style.height = `${this.overlayClamp(this.termEl.clientHeight - strip, this.groupFloor())}px`;
      this.groupOverlay!.hidden = false;
      this.groupView.show();
      this.updateTermShift();
    }
  }

  /** Toggle the file-editor overlay (#174): file tree + code editor +
   *  search/replace. Ungated — works in every pane type, plain terminals
   *  included. Same no-resize overlay mechanics as the git/audit views; only
   *  one overlay is open at a time. The tree roots at the pane's live cwd. */
  toggleFileEditView(): void {
    if (!this.fileEditView) {
      this.fileEditView = new FileEditView({
        getCwd: () => this.cwdRaw,
        onClose: () => this.toggleFileEditView(),
        isAgentWorktree: () =>
          this.orchRoleName === "worker" || this.orchRoleName === "reviewer",
      });
      this.fileEditOverlay = document.createElement("div");
      this.fileEditOverlay.className = "git-overlay";
      this.fileEditOverlay.hidden = true;
      this.fileEditOverlay.append(
        this.fileEditView.el,
        this.makeOverlayDivider(() => this.fileEditOverlay!)
      );
      this.el.appendChild(this.fileEditOverlay);
    }
    if (this.fileEditView.visible) {
      this.fileEditView.hide();
      this.fileEditOverlay!.hidden = true;
      this.updateTermShift();
      this.focus();
    } else {
      if (this.issuesView?.visible) this.toggleIssuesView();
      if (this.gitView?.visible) this.toggleGitView();
      if (this.tasksOverlay && !this.tasksOverlay.hidden) this.toggleTasksView();
      if (this.auditOverlay && !this.auditOverlay.hidden) this.toggleAuditView();
      if (this.groupOverlay && !this.groupOverlay.hidden) this.toggleGroupView();
      const strip = Math.max(140, Math.round(this.el.clientHeight * 0.35));
      this.fileEditOverlay!.style.height = `${this.overlayClamp(this.termEl.clientHeight - strip)}px`;
      this.fileEditOverlay!.hidden = false;
      this.fileEditView.show();
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

  /** The orchestration agent id this pane hosts, if any. Lets a cancelled
   *  spawn (#106) find and close the pane it opened before the bind timed out. */
  get orchAgentId(): string | null {
    return this.orchAgent;
  }

  /** This pane's orchestration role ("orchestrator" | "worker" | "reviewer"),
   *  or null for a non-orchestration pane. Lets group-wide actions (#46) tell
   *  the orchestrator's own pane apart from its workers/reviewers. */
  get orchRole(): string | null {
    return this.orchRoleName;
  }

  /** Whichever overlay (git / tasks / audit / group) is currently covering
   *  the terminal. */
  private activeOverlay(): HTMLElement | null {
    if (this.gitOverlay && !this.gitOverlay.hidden) return this.gitOverlay;
    if (this.issuesOverlay && !this.issuesOverlay.hidden) return this.issuesOverlay;
    if (this.tasksOverlay && !this.tasksOverlay.hidden) return this.tasksOverlay;
    if (this.auditOverlay && !this.auditOverlay.hidden) return this.auditOverlay;
    if (this.groupOverlay && !this.groupOverlay.hidden) return this.groupOverlay;
    if (this.fileEditOverlay && !this.fileEditOverlay.hidden) return this.fileEditOverlay;
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
    // Enter/Escape commit AND blur commits; the first commit swaps the input
    // back, and detaching the focused input itself fires blur → a second commit.
    // makeRenameCommit is idempotent so that redundant call is a no-op, Escape
    // (save=false) beats the trailing blur-save, and — for #113 — a blur caused
    // by an orchestrator-driven grid/dock move (input no longer connected) is
    // treated as a cancel rather than saving a half-typed name (see isConnected).
    const commit = makeRenameCommit({
      value: () => input.value,
      isConnected: () => input.isConnected,
      save: (name) => {
        const changed = name !== this.name;
        this.name = name;
        // Sync a human rename to the backend so the roster name matches the
        // pane title and the human's choice takes precedence over any later
        // orchestrator rename_agent (#95r). Best-effort: the title is already
        // updated locally, so a backend hiccup is non-fatal. Skip the round-trip
        // when nothing changed so a no-op Enter/blur doesn't re-broadcast a rename.
        if (this.orchAgent && changed) {
          invoke("orch_agent_renamed", { agentId: this.orchAgent, name }).catch(() => {});
        }
      },
      restore: () => {
        // Put the label back showing the current name (the pre-edit name on a
        // cancel, the saved name on a commit), then swap the input out. swapEditor
        // tolerates the input having been detached OR moved mid-edit by a grid/dock
        // restructure: it leaves the header consistent (label back, no orphaned
        // input) and only reports `live` — safe to refocus — when the input was
        // still on the document, i.e. the ordinary Enter/click-away path.
        this.titleEl.textContent = this.name;
        if (swapEditor(input, this.titleEl).live) this.focus();
      },
    });
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

  /** Copy `text` to the system clipboard, surfacing a toast if the write fails
   *  outright (locked-down webview) — otherwise a failed OSC 52 copy would
   *  silently no-op and reintroduce the "said copied, clipboard empty" symptom
   *  from #65 with no signal to the user. */
  private async copyToClipboard(text: string): Promise<void> {
    const ok = await writeClipboard(text);
    if (!ok) showToast("Copy failed — click the pane and try again.");
  }

  /** Build the loomux steering strip and dock it under the terminal (#43,
   *  option C). It is a plain DOM textarea — NOT part of xterm — so it never
   *  steals the terminal's keys: keystrokes only reach it while it holds focus
   *  (click or Alt+P). Enter submits; Shift+Enter inserts a newline (the box
   *  wraps and grows, #100); Esc hands focus back to the term. */
  private buildComposeStrip(): void {
    const strip = document.createElement("div");
    strip.className = "orch-compose";

    const row = document.createElement("div");
    row.className = "orch-compose-row";
    // The textarea floats inside a fixed-height field: it grows UPWARD over the
    // terminal (see .orch-compose CSS) so a multi-line draft never shrinks the
    // strip's flow footprint — that would resize .pane-term / the PTY (#100).
    const field = document.createElement("div");
    field.className = "orch-compose-field";
    const input = document.createElement("textarea");
    input.className = "dlg-input orch-compose-input";
    // Terse enough to sit on one line at typical pane widths — a long hint here
    // wraps the box to multi-line before the human even types (#163). The full
    // Shift+Enter/Esc rules live in this method's doc comment, not the ghost text.
    input.placeholder = "Steer the orchestrator — Enter sends";
    input.rows = 1;
    input.spellcheck = false;
    input.autocomplete = "off";
    input.addEventListener("keydown", (e) => {
      // Keep this keydown from bubbling to pane/ancestor handlers. (App
      // shortcuts are dispatched capture-phase on `document` and still fire
      // while the strip is focused — but Enter/Esc/plain typing aren't app
      // shortcuts, so the strip handles them normally regardless.)
      e.stopPropagation();
      // Only Enter (send) and Escape (back to terminal) are ours; Shift+Enter,
      // IME-commit Enter, and ordinary typing fall through to the textarea so it
      // inserts a newline and auto-grows. See steerKeyAction for the rules.
      switch (steerKeyAction(e)) {
        case "submit":
          e.preventDefault();
          void this.submitCompose();
          break;
        case "blur":
          e.preventDefault();
          this.focus();
          break;
      }
    });
    // Reflow the box on every content change (typing, newline, paste, cut) so it
    // tracks the draft's line count up to the CSS cap.
    input.addEventListener("input", () => this.growCompose());
    // Ctrl+V of a screenshot: pull image blobs out of the clipboard and queue
    // them as attachments (#72). Text pastes fall through to the input's default
    // handling untouched — we only preventDefault when we actually took images.
    input.addEventListener("paste", (e) => {
      const files = imagesFromDataTransfer(e.clipboardData);
      if (files.length === 0) return;
      e.preventDefault();
      for (const f of files) void this.addAttachment(f, f.name);
    });

    // Attach affordance: a paperclip that opens a native file picker. A hidden
    // <input type=file> keeps the styling ours while reusing the OS dialog.
    const attach = document.createElement("button");
    attach.className = "dlg-btn orch-compose-attach";
    attach.type = "button";
    attach.title = "Attach image(s) — or paste a screenshot with Ctrl+V";
    attach.setAttribute("aria-label", "Attach images");
    attach.innerHTML = PAPERCLIP_ICON;
    const filePicker = document.createElement("input");
    filePicker.type = "file";
    filePicker.accept = "image/*";
    filePicker.multiple = true;
    filePicker.style.display = "none";
    attach.addEventListener("click", (e) => {
      e.stopPropagation();
      filePicker.click();
    });
    filePicker.addEventListener("change", () => {
      const files = filePicker.files ? Array.from(filePicker.files) : [];
      for (const f of files) void this.addAttachment(f, f.name);
      filePicker.value = ""; // allow re-picking the same file next time
    });

    // Voice-prompt push-to-talk (#58): click to record, click again to stop and
    // transcribe locally. Transcript is inserted into the input, NOT submitted —
    // the human reviews it and hits Enter, same as typing.
    const mic = document.createElement("button");
    mic.className = "dlg-btn orch-compose-mic";
    mic.type = "button";
    mic.title = "Voice prompt — click to record, click again to transcribe";
    mic.setAttribute("aria-label", "Record voice prompt");
    mic.innerHTML = MIC_ICON;
    mic.addEventListener("click", (e) => {
      e.stopPropagation();
      voiceController.toggleForCompose(this);
    });

    const send = document.createElement("button");
    send.className = "dlg-btn primary orch-compose-send";
    send.textContent = "Send";
    send.addEventListener("click", (e) => {
      e.stopPropagation();
      void this.submitCompose();
    });
    // #100 wraps the textarea in a fixed-height field so its upward auto-grow
    // never resizes the PTY; #58's mic sits between the paperclip and Send.
    field.appendChild(input);
    row.append(field, attach, filePicker, mic, send);
    this.micBtn = mic;

    // Thumbnail-chip row for queued images (#72). Hidden (via .orch-compose-chips
    // being empty + CSS) until something is queued; kept above the status slot.
    const chips = document.createElement("div");
    chips.className = "orch-compose-chips";

    // Fixed-height slot (see .orch-compose-status): always in layout, so
    // showing/hiding a rejected-send message never changes the strip's height
    // and never resizes .pane-term / the PTY.
    const status = document.createElement("div");
    status.className = "orch-compose-status";

    strip.append(row, chips, status);
    this.composeInput = input;
    this.composeStatus = status;
    this.composeChips = chips;
    this.el.appendChild(strip);
    // Set the box's initial one-line height explicitly (it's attached now), so
    // the baseline matches the field's reserved height before any typing.
    this.growCompose();
  }

  /** Queue one image for the next steer: vet it, base64 it to the backend
   *  scratch dir, and add a thumbnail chip. Refusals (wrong type, oversize, too
   *  many) surface as a toast and are dropped. */
  private async addAttachment(blob: Blob, name: string): Promise<void> {
    if (!this.orchGroup || !this.composeChips) return;
    const check = checkAttachment(blob.type, blob.size, this.attachments.length);
    if (!check.ok) {
      showToast(attachRejectMessage(check.reason, name));
      return;
    }
    try {
      const bytes = new Uint8Array(await blob.arrayBuffer());
      const saved = await invoke<{ path: string; cli: string }>("orch_save_attachment", {
        groupId: this.orchGroup,
        ext: check.ext,
        dataB64: bytesToBase64(bytes),
      });
      this.orchCli = saved.cli; // format references the way this orchestrator's CLI reads them
      // Only mint the thumbnail URL once the file is safely on disk.
      const url = URL.createObjectURL(blob);
      this.attachments.push({ path: saved.path, url, name: name || `image.${check.ext}` });
      this.renderChips();
    } catch (err) {
      showToast(`Attach failed: ${String(err)}`);
    }
  }

  /** Remove a queued attachment by its on-disk path, revoking its thumbnail URL.
   *  The scratch file itself is left for the group-end sweep (the cheap cleanup
   *  policy — no per-image delete round-trip). */
  private removeAttachment(path: string): void {
    const idx = this.attachments.findIndex((a) => a.path === path);
    if (idx < 0) return;
    URL.revokeObjectURL(this.attachments[idx].url);
    this.attachments.splice(idx, 1);
    this.renderChips();
  }

  /** Rebuild the thumbnail-chip row from `this.attachments`. */
  private renderChips(): void {
    const chips = this.composeChips;
    if (!chips) return;
    chips.replaceChildren();
    for (const a of this.attachments) {
      const chip = document.createElement("span");
      chip.className = "orch-compose-chip";
      chip.title = a.name;
      const thumb = document.createElement("img");
      thumb.className = "orch-compose-chip-thumb";
      thumb.src = a.url;
      thumb.alt = a.name;
      const rm = document.createElement("button");
      rm.className = "orch-compose-chip-x";
      rm.type = "button";
      rm.textContent = "✕";
      rm.title = `Remove ${a.name}`;
      rm.setAttribute("aria-label", `Remove ${a.name}`);
      rm.addEventListener("click", (e) => {
        e.stopPropagation();
        this.removeAttachment(a.path);
      });
      chip.append(thumb, rm);
      chips.appendChild(chip);
    }
  }

  /** Drop every queued attachment, revoking thumbnail URLs. Used after a
   *  successful send and on dispose. */
  private clearAttachments(): void {
    for (const a of this.attachments) URL.revokeObjectURL(a.url);
    this.attachments = [];
    this.renderChips();
  }

  /** Focus the steering strip (Alt+P). No-op on non-orchestrator panes. */
  focusCompose(): void {
    if (!this.composeInput) return;
    this.composeInput.focus();
    this.composeInput.select();
  }

  /** Auto-grow the steer box to fit its draft, capped at the CSS `max-height`
   *  (a few lines). The box is absolutely positioned and grows upward over the
   *  terminal, so its height changes never touch .pane-term / the PTY (#100).
   *  Past the cap it scrolls internally instead of getting taller. */
  private growCompose(): void {
    const t = this.composeInput;
    if (!t) return;
    // Collapse to content height first so the box can also SHRINK (e.g. after a
    // send or a delete), then measure and clamp to the cap.
    t.style.height = "auto";
    const cs = getComputedStyle(t);
    // scrollHeight is content+padding but excludes the border; under border-box
    // the applied height must include it, or the box under-sizes by ~2px and
    // clips the last line. maxHeight (border-box) is the CSS cap.
    const border = (parseFloat(cs.borderTopWidth) || 0) + (parseFloat(cs.borderBottomWidth) || 0);
    const maxPx = parseFloat(cs.maxHeight) || 0;
    const { heightPx, scroll } = steerBoxHeight(t.scrollHeight + border, maxPx);
    t.style.height = `${heightPx}px`;
    t.style.overflowY = scroll ? "auto" : "hidden";
  }

  // ----- VoiceTargetPane (#58): the surface the global voiceController drives.
  // The controller owns the single-capture state machine; a Pane only knows how
  // to receive a transcript and show a recording indicator.

  /** Is this pane's compose box the focused element? Decides caret-insert vs
   *  terminal-paste when the voice hotkey fires. */
  isComposeFocused(): boolean {
    return !!this.composeInput && document.activeElement === this.composeInput;
  }

  /** Reflect the capture phase on this pane's indicator. For a compose target
   *  it's the mic button (pulse while recording, spin while transcribing); for a
   *  terminal target it's a lazily-created overlay badge floating over `.xterm`
   *  (so it never resizes the PTY). */
  setVoicePhase(kind: "compose" | "terminal", phase: VoicePhase): void {
    if (kind === "compose") {
      this.micBtn?.classList.toggle("recording", phase === "recording");
      this.micBtn?.classList.toggle("transcribing", phase === "transcribing");
      return;
    }
    if (phase === "off") {
      this.voiceIndicator?.remove();
      this.voiceIndicator = null;
      return;
    }
    if (!this.voiceIndicator) {
      const badge = document.createElement("div");
      badge.className = "pane-voice-indicator";
      this.termEl.appendChild(badge);
      this.voiceIndicator = badge;
    }
    const recording = phase === "recording";
    this.voiceIndicator.classList.toggle("transcribing", !recording);
    this.voiceIndicator.innerHTML = recording
      ? `<span class="pane-voice-dot"></span>Recording — Alt+S to insert · Esc to cancel`
      : `<span class="pane-voice-spinner"></span>Transcribing… · Esc to cancel`;
  }

  /** Route a transcript into this pane's terminal as if pasted — xterm's paste
   *  path applies bracketed-paste semantics (when the app enabled them) and adds
   *  NO trailing newline, so the human reviews and presses Enter. */
  pasteToTerminal(text: string): void {
    if (this.disposed) return; // pane closed during transcription — drop it
    const t = text.trim();
    if (t) this.term.paste(t);
  }

  /** Surface a voice status/error on the strip (compose targets have one). */
  showVoiceStatus(msg: string): void {
    this.showComposeStatus(msg);
  }

  /** Insert transcribed text into the strip at the caret (or append), keeping a
   *  single space between words, then focus the input so the human can edit and
   *  press Enter. Never auto-submits. */
  insertTranscript(text: string): void {
    if (this.disposed) return; // pane closed during transcription — drop it
    const input = this.composeInput;
    if (!input) return;
    const t = text.trim();
    if (!t) return;
    const start = input.selectionStart ?? input.value.length;
    const end = input.selectionEnd ?? input.value.length;
    const before = input.value.slice(0, start);
    const after = input.value.slice(end);
    // Add a separating space only when butting up against existing text.
    const lead = before && !/\s$/.test(before) ? " " : "";
    const trail = after && !/^\s/.test(after) ? " " : "";
    input.value = before + lead + t + trail + after;
    const caret = (before + lead + t).length;
    input.focus();
    input.setSelectionRange(caret, caret);
    // Setting .value programmatically doesn't fire the "input" event that drives
    // the auto-grow (#100), so reflow explicitly — a dictated multi-line prompt
    // must expand the box, not sit clipped at one row until the human types.
    this.growCompose();
  }

  /** Show a transient status line under the strip (errors only — a successful
   *  send is confirmed by the message landing in the terminal above). */
  private showComposeStatus(msg: string): void {
    const status = this.composeStatus;
    if (!status) return;
    status.textContent = msg;
    status.title = msg; // full text if the one-line slot ellipsises it
    status.classList.add("show");
    clearTimeout(this.composeStatusTimer);
    this.composeStatusTimer = window.setTimeout(() => status.classList.remove("show"), 6000);
  }

  /** Enqueue the strip's text to the orchestrator through loomux's serialized
   *  delivery path. Each Enter enqueues one message (rapid sends queue in
   *  arrival order backend-side), so the input stays live rather than locking
   *  while a send is in flight. Clears optimistically; on failure the text is
   *  restored — unless the human has already started a newer draft — so a
   *  rejected message (paused group, dead orchestrator) isn't lost. */
  private async submitCompose(): Promise<void> {
    const input = this.composeInput;
    if (!input || !this.orchGroup) return;
    const draft = input.value;
    // Queued images each become an "Attached image: <path>" line (#72); a
    // message may be images-only (no typed text), so gate on either being
    // present rather than on the text alone.
    const queued = this.attachments;
    const text = composeSteerText(draft, queued.map((a) => a.path), this.orchCli);
    if (!text) return;
    input.value = "";
    this.growCompose(); // collapse the (now empty) box back to one line
    this.attachments = [];
    this.renderChips();
    this.composeStatus?.classList.remove("show");
    try {
      await invoke("orch_steer", { groupId: this.orchGroup, text });
      // Sent: the scratch files have served their purpose (the agent reads them
      // by path); drop only the thumbnail URLs. The files are swept on group end.
      for (const a of queued) URL.revokeObjectURL(a.url);
    } catch (err) {
      // Restore the draft and re-queue the images so a rejected send (paused
      // group, dead orchestrator) isn't lost — unless the human already started
      // a newer draft, which we must not clobber.
      if (input.value === "") {
        input.value = draft;
        this.growCompose(); // regrow to fit the restored draft
      }
      if (this.attachments.length === 0) {
        this.attachments = queued;
        this.renderChips();
      } else {
        for (const a of queued) URL.revokeObjectURL(a.url); // superseded; free them
      }
      this.showComposeStatus(`Not sent: ${String(err)}`);
    }
  }

  /** Tear down DOM + terminal. Kills the PTY unless it already exited. */
  dispose(killBackend = true): void {
    if (this.disposed) return;
    this.disposed = true;
    this.resizeObs.disconnect();
    clearTimeout(this.fitTimer);
    clearTimeout(this.shiftTimer);
    clearTimeout(this.composeStatusTimer);
    // Abort any in-flight voice capture aimed at this pane (releases the mic).
    voiceController.notifyPaneDisposed(this);
    this.voiceIndicator?.remove();
    this.clearAttachments(); // revoke any lingering thumbnail object URLs
    this.gitView?.dispose();
    this.issuesView?.dispose();
    this.tasksView?.dispose();
    this.auditView?.dispose();
    this.groupView?.dispose();
    this.fileEditView?.dispose();
    if (this.ptyId !== null) {
      detachOutput(this.ptyId);
      detachGitWatch(this.ptyId);
      if (killBackend) killPty(this.ptyId).catch(() => {});
    }
    this.term.dispose();
    this.el.remove();
  }
}

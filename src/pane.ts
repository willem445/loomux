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
import { pathTail, type ShellKind } from "./panesetup";
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
import { heldPresentation } from "./heldbadge";
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
import {
  embedDragGrow,
  fracFromGrow,
  clampEmbedFrac,
  embedSideFloors,
  embedCenterFloor,
  DEFAULT_EMBED_FRAC,
  EMBED_MIN_PANEL_PX,
  EMBED_SIDES,
  type EmbedSide,
} from "./embedsplit";
import { showContextMenu, type MenuItem } from "./contextmenu";
import {
  exitDiagnosticLine,
  keepOpenOnExit,
  type ExitInfo,
  type KeepOpenReason,
  type DirtyHost,
  type PaneBufferReport,
} from "./dirtystate";
import { FileEditView } from "./fileedit";
import { FileExplorerView } from "./fileexplorer";
import { WorkflowView } from "./workflowview";
import { WORKFLOW_FILE } from "./workflowmodel";
import type { PersistedPane, PersistedPaneKind } from "./tabstore";
import type { TabPaneInfo } from "./tabcounts";

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

/** What `setConnected` needs to render the cross-workspace channel chip/accent
 *  (#271). Built by channel.ts's `channelBadge` from the live `OrchChannel`/
 *  `orch-channel` payload — pane.ts stays a pure renderer of it, same division as
 *  `setBadge`/`PaneBadge` above. */
export interface PaneChannelBadge {
  channelId: string;
  /** Per-channel accent, so two concurrently-active channels read as visually
   *  distinct sets of panes (channel.ts's `channelColor`). */
  color: string;
  /** Short chip text, e.g. "⇄2" (channel.ts's `channelChipLabel`). */
  label: string;
  /** Other members' display names, for the chip's tooltip. */
  peers: string[];
  /** THIS pane's direction in the channel (#271 W3 addendum, part C): drives the
   *  chip's arrow — outward (▲) for the sender, inward (▼) for a receiver. */
  direction: "sender" | "receiver";
  /** Whether this pane can currently `channel_send` — always true for the
   *  sender; for a receiver, true only while it holds the reply credit. */
  canSend: boolean;
  /** True for a delivery-only member (no token, ever) — the "receive-only"
   *  chip variant, distinct from a full receiver simply out of credit right
   *  now (which still reads as a plain receiver — it WILL be able to reply). */
  deliveryOnly: boolean;
  /** The channel's current sender — agent id/name — or null if unresolved.
   *  Used by panemenu.ts's join-compatibility rule ("Join as receiver —
   *  driven by {sender}") and by `orchestration.ts` to fill
   *  `PaneConnectState.senderId`/`senderName`. */
  senderId: string | null;
  senderName: string | null;
}

export interface PaneOptions {
  name?: string;
  cwd?: string;
  command?: string;
  /** Which interactive shell a plain Terminal pane spawns (#194 P2). Only used
   *  for shell panes (no `command`/`argv`); omitted panes spawn PowerShell. */
  shellKind?: ShellKind;
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
  /** A STANDALONE pane's channel-scoped MCP identity (#271 W3 addendum, parts
   *  A/C): set by the launcher after `orch_solo_prepare`/`orch_solo_bind` for a
   *  newly-spawned pane. Deliberately NOT `orchGroup`/`orchRole`/`orchAgent` —
   *  those gate the full orchestration chrome (task board, audit button, group
   *  badge, steering strip), which a plain standalone pane must never show just
   *  because it can now join a channel. A pane with no identity at spawn time
   *  (launched before this feature, or adopted later) gets one via
   *  `setChannelAgent` post-construction instead. */
  channelAgent?: { group: string; agentId: string; role: string; canSend: boolean };
  /** Open without stealing keyboard focus (issue #117): an orchestrator-driven
   *  spawn must not yank the cursor from the pane the human is typing in. The
   *  human-initiated paths leave this unset (focus the new pane); only the
   *  orch-spawn-request path sets it. Grid.openPane resolves the actual
   *  decision — an empty grid still focuses regardless (see panefocus.ts). */
  background?: boolean;
  /** Recorded resumable agent session id (#194): so a restored Agent pane can
   *  `--resume <id>` back into its prior context (resuming into an idle TUI
   *  costs nothing until a prompt is sent). Set by the launcher for
   *  session-capable CLIs; absent for terminals/orchestration and best-effort
   *  CLIs. Retained for the layout snapshot — never used to drive the PTY. */
  sessionId?: string;
}

/** The PTY-less CONTENT pane kinds. A pane of one of these kinds IS a surface —
 *  a file manager (#214), the file editor, the git view (#217), or the workflow
 *  builder (#222) — rather than a process. They share every pane mechanic (split,
 *  dock, drag, maximize, restore) and differ only in which view fills the content box. */
export type ContentPaneKind = "files" | "editor" | "git" | "workflow";

/** What to CALL each content kind when a message has to name it ("the git view isn't
 *  available in a workflow pane"). A table rather than a ternary chain, so a fifth kind
 *  is a row and not a nested conditional nobody re-reads. */
const CONTENT_KIND_LABEL: Record<ContentPaneKind, string> = {
  files: "file explorer",
  editor: "file editor",
  git: "git",
  workflow: "workflow",
};

/** What a content pane needs: which surface, the root it is pointed at, and a name.
 *  Deliberately NOT part of PaneOptions — every field there describes a PTY spawn,
 *  and a content pane never has one. */
export interface ContentPaneOptions {
  kind: ContentPaneKind;
  name: string;
  /** Absolute path the surface is rooted at: the folder a manager lists / an editor
   *  trees, or a directory inside the repo a git view shows. Validated for real by
   *  the caller before we get here — `ftRootIsDir` for files/editor, `gitRepoRoot`
   *  for git — so this never builds a pane around a root that isn't what it claims. */
  root: string;
  /** EDITOR kind: a root-relative file to open immediately. Set by the file browser's
   *  "Open in file editor pane" (#217); absent from the welcome flow, which opens the
   *  editor on its tree with nothing selected.
   *
   *  WORKFLOW kind (#222): which workflow file to edit, root-relative. Defaults to
   *  `.loomux/workflow.yml` when absent — the welcome flow's case — and is set when the
   *  browser opens a *different* YAML as a workflow. */
  file?: string;
  /** Open without stealing keyboard focus (same contract as PaneOptions). */
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
  /** The pane's PERSISTED identity changed without any grid mutation, so the saved
   *  layout is now stale: a content pane was re-rooted, or a pane was renamed (#214).
   *  Nothing opened or closed, so no grid event fires and nothing would otherwise
   *  re-persist until the next unrelated one — meaning a quit right after a re-root
   *  would restore the OLD root. The host re-persists. */
  onRecordChanged: (pane: Pane) => void;
  /** Open an EDITOR pane beside `pane` (#217) — the file browser's "Open in file
   *  editor pane". The pane can't reach the grid itself (it doesn't know which tab
   *  it is in), so it asks its host, exactly as `onSplit` does for a welcome pane. */
  onOpenEditorPane: (pane: Pane, opts: { name: string; root: string; file?: string }) => void;
  /** Open a WORKFLOW pane beside `pane` (#222) — the file browser's "Open in workflow
   *  pane" on a YAML row. Same shape, same reason, as `onOpenEditorPane`. */
  onOpenWorkflowPane: (pane: Pane, opts: { name: string; root: string; file: string }) => void;
  /** Right-click on the pane header (#271): the pane can't build/show its own connect
   *  menu — that needs the cross-tab armed-connect state and the backend wrappers,
   *  neither of which a Pane knows about — so, like `onOpenEditorPane`, it asks its
   *  host. `x`/`y` are viewport coords for `showContextMenu`. */
  onPaneContextMenu: (pane: Pane, x: number, y: number) => void;
  /** The pane's own channel chip was clicked (#271's "easy close" requirement: a
   *  one-click disconnect from the indicator itself, not just the menu). */
  onDisconnectChannel: (pane: Pane) => void;
}

/** Every view that can occupy a pane's embed-panel slot (#361) — a real flex
 *  sibling of the terminal instead of a floating overlay. Deliberately
 *  excludes the file-editor overlay: it already has a strictly better
 *  embedding path (the editor CONTENT PANE, #217, a whole pane rather than a
 *  same-pane sub-panel) — see doc/design/embedded-panels.md. */
type EmbedKind = "tasks" | "git" | "issues" | "audit" | "group";

const EMBED_KINDS: readonly EmbedKind[] = ["tasks", "git", "issues", "audit", "group"];

/** The subset of `EmbedKind`s whose embed preference is captured for a whole-
 *  session-restart restore (`Pane.capture()` / `Pane.restoreEmbed`) — the
 *  orchestration-family views, which stay DORMANT across a restart and so
 *  have a natural "captured, then reapplied once the real pane exists" hook
 *  (`main.ts`'s `resumeDormantGroup`). git/issues are embeddable on every
 *  pane kind but have no equivalent hook today — see
 *  doc/design/embedded-panels.md. */
const RESTORABLE_EMBED_KINDS: readonly EmbedKind[] = ["tasks", "audit", "group"];

function isRestorableEmbedKind(kind: EmbedKind): kind is "tasks" | "audit" | "group" {
  return (RESTORABLE_EMBED_KINDS as readonly string[]).includes(kind);
}

/** One embeddable view's plumbing, registered once that view is lazily
 *  constructed. Lets the generic engine (`openView`/`closeView`/`toggleView`/
 *  `embedViewAtSide`/`reclampViewFloor`) treat all five views uniformly
 *  without hardcoding any one view's class. */
interface EmbedEntry {
  /** The view's own floating-overlay host (unchanged pre-#361 mechanics). */
  overlayEl: HTMLElement;
  /** The view's own root element — moved between `overlayEl` and whichever
   *  `EmbedSide`'s panel it's currently docked to. */
  viewEl: HTMLElement;
  /** Called every time the view becomes visible, in either mode. */
  show: () => void;
  /** Called every time the view is about to become hidden, in either mode —
   *  extra per-view cleanup beyond hiding its host (e.g. `GitView.hide()`
   *  dismisses an open context menu). Optional: most views need nothing
   *  beyond the generic hide. */
  hide?: () => void;
  /** Reflect whether this view is currently docked to ANY embed slot —
   *  updates the view's own header toggle button. Side-agnostic on purpose:
   *  the button reads "embedded" vs "floating," not which edge. */
  setPanelActive: (active: boolean) => void;
  /** The live floor (px) for the OVERLAY height clamp, and for the BOTTOM
   *  slot's own height floor specifically (unchanged from the
   *  pre-multi-slot design). Most views share the generic default
   *  (`EMBED_MIN_PANEL_PX`); the group panel measures its own fixed chrome
   *  (`Pane.groupFloor`). NOT used for the left/right slots' WIDTH floor —
   *  see `EMBED_MIN_PANEL_PX`'s own doc comment in embedsplit.ts for why
   *  that one deliberately stays a fixed constant instead. */
  floorPx: () => number;
}

/** One embed slot's live DOM + state — one instance per `EmbedSide`, created
 *  together in `ensureEmbedHost`. `kind`/`frac` are `null`/default when the
 *  slot is empty; `panelEl`/`dividerEl` exist permanently once created,
 *  hidden when empty (the same "create once, toggle `hidden`, never
 *  destroy" idiom every overlay in this file already uses). */
interface EmbedSlotState {
  side: EmbedSide;
  kind: EmbedKind | null;
  frac: number;
  panelEl: HTMLElement;
  dividerEl: HTMLElement;
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
  /** Every view registered as embeddable so far (#361), keyed by kind. Built
   *  lazily — one entry per view, added the first time that view's own
   *  `ensureXView()` runs — so a pane that never opens (say) the group panel
   *  never pays for its entry. The generic open/close/toggle engine below
   *  (`openView`/`closeView`/`toggleView`/`embedViewAtSide`/`unembedView`)
   *  treats every kind uniformly through this registry instead of hardcoding
   *  any one view's class — see doc/design/embedded-panels.md. */
  private embedRegistry = new Map<EmbedKind, EmbedEntry>();
  /** Up to THREE simultaneous embed slots — left, right, bottom (#361
   *  generalization from a single bottom-only slot) — each independently
   *  holding at most one view. `null` = nothing embedded anywhere; every
   *  view opens as its floating overlay by default (unchanged pre-#361
   *  behavior). Docking a kind that's already embedded elsewhere, or
   *  docking a DIFFERENT kind onto an already-occupied side, SWAPS that
   *  ONE slot's occupant (`embedViewAtSide`) — the other two slots are
   *  untouched either way. Created together, lazily, in `ensureEmbedHost`. */
  private embedSlots: Record<EmbedSide, EmbedSlotState> | null = null;
  /** Lazily created wrapper that turns `termEl` into a flex sibling of the
   *  left/right/bottom embed slots instead of the pane's direct flex:1
   *  child. Created once, on the first embed of ANY kind, and left in place
   *  afterward — with every slot's panel/divider `hidden`, `termEl` alone in
   *  the nested structure lays out identically to being `.pane`'s direct
   *  child (see `ensureEmbedHost`'s own doc comment for the exact
   *  structure, a two-level nesting: `embedHostEl` > [`embedRowEl`,
   *  bottom's divider + slot] and `embedRowEl` > [left's divider + slot,
   *  `embedCenterEl`], `embedCenterEl` > [`termEl`, right's divider +
   *  slot]). Bottom spans the row's full width rather than sitting only
   *  beside term — the simpler of the two corner-layout choices (see
   *  doc/design/embedded-panels.md's "Layout" section). Nested, not a flat
   *  5-child row, so every divider's two sides are a real, single DOM
   *  element pair (grid.ts's own nested-split-tree shape) — the left
   *  divider's far side is `embedCenterEl` as ONE element, not "term plus
   *  whatever's on the right," which is what keeps each divider's own
   *  drag math a plain two-element `embedDragGrow` call (see
   *  `dividerPair`/`dividerFloors`). */
  private embedHostEl: HTMLElement | null = null;
  private embedRowEl: HTMLElement | null = null;
  private embedCenterEl: HTMLElement | null = null;
  /** Fold-group toggle (orchestrator panes only, #46): minimizes every
   *  worker/reviewer pane in the group to the dock, or restores them all. */
  private groupMinBtn: HTMLButtonElement;
  /** Fullscreen toggle; its glyph flips to a restore affordance when active. */
  private maximizeBtn: HTMLButtonElement;
  private orchGroup: string | null = null;
  private orchRoleName: string | null = null;
  private orchAgent: string | null = null;
  /** Standalone pane's channel-scoped identity (#271 W3 addendum) — a carrier
   *  DELIBERATELY separate from orchGroup/orchAgent/orchRoleName (those gate
   *  the full orchestration chrome; a plain standalone pane must never show
   *  it just because it can now join a channel). Set at construction
   *  (`opts.channelAgent`) or later via `setChannelAgent` (adopt-on-connect). */
  private channelAgentInfo: { group: string; agentId: string; role: string; canSend: boolean } | null = null;
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
  /** "delivery held" chip in the header (#246): the moment loomux is
   *  withholding an outbound prompt to this pane because it believes the
   *  human's own input occupies the CLI's box. Hidden until the backend
   *  flags a hold in progress; cleared by its own paired event, never a
   *  frontend timer. Purely informational — unlike attnChip, clicking it
   *  does nothing to acknowledge/clear (only the backend resolving the hold
   *  does). */
  private heldChip: HTMLElement;
  private heldReason: string | null = null;
  /** Cross-workspace channel chip (#271): shown when this pane is a live channel
   *  member. Clicking it disconnects — the "easy close from the indicator itself"
   *  requirement — separate from the pane-menu Disconnect item, same destination. */
  private channelChip: HTMLButtonElement;
  private channelInfo: PaneChannelBadge | null = null;
  /** Notified when something the dock chip shows changes (attention state or
   *  the pane name); the grid uses it to keep a minimized pane's chip in sync,
   *  since a docked pane's header is out of the DOM (#6, #95r). */
  private dockSyncListener: (() => void) | null = null;
  /** True for agent/command panes (vs plain shells). */
  private launchedCommand = false;
  /** Whether this pane's pty has emitted a single byte since it spawned.
   *  Distinguishes a crash-with-real-output from one that died silently
   *  before printing anything — the DOA-revival signature (#281/#280) — so
   *  the exit banner (and #280's auto-close) can tell them apart. */
  private receivedOutput = false;
  /** Spawn inputs retained for the session-restore layout snapshot (#194): how
   *  this pane was launched, so `capture()` can serialize it. Record-only —
   *  never read back to drive the live PTY. */
  private spawnCommand: string | null = null;
  private spawnArgv: string[] | null = null;
  private spawnShellKind: ShellKind | null = null;
  private agentSessionId: string | null = null;
  private shiftTimer: number | undefined;
  private fit = new FitAddon();
  private resizeObs: ResizeObserver;
  private disposed = false;
  /** Welcome / pane-setup content (#194): a pane can exist with NO PTY, showing
   *  the setup form until the user picks a kind. The PTY spawns only on submit
   *  (`startFromWelcome`), so the no-resize invariant holds — there's nothing to
   *  resize before then. Null once the pane has become a real terminal. */
  private welcomeEl: HTMLElement | null = null;
  /** Dormant restore placeholder (#194 P4): a pane rebuilt from a persisted leaf
   *  that we deliberately did NOT auto-spawn — an agent CLI with no resumable id
   *  (a Start button), or an orchestration pane whose whole group stays dormant
   *  until the human resumes it (a Resume button). No PTY until acted on, so the
   *  no-resize invariant holds. `dormantRecord` is the leaf it was built from, so
   *  capture() re-serializes it verbatim: a session closed without resuming
   *  still offers the same restore next boot. Null for any live/welcome pane. */
  private dormantEl: HTMLElement | null = null;
  private dormantRecord: PersistedPane | null = null;
  /** CONTENT pane (#214 files, #217 editor + git): the pane's permanent content is a
   *  view — the file manager, the file editor, or the git view — rooted at
   *  `contentRoot`. No terminal is ever opened and no PTY ever spawns (the
   *  `startWelcome` precedent taken to its conclusion: a pane that is content, not a
   *  process), so the no-resize invariant holds trivially — there is no ConPTY to
   *  resize. `contentKind` non-null IS the "this is a content pane" flag; `contentRoot`
   *  doubles as the pane's cwd for the capture and for "open in editor". Exactly one
   *  of the four views below is non-null on such a pane; all are null elsewhere. */
  private contentKind: ContentPaneKind | null = null;
  private contentRoot: string | null = null;
  /** The file a content pane opened ON: the editor's open file, or the workflow pane's
   *  workflow file (#222). Null for the kinds whose only input is a root. */
  private contentFile: string | null = null;
  private filesView: FileExplorerView | null = null;
  private editorPaneView: FileEditView | null = null;
  private gitPaneView: GitView | null = null;
  private workflowPaneView: WorkflowView | null = null;
  /** True once the pane's process has exited but the pane was kept open to show
   *  its output (notifyExited). The counter must not count a dead agent as live
   *  (#194 P4 LOW-7). */
  private exited = false;
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

    // "Delivery held" chip (#246): purely informational, no click handler —
    // the hold only clears when the backend resolves it.
    this.heldChip = document.createElement("span");
    this.heldChip.className = "pane-held";
    this.heldChip.hidden = true;
    header.appendChild(this.heldChip);

    // Cross-workspace channel chip (#271): shown only while this pane is a live
    // channel member. Clicking it disconnects directly — the "easy close from the
    // indicator itself" requirement, distinct from (but going to the same place
    // as) the pane-menu's Disconnect item.
    this.channelChip = document.createElement("button");
    this.channelChip.className = "pane-channel";
    this.channelChip.hidden = true;
    this.channelChip.addEventListener("click", (e) => {
      e.stopPropagation();
      this.events.onDisconnectChannel(this);
    });
    header.appendChild(this.channelChip);

    // The connect gesture (#271): right-click anywhere on the header shows the
    // Connect/Disconnect menu. Buttons that already have their own contextmenu
    // handling (the editor button, below) call stopPropagation in theirs, so this
    // never double-fires for them. The pane itself can't build the menu — that
    // needs the cross-tab armed-connect state and the backend wrappers — so, like
    // `onOpenEditorPane`, it asks its host.
    header.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      this.events.onPaneContextMenu(this, (e as MouseEvent).clientX, (e as MouseEvent).clientY);
    });

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

    // The three overlay buttons below are `pty-only`: their panels FLOAT over the
    // terminal and are sized from its height, so they mean nothing on a pane that
    // has no terminal. CSS hides them on a files pane (#214) — see the toggles,
    // which refuse the hotkey path for the same reason.
    const issuesBtn = document.createElement("button");
    issuesBtn.className = "pane-btn pty-only";
    issuesBtn.innerHTML = ISSUES_ICON;
    issuesBtn.title = "GitHub issues (Alt+I)";
    issuesBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.toggleIssuesView();
    });
    header.appendChild(issuesBtn);

    const gitBtn = document.createElement("button");
    gitBtn.className = "pane-btn pty-only";
    gitBtn.innerHTML = GIT_ICON;
    gitBtn.title = "Git view (Alt+G)";
    gitBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.toggleGitView();
    });
    header.appendChild(gitBtn);

    // File-editor overlay (#174). Unconditional — every pane type gets it,
    // including plain terminals (unlike the orchestration-gated buttons above).
    // Except a files pane, which IS this surface already.
    this.fileEditBtn = document.createElement("button");
    this.fileEditBtn.className = "pane-btn pty-only";
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
      this.requestClose();
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
    // Retain the launch inputs for a later capture() into the persisted layout
    // (#194). Purely a record — the spawn below still reads them straight off opts.
    this.spawnCommand = opts.command ?? null;
    this.spawnArgv = opts.argv ?? null;
    this.spawnShellKind = opts.shellKind ?? null;
    this.agentSessionId = opts.sessionId ?? null;
    if (opts.badge) this.setBadge(opts.badge);
    if (opts.orchAgent) this.orchAgent = opts.orchAgent;
    if (opts.channelAgent) this.channelAgentInfo = opts.channelAgent;
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

    await this.attachPty(opts);
  }

  /** Spawn (or respawn) the PTY for this already-open terminal and wire output /
   *  git-watch / the ordered input writer to it. Split out of `start` so the
   *  fresh-session backstop (`respawnFresh`) can reuse it without re-opening the
   *  terminal (#194 BUG-1). */
  private async attachPty(opts: PaneOptions): Promise<void> {
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
        shellKind: opts.shellKind,
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
      attachOutput(ptyId, (bytes) => {
        this.receivedOutput ||= bytes.length > 0;
        this.term.write(bytes);
      });
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

  /** Respawn this pane with a FRESH process in place, reusing the already-open
   *  terminal — the runtime backstop when a resumed agent's `--resume` exited on
   *  a missing/deleted conversation (#194 BUG-1). Same pane, position, and cwd;
   *  clears the dead error text and starts `opts`' command with a fresh ordered
   *  writer bound to the new PTY. Not itself a resume, so it can't re-trigger the
   *  fallback (the caller also makes the fallback one-shot). */
  async respawnFresh(opts: PaneOptions = {}): Promise<void> {
    if (this.disposed) return;
    this.exited = false;
    this.ptyId = null;
    if (opts.name) this.setName(opts.name);
    this.launchedCommand = !!opts.command?.trim();
    this.spawnCommand = opts.command ?? null;
    this.spawnArgv = opts.argv ?? null;
    this.spawnShellKind = opts.shellKind ?? null;
    this.agentSessionId = opts.sessionId ?? null;
    if (opts.cwd) {
      this.cwdRaw = opts.cwd;
      void this.refreshDir(opts.cwd);
    }
    this.term.reset(); // wipe the "No conversation found …" error + resume banner
    this.writer = createOrderedWriter(); // a fresh input pipe for the new PTY
    await this.attachPty(opts);
  }

  /** Render the welcome / pane-setup surface in this pane instead of a terminal
   *  (#194). No terminal is opened and no PTY is spawned — the pane is inert
   *  content until the user submits, so nothing can trigger a ConPTY resize
   *  before then (constraint 1). `formEl` is the welcome form's root DOM. */
  startWelcome(formEl: HTMLElement): void {
    this.setName("welcome");
    this.el.classList.add("is-welcome");
    const wrap = document.createElement("div");
    wrap.className = "pane-welcome";
    wrap.appendChild(formEl);
    this.welcomeEl = wrap;
    this.el.appendChild(wrap);
  }

  /** True while this pane is showing the welcome form (no PTY yet). */
  get isWelcome(): boolean {
    return this.welcomeEl !== null;
  }

  /** Focus the welcome form's preferred initial control (its `data-initial-focus`
   *  marker — the repository field — falling back to the first focusable). Harmless
   *  on a hidden tab: focusing inside a `display:none` subtree is a no-op. */
  focusWelcome(): void {
    const el =
      this.welcomeEl?.querySelector<HTMLElement>("[data-initial-focus]") ??
      this.welcomeEl?.querySelector<HTMLElement>("select, input, button");
    el?.focus();
  }

  /** Convert a welcome pane into a real terminal: tear down the setup surface and
   *  spawn the chosen kind in place. The PTY is created here — its first and only
   *  spawn — so the welcome-before-PTY flow never resizes anything. */
  async startFromWelcome(opts: PaneOptions = {}): Promise<void> {
    this.welcomeEl?.remove();
    this.welcomeEl = null;
    this.el.classList.remove("is-welcome");
    await this.start(opts, true);
  }

  /** Turn this pane into a CONTENT pane (#214 files, #217 editor + git): its content
   *  becomes one of three views, rooted at `opts.root` —
   *
   *    files  — FileExplorerView: a native-style file MANAGER (browse, open with the
   *             OS default app, new folder/file, rename, delete, jump-to-file). NOT
   *             the in-app editor: the human's ruling on #214 is that a .png belongs
   *             in an image viewer and a .pdf in a PDF reader.
   *    editor — FileEditView: the #174 file tree + code editor + #207 streaming
   *             search, EMBEDDED (no ✕, no Esc-to-close; the pane's ✕ closes it, and
   *             asks first when a buffer is dirty — see confirmClose).
   *    git    — GitView: graph, status, diffs, staging, #208 worktree switching, over
   *             the repo `root` names. Embedded on the same terms.
   *
   *  Used both to convert a welcome pane in place (the user picked the kind) and to
   *  open one directly on restore or from the browser's "open in editor pane".
   *
   *  No terminal is opened and no PTY is ever spawned, so:
   *   - nothing can resize a ConPTY from here (constraint 1 holds by construction —
   *     there is no ConPTY);
   *   - `.pane-term` stays in the layout but empty, and `.pane-content` covers it the
   *     way `.pane-welcome` does, so the pane's own chrome (splits, dock, maximize)
   *     works unchanged;
   *   - the PTY-dependent chrome (folder + branch chips, the git/issues/file-editor
   *     overlay buttons — all of which float over the TERMINAL and are sized from it)
   *     is hidden via `.is-content` rather than left clickable and inert.
   *
   *  Each view fills the content box and lays ITSELF out (all three are `flex: 1`,
   *  and GitView re-clamps its sub-panes against its own live size via its own
   *  ResizeObserver) — which is the whole of the "second sizing model" the git view
   *  needed to become pane content: a box, not a terminal to measure.
   *
   *  `root` must already be what it claims — a readable directory (files/editor) or a
   *  git work tree (git) — validated by the caller at setup and again at restore. */
  startContent(opts: ContentPaneOptions): void {
    this.welcomeEl?.remove(); // converting a setup pane in place
    this.welcomeEl = null;
    this.el.classList.remove("is-welcome");
    // ONE class for all three kinds: the chrome they hide is identical (everything that
    // describes a shell or floats over a terminal), and the surfaces style themselves.
    // A per-kind class would be a hook with nothing on the other end of it.
    this.el.classList.add("is-content");
    this.contentKind = opts.kind;
    this.contentFile = opts.file ?? null;
    this.setContentRoot(opts.root);
    this.setName(opts.name);

    const view = this.buildContentView(opts);
    const wrap = document.createElement("div");
    wrap.className = "pane-content";
    wrap.appendChild(view.el);
    this.el.appendChild(wrap);
    // ATTACH, THEN show. `GitView.show()` clamps its sub-panes against its container's
    // live size, so showing it before it is in the document would measure a zero-width
    // box. (Its ResizeObserver would recover on the next frame, but a view that has to
    // be rescued by a resize event is a view that flashes wrong first.)
    view.show();
    // The editor pane may have been opened ON a file (the browser's "open in editor
    // pane"). `openPath` waits for the listing show() just kicked off, so the reveal
    // lands in the tree that ends up on screen rather than racing it.
    if (opts.file && this.editorPaneView) void this.editorPaneView.openPath(opts.file);
    if (!opts.background) this.focus();
  }

  /** Construct the view a content pane hosts (not shown yet — see startContent). Split
   *  out so the per-kind wiring reads as three cases, not one branching block. */
  private buildContentView(opts: ContentPaneOptions): { el: HTMLElement; show(): void } {
    // Re-rooting from a view's own folder picker re-roots the PANE, so the persisted
    // record follows and a restore reopens what was actually on screen.
    //
    // The TITLE follows only if it was auto-derived from the old root — the same
    // "don't clobber what the human typed" rule the welcome form's name field uses
    // (nameDirty). A pane the user renamed to "docs" keeps that name across a re-root;
    // one still called "loomux" (its old folder) becomes the new folder, instead of
    // sitting there naming a directory it no longer shows.
    const adoptRoot = (root: string): void => {
      const autoNamed = this.name === this.defaultContentName(this.contentRoot);
      this.setContentRoot(root);
      if (autoNamed) this.setName(this.defaultContentName(root));
      // Re-persist NOW. No grid event fired (nothing opened or closed), so without this
      // the new root would sit unsaved until some unrelated layout change came along —
      // and a quit in between would restore the old one (rev-99 finding 4).
      this.events.onRecordChanged(this);
    };

    if (opts.kind === "files") {
      this.filesView = new FileExplorerView({
        getRoot: () => this.contentRoot ?? "",
        onRootChanged: adoptRoot,
        // Right-click → "Open in file editor pane" (#217): an editor pane beside this
        // one, rooted where this browser is rooted, with the clicked file open. The
        // browser hands over a root-relative path and stays exactly where it was.
        onOpenEditorPane: (req) =>
          this.events.onOpenEditorPane(this, {
            name: req.file ? pathTail(req.file) : this.defaultContentName(req.root) || "editor",
            root: req.root,
            file: req.file ?? undefined,
          }),
        // Right-click a .yml → "Open in workflow pane" (#222). Named after the FILE, like
        // the editor pane is: a pane called "workflow.yml" says what it is showing, while
        // one called after the repo would collide with every other pane in it.
        onOpenWorkflowPane: (req) =>
          this.events.onOpenWorkflowPane(this, {
            name: pathTail(req.file) || "workflow",
            root: req.root,
            file: req.file,
          }),
      });
      return this.filesView;
    }

    if (opts.kind === "editor") {
      this.editorPaneView = new FileEditView({
        getCwd: () => this.contentRoot,
        // Never called: `embedded` drops the ✕ and the Esc binding, which are the only
        // two things that request a close. The pane's own ✕ is the close affordance.
        onClose: () => {},
        embedded: true,
        onRootChanged: adoptRoot,
      });
      return this.editorPaneView;
    }

    if (opts.kind === "workflow") {
      // The workflow file rides in `file`, exactly as the editor's open file does, so the
      // capture/restore path needed no new field (tabstore.ts). Absent = the default
      // `.loomux/workflow.yml`, which is what the welcome form creates.
      this.workflowPaneView = new WorkflowView({
        getRoot: () => this.contentRoot,
        getFile: () => this.contentFile ?? WORKFLOW_FILE,
        onClose: () => {}, // never called — see the editor's note above
        embedded: true,
      });
      return this.workflowPaneView;
    }

    this.gitPaneView = new GitView({
      getCwd: () => this.contentRoot,
      onClose: () => {}, // never called — see the editor's note above
      embedded: true,
    });
    return this.gitPaneView;
  }

  /** True when this pane is a PTY-less content pane (#214 files, #217 editor / git) —
   *  no PTY, ever. The kind itself stays private: nothing outside needs to know WHICH
   *  surface it is, and the moment something does, it should ask a question about the
   *  behavior it cares about rather than switch on the kind. */
  get isContent(): boolean {
    return this.contentKind !== null;
  }

  /** The human asked to close this pane — from its header ✕, its dock chip's ✕, or
   *  Ctrl+Shift+W. THE single entry point for a human-initiated single-pane close, so
   *  every affordance goes through one path instead of re-deriving it (the dock chip's
   *  ✕ used to call `grid.closePane` directly — #214 rev-100).
   *
   *  It runs the unsaved-edits guard (`confirmClose`) HERE, and only tells the host to
   *  close once the human has said yes. Anything calling `grid.closePane` directly
   *  bypasses that guard — exactly the bug this method exists to prevent, now that a
   *  pane can hold a dirty buffer (an editor pane's, or an Alt+F overlay's).
   *
   *  `closing` is a one-shot latch, and it is load-bearing: the guard is ASYNC (a
   *  modal), while the app's shortcut handler is registered capture-phase on
   *  `document` — so a second Ctrl+Shift+W while the discard dialog is up still
   *  reaches this method. Without the latch it would stack a second identical dialog
   *  for the same pane, and answering both would re-enter `closePane` on a disposed
   *  pane. Released on a declined close so the human can try again.
   *
   *  Automatic closes (a PTY exiting, a group ending, a tab disposing) deliberately do
   *  NOT come here — they are bulk operations with their own semantics, not "the human
   *  closed this pane". The tab-close path asks its own question (see
   *  `hasUnsavedWork` / tabbar's arm-and-confirm). */
  requestClose(): void {
    if (this.closing || this.disposed) return;
    this.closing = true;
    void this.confirmClose().then(
      (ok) => {
        this.closing = false;
        if (ok && !this.disposed) this.events.onCloseRequest(this);
      },
      () => {
        this.closing = false; // a failed dialog must not wedge the pane shut
      }
    );
  }

  /** True while a close request is waiting on the human's answer. */
  private closing = false;

  /** The view in this pane that owns unsaved work, if any — asked ONCE, here, so every
   *  guard (close, tab-close, app-quit, a dead process) sees the same set of holders and
   *  a new one cannot be wired into two of the four and forgotten in the others.
   *
   *  THREE can hold a buffer: the editor PANE (#217 — the pane IS the buffer), the
   *  WORKFLOW pane (#222 — same, over `.loomux/workflow.yml`), and the Alt+F OVERLAY
   *  inside any terminal/agent pane (#174) — the likeliest one, because it is the one you
   *  forget you left open. They share the contract (`dirty` / `canDiscard` / `bufferReport`)
   *  rather than a base class: it is three methods, and the shared gate that matters is
   *  the pure one in dirtystate.ts. */
  private unsavedHolder(): {
    readonly dirty: boolean;
    canDiscard(): Promise<boolean>;
    bufferReport(): { file: string | null; dirty: boolean } | null;
  } | null {
    return this.editorPaneView ?? this.workflowPaneView ?? this.fileEditView;
  }

  /** May the human close this pane right now? True unless it holds unsaved work they
   *  then decline to discard. */
  async confirmClose(): Promise<boolean> {
    const holder = this.unsavedHolder();
    return holder ? holder.canDiscard() : true;
  }

  /** Does this pane hold unsaved edits RIGHT NOW — without asking the human anything?
   *  `confirmClose` prompts; this only reports. The tab-close path needs the fact
   *  before it can decide how to ask (a tab holding unsaved work closes behind the
   *  same arm-and-confirm an orchestration tab does — tabbar.ts), and a question that
   *  pops its own modal is no use to a synchronous bulk teardown. */
  hasUnsavedWork(): boolean {
    return this.unsavedHolder()?.dirty ?? false;
  }

  /** Point this content pane at `root`, keeping `cwdRaw` in step so the chrome that
   *  legitimately works without a PTY — "open in editor", the capture's cwd —
   *  targets the folder actually on screen. */
  private setContentRoot(root: string): void {
    this.contentRoot = root;
    this.cwdRaw = root;
  }

  /** The title a content pane gets when nobody has named it: the root's short name.
   *  The SAME derivation the welcome form uses, so `name === defaultContentName(root)`
   *  reliably means "this title was auto-derived, not typed by the human". */
  private defaultContentName(root: string | null): string {
    return root ? pathTail(root) || root : "";
  }

  /** This pane's working directory: a shell's live (OSC 7) cwd, an agent's launch
   *  folder, or a files pane's root. Null when it has none yet (a welcome pane).
   *  Used to seed a split's welcome form with the folder you split FROM. */
  get workdir(): string | null {
    return this.cwdRaw;
  }

  /** Render a DORMANT restore placeholder (#194 P4): no terminal, no PTY, just
   *  `contentEl` (a Start/Resume affordance the caller wires). `record` is the
   *  persisted leaf this pane stands in for, retained so capture() re-serializes
   *  it unchanged — a restore left dormant persists identically for next boot.
   *  Nothing here spawns anything, honoring the group no-double-spawn contract. */
  startDormant(record: PersistedPane, contentEl: HTMLElement): void {
    this.setName(record.name);
    this.el.classList.add("is-dormant");
    const wrap = document.createElement("div");
    wrap.className = "pane-dormant";
    wrap.appendChild(contentEl);
    this.dormantEl = wrap;
    this.dormantRecord = record;
    this.el.appendChild(wrap);
  }

  /** True while this pane is a dormant restore placeholder (no PTY yet). */
  get isDormant(): boolean {
    return this.dormantEl !== null;
  }

  /** The kind of a dormant placeholder ("agent" | "orch"), or null when not
   *  dormant. Lets the grid/tab-bar tell a dormant Start pane from a dormant
   *  group pane without re-reading the whole record. */
  get dormantKind(): PersistedPaneKind | null {
    return this.dormantRecord?.paneKind ?? null;
  }

  /** Convert a dormant agent placeholder into a live pane when the human clicks
   *  Start: tear down the placeholder and spawn the recorded command in place.
   *  Only used for `dormant-agent` (no-session CLIs) — a dormant GROUP is revived
   *  through resumeOrchSession, never here (the double-spawn contract). */
  async startFromDormant(opts: PaneOptions = {}): Promise<void> {
    this.dormantEl?.remove();
    this.dormantEl = null;
    this.dormantRecord = null;
    this.el.classList.remove("is-dormant");
    await this.start(opts, true);
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
    // No open terminal = a welcome, dormant, or files pane. WebglAddon.activate()
    // throws on such a terminal (caught below, but pointlessly) and there is nothing
    // to render anyway — setHidden() reaches here on every tab switch, so this is a
    // real throw/catch per hidden PTY-less pane, not a hypothetical.
    if (this.webgl || this.hiddenTab || !this.hasTerminal()) return;
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
    // A pane whose terminal was never opened (welcome / dormant / files) has no
    // viewport to serialize — an empty string leaves the tab preview blank for
    // that slot rather than painting a phantom 80×24 of nothing.
    if (this.disposed || !this.hasTerminal()) return "";
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

  /** Flag (or clear) that loomux is currently withholding a prompt delivery
   *  to this pane because it believes the human's own input occupies the
   *  CLI's box (#246) — driven by the backend's paired
   *  orch-delivery-held / orch-delivery-held-cleared events. Idempotent on
   *  the reason, same as `setAttention`. `null` clears the badge. Header
   *  chrome only: this never touches the pane's size, so the no-PTY-resize
   *  invariant holds trivially. */
  setHeld(reason: string | null, detail?: string): void {
    if (reason === this.heldReason) return;
    this.heldReason = reason;
    if (!reason) {
      this.heldChip.hidden = true;
      delete this.heldChip.dataset.reason;
    } else {
      const { label } = heldPresentation(reason);
      this.heldChip.textContent = label;
      this.heldChip.title = detail ?? "Prompt delivery paused — human input occupies this pane's box";
      this.heldChip.dataset.reason = reason;
      this.heldChip.hidden = false;
    }
  }

  /** Mark (or clear) this pane's cross-workspace channel membership (#271): a
   *  colored/numbered chip before the title plus a `--connect-color` accent, so
   *  panes on either end of a channel — and a third pane joined into it — read as
   *  one connected set even across tabs. `null` clears it (disconnect/teardown).
   *
   *  #271 W3 addendum, part C: the chip also carries a DIRECTION arrow — ▲
   *  (outward) for the sender, ▼ (inward) for a receiver — and a distinct
   *  `receive-only` CSS variant for a delivery-only member (no token, ever),
   *  so the direction and the honest capability both read at a glance. */
  setConnected(info: PaneChannelBadge | null): void {
    this.channelInfo = info;
    if (!info) {
      this.channelChip.hidden = true;
      this.el.classList.remove("connected");
      this.el.style.removeProperty("--connect-color");
      delete this.channelChip.dataset.channel;
      delete this.channelChip.dataset.direction;
      this.channelChip.classList.remove("receive-only");
    } else {
      const arrow = info.direction === "sender" ? "▲" : "▼";
      this.channelChip.textContent = `${arrow} ${info.label}`;
      const capability = info.deliveryOnly
        ? "receive-only — it has no channel token"
        : info.direction === "sender"
          ? "you are the SENDER — you may message anyone connected, any time"
          : info.canSend
            ? "you are a RECEIVER with a reply credit — you may answer the sender now"
            : "you are a RECEIVER — you may answer once the sender messages you";
      this.channelChip.title = `Channel ${info.channelId} — connected to ${
        info.peers.join(", ") || "…"
      } (${capability}). Click to disconnect.`;
      this.channelChip.dataset.channel = info.channelId;
      this.channelChip.dataset.direction = info.direction;
      this.channelChip.classList.toggle("receive-only", info.deliveryOnly);
      this.channelChip.hidden = false;
      this.el.classList.add("connected");
      this.el.style.setProperty("--connect-color", info.color);
    }
    // A minimized pane's element is detached; mirror to the dock chip (#95r's
    // precedent, same listener setAttention/setName already use).
    this.dockSyncListener?.();
  }

  /** The channel this pane currently belongs to, or null — panemenu.ts's
   *  `PaneConnectState.channelId` reads this to decide Connect vs. Disconnect. */
  get channelId(): string | null {
    return this.channelInfo?.channelId ?? null;
  }

  /** Current channel badge, or null — lets the grid render an equivalent
   *  indicator on the dock chip while this pane is minimized, mirroring the
   *  `attention` getter just above. */
  get channelBadge(): PaneChannelBadge | null {
    return this.channelInfo;
  }

  /** Toggle the "armed connect source" visual (#271's "visible pending state"
   *  requirement) — the persistent cue that THIS pane is what a right-click
   *  elsewhere will complete a channel against, until it's completed or
   *  cancelled (self-click or Esc). Never touches the channel chip itself: a
   *  free pane being armed has no channel yet. */
  setPendingConnect(pending: boolean): void {
    this.el.classList.toggle("connect-pending", pending);
  }

  /** Has `dispose()` already run? (#271 review finding 1.) Orchestration.ts's
   *  module-level "armed connect source" reference has no dispose hook of its
   *  own — it's a plain `Pane` object reference, so a closed pane doesn't
   *  un-arm itself — so the connect-menu wiring checks this lazily, on the
   *  next menu-open, rather than needing a new close callback. Deliberately
   *  NOT `!this.el.isConnected`: a MINIMIZED (docked) pane also detaches its
   *  `.el` from the DOM while very much still alive and a valid connect
   *  target, so DOM attachment can't stand in for "this pane still exists". */
  get isDisposed(): boolean {
    return this.disposed;
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

  /** Refuse an overlay on a CONTENT pane (#214/#217), with a reason.
   *
   *  Every pane overlay (git, issues, tasks, audit, group, file editor) floats over
   *  `.pane-term` and takes its height from it — `overlayClamp` measures
   *  `termEl.clientHeight`, and `updateTermShift` reads the live `.xterm-screen` to
   *  keep the cursor visible under the panel. A content pane has no terminal at all,
   *  so those measurements have no meaning and the panel would open into a zero-height
   *  box. They are therefore cleanly OFF there (buttons hidden by `.is-content`,
   *  hotkeys answered with this) rather than half-working.
   *
   *  #214 deferred "the git view over a files root" to a second overlay sizing model.
   *  #217 answers it by the other road, and the answer is why this refusal can stay:
   *  you don't overlay a git view onto a content pane, you OPEN A GIT PANE (the view
   *  as pane content, sized by the pane's own box). The surfaces that needed a
   *  terminal underneath still say so; the ones that never did are now panes. */
  private refuseOverlay(what: string): boolean {
    if (!this.isContent) return false;
    const kind = CONTENT_KIND_LABEL[this.contentKind!];
    showToast(`${what} isn't available in a ${kind} pane.`, "info");
    return true;
  }

  /** Toggle the git view. It FLOATS over the top of the terminal — the
   *  terminal keeps its full size and PTY dimensions, so toggling never
   *  triggers a resize repaint (which would push duplicate TUI frames into
   *  scrollback). The bottom strip of the terminal stays visible and usable,
   *  with a draggable divider on the overlay's lower edge. */
  toggleGitView(): void {
    // Alt+G on a git pane: the pane already IS the git view. Refusing with a toast
    // would be absurd — just put the cursor in it.
    if (this.contentKind === "git") {
      this.focus();
      return;
    }
    if (this.refuseOverlay("The git view")) return;
    this.ensureGitView();
    this.toggleView("git");
  }

  /** Lazily construct the git view and register it into `embedRegistry`
   *  (#361) — the error-recovery `openView` wraps every `show()` in (never
   *  leave the pane half-toggled) generalizes what was originally a
   *  git-specific `try`/`catch` here, since a `refresh()` failure was the
   *  one case any view's `show()` could throw synchronously. */
  private ensureGitView(): void {
    if (this.gitView) return;
    this.gitView = new GitView({
      getCwd: () => this.cwdRaw,
      onClose: () => this.toggleGitView(),
      onRepoAction: () => {
        if (this.cwdRaw) void this.refreshDir(this.cwdRaw);
      },
      onEmbedMenu: (anchor) => this.showEmbedMenu("git", anchor),
    });
    this.gitOverlay = document.createElement("div");
    this.gitOverlay.className = "git-overlay";
    this.gitOverlay.hidden = true;
    this.gitOverlay.append(this.gitView.el, this.makeOverlayDivider(() => this.gitOverlay!));
    this.el.appendChild(this.gitOverlay);
    this.embedRegistry.set("git", {
      overlayEl: this.gitOverlay,
      viewEl: this.gitView.el,
      show: () => this.gitView!.show(),
      hide: () => this.gitView!.hide(),
      setPanelActive: (active) => this.gitView!.setPanelActive(active),
      floorPx: () => EMBED_MIN_PANEL_PX,
    });
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

  /** Re-apply `kind`'s floor to whichever host it's CURRENTLY shown in (#361
   *  generalizes what was originally `reclampGroupOverlay`, the only view
   *  whose floor can grow after it opens — the suspended banner appearing
   *  inside the group panel, #83 rev-58). Only touches the host when the
   *  floor actually moves it — typically a bump UP — so it never fights the
   *  human's chosen size. In embed mode this nudges the divider's flex-grow
   *  via `embedDragGrow` with a zero delta, which — because a size already
   *  BELOW the new floor makes `sizePanel - minPanelPx` negative — still
   *  produces exactly the corrective nudge; see embedsplit.ts. */
  private reclampViewFloor(kind: EmbedKind): void {
    const side = this.sideOf(kind);
    if (side) {
      this.reclampSlotDivider(side);
      return;
    }
    const entry = this.embedRegistry.get(kind);
    if (!entry || entry.overlayEl.hidden) return;
    const cur = entry.overlayEl.offsetHeight;
    const clamped = this.overlayClamp(cur, entry.floorPx());
    if (clamped !== cur) {
      entry.overlayEl.style.height = `${clamped}px`;
      this.updateTermShift();
    }
  }

  /** Which `EmbedSide` (if any) `kind` currently occupies. Only three sides
   *  exist, so a linear scan is simpler and safer than keeping a second,
   *  separately-maintained reverse-lookup map in sync with `embedSlots`. */
  private sideOf(kind: EmbedKind): EmbedSide | null {
    if (!this.embedSlots) return null;
    for (const side of EMBED_SIDES) {
      if (this.embedSlots[side].kind === kind) return side;
    }
    return null;
  }

  /** The OTHER element in `side`'s divider pair — i.e. not the slot's own
   *  panel. Left's counterpart is the composite `embedCenterEl`; right's and
   *  bottom's are the plain `termEl` / `embedRowEl` (see `ensureEmbedHost`'s
   *  doc comment for the nested structure this reflects). */
  private counterpartEl(side: EmbedSide): HTMLElement {
    switch (side) {
      case "left":
        return this.embedCenterEl!;
      case "right":
        return this.termEl;
      case "bottom":
        return this.embedRowEl!;
    }
  }

  /** `side`'s divider pair as `{beforeEl, afterEl}` (the two elements a drag
   *  redistributes flex-grow between) plus which screen axis it drags along.
   *  "Before" is whichever element sits physically before the divider in
   *  reading order — left's own slot for `"left"` (dragging right grows it),
   *  the terminal for `"right"` (dragging right grows IT, shrinking the
   *  slot), the row for `"bottom"` (dragging down grows it). Matches
   *  `embedDragGrow`'s convention (`before` grows with a positive delta)
   *  exactly, mirroring grid.ts's own split-divider math. */
  private dividerPair(side: EmbedSide): { beforeEl: HTMLElement; afterEl: HTMLElement; horizontal: boolean } {
    const slot = this.embedSlots![side];
    switch (side) {
      case "left":
        return { beforeEl: slot.panelEl, afterEl: this.embedCenterEl!, horizontal: true };
      case "right":
        return { beforeEl: this.termEl, afterEl: slot.panelEl, horizontal: true };
      case "bottom":
        return { beforeEl: this.embedRowEl!, afterEl: slot.panelEl, horizontal: false };
    }
  }

  /** The `embedDragGrow` floor pair for `side`'s divider, evaluated LIVE
   *  (the group panel's floor can grow after it opens; the left divider's
   *  far-side floor depends on whether right is CURRENTLY occupied). See
   *  embedsplit.ts's `embedSideFloors`/`embedCenterFloor` for the actual
   *  precedence math this only ever plugs live values into. */
  private dividerFloors(side: EmbedSide): { beforeFloorPx: number; afterFloorPx: number } {
    if (side === "left") {
      const right = this.embedSlots!.right;
      const rightFloorPx = right.kind !== null ? EMBED_MIN_PANEL_PX : null;
      return { beforeFloorPx: EMBED_MIN_PANEL_PX, afterFloorPx: embedCenterFloor(rightFloorPx) };
    }
    if (side === "right") return embedSideFloors("right", EMBED_MIN_PANEL_PX);
    // bottom
    const bottomKind = this.embedSlots!.bottom.kind;
    const panelFloorPx = bottomKind ? this.embedRegistry.get(bottomKind)!.floorPx() : EMBED_MIN_PANEL_PX;
    return embedSideFloors("bottom", panelFloorPx);
  }

  /** Set `side`'s divider pair directly from the slot's own PANEL share
   *  (`frac` — always "how much of the pair the panel itself gets," never
   *  "before" or "after," since which one the panel physically is differs
   *  per side). Position-agnostic on purpose: `flex-grow` only encodes a
   *  ratio, not which sibling is which, so this never needs to know
   *  before/after itself — only `dividerPair`'s drag handler does. */
  private applySlotGrow(side: EmbedSide, frac: number): void {
    const slot = this.embedSlots![side];
    const clamped = clampEmbedFrac(frac);
    slot.panelEl.style.flex = `${clamped} 1 0`;
    this.counterpartEl(side).style.flex = `${1 - clamped} 1 0`;
  }

  /** Re-apply `side`'s CURRENT floors to its CURRENT sizes (a zero-delta
   *  "drag") — the correction for a floor that grew (or, for left, whose
   *  composed far-side floor changed because right's occupancy changed)
   *  since the slot was last sized. Only touches it when the clamp actually
   *  moves it, so it never fights the human's chosen size. Zero delta still
   *  produces a real nudge when a side is already below its (possibly new)
   *  floor, because `sizeAfter - minAfterPx` (or the before equivalent) goes
   *  negative in `embedDragGrow`'s own clamp — see embedsplit.ts. */
  private reclampSlotDivider(side: EmbedSide): void {
    if (!this.embedSlots) return;
    const slot = this.embedSlots[side];
    if (slot.kind === null || slot.panelEl.hidden) return;
    const { beforeEl, afterEl, horizontal } = this.dividerPair(side);
    const sizeBefore = horizontal ? beforeEl.offsetWidth : beforeEl.offsetHeight;
    const sizeAfter = horizontal ? afterEl.offsetWidth : afterEl.offsetHeight;
    const growBefore = parseFloat(beforeEl.style.flexGrow || "1");
    const growAfter = parseFloat(afterEl.style.flexGrow || "1");
    const { beforeFloorPx, afterFloorPx } = this.dividerFloors(side);
    const grow = embedDragGrow(sizeBefore, sizeAfter, growBefore, growAfter, 0, beforeFloorPx, afterFloorPx);
    if (grow.growBefore === growBefore && grow.growAfter === growAfter) return;
    beforeEl.style.flex = `${grow.growBefore} 1 0`;
    afterEl.style.flex = `${grow.growAfter} 1 0`;
    const panelGrow = parseFloat(slot.panelEl.style.flexGrow || "1");
    const counterpartGrow = parseFloat(this.counterpartEl(side).style.flexGrow || "1");
    slot.frac = fracFromGrow(counterpartGrow, panelGrow);
    this.updateTermShift();
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
    if (this.refuseOverlay("The issues view")) return;
    this.ensureIssuesView();
    this.toggleView("issues");
  }

  /** Lazily construct the issues view and register it into `embedRegistry`
   *  (#361). */
  private ensureIssuesView(): void {
    if (this.issuesView) return;
    this.issuesView = new IssuesView({
      getCwd: () => this.cwdRaw,
      onClose: () => this.toggleIssuesView(),
      onEmbedMenu: (anchor) => this.showEmbedMenu("issues", anchor),
    });
    this.issuesOverlay = document.createElement("div");
    this.issuesOverlay.className = "git-overlay";
    this.issuesOverlay.hidden = true;
    this.issuesOverlay.append(
      this.issuesView.el,
      this.makeOverlayDivider(() => this.issuesOverlay!)
    );
    this.el.appendChild(this.issuesOverlay);
    this.embedRegistry.set("issues", {
      overlayEl: this.issuesOverlay,
      viewEl: this.issuesView.el,
      show: () => this.issuesView!.show(),
      hide: () => this.issuesView!.hide(),
      setPanelActive: (active) => this.issuesView!.setPanelActive(active),
      floorPx: () => EMBED_MIN_PANEL_PX,
    });
  }

  /** Toggle the task board open/closed (`Alt+T`, and the board's own ✕) — in
   *  EITHER mode. Which mode/side it opens in is a separate, persisted
   *  preference (see `embedViewAtSide`); this never changes it. */
  toggleTasksView(): void {
    if (!this.orchGroup || this.tasksBtn.hidden) return;
    this.ensureTasksView();
    this.toggleView("tasks");
  }

  /** Lazily construct the task board and register it into `embedRegistry`
   *  (#361). */
  private ensureTasksView(): void {
    if (this.tasksView) return;
    this.tasksView = new TasksView(this.orchGroup!, {
      onClose: () => this.toggleTasksView(),
      onEmbedMenu: (anchor) => this.showEmbedMenu("tasks", anchor),
    });
    this.tasksOverlay = document.createElement("div");
    this.tasksOverlay.className = "git-overlay";
    this.tasksOverlay.hidden = true;
    this.tasksOverlay.append(this.tasksView.el, this.makeOverlayDivider(() => this.tasksOverlay!));
    this.el.appendChild(this.tasksOverlay);
    this.embedRegistry.set("tasks", {
      overlayEl: this.tasksOverlay,
      viewEl: this.tasksView.el,
      show: () => this.tasksView!.show(),
      setPanelActive: (active) => this.tasksView!.setPanelActive(active),
      floorPx: () => EMBED_MIN_PANEL_PX,
    });
  }

  // ==================== #361: the generic embed engine ====================
  // Shared by every EmbedKind (tasks/git/issues/audit/group) through
  // `embedRegistry` — see doc/design/embedded-panels.md for the full design,
  // including why this is the legitimate side of the no-PTY-resize-for-chrome
  // rule (CLAUDE.md constraint 1) and why the file-editor overlay is
  // deliberately NOT part of this set.

  /** Lazily promote `termEl` from being `.pane`'s own direct flex:1 child to
   *  living inside a NESTED flex structure alongside up to three embed
   *  slots — left, right, bottom (#361 generalization from a single
   *  bottom-only slot). Created once, on the first embed of ANY kind, and
   *  left in place afterward: with every slot's panel/divider `[hidden]`
   *  (`display: none !important`, styles.css), `termEl` alone lays out
   *  identically to being `.pane`'s direct child, so there is nothing to
   *  undo when nothing is embedded.
   *
   *  The structure, two levels of nesting deep:
   *  ```
   *  embedHostEl (column)
   *    embedRowEl (row, the width axis)
   *      left divider + slot        (hidden unless occupied)
   *      embedCenterEl (row)
   *        termEl
   *        right divider + slot     (hidden unless occupied)
   *    bottom divider + slot        (hidden unless occupied)
   *  ```
   *  Bottom spans the row's FULL width (a sibling of `embedRowEl`, not
   *  nested inside it) rather than sitting only beside `termEl` — the
   *  simpler of the two corner-layout choices (see
   *  doc/design/embedded-panels.md's "Layout" section). NESTED, not a flat
   *  5-child row, so every divider's two sides are a real, single DOM
   *  element pair — see `dividerPair`/`dividerFloors` for why that's what
   *  keeps each divider's own drag math a plain two-element
   *  `embedDragGrow` call instead of a "sum of several siblings" problem. */
  private ensureEmbedHost(): void {
    if (this.embedHostEl) return;
    const host = document.createElement("div");
    host.className = "pane-embed-host";
    this.el.insertBefore(host, this.termEl);

    const row = document.createElement("div");
    row.className = "pane-embed-row";
    const center = document.createElement("div");
    center.className = "pane-embed-center";

    const left = this.makeEmbedSlot("left");
    const right = this.makeEmbedSlot("right");
    const bottom = this.makeEmbedSlot("bottom");

    center.append(this.termEl, right.dividerEl, right.panelEl);
    row.append(left.panelEl, left.dividerEl, center);
    host.append(row, bottom.dividerEl, bottom.panelEl);

    this.embedHostEl = host;
    this.embedRowEl = row;
    this.embedCenterEl = center;
    this.embedSlots = { left, right, bottom };
  }

  /** Build one embed slot's permanent (created-once, `hidden`-toggled) DOM:
   *  its panel and its divider. */
  private makeEmbedSlot(side: EmbedSide): EmbedSlotState {
    const panelEl = document.createElement("div");
    panelEl.className = `pane-embed-panel side-${side}`;
    panelEl.hidden = true;
    const dividerEl = document.createElement("div");
    dividerEl.className = `pane-embed-divider side-${side}`;
    dividerEl.hidden = true;
    const slot: EmbedSlotState = { side, kind: null, frac: DEFAULT_EMBED_FRAC, panelEl, dividerEl };
    this.wireEmbedDivider(slot);
    return slot;
  }

  /** Draggable divider for one embed slot. Mirrors grid.ts's own
   *  split-divider math exactly (embedsplit.ts) — the terminal's box
   *  genuinely resizes here (a real flex layout, not an
   *  absolutely-positioned overlay), so the SAME frame-debounced
   *  ResizeObserver → applyFit() path a grid split's divider drag already
   *  drives fires on every real size change, for all three sides alike. */
  private wireEmbedDivider(slot: EmbedSlotState): void {
    slot.dividerEl.addEventListener("mousedown", (e) => {
      e.preventDefault();
      const { beforeEl, afterEl, horizontal } = this.dividerPair(slot.side);
      const startPos = horizontal ? e.clientX : e.clientY;
      const sizeBefore = horizontal ? beforeEl.offsetWidth : beforeEl.offsetHeight;
      const sizeAfter = horizontal ? afterEl.offsetWidth : afterEl.offsetHeight;
      const growBefore = parseFloat(beforeEl.style.flexGrow || "1");
      const growAfter = parseFloat(afterEl.style.flexGrow || "1");
      slot.dividerEl.classList.add("dragging");
      const move = (ev: MouseEvent) => {
        const pos = horizontal ? ev.clientX : ev.clientY;
        const { beforeFloorPx, afterFloorPx } = this.dividerFloors(slot.side);
        const grow = embedDragGrow(sizeBefore, sizeAfter, growBefore, growAfter, pos - startPos, beforeFloorPx, afterFloorPx);
        beforeEl.style.flex = `${grow.growBefore} 1 0`;
        afterEl.style.flex = `${grow.growAfter} 1 0`;
      };
      const up = () => {
        slot.dividerEl.classList.remove("dragging");
        window.removeEventListener("mousemove", move);
        window.removeEventListener("mouseup", up);
        // Terminal (one per drag, not per mousemove) — mirrors grid.ts's own
        // split divider: persist the settled fraction so a restore
        // reproduces THIS size, not the one before the drag. `frac` is
        // always the PANEL's own share regardless of which side of the pair
        // it physically is (see `applySlotGrow`'s doc comment) —
        // `fracFromGrow(counterpartGrow, panelGrow)` extracts exactly that.
        const panelGrow = parseFloat(slot.panelEl.style.flexGrow || "1");
        const counterpartGrow = parseFloat(this.counterpartEl(slot.side).style.flexGrow || "1");
        slot.frac = fracFromGrow(counterpartGrow, panelGrow);
        this.events.onRecordChanged(this);
      };
      window.addEventListener("mousemove", move);
      window.addEventListener("mouseup", up);
    });
  }

  /** Whether `kind`'s view is currently on screen, in EITHER mode. */
  private isViewVisible(kind: EmbedKind): boolean {
    const entry = this.embedRegistry.get(kind);
    if (!entry) return false;
    const side = this.sideOf(kind);
    return side ? !this.embedSlots![side].panelEl.hidden : !entry.overlayEl.hidden;
  }

  /** Close `kind`'s view, in whichever mode it's currently shown. Does NOT
   *  un-dock it — a docked-but-closed view stays docked (`slot.kind` is
   *  untouched), exactly mirroring how a never-embedded view stays parked
   *  in its own hidden overlay between opens. `unembedView` is the
   *  separate, explicit action that actually clears a slot. */
  private closeView(kind: EmbedKind): void {
    const entry = this.embedRegistry.get(kind);
    if (!entry) return;
    entry.hide?.();
    const side = this.sideOf(kind);
    if (side) {
      const slot = this.embedSlots![side];
      slot.dividerEl.hidden = true;
      slot.panelEl.hidden = true;
      // Return the view to its OWN overlay host (#361 rev-38 blocker): a
      // slot's panel must never retain a closed/evicted occupant's element.
      // `openView`'s embedded branch below also self-enforces this with
      // `replaceChildren` (belt and suspenders — a panel can never hold
      // more than one child regardless of what called it), but parking the
      // element back in its OWN overlay (rather than just detaching it) is
      // what keeps it reachable and correctly `hidden` the next time THIS
      // view opens as an overlay, exactly where a never-embedded view
      // already lives between opens.
      entry.overlayEl.insertBefore(entry.viewEl, entry.overlayEl.firstChild);
    } else {
      entry.overlayEl.hidden = true;
    }
    this.updateTermShift();
    this.focus();
  }

  /** Close every OTHER floating overlay (embeddable or not — file-edit
   *  included) before `kind` opens AS AN OVERLAY: they genuinely collide,
   *  only one floating panel fits over the terminal. Never called for an
   *  embed-mode open (see `openView`) — the whole point of embedding is that
   *  it does NOT collide with a floating panel (#361 NB-4), for any of the
   *  (now up to three, simultaneous) docked views alike. A docked view is
   *  therefore left alone by this loop (`this.sideOf(kind) === null` guards
   *  it, mirroring how it's never the one with an open overlay anyway). */
  private closeOtherOverlays(except?: EmbedKind): void {
    for (const kind of EMBED_KINDS) {
      if (kind === except) continue;
      const entry = this.embedRegistry.get(kind);
      if (entry && this.sideOf(kind) === null && !entry.overlayEl.hidden) this.closeView(kind);
    }
    if (this.fileEditView?.visible) this.toggleFileEditView();
  }

  /** Show `kind`'s view in whichever mode it's currently set to. Wraps the
   *  view's own `show()` in the same never-leave-the-pane-half-toggled
   *  recovery `toggleGitView` originally had for itself — generalized here
   *  because any view's `show()` (a refresh that can throw) has the same
   *  failure shape, not just git's. */
  private openView(kind: EmbedKind): void {
    const entry = this.embedRegistry.get(kind);
    if (!entry) return;
    const side = this.sideOf(kind);
    try {
      if (side) {
        const slot = this.embedSlots![side];
        // `replaceChildren`, not `appendChild` (#361 rev-38 blocker): a
        // slot's panel may only ever hold ONE occupant, and this makes that
        // an invariant of the call itself rather than something every
        // caller has to get right by first evicting whoever was there —
        // even if a future code path forgot to, this can't leave two views
        // stacked and both visible.
        slot.panelEl.replaceChildren(entry.viewEl);
        this.applySlotGrow(side, slot.frac);
        slot.dividerEl.hidden = false;
        slot.panelEl.hidden = false;
        // The share just applied may be stale (a restored preference
        // captured under a smaller floor, a floor that grew while closed,
        // or — for left specifically — the OTHER slot's occupancy having
        // changed since) — reclamp against the CURRENT floor immediately,
        // the same correction a content-driven floor growth applies while
        // already open (#361 rev-38 NB3; see `reclampViewFloor`).
        this.reclampViewFloor(kind);
      } else {
        this.closeOtherOverlays(kind);
        entry.overlayEl.insertBefore(entry.viewEl, entry.overlayEl.firstChild);
        const strip = Math.max(140, Math.round(this.el.clientHeight * 0.35));
        entry.overlayEl.style.height = `${this.overlayClamp(this.termEl.clientHeight - strip, entry.floorPx())}px`;
        entry.overlayEl.hidden = false;
      }
      entry.show();
      this.updateTermShift();
    } catch (err) {
      entry.hide?.();
      if (side) {
        const slot = this.embedSlots![side];
        slot.dividerEl.hidden = true;
        slot.panelEl.hidden = true;
        entry.overlayEl.insertBefore(entry.viewEl, entry.overlayEl.firstChild);
      } else {
        entry.overlayEl.hidden = true;
      }
      this.termEl.style.transform = "";
      throw err;
    }
  }

  /** Toggle `kind`'s view open/closed, in whichever mode it's currently set
   *  to. The shared entry point every embeddable view's public hotkey
   *  method (`toggleTasksView`, `toggleGitView`, …) delegates to after its
   *  own view-specific gating and lazy `ensureXView()`. */
  private toggleView(kind: EmbedKind): void {
    if (this.isViewVisible(kind)) this.closeView(kind);
    else this.openView(kind);
  }

  /** Show the side-picker menu (#361) — a view's own header embed button,
   *  clicked. Left/Right/Bottom (the currently-docked one, if any, checked),
   *  plus "Un-embed" when it's docked anywhere. Built and shown here, not in
   *  each view: the views don't need to know `EmbedSide` exists at all, only
   *  that clicking their button asks the pane "where should I go?" — same
   *  division of responsibility the rest of this engine already keeps
   *  (views are dumb UI; the pane owns embed state). Reuses
   *  `contextmenu.ts`'s existing `showContextMenu` rather than a bespoke
   *  dropdown. */
  private showEmbedMenu(kind: EmbedKind, anchor: HTMLElement): void {
    const entry = this.embedRegistry.get(kind);
    if (!entry) return;
    const currentSide = this.sideOf(kind);
    const rect = anchor.getBoundingClientRect();
    const SIDE_LABEL: Record<EmbedSide, string> = { left: "Embed left", right: "Embed right", bottom: "Embed bottom" };
    const items: MenuItem<EmbedSide | "unembed">[] = EMBED_SIDES.map((side) => ({
      label: (currentSide === side ? "✓ " : "") + SIDE_LABEL[side],
      action: side,
    }));
    if (currentSide !== null) {
      items.push({ label: "", separator: true }, { label: "Un-embed — back to a floating overlay", action: "unembed" });
    }
    showContextMenu(rect.left, rect.bottom + 4, items, (action) => {
      if (action === "unembed") this.unembedView(kind);
      else this.embedViewAtSide(kind, action);
    });
  }

  /** Dock `kind` to `side` (#361) — the side-picker menu's action. Docking
   *  onto an OCCUPIED side SWAPS that ONE slot's occupant: whoever was there
   *  is CLOSED outright, not demoted back to an overlay (a silent reopen
   *  elsewhere would be a more surprising UX than "the slot now shows what
   *  you asked for, and the previous occupant is closed — the same one
   *  click that opened it reopens it") — the OTHER two slots are always
   *  left untouched. If `kind` is already docked to a DIFFERENT side, it
   *  moves (leaves that side first). Either way the slot's occupant +
   *  fraction are a PERSISTED preference (tabs.json, via
   *  `onRecordChanged`). A discrete, user-initiated layout change (see
   *  doc/design/embedded-panels.md) — never fired from a resize or a
   *  refresh. */
  private embedViewAtSide(kind: EmbedKind, side: EmbedSide): void {
    const entry = this.embedRegistry.get(kind);
    if (!entry) return;
    this.ensureEmbedHost();
    const currentSide = this.sideOf(kind);
    if (currentSide === side) {
      // Already docked here — just make sure it's actually showing (it may
      // be docked-but-closed).
      if (!this.isViewVisible(kind)) this.openView(kind);
      return;
    }
    // Leave whichever OTHER side this kind currently occupies, if any.
    if (currentSide !== null) {
      const wasVisible = this.isViewVisible(kind);
      this.embedSlots![currentSide].kind = null;
      if (wasVisible) this.closeView(kind);
    }
    // Evict whoever (if anyone) is currently on the TARGET side.
    const targetSlot = this.embedSlots![side];
    if (targetSlot.kind !== null) {
      this.embedRegistry.get(targetSlot.kind)?.setPanelActive(false);
      this.closeView(targetSlot.kind);
      targetSlot.kind = null;
    }
    const wasOverlayOpen = !entry.overlayEl.hidden;
    if (wasOverlayOpen) entry.overlayEl.hidden = true; // it's about to move into the slot
    targetSlot.kind = kind;
    targetSlot.frac = clampEmbedFrac(targetSlot.frac);
    entry.setPanelActive(true);
    this.openView(kind); // docking always shows it
    // Right's occupancy just changed (moved onto it, off it, or evicted
    // from it) — left's composed far-side floor depends on that (see
    // dividerFloors's "left" case), so reclamp it too.
    if (side === "right" || currentSide === "right") this.reclampSlotDivider("left");
    this.events.onRecordChanged(this);
  }

  /** Un-dock `kind` (#361) — back to the floating overlay, staying open if
   *  it was. A no-op if it isn't currently docked anywhere. */
  private unembedView(kind: EmbedKind): void {
    const entry = this.embedRegistry.get(kind);
    if (!entry) return;
    const side = this.sideOf(kind);
    if (side === null) return;
    const wasVisible = this.isViewVisible(kind);
    if (wasVisible) this.closeView(kind);
    this.embedSlots![side].kind = null;
    entry.setPanelActive(false);
    if (wasVisible) this.openView(kind);
    if (side === "right") this.reclampSlotDivider("left");
    this.events.onRecordChanged(this);
  }

  /** Reapply persisted embed preferences (#361) — called once, right after
   *  a resumed/restored orch pane is wired up with its group, so every view
   *  that was docked and open when the layout was captured comes back the
   *  same way, on the same side. Entries naming a kind this pane can't show
   *  right now (the gating button is hidden) are silently skipped — restore
   *  doesn't carry this far for git/issues either (see main.ts's
   *  `resumeDormantGroup`); only orchestration-family kinds
   *  (tasks/audit/group) are ever restored this way today — see
   *  `PersistedPane.embeds`'s decode. */
  restoreEmbeds(embeds: readonly { view: EmbedKind; side: EmbedSide; share: number }[]): void {
    if (!this.orchGroup) return;
    for (const e of embeds) {
      switch (e.view) {
        case "tasks":
          if (this.tasksBtn.hidden) continue;
          this.ensureTasksView();
          break;
        case "audit":
          if (this.auditBtn.hidden) continue;
          this.ensureAuditView();
          break;
        case "group":
          if (this.groupBtn.hidden) continue;
          this.ensureGroupView();
          break;
        default:
          continue; // git/issues aren't orch-gated the same way; not captured for restore today
      }
      this.ensureEmbedHost();
      const entry = this.embedRegistry.get(e.view)!;
      const slot = this.embedSlots![e.side];
      slot.kind = e.view;
      slot.frac = clampEmbedFrac(e.share);
      entry.setPanelActive(true);
      this.openView(e.view);
    }
  }

  /** Toggle the audit-log viewer overlay (any orchestration pane). Same
   *  no-resize overlay mechanics as the git/task views; only one overlay is
   *  open at a time. */
  toggleAuditView(): void {
    if (!this.orchGroup || this.auditBtn.hidden) return;
    this.ensureAuditView();
    this.toggleView("audit");
  }

  /** Lazily construct the audit view and register it into `embedRegistry`
   *  (#361). */
  private ensureAuditView(): void {
    if (this.auditView) return;
    this.auditView = new AuditView(this.orchGroup!, {
      onClose: () => this.toggleAuditView(),
      onEmbedMenu: (anchor) => this.showEmbedMenu("audit", anchor),
    });
    this.auditOverlay = document.createElement("div");
    this.auditOverlay.className = "git-overlay";
    this.auditOverlay.hidden = true;
    this.auditOverlay.append(this.auditView.el, this.makeOverlayDivider(() => this.auditOverlay!));
    this.el.appendChild(this.auditOverlay);
    this.embedRegistry.set("audit", {
      overlayEl: this.auditOverlay,
      viewEl: this.auditView.el,
      show: () => this.auditView!.show(),
      setPanelActive: (active) => this.auditView!.setPanelActive(active),
      floorPx: () => EMBED_MIN_PANEL_PX,
    });
  }

  /** Toggle the group lifecycle panel overlay (orchestrator panes). Same
   *  no-resize overlay mechanics as the other views; only one is open. */
  toggleGroupView(): void {
    if (!this.orchGroup || this.groupBtn.hidden) return;
    this.ensureGroupView();
    this.toggleView("group");
  }

  /** Lazily construct the group lifecycle view and register it into
   *  `embedRegistry` (#361) — its floor is its own measured chrome
   *  (`groupFloor`), not the generic default, so the footer/suspended
   *  banner never clip in either hosting mode. */
  private ensureGroupView(): void {
    if (this.groupView) return;
    this.groupView = new GroupView(this.orchGroup!, {
      onClose: () => this.toggleGroupView(),
      // Mirror the header's fold-group toggle inside the lifecycle panel (#46).
      onToggleMinimize: () => this.events.onToggleGroupMinimize(this),
      // Content grew (e.g. the suspended banner appeared) — re-clamp
      // whichever host is currently active so the footer never slides under
      // overflow:hidden (#83 rev-58; generalized to embed mode by #361).
      onResize: () => this.reclampViewFloor("group"),
      // The orchestrator pane's cwd IS the group's repo (create_orchestration
      // opens it there) — the workflow toggle's ON-confirm preview reads it
      // live rather than snapshotting at open time (#316).
      getRepo: () => this.cwdRaw,
      onEmbedMenu: (anchor) => this.showEmbedMenu("group", anchor),
    });
    this.groupOverlay = document.createElement("div");
    this.groupOverlay.className = "git-overlay";
    this.groupOverlay.hidden = true;
    this.groupOverlay.append(
      this.groupView.el,
      this.makeOverlayDivider(() => this.groupOverlay!, () => this.groupFloor())
    );
    this.el.appendChild(this.groupOverlay);
    this.embedRegistry.set("group", {
      overlayEl: this.groupOverlay,
      viewEl: this.groupView.el,
      show: () => this.groupView!.show(),
      // Stops the poll timer `show()` starts (#361 rev-38 NB2) — without
      // this, every close/eviction left it running, and swapping the embed
      // slot turned what was a rare pre-existing leak into an easy one to
      // hit on every reopen.
      hide: () => this.groupView!.hide(),
      setPanelActive: (active) => this.groupView!.setPanelActive(active),
      floorPx: () => this.groupFloor(),
    });
  }

  /** Toggle the file-editor overlay (#174): file tree + code editor +
   *  search/replace. Ungated — works in every pane type, plain terminals
   *  included. Same no-resize overlay mechanics as the git/audit views; only
   *  one overlay is open at a time. The tree roots at the pane's live cwd. */
  toggleFileEditView(): void {
    // Alt+F on an editor pane: the pane already IS the file editor. Same for a files
    // pane, whose surface is the file MANAGER — a sibling of the editor, and the pane
    // the user is looking at either way. Refusing with a toast would be absurd; just
    // put the cursor in it.
    if (this.contentKind === "editor" || this.contentKind === "files") {
      this.focus();
      return;
    }
    if (this.refuseOverlay("The file editor")) return;
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
      // The file editor is never embeddable (see doc/design/embedded-panels.md's
      // "What's excluded" section), but it still collides with every OTHER
      // floating overlay the same way they collide with each other — an
      // EMBEDDED one is correctly left alone (#361 NB-4 coexistence).
      this.closeOtherOverlays();
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

  /** Bind (or clear) this pane's standalone channel identity after
   *  construction (#271 W3 addendum, part A3) — used by the Connect
   *  gesture's adopt-on-connect path (`orch_solo_adopt`) for a pane that had
   *  none at spawn time (launched before this feature, or on a CLI the human
   *  didn't opt into channel tools for at launch). Never touches
   *  orchGroup/orchAgent/orchRoleName — this carrier stays deliberately
   *  separate so a plain standalone pane never lights up the orchestration
   *  chrome. */
  setChannelAgent(info: { group: string; agentId: string; role: string; canSend: boolean } | null): void {
    this.channelAgentInfo = info;
  }

  get channelAgentGroupId(): string | null {
    return this.channelAgentInfo?.group ?? null;
  }

  get channelAgentAgentId(): string | null {
    return this.channelAgentInfo?.agentId ?? null;
  }

  get channelAgentRole(): string | null {
    return this.channelAgentInfo?.role ?? null;
  }

  get channelAgentCanSend(): boolean {
    return this.channelAgentInfo?.canSend ?? false;
  }

  /** Whether this pane was launched with a command (an agent CLI), as
   *  opposed to a plain interactive shell — the same distinction
   *  `liveKind()` makes internally. #271 W3 addendum, part A3: the
   *  adopt-on-connect gesture is offered for agent panes with no channel
   *  identity yet; a plain terminal stays not-capable regardless. */
  get isAgentPane(): boolean {
    return this.launchedCommand;
  }

  /** Whether this pane's process has emitted a single byte since it spawned
   *  (#280/#281) — a crashed pane that never did is a DOA revival, not a
   *  crash worth keeping open to read. */
  get hasReceivedOutput(): boolean {
    return this.receivedOutput;
  }

  /** Capture this pane as a serializable record for the persisted layout (#194).
   *  Reads only the retained launch inputs plus the live cwd — no geometry, no
   *  PTY — so it is safe under the no-resize invariant and works even on a hidden
   *  tab. Returns null for a welcome (setup-state) pane: it has no chosen kind
   *  yet, so there is nothing to restore. main.ts pairs these with the flex
   *  weights from grid.layoutSnapshot() to build the PersistedLayoutNode tree;
   *  panerestore.ts decides what each record becomes on restore. */
  capture(): PersistedPane | null {
    if (this.isWelcome) return null;
    // A dormant restore placeholder persists exactly as it came in, so a session
    // closed without resuming offers the identical restore next boot.
    if (this.dormantRecord) return { ...this.dormantRecord };
    const kind = this.liveKind();
    return {
      paneKind: kind,
      name: this.name,
      cwd: this.cwdRaw,
      command: kind === "agent" ? this.spawnCommand : null,
      argv: kind === "agent" ? this.spawnArgv : null,
      shellKind: kind === "terminal" ? this.spawnShellKind : null,
      // Capture the session id for orch panes too (#194.5) so a group resume
      // restores exactly the captured members from their own recorded sessions.
      sessionId: kind === "agent" || kind === "orch" ? this.agentSessionId : null,
      // The orchestration role distinguishes the orchestrator from its delegates.
      role: kind === "orch" ? this.orchRoleName : null,
      // An editor pane's OPEN FILE (#217) — a path, never a buffer. Without it a pane
      // opened on `src/pane.ts` (and titled after it) restores as a bare tree that
      // names a file it isn't showing. The file is re-read from disk on restore; what
      // was typed and not saved is deliberately not persisted (panerestore.ts).
      // A workflow pane (#222) records the workflow file it is on, for the same reason
      // and on the same terms.
      file:
        kind === "editor"
          ? this.editorPaneView?.openPathRel ?? null
          : kind === "workflow"
            ? this.workflowPaneView?.openPathRel ?? null
            : null,
      // Every view CURRENTLY docked (#361), and at what side + share of the
      // split — up to three entries, one per occupied slot. Empty = nothing
      // embedded — every view opens as its floating overlay (the default).
      // Only the orchestration-family kinds (tasks/audit/group) are
      // captured for restore: git/issues are available on every pane kind,
      // but nothing short-lived like a plain terminal restore has the
      // natural "captured, then reapplied once the real pane exists" hook
      // orch panes get from staying dormant — see
      // doc/design/embedded-panels.md's persistence section. The share
      // mirrors how a split's own `weight` is already persisted as a
      // flex-grow ratio rather than a pixel size — not new geometry-
      // persistence territory, the same one grid.layoutSnapshot() occupies.
      embeds:
        kind === "orch" && this.embedSlots
          ? EMBED_SIDES.flatMap((side) => {
              const slot = this.embedSlots![side];
              return slot.kind !== null && isRestorableEmbedKind(slot.kind)
                ? [{ view: slot.kind, side, share: slot.frac }]
                : [];
            })
          : [],
    };
  }

  /** The persisted record a dormant restore placeholder stands in for, or null
   *  when this pane isn't dormant. Lets a whole-group resume read the CAPTURED
   *  group members (session id + role) straight off the tab's dormant orch
   *  placeholders — the set that was live at close — rather than the backend's
   *  full historical roster (#194.5). */
  get restoreRecord(): PersistedPane | null {
    return this.dormantRecord;
  }

  /** This pane's persisted kind from its live launch state: a CONTENT kind (#214/#217,
   *  no PTY at all) > orch (any orchestration role) > agent (launched a command) >
   *  plain terminal. `capture()`'s per-kind ternaries above then null every field a
   *  content pane doesn't have (command, argv, shellKind, sessionId, role), leaving
   *  exactly {paneKind, name, cwd:=root} — all it needs to come back. */
  private liveKind(): PersistedPaneKind {
    if (this.contentKind !== null) return this.contentKind;
    return this.orchGroup ? "orch" : this.launchedCommand ? "agent" : "terminal";
  }

  /** Classify this pane for the per-tab agent counter / orch markers (#194 P4,
   *  tabcounts.ts). A welcome (setup) or dormant placeholder reports `live:false`
   *  so it never inflates the count; a running pane reports its kind + that it has
   *  a PTY. A content pane reports its own kind (files / editor / git), which the
   *  counter ignores outright — those are viewers, not agents (#214, #217). Reads no
   *  geometry, so it's safe on a hidden tab. */
  tabPaneInfo(): TabPaneInfo {
    if (this.isWelcome) return { kind: "terminal", live: false };
    if (this.dormantRecord) {
      return { kind: this.dormantRecord.paneKind === "orch" ? "orch" : "agent", live: false };
    }
    // A content pane has no PTY by design, so `live` can't be derived from one; it is
    // fully functional the moment it exists.
    if (this.contentKind !== null) return { kind: this.contentKind, live: true };
    const kind = this.liveKind();
    return { kind, live: this.ptyId !== null && !this.exited, connectedChannel: this.channelId };
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
        // The pane's persisted name just changed with no grid mutation — same
        // stale-snapshot class as a files-pane re-root, so re-persist here too.
        // (persistTabs dedups on identical bytes, so a no-op rename costs nothing.)
        if (changed) this.events.onRecordChanged(this);
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

  /** Should this pane survive its process's death — and why? Null to dispose it.
   *
   *  Two reasons, composed in the pure `keepOpenOnExit` (dirtystate.ts): a command pane
   *  that died unexpectedly stays so the human can read the error (the original rule),
   *  and — new in #219 — a pane holding a DIRTY Alt+F buffer stays no matter how it
   *  died, because an automatic teardown must never destroy work the human never agreed
   *  to lose. Clean exits and loomux-initiated kills close as usual, unless that. */
  keepOpenOnExit(exit: ExitInfo): KeepOpenReason | null {
    return keepOpenOnExit({
      launchedCommand: this.launchedCommand,
      exit,
      hasUnsavedWork: this.hasUnsavedWork(),
    });
  }

  /** Announce a kept-open pane's exit inside its terminal, saying WHY it is still here.
   *  A pane that outlives its process for an unsaved buffer must say so — otherwise it
   *  reads as a bug ("why didn't this close?") and the buffer it is protecting stays
   *  invisible, which is how it gets lost anyway. */
  notifyExited(code: number | null, reason: KeepOpenReason = "output"): void {
    // The pane stays open to show output, but its process is DEAD — mark it so the
    // agent counter stops counting it as live (#194 P4 LOW-7). ptyId is left set
    // (the buffer/scrollback is still attached) so isExited, not ptyId, gates live.
    this.exited = true;
    const codeTxt = code === null ? "" : ` (code ${code})`;
    const why =
      reason === "unsaved"
        ? "— kept open: the file editor (Alt+F) here has UNSAVED edits. Save them, then close with Ctrl+Shift+W"
        : "— pane kept open so you can read the output; close it with Ctrl+Shift+W";
    this.term.writeln(`
[91mprocess exited${codeTxt}[0m [90m${why}[0m`);
    // A crashed pane that ALSO holds unsaved edits gets both facts: the dead process is
    // the louder one, but the buffer is the one that can still be lost.
    if (reason === "output" && this.hasUnsavedWork()) {
      this.term.writeln(
        `[93mThe file editor (Alt+F) in this pane has unsaved edits — closing the pane will ask.[0m`
      );
    }
    this.setName(`${this.name} · exited`);
    // A crash that never produced a single byte (#281) reads as a bare exit
    // code with nothing to go on -- say so explicitly rather than leaving
    // the human to guess whether this ever even started.
    if (reason === "output") {
      const diag = exitDiagnosticLine(this.receivedOutput);
      if (diag) this.term.writeln(`\r\n[90m${diag}[0m`);
    }
  }

  /** What this pane is holding, for the app-quit guard's enumeration (#219): its editor's
   *  buffer — the pane's own (an editor pane) or its Alt+F overlay's — labelled with
   *  the tab and pane it lives in, so the confirm can say WHERE. Null when the pane has no
   *  editor with a file open at all. */
  bufferReport(tab: string): PaneBufferReport | null {
    const report = this.unsavedHolder()?.bufferReport();
    if (!report) return null;
    // "pane" vs "overlay" is what the quit confirm needs to say WHERE the work is: a
    // content pane is visibly what it is, while an Alt+F overlay is tucked inside a
    // terminal that looks like any other.
    const host: DirtyHost = this.editorPaneView || this.workflowPaneView ? "pane" : "overlay";
    return { tab, pane: this.name, host, file: report.file, dirty: report.dirty };
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
    // A setup-state pane has no terminal; route focus into its welcome form so
    // keyboard nav (Alt+arrow), window-refocus (main.ts), and dock-restore land
    // on a usable control instead of no-op'ing on an unopened terminal (rev-74
    // LOW-6). `isWelcome` is null once the pane becomes a real terminal.
    if (this.isWelcome) {
      this.focusWelcome();
      return;
    }
    // A dormant placeholder likewise has no terminal — land focus on its
    // Start/Resume affordance so keyboard nav reaches a usable control.
    if (this.dormantEl) {
      this.dormantEl.querySelector<HTMLElement>("button, [tabindex]")?.focus();
      return;
    }
    // A content pane has no terminal either: focus its view (each is tabIndex -1), so
    // Alt+arrow nav, window refocus, and dock-restore land ON the surface instead of
    // no-oping on a terminal that was never opened. None of them grabs an inner control
    // — the user clicks into whichever they want.
    if (this.filesView) {
      this.filesView.focus();
      return;
    }
    if (this.editorPaneView) {
      this.editorPaneView.el.focus();
      return;
    }
    if (this.workflowPaneView) {
      this.workflowPaneView.focus();
      return;
    }
    if (this.gitPaneView) {
      this.gitPaneView.el.focus();
      // NOT a refresh. Refreshing on focus is the obvious idea and it is wrong: the
      // git view rebuilds its changes strip from scratch (`renderWorking` →
      // `replaceChildren`), which includes the COMMIT MESSAGE textarea — so a refresh
      // fired by "the window regained focus" or "you tabbed back to this pane" would
      // silently wipe a half-typed commit message. That never bit the overlay, whose
      // only refresh trigger is a shell prompt (impossible while you type into the
      // view). A git pane has no prompt and no PTY to hang a git watch off, so it
      // refreshes on open, after its own actions, and on the ↻ button — an explicit,
      // safe pull rather than an implicit, destructive one.
      return;
    }
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

  /** Does this pane have an OPEN terminal? `term.element` is set by `term.open()`,
   *  which only the PTY-backed start paths call — so this is false for a files pane
   *  (#214, never), and for a welcome or dormant pane (not yet). The single honest
   *  answer to "can this pane take a paste / be serialized / hold a WebGL context",
   *  used by all three; without it, dictating (Alt+S) at a files pane would record,
   *  transcribe, and paste the transcript into an xterm that isn't there. */
  hasTerminal(): boolean {
    return !!this.term.element;
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
    // The surfaces a CONTENT pane hosts (#214, #217). Exactly one is ever non-null.
    this.filesView?.dispose();
    this.editorPaneView?.dispose();
    this.gitPaneView?.dispose();
    this.workflowPaneView?.dispose();
    if (this.ptyId !== null) {
      detachOutput(this.ptyId);
      detachGitWatch(this.ptyId);
      if (killBackend) killPty(this.ptyId).catch(() => {});
    }
    this.term.dispose();
    this.el.remove();
  }
}

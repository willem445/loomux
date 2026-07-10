import "./styles.css";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { showToast } from "./toast";
import type { Grid } from "./grid";
import { Workspace } from "./workspace";
import { TabManager } from "./tabs";
import { TabBar } from "./tabbar";
import type { Pane, PaneEvents } from "./pane";
import { SessionBrowser } from "./sessions";
import {
  ensureOutputRouter,
  onPtyExit,
  loadUiTabs,
  saveUiTabs,
  type PtyExit,
  type SessionInfo,
} from "./pty";
import { matchShortcut } from "./shortcuts";
import { voiceController } from "./voicecontrol";
import { initStatusBar } from "./statusbar";
import { initHintBar } from "./hintbar";
import { WelcomeForm, type WelcomeResult } from "./launcher";
import {
  initOrchestration,
  launchOrchestrator,
  orchSessionRoles,
  resumeOrchSession,
  type OrchWiring,
  type OrchTarget,
  type OrchestratorConfig,
  type AttentionItem,
} from "./orchestration";
import { tabAttention, sameAttention, findPaneByPty } from "./tabroute";
import { encodeTabs, decodeTabs } from "./tabstore";

// Surface unexpected errors as a visible banner instead of a silently
// broken UI — a user-facing "crash" should always come with a message.
function showFatal(msg: string): void {
  let el = document.getElementById("app-error");
  if (!el) {
    el = document.createElement("div");
    el.id = "app-error";
    el.addEventListener("click", () => el!.classList.remove("visible"));
    document.body.appendChild(el);
  }
  el.textContent = msg;
  el.classList.add("visible");
}
window.addEventListener("error", (e) => {
  // The banner only shows e.message, which for a cross-module DOM error hides
  // the throwing frame. Log the underlying Error's stack so the next live
  // occurrence of the intermittent pane-rename NotFoundError (#113) — whose
  // exact reentrant trigger we could not pin from static reading — is captured
  // with its call site instead of just the opaque message.
  console.error("uncaught error:", e.error ?? e.message, "\n", e.error?.stack ?? "(no stack)");
  showFatal(`error: ${e.message}`);
});
window.addEventListener("unhandledrejection", (e) => {
  console.error("unhandled rejection:", e.reason);
  showFatal(`unhandled: ${String(e.reason)}`);
});

const sessionsEl = document.getElementById("sessions")!;
const stackEl = document.getElementById("workspace-stack")!;
const tabBarEl = document.getElementById("tab-bar")!;

// Project tabs (#63): each tab is a Workspace (its own Grid + dock). The old
// module-scope single `grid` is gone; everything acts on the ACTIVE tab's grid.
const tabs = new TabManager<Workspace>((id) => {
  const ws = new Workspace(id, (w) => {
    // Last pane in this tab closed (a human ✕, or a background agent exiting) →
    // keep the tab's grid non-empty by refilling with the welcome / pane-setup
    // surface (#194). This is safe for a hidden/background tab now that the
    // welcome is IN-PANE content, not a floating modal over the active tab — the
    // old MED-1 "silent shell only" rule existed solely to avoid that overlay.
    openWelcomeIn(w);
  });
  stackEl.appendChild(ws.el);
  return ws;
});

/** The tab strip, assigned once boot mounts it. Held so the keyboard
 *  Ctrl+Shift+K routes through the same two-step close-confirm the ✕ uses. */
let tabBar: TabBar<Workspace> | null = null;

/** The active tab's grid — the single-grid `grid` of the pre-tabs app. */
const activeGrid = (): Grid => tabs.activeWorkspace.grid;

// Voice push-to-talk (#58, Alt+S): the global capture controller finds its
// insertion target via the active pane (of the active tab).
voiceController.init(() => activeGrid().activePane);

/** Pane events bound to a specific workspace, so a pane always acts on its own
 *  tab's grid — never whichever tab happens to be active when the event fires. */
function eventsFor(ws: Workspace): PaneEvents {
  return {
    onFocus: (pane) => ws.grid.setActive(pane),
    onCloseRequest: (pane) => ws.grid.closePane(pane),
    onSplit: (pane, dir) => openWelcomeIn(ws, dir, pane),
    onMinimize: (pane) => ws.grid.minimize(pane),
    onMaximize: (pane) => ws.grid.toggleMaximize(pane),
    onToggleGroupMinimize: (pane) => {
      const groupId = pane.orchGroupId;
      if (groupId) ws.grid.toggleGroupMinimize(groupId);
    },
  };
}

/** Find a pane by pty id across ALL tabs — a PTY exit / focus / rename can
 *  belong to any tab, not just the active one. Scans live panes (never a
 *  maintained side-map, which a pane close would leave stale); the pure core is
 *  `findPaneByPty` (tabroute.ts), unit-tested. */
function findPaneAcrossTabs(ptyId: number): { ws: Workspace; pane: Pane } | null {
  return findPaneByPty(tabs.tabs, (ws) => ws.grid, ptyId);
}

// ---------- project tabs: orchestration routing (#63) ----------

/** Open a new tab the way the user expects (#63): create + activate it, then
 *  present the welcome / pane-setup surface — the SAME starting surface a fresh
 *  loomux pane shows. The welcome pane fills the tab immediately, so it's never
 *  left blank; the user picks the pane's kind from there (#194). */
function openUserTab(): void {
  const ws = tabs.newTab();
  openWelcomeIn(ws);
  persistTabs();
}

/** A short project name for a tab, from a repo/worktree path's last segment. */
function projectName(path: string): string {
  const parts = path.replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || "project";
}

/** Launch an orchestrator into its OWN project tab (created + activated + named
 *  from the repo), then bind the group→tab routing so its workers land here and
 *  focus/attention resolve to this tab (#63). */
async function launchOrchestratorTab(config: OrchestratorConfig): Promise<void> {
  const ws = tabs.newTab();
  tabs.renameTab(ws.id, projectName(config.repo));
  const { groupId } = await launchOrchestrator(ws.grid, eventsFor(ws), config);
  tabs.bindGroup(groupId, ws.id);
  persistTabs();
}

/** Apply an attention scan across all tabs: badge each pane by its pty (the
 *  pre-tabs behavior, now spanning every tab) AND badge the tab-strip entry of
 *  any tab that owns a needs-attention pty — so a hidden tab's blocked agent
 *  still surfaces (#63). Uses a live pty→tab map built from the actual
 *  panes, so plain (#40) panes badge their tab too, not just bound agents. */
function applyAttention(items: AttentionItem[]): void {
  const byPty = new Map<number, AttentionItem>();
  for (const it of items) if (it.pty_id !== null) byPty.set(it.pty_id, it);
  const ptyToWs = new Map<number, string>();
  for (const ws of tabs.tabs) {
    for (const pane of ws.grid.allPanes()) {
      if (pane.ptyId === null) continue;
      ptyToWs.set(pane.ptyId, ws.id);
      const it = byPty.get(pane.ptyId);
      pane.setAttention(it ? it.reason : null, it?.detail);
    }
  }
  // Dedup against the current set so the 3-second re-emits don't re-render the
  // tab bar when nothing changed.
  const next = tabAttention(items, ptyToWs);
  if (!sameAttention(tabs.tabAttention, next)) tabs.setTabAttention(next);
}

/** The tab layer as the orchestration event router sees it (OrchWiring). */
const orchWiring: OrchWiring = {
  targetForGroup(req): OrchTarget {
    let ws = tabs.workspaceForGroup(req.group_id);
    if (!ws) {
      // First sight of a group with no tab (e.g. a rejoin before its
      // orchestrator restored) — open a background project tab for it.
      ws = tabs.newTab(false);
      tabs.renameTab(ws.id, projectName(req.cwd || req.name));
      tabs.bindGroup(req.group_id, ws.id);
      persistTabs();
    }
    return { grid: ws.grid, paneEvents: eventsFor(ws) };
  },
  findByPty(ptyId): Pane | undefined {
    return findPaneAcrossTabs(ptyId)?.pane;
  },
  allGrids(): Grid[] {
    return tabs.tabs.map((ws) => ws.grid);
  },
  focusPty(ptyId): void {
    const found = findPaneAcrossTabs(ptyId);
    if (!found) return;
    tabs.switchTo(found.ws.id); // switch to the pane's TAB first…
    found.ws.grid.setActive(found.pane); // …then focus the pane.
    found.pane.focus();
  },
  applyAttention,
};

// ---------- project tabs: persistence (#63) ----------
// The tab set (name / color / order / active tab / owning group) persists to
// durable BACKEND storage via a typed command (loadUiTabs/saveUiTabs → the
// atomic, corrupt-safe tabs.json in AppData; see src-tauri/src/uistate.rs),
// NOT localStorage — so it survives a webview data clear and sits alongside the
// app's other durable state. tabstore.ts owns the schema (encode/decode +
// validation); a bad file is quarantined backend-side and we degrade to a fresh
// tab without losing it. Pane/PTY contents are not captured — see restoreTabs /
// the design doc for what does and does not revive, and why.

/** The pre-backend localStorage key, read once for migration then retired. */
const LEGACY_TABS_KEY = "loomux.tabs";

/** The last snapshot actually written, so persistTabs is a no-op when nothing
 *  in the persisted set changed. tabs.onChange also fires for attention-scan
 *  updates (every ~3s) and renames-in-progress, none of which alter the saved
 *  fields — without this dedup we'd rewrite identical bytes to disk on a timer. */
let lastPersisted: string | null = null;

/** Persist the current tab set to the backend when it actually changed.
 *  Fire-and-forget: persistence is best-effort and must never block or crash the
 *  UI (a failed write just means the last change isn't durable until the next). */
function persistTabs(): void {
  const encoded = encodeTabs(tabs.snapshot());
  if (encoded === lastPersisted) return;
  lastPersisted = encoded;
  void saveUiTabs(encoded).catch(() => {
    // The write didn't land — allow the next change to retry the same bytes.
    lastPersisted = null;
  });
}

/** Load the persisted tab-set JSON, migrating a pre-backend localStorage blob on
 *  first run after upgrade: read the legacy key ONCE, hand it to the backend,
 *  and clear it so the backend copy is thereafter the single source of truth. */
async function loadPersistedTabs(): Promise<string | null> {
  const fromBackend = await loadUiTabs();
  if (fromBackend !== null) return fromBackend;
  // No backend copy yet. One-time migration from the pre-backend localStorage.
  const legacy = localStorage.getItem(LEGACY_TABS_KEY);
  if (legacy !== null) {
    localStorage.removeItem(LEGACY_TABS_KEY);
    // Adopt the legacy blob as the backend copy immediately, so a crash before
    // the next change doesn't lose it (and we never read localStorage again).
    void saveUiTabs(legacy).catch(() => {});
    return legacy;
  }
  return null;
}

/** Recreate the saved tab set on boot, rebinding each tab's group so a restored
 *  session for that group routes into its own tab (see restoreSession). Returns
 *  whether any tabs were restored (false → caller seeds one default tab).
 *
 *  DELIBERATE SCOPE (design doc — not a limitation we couldn't overcome): only
 *  the tab SHELLS revive here (name / color / order / active / group binding).
 *  Live agent panes/PTYs are NOT auto-spawned on boot — reviving N
 *  orchestrator+worker CLIs on every launch would burn the user's credits and
 *  spawn a process storm (#78) without them asking. Instead the persisted group
 *  binding makes revival routing-correct: when the human restores that group's
 *  session (or a spawn/rejoin event arrives for it), it lands in THIS tab via
 *  the group→tab routing, not whatever tab is active. */
async function restoreTabs(): Promise<boolean> {
  const saved = decodeTabs(await loadPersistedTabs());
  if (!saved) return false;
  for (const t of saved.tabs) {
    const ws = tabs.newTab(false);
    tabs.renameTab(ws.id, t.name);
    tabs.setColor(ws.id, t.color);
    if (t.groupId) tabs.bindGroup(t.groupId, ws.id);
  }
  const activeWs = tabs.tabs[saved.activeIndex];
  if (activeWs) tabs.switchTo(activeWs.id);
  return true;
}

// PTYs whose exit event arrived before their pane finished starting.
const earlyExits = new Map<number, PtyExit>();

// ---------- welcome / pane-setup surface (#194) ----------
// There is no global "agent mode" anymore: every pane opens as the welcome /
// pane-setup surface, where the user declares its kind (Agent / Orchestrator /
// Terminal). The setup pane has no PTY until the user submits — so nothing can
// resize a ConPTY before then (constraint 1).

/** Open a welcome / pane-setup pane in `ws`, wiring its submit to spawn the
 *  chosen kind. Returns the setup pane (already placed; PTY-less until submit). */
function openWelcomeIn(ws: Workspace, dir: "row" | "column" = "row", relativeTo?: Pane): Pane {
  const form = new WelcomeForm();
  const pane = ws.grid.openWelcomePane(eventsFor(ws), form.el, dir, relativeTo);
  form.onSubmit = (result) => void handleWelcomeSubmit(ws, pane, result);
  return pane;
}

/** Act on a welcome submission: convert the setup pane into the chosen kind.
 *  Terminal → a shell in place; Agent → the first pane in place, the rest fanned
 *  out beside it; Orchestrator → its own project tab (the setup pane retires). */
async function handleWelcomeSubmit(ws: Workspace, pane: Pane, result: WelcomeResult): Promise<void> {
  if (result.kind === "terminal") {
    // shellKind is captured (#194) but Phase 1 spawns the default shell only;
    // per-kind shell spawning lands in Phase 2, so it isn't threaded to the PTY yet.
    await pane.startFromWelcome({ name: result.name, cwd: result.cwd });
    reapIfExited(ws, pane);
    persistTabs();
    return;
  }

  if (result.kind === "orchestrator") {
    await launchOrchestratorTab(result.config);
    // The setup pane has served its purpose. A split slot just closes; a
    // dedicated welcome tab (fresh start / Ctrl+T) closes entirely so we don't
    // strand a blank tab beside the new orchestrator tab. (The sole-pane /
    // sole-tab case can't happen here — launchOrchestratorTab just added a tab.)
    if (ws.grid.paneCount > 1) ws.grid.closePane(pane);
    else if (tabs.count > 1) tabs.closeTab(ws.id);
    return;
  }

  // Agent panes: the setup pane becomes the first agent; any extras fan out
  // beside it, alternating split direction so a fleet lays out as a matrix
  // instead of ever-thinner slivers.
  const [first, ...rest] = result.specs;
  await pane.startFromWelcome({ name: first.name, cwd: first.cwd, command: first.command });
  reapIfExited(ws, pane);
  let prev: Pane = pane;
  let d: "row" | "column" = "column";
  for (const spec of rest) {
    const p = await ws.grid.openPane(
      { name: spec.name, cwd: spec.cwd, command: spec.command },
      eventsFor(ws),
      d,
      prev
    );
    reapIfExited(ws, p);
    prev = p;
    d = d === "row" ? "column" : "row";
  }
  persistTabs();
}

/** Open a welcome pane in the active tab — the entry point the toolbar/shortcuts
 *  use for a "new pane". */
const openPane = (dir: "row" | "column" = "row", relativeTo?: Pane): void => {
  openWelcomeIn(tabs.activeWorkspace, dir, relativeTo);
};

function reapIfExited(ws: Workspace, pane: Pane): void {
  if (pane.ptyId === null) return;
  const exit = earlyExits.get(pane.ptyId);
  if (!exit) return;
  earlyExits.delete(pane.ptyId);
  if (pane.keepOpenOnExit(exit)) pane.notifyExited(exit.exit_code);
  else ws.grid.closePane(pane, false);
}

const sessions = new SessionBrowser(
  sessionsEl,
  (s: SessionInfo) => {
    void restoreSession(s);
  },
  orchSessionRoles
);

async function restoreSession(s: SessionInfo): Promise<void> {
  // Recorded orchestration sessions restore into their group — MCP identity,
  // badges, and task board included — instead of a powerless plain `--resume`.
  const orchRole = s.source === "claude" ? sessions.roleFor(s) : undefined;
  if (orchRole) {
    // Route a restored group into the tab that OWNS it, if one exists — a
    // persisted tab (its shell restored on boot) whose group binding survived,
    // or a tab already hosting that group this session. This is the real
    // persistence↔restore integration (#63): the group re-inhabits its own tab
    // through the resume machinery, not whatever tab happens to be active. Only
    // when no tab owns the group does it land in the active tab.
    const owning = tabs.workspaceForGroup(orchRole.group_id);
    const ws = owning ?? tabs.activeWorkspace;
    if (owning && owning.id !== tabs.activeTabId) tabs.switchTo(owning.id);
    try {
      const restored = await resumeOrchSession(ws.grid, eventsFor(ws), s.id, {
        group: orchRole.group_id,
        role: orchRole.role,
      });
      // Bind the restored group to this tab so its rejoined workers spawn here
      // and focus/attention resolve here (#63); idempotent when the tab already
      // owned it. Pane lookups scan live panes, so there's no per-pty binding.
      if (restored) {
        tabs.bindGroup(restored.groupId, ws.id);
        persistTabs();
      }
    } catch (err) {
      showFatal(String(err));
    }
    return;
  }
  // Plain (non-orchestration) sessions restore into the active tab.
  const ws = tabs.activeWorkspace;
  const name =
    (s.source === "claude" ? "claude · " : "copilot · ") +
    (s.title.length > 34 ? s.title.slice(0, 34) + "…" : s.title);
  const pane = await ws.grid.openPane(
    { name, cwd: s.cwd || undefined, command: s.resume_command },
    eventsFor(ws),
    ws.grid.paneCount >= 2 ? "column" : "row"
  );
  reapIfExited(ws, pane);
}

// When a process exits on its own, retire its pane — unless it was a
// command pane dying with an error, which stays open to show the output.
void onPtyExit((exit) => {
  const found = findPaneAcrossTabs(exit.id);
  if (!found) {
    earlyExits.set(exit.id, exit);
    // A pane that never finishes starting would leak its entry forever.
    window.setTimeout(() => earlyExits.delete(exit.id), 5 * 60_000);
    return;
  }
  const { ws, pane } = found;
  if (pane.keepOpenOnExit(exit)) pane.notifyExited(exit.exit_code);
  else ws.grid.closePane(pane, false);
});

// Global shortcuts (terminals decline these in their key handlers).
document.addEventListener(
  "keydown",
  (e) => {
    const action = matchShortcut(e);
    if (!action) return;
    e.preventDefault();
    e.stopPropagation();
    switch (action) {
      case "split-right":
        openPane("row");
        break;
      case "split-down":
        openPane("column");
        break;
      case "close-pane": {
        const g = activeGrid();
        if (g.activePane) g.closePane(g.activePane);
        break;
      }
      case "new-tab":
        void openUserTab();
        break;
      case "close-tab":
        // Route through the strip's two-step confirm (destructive if the tab
        // owns a group), same as clicking its ✕ (LOW-1).
        if (tabs.activeTabId) tabBar?.requestClose(tabs.activeTabId);
        break;
      case "next-tab":
        tabs.nextTab();
        break;
      case "prev-tab":
        tabs.prevTab();
        break;
      case "toggle-sessions":
        sessions.toggle();
        break;
      case "toggle-git":
        activeGrid().activePane?.toggleGitView();
        break;
      case "toggle-issues":
        activeGrid().activePane?.toggleIssuesView();
        break;
      case "toggle-files":
        activeGrid().activePane?.toggleFileEditView();
        break;
      case "open-editor":
        void activeGrid().activePane?.openInEditor();
        break;
      case "toggle-tasks":
        activeGrid().activePane?.toggleTasksView();
        break;
      case "toggle-audit":
        activeGrid().activePane?.toggleAuditView();
        break;
      case "toggle-group":
        activeGrid().activePane?.toggleGroupView();
        break;
      case "focus-compose":
        activeGrid().activePane?.focusCompose();
        break;
      case "voice-ptt":
        voiceController.toggleFromHotkey();
        break;
      case "maximize-pane": {
        const g = activeGrid();
        if (g.activePane) g.toggleMaximize(g.activePane);
        break;
      }
      case "minimize-pane": {
        const g = activeGrid();
        if (g.activePane) g.minimize(g.activePane);
        break;
      }
      case "rename-pane":
        activeGrid().activePane?.startRename();
        break;
      case "focus-left":
        activeGrid().moveFocus("left");
        break;
      case "focus-right":
        activeGrid().moveFocus("right");
        break;
      case "focus-up":
        activeGrid().moveFocus("up");
        break;
      case "focus-down":
        activeGrid().moveFocus("down");
        break;
    }
  },
  { capture: true }
);

// Top bar buttons.
document.getElementById("btn-sessions")!.addEventListener("click", () => sessions.toggle());
document.getElementById("btn-split-right")!.addEventListener("click", () => openPane("row"));
document.getElementById("btn-split-down")!.addEventListener("click", () => openPane("column"));

// Keep the browser from hijacking terminal-relevant defaults (Ctrl+F etc.
// stays inside the shell; F5/F7 reach TUI apps instead of the webview).
window.addEventListener("contextmenu", (e) => {
  if ((e.target as HTMLElement).closest(".pane-term")) e.preventDefault();
});

// WebView2 can come up without keyboard focus; make sure the active
// terminal reclaims it whenever the window is (re)focused.
window.addEventListener("focus", () => activeGrid().activePane?.focus());

// Stamp the running app version into the brand badge (single source of
// truth: tauri.conf.json). Non-fatal — the badge just stays blank if the
// backend can't answer.
void (async () => {
  try {
    const el = document.getElementById("app-version");
    if (el) el.textContent = `v${await getVersion()}`;
  } catch {
    /* version is cosmetic; ignore */
  }
})();

// Crash observability (issue #53): if the previous run died without a clean
// exit, the backend armed a notice naming the newest crash log. Drain it once
// and surface it as an info toast so the user knows there's something to read.
void (async () => {
  try {
    const notice = await invoke<string | null>("take_startup_notice");
    if (notice) showToast(notice, "info");
  } catch {
    /* observability is best-effort; never block startup on it */
  }
})();

// Start streaming CPU/mem/GPU/VRAM into the bottom status bar.
initStatusBar();

// Let the shortcut hint bar scroll horizontally on a vertical wheel when it
// overflows a narrow window.
initHintBar();

// Orchestration is tab-aware (#63): spawns land in their group's tab (created on
// first sight), focus switches tab then pane, group-end closes the owning tab's
// panes, and attention badges hidden tabs' strip entries. The router
// (orchWiring) is implemented over the TabManager above. Wired before any
// orchestrator can launch (below), so no spawn event races an unready router.
initOrchestration(orchWiring);

// Boot the tab layer. Restoring the tab set is now async (it reads the durable
// backend store), so the whole seed → mount → fill sequence is one async flow.
// Preview thumbnails serialize live on hover (see TabBar) from the in-memory
// buffer — no layout, no PTY resize (#63 no-resize invariant).
void (async () => {
  // Restore the saved tab set, or seed the one default tab (never-zero-tabs
  // floor). Subscribe persistTabs AFTER restore so rebuilding the saved set
  // doesn't redundantly write it straight back.
  const didRestore = await restoreTabs();
  if (!didRestore) tabs.newTab();
  tabs.onChange(persistTabs);
  // The "+" button opens a real starting surface, same as the shortcut.
  tabBar = new TabBar(tabBarEl, tabs, () => void openUserTab());

  await ensureOutputRouter();
  // Empty-tab fill (#194): every empty tab — a restored tab whose panes don't
  // auto-revive, a group-bound tab whose orchestrator hasn't resumed, or the
  // brand-new default tab on a fresh start — opens the welcome / pane-setup
  // surface. It's in-pane content (no PTY until submit), so filling a background
  // tab no longer risks the old floating-modal-over-the-active-tab problem.
  if (didRestore) {
    for (const ws of tabs.tabs) {
      if (ws.grid.paneCount === 0) openWelcomeIn(ws);
    }
  } else if (tabs.activeWorkspace.grid.paneCount === 0) {
    openWelcomeIn(tabs.activeWorkspace);
  }
})();

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
import { ensureOutputRouter, onPtyExit, type PtyExit, type SessionInfo } from "./pty";
import { matchShortcut } from "./shortcuts";
import { voiceController } from "./voicecontrol";
import { initStatusBar } from "./statusbar";
import { initHintBar } from "./hintbar";
import { AgentLauncher } from "./launcher";
import { getAgentMode, setAgentMode } from "./agents";
import { initOrchestration, launchOrchestrator, orchSessionRoles, resumeOrchSession } from "./orchestration";

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
    // Last pane in this tab closed → keep the tab's grid non-empty.
    void openPaneIn(w);
  });
  stackEl.appendChild(ws.el);
  return ws;
});

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
    onSplit: (pane, dir) => void openPaneIn(ws, dir, pane),
    onMinimize: (pane) => ws.grid.minimize(pane),
    onMaximize: (pane) => ws.grid.toggleMaximize(pane),
    onToggleGroupMinimize: (pane) => {
      const groupId = pane.orchGroupId;
      if (groupId) ws.grid.toggleGroupMinimize(groupId);
    },
  };
}

/** Find a pane by pty id across ALL tabs — a PTY exit can belong to any tab,
 *  not just the active one. */
function findPaneAcrossTabs(ptyId: number): { ws: Workspace; pane: Pane } | null {
  for (const ws of tabs.tabs) {
    const pane = ws.grid.findByPtyId(ptyId);
    if (pane) return { ws, pane };
  }
  return null;
}

// PTYs whose exit event arrived before their pane finished starting.
const earlyExits = new Map<number, PtyExit>();

async function openShellIn(
  ws: Workspace,
  dir: "row" | "column" = "row",
  relativeTo?: Pane
): Promise<Pane> {
  const pane = await ws.grid.openPane({}, eventsFor(ws), dir, relativeTo);
  reapIfExited(ws, pane);
  return pane;
}

// ---------- agent mode ----------
// When on, new panes host an agent CLI (chosen in the launcher dialog)
// instead of a plain shell. Persisted across restarts.

const launcher = new AgentLauncher();
let agentMode = getAgentMode();

const btnAgentMode = document.getElementById("btn-agent-mode")!;
btnAgentMode.addEventListener("click", () => toggleAgentMode());

function toggleAgentMode(): void {
  agentMode = !agentMode;
  setAgentMode(agentMode);
  renderAgentMode();
}

function renderAgentMode(): void {
  btnAgentMode.classList.toggle("on", agentMode);
  const what = agentMode ? "agent" : "terminal";
  document.getElementById("btn-split-right")!.title = `New ${what} right (Ctrl+Shift+E)`;
  document.getElementById("btn-split-down")!.title = `New ${what} below (Ctrl+Shift+O)`;
}
renderAgentMode();

/** Open a new pane honoring the current mode: agent mode routes through the
 *  launcher dialog; cancelling only falls back to a shell when the grid
 *  would otherwise be empty. The launcher can resolve to one pane, a fleet
 *  of N panes, or an orchestrator group (which opens its own panes). */
async function openPaneIn(
  ws: Workspace,
  dir: "row" | "column" = "row",
  relativeTo?: Pane
): Promise<void> {
  if (!agentMode) {
    await openShellIn(ws, dir, relativeTo);
    return;
  }
  const result = await launcher.show();
  if (!result) {
    if (ws.grid.paneCount === 0) await openShellIn(ws, dir);
    else ws.grid.activePane?.focus();
    return;
  }
  if (result.kind === "orchestrator") {
    await launchOrchestrator(ws.grid, eventsFor(ws), result.config);
    return;
  }
  // Alternate split direction pane-to-pane so a fleet lays out as a grid
  // instead of ever-thinner slivers.
  let prev = relativeTo;
  let d = dir;
  for (const spec of result.specs) {
    const pane = await ws.grid.openPane(
      { name: spec.name, cwd: spec.cwd, command: spec.command },
      eventsFor(ws),
      d,
      prev
    );
    reapIfExited(ws, pane);
    prev = pane;
    d = d === "row" ? "column" : "row";
  }
}

/** Open a pane in the active tab — the entry point the toolbar/shortcuts use. */
const openPane = (dir: "row" | "column" = "row", relativeTo?: Pane): Promise<void> =>
  openPaneIn(tabs.activeWorkspace, dir, relativeTo);

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
  // Sessions restore into the active tab.
  const ws = tabs.activeWorkspace;
  // Recorded orchestration sessions restore into their group — MCP
  // identity, badges, and task board included — instead of a powerless
  // plain `--resume`.
  const orchRole = s.source === "claude" ? sessions.roleFor(s) : undefined;
  if (orchRole) {
    try {
      await resumeOrchSession(ws.grid, eventsFor(ws), s.id, {
        group: orchRole.group_id,
        role: orchRole.role,
      });
    } catch (err) {
      showFatal(String(err));
    }
    return;
  }
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
    // The launcher dialog is modal: it handles Enter/Escape itself and
    // app shortcuts must not fire behind it.
    if (launcher.isOpen) return;
    const action = matchShortcut(e);
    if (!action) return;
    e.preventDefault();
    e.stopPropagation();
    switch (action) {
      case "split-right":
        void openPane("row");
        break;
      case "split-down":
        void openPane("column");
        break;
      case "toggle-agent-mode":
        toggleAgentMode();
        break;
      case "close-pane": {
        const g = activeGrid();
        if (g.activePane) g.closePane(g.activePane);
        break;
      }
      case "new-tab":
        tabs.newTab();
        break;
      case "close-tab":
        tabs.closeTab(tabs.activeTabId!);
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
document.getElementById("btn-split-right")!.addEventListener("click", () => void openPane("row"));
document.getElementById("btn-split-down")!.addEventListener("click", () => void openPane("column"));

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

// Orchestration: open badged panes when the backend spawns agents. Every event
// resolves to the ACTIVE tab for now; TODO(#63 phase 3, worker B) routes by
// group_id/pty_id across all tabs (see OrchTargetResolver).
initOrchestration(() => {
  const ws = tabs.activeWorkspace;
  return { grid: ws.grid, paneEvents: eventsFor(ws) };
});

// Seed the one default tab (the "never zero tabs" floor) and mount the tab bar,
// then open the first pane in it once the output router is up.
tabs.newTab();
new TabBar(tabBarEl, tabs);

void (async () => {
  await ensureOutputRouter();
  await openPane();
})();

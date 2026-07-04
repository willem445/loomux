import "./styles.css";
import { getVersion } from "@tauri-apps/api/app";
import { Grid } from "./grid";
import type { Pane, PaneEvents } from "./pane";
import { SessionBrowser } from "./sessions";
import { ensureOutputRouter, onPtyExit, type PtyExit, type SessionInfo } from "./pty";
import { matchShortcut } from "./shortcuts";
import { initStatusBar } from "./statusbar";
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
window.addEventListener("error", (e) => showFatal(`error: ${e.message}`));
window.addEventListener("unhandledrejection", (e) =>
  showFatal(`unhandled: ${String(e.reason)}`)
);

const gridRoot = document.getElementById("grid-root")!;
const paneDock = document.getElementById("pane-dock")!;
const sessionsEl = document.getElementById("sessions")!;

const grid = new Grid(gridRoot, paneDock, () => {
  // Last pane closed → always keep one pane alive.
  void openPane();
});

const paneEvents: PaneEvents = {
  onFocus: (pane) => grid.setActive(pane),
  onCloseRequest: (pane) => grid.closePane(pane),
  onSplit: (pane, dir) => void openPane(dir, pane),
  onMinimize: (pane) => grid.minimize(pane),
  onMaximize: (pane) => grid.toggleMaximize(pane),
};

// PTYs whose exit event arrived before their pane finished starting.
const earlyExits = new Map<number, PtyExit>();

async function openShell(dir: "row" | "column" = "row", relativeTo?: Pane): Promise<Pane> {
  const pane = await grid.openPane({}, paneEvents, dir, relativeTo);
  reapIfExited(pane);
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
async function openPane(dir: "row" | "column" = "row", relativeTo?: Pane): Promise<void> {
  if (!agentMode) {
    await openShell(dir, relativeTo);
    return;
  }
  const result = await launcher.show();
  if (!result) {
    if (grid.paneCount === 0) await openShell(dir);
    else grid.activePane?.focus();
    return;
  }
  if (result.kind === "orchestrator") {
    await launchOrchestrator(grid, paneEvents, result.config);
    return;
  }
  // Alternate split direction pane-to-pane so a fleet lays out as a grid
  // instead of ever-thinner slivers.
  let prev = relativeTo;
  let d = dir;
  for (const spec of result.specs) {
    const pane = await grid.openPane(
      { name: spec.name, cwd: spec.cwd, command: spec.command },
      paneEvents,
      d,
      prev
    );
    reapIfExited(pane);
    prev = pane;
    d = d === "row" ? "column" : "row";
  }
}

function reapIfExited(pane: Pane): void {
  if (pane.ptyId === null) return;
  const exit = earlyExits.get(pane.ptyId);
  if (!exit) return;
  earlyExits.delete(pane.ptyId);
  if (pane.keepOpenOnExit(exit)) pane.notifyExited(exit.exit_code);
  else grid.closePane(pane, false);
}

const sessions = new SessionBrowser(
  sessionsEl,
  (s: SessionInfo) => {
    void restoreSession(s);
  },
  orchSessionRoles
);

async function restoreSession(s: SessionInfo): Promise<void> {
  // Recorded orchestration sessions restore into their group — MCP
  // identity, badges, and task board included — instead of a powerless
  // plain `--resume`.
  const orchRole = s.source === "claude" ? sessions.roleFor(s) : undefined;
  if (orchRole) {
    try {
      await resumeOrchSession(grid, paneEvents, s.id, {
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
  const pane = await grid.openPane(
    { name, cwd: s.cwd || undefined, command: s.resume_command },
    paneEvents,
    grid.paneCount >= 2 ? "column" : "row"
  );
  reapIfExited(pane);
}

// When a process exits on its own, retire its pane — unless it was a
// command pane dying with an error, which stays open to show the output.
void onPtyExit((exit) => {
  const pane = grid.findByPtyId(exit.id);
  if (!pane) {
    earlyExits.set(exit.id, exit);
    // A pane that never finishes starting would leak its entry forever.
    window.setTimeout(() => earlyExits.delete(exit.id), 5 * 60_000);
    return;
  }
  if (pane.keepOpenOnExit(exit)) pane.notifyExited(exit.exit_code);
  else grid.closePane(pane, false);
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
      case "close-pane":
        if (grid.activePane) grid.closePane(grid.activePane);
        break;
      case "toggle-sessions":
        sessions.toggle();
        break;
      case "toggle-git":
        grid.activePane?.toggleGitView();
        break;
      case "open-editor":
        void grid.activePane?.openInEditor();
        break;
      case "toggle-tasks":
        grid.activePane?.toggleTasksView();
        break;
      case "toggle-audit":
        grid.activePane?.toggleAuditView();
        break;
      case "toggle-group":
        grid.activePane?.toggleGroupView();
        break;
      case "maximize-pane":
        if (grid.activePane) grid.toggleMaximize(grid.activePane);
        break;
      case "minimize-pane":
        if (grid.activePane) grid.minimize(grid.activePane);
        break;
      case "rename-pane":
        grid.activePane?.startRename();
        break;
      case "focus-left":
        grid.moveFocus("left");
        break;
      case "focus-right":
        grid.moveFocus("right");
        break;
      case "focus-up":
        grid.moveFocus("up");
        break;
      case "focus-down":
        grid.moveFocus("down");
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
window.addEventListener("focus", () => grid.activePane?.focus());

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

// Start streaming CPU/mem/GPU/VRAM into the bottom status bar.
initStatusBar();

// Orchestration: open badged panes when the backend spawns agents.
initOrchestration(grid, paneEvents);

void (async () => {
  await ensureOutputRouter();
  await openPane();
})();

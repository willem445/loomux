import "./styles.css";
import { Grid } from "./grid";
import type { Pane, PaneEvents } from "./pane";
import { SessionBrowser } from "./sessions";
import { ensureOutputRouter, onPtyExit, type SessionInfo } from "./pty";
import { matchShortcut } from "./shortcuts";

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
const sessionsEl = document.getElementById("sessions")!;

const grid = new Grid(gridRoot, () => {
  // Last pane closed → always keep one shell alive.
  void openShell();
});

const paneEvents: PaneEvents = {
  onFocus: (pane) => grid.setActive(pane),
  onCloseRequest: (pane) => grid.closePane(pane),
  onSplit: (pane, dir) => void openShell(dir, pane),
};

// PTYs whose exit event arrived before their pane finished starting.
const earlyExits = new Set<number>();

async function openShell(dir: "row" | "column" = "row", relativeTo?: Pane): Promise<Pane> {
  const pane = await grid.openPane({}, paneEvents, dir, relativeTo);
  reapIfExited(pane);
  return pane;
}

function reapIfExited(pane: Pane): void {
  if (pane.ptyId !== null && earlyExits.delete(pane.ptyId)) {
    grid.closePane(pane, false);
  }
}

const sessions = new SessionBrowser(sessionsEl, (s: SessionInfo) => {
  void restoreSession(s);
});

async function restoreSession(s: SessionInfo): Promise<void> {
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

// When a process exits on its own, retire its pane.
void onPtyExit(({ id }) => {
  const pane = grid.findByPtyId(id);
  if (pane) grid.closePane(pane, false);
  else earlyExits.add(id);
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
        void openShell("row");
        break;
      case "split-down":
        void openShell("column");
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
document.getElementById("btn-split-right")!.addEventListener("click", () => void openShell("row"));
document.getElementById("btn-split-down")!.addEventListener("click", () => void openShell("column"));

// Keep the browser from hijacking terminal-relevant defaults (Ctrl+F etc.
// stays inside the shell; F5/F7 reach TUI apps instead of the webview).
window.addEventListener("contextmenu", (e) => {
  if ((e.target as HTMLElement).closest(".pane-term")) e.preventDefault();
});

// WebView2 can come up without keyboard focus; make sure the active
// terminal reclaims it whenever the window is (re)focused.
window.addEventListener("focus", () => grid.activePane?.focus());

void (async () => {
  await ensureOutputRouter();
  await openShell();
})();

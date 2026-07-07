// App-level keyboard shortcuts, shared between the document handler and
// each terminal's custom key handler (which must decline them so they
// bubble up instead of being eaten by the shell).

export type ShortcutAction =
  | "split-right"
  | "split-down"
  | "close-pane"
  | "toggle-sessions"
  | "toggle-agent-mode"
  | "toggle-git"
  | "toggle-issues"
  | "open-editor"
  | "toggle-tasks"
  | "toggle-audit"
  | "toggle-group"
  | "focus-compose"
  | "maximize-pane"
  | "minimize-pane"
  | "rename-pane"
  | "focus-left"
  | "focus-right"
  | "focus-up"
  | "focus-down";

export function matchShortcut(e: KeyboardEvent): ShortcutAction | null {
  if (e.ctrlKey && e.shiftKey && !e.altKey) {
    switch (e.code) {
      case "KeyE": return "split-right";
      case "KeyO": return "split-down";
      case "KeyW": return "close-pane";
      case "KeyP": return "toggle-sessions";
      case "KeyA": return "toggle-agent-mode";
      case "KeyM": return "maximize-pane";
    }
  }
  if (e.altKey && !e.ctrlKey && !e.shiftKey) {
    switch (e.code) {
      case "KeyM": return "minimize-pane";
      case "ArrowLeft": return "focus-left";
      case "ArrowRight": return "focus-right";
      case "ArrowUp": return "focus-up";
      case "ArrowDown": return "focus-down";
      // Alt+G, not Ctrl+Shift+G: WebView2 consumes that as its
      // find-previous accelerator before the page ever sees it.
      case "KeyG": return "toggle-git";
      case "KeyI": return "toggle-issues";
      case "KeyE": return "open-editor";
      case "KeyT": return "toggle-tasks";
      case "KeyA": return "toggle-audit";
      case "KeyO": return "toggle-group";
      case "KeyP": return "focus-compose";
    }
  }
  if (e.code === "F2" && !e.ctrlKey && !e.altKey && !e.shiftKey) return "rename-pane";
  return null;
}

export const isAppShortcut = (e: KeyboardEvent): boolean => matchShortcut(e) !== null;

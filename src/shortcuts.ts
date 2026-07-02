// App-level keyboard shortcuts, shared between the document handler and
// each terminal's custom key handler (which must decline them so they
// bubble up instead of being eaten by the shell).

export type ShortcutAction =
  | "split-right"
  | "split-down"
  | "close-pane"
  | "toggle-sessions"
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
    }
  }
  if (e.altKey && !e.ctrlKey && !e.shiftKey) {
    switch (e.code) {
      case "ArrowLeft": return "focus-left";
      case "ArrowRight": return "focus-right";
      case "ArrowUp": return "focus-up";
      case "ArrowDown": return "focus-down";
    }
  }
  if (e.code === "F2" && !e.ctrlKey && !e.altKey && !e.shiftKey) return "rename-pane";
  return null;
}

export const isAppShortcut = (e: KeyboardEvent): boolean => matchShortcut(e) !== null;

// App-level keyboard shortcuts, shared between the document handler and
// each terminal's custom key handler (which must decline them so they
// bubble up instead of being eaten by the shell).

export type ShortcutAction =
  | "split-right"
  | "split-down"
  | "close-pane"
  | "new-tab"
  | "close-tab"
  | "next-tab"
  | "prev-tab"
  | "move-tab-left"
  | "move-tab-right"
  | "toggle-sessions"
  | "toggle-git"
  | "toggle-issues"
  | "toggle-files"
  | "open-editor"
  | "toggle-tasks"
  | "toggle-audit"
  | "toggle-group"
  | "focus-compose"
  | "voice-ptt"
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
      // Ctrl+Shift+A is intentionally unbound: it toggled the removed agents
      // mode (#194). Left free pending a repurpose decision.
      case "KeyM": return "maximize-pane";
      // Project tabs (#63). T=new, K=close; the bracket keys page between tabs
      // (VSCode-style) and stay clear of Alt+arrows (pane focus) and the browser
      // accelerators WebView2 eats (Ctrl+Tab / Ctrl+PageUp).
      case "KeyT": return "new-tab";
      case "KeyK": return "close-tab";
      case "BracketRight": return "next-tab";
      case "BracketLeft": return "prev-tab";
    }
  }
  // Tab REORDER (#379): same bracket keys as switching, plus Alt — the
  // keyboard alternative to dragging. The issue's suggested Ctrl+Shift+
  // PgUp/PgDn would have been a fresh convention; this instead extends the
  // bracket-key pair the app already uses for tab navigation, so "move" reads
  // as "switch, but Alt for real."
  if (e.ctrlKey && e.shiftKey && e.altKey) {
    switch (e.code) {
      case "BracketRight": return "move-tab-right";
      case "BracketLeft": return "move-tab-left";
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
      // Alt+F (files). Free in loomux; not a WebView2 accelerator (Ctrl+F is —
      // that's why the in-file find uses a button, not Ctrl+F). (#174)
      case "KeyF": return "toggle-files";
      case "KeyE": return "open-editor";
      case "KeyT": return "toggle-tasks";
      case "KeyA": return "toggle-audit";
      case "KeyO": return "toggle-group";
      case "KeyP": return "focus-compose";
      // Alt+S (voice / "speak"). NOT Alt+V: that's Claude Code's paste-image
      // binding, and loomux intercepting it stole it inside agent panes. NOT
      // Alt+M either (that's minimize-pane). Alt+S is free in loomux, unused by
      // Claude Code, and not a readline word-motion binding.
      case "KeyS": return "voice-ptt";
    }
  }
  if (e.code === "F2" && !e.ctrlKey && !e.altKey && !e.shiftKey) return "rename-pane";
  return null;
}

export const isAppShortcut = (e: KeyboardEvent): boolean => matchShortcut(e) !== null;

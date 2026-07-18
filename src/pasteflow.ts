// Pure copy/paste gesture decisions for terminal panes (#370). DOM-free so
// node:test can pin the key matching and the right-click menu SHAPE without a
// browser; contextmenu.ts renders the menu and pane.ts wires the keys/click.
//
// THE BUG THIS EXISTS TO FIX. Terminal panes bound paste to Ctrl+Shift+V only
// (Windows Terminal convention — plain Ctrl+V is a shell's rare "quoted
// insert next char" readline binding) and swallowed every clipboard-read
// failure with `.catch(() => {})`. Users hit plain Ctrl+V from muscle memory
// and got nothing, with no way to tell "wrong key" from "clipboard blocked"
// apart. The fix: plain Ctrl+V pastes too (quoted-insert is a readline corner
// most users have never touched; every other terminal emulator in the app's
// target audience already binds it), and a genuine read failure is a menu
// item / keystroke that visibly does nothing rather than silently nothing —
// see clipboard.ts's readClipboard, which pane.ts surfaces via showToast.

// `import type` only — it erases at compile time, so node:test's strip-only
// TS loader never has to resolve it as a module (a VALUE import of another
// src file is what it can't do; see filemenu.ts's header for the same
// discipline, and panemenu.ts for the identical `MenuItem<A>` reuse).
import type { MenuItem } from "./contextmenu";

/** The subset of a KeyboardEvent the gesture matchers need — kept minimal so
 *  tests build one as a plain object instead of a real DOM event. */
export interface PasteKeyEvent {
  ctrlKey: boolean;
  shiftKey: boolean;
  code: string;
}

/** Is this keydown a terminal paste? Ctrl+V (added by #370) or the original
 *  Ctrl+Shift+V (kept — some users' muscle memory is the other way). */
export function isPasteKey(e: PasteKeyEvent): boolean {
  return e.ctrlKey && e.code === "KeyV";
}

/** Is this keydown the terminal's copy gesture? Ctrl+Shift+C only — plain
 *  Ctrl+C stays SIGINT, never copy; that one is not up for the same
 *  Ctrl+V-style relaxation (breaking Ctrl+C would be far worse than a
 *  once-removed quoted-insert). */
export function isCopyKey(e: PasteKeyEvent): boolean {
  return e.ctrlKey && e.shiftKey && e.code === "KeyC";
}

export type TermMenuAction = { kind: "copy" } | { kind: "paste" };

export type TermMenuItem = MenuItem<TermMenuAction>;

const NO_SELECTION_REASON = "Select text in the terminal first.";

/** Build the terminal's right-click menu (#370: a discoverable Paste
 *  affordance every pane kind was missing). Copy is offered but disabled
 *  without a selection — visible-but-disabled, same convention filemenu.ts
 *  uses for an inapplicable-not-unsupported item, so right-clicking without a
 *  selection still teaches "Copy lives here" instead of hiding it. Paste is
 *  always offered: an empty clipboard is a harmless no-op (readClipboard),
 *  not a reason to grey the item out. */
export function buildTerminalMenu(hasSelection: boolean): TermMenuItem[] {
  return [
    {
      label: "Copy",
      action: { kind: "copy" },
      disabled: !hasSelection,
      reason: hasSelection ? undefined : NO_SELECTION_REASON,
    },
    { label: "Paste", action: { kind: "paste" } },
  ];
}

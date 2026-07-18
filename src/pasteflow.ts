// Pure copy/paste gesture decisions for terminal panes (#370). DOM-free so
// node:test can pin the key matching and the right-click menu SHAPE without a
// browser; contextmenu.ts renders the menu and pane.ts wires the keys/click.
//
// THE BUG THIS EXISTS TO FIX. Terminal panes bound paste to Ctrl+Shift+V only
// (Windows Terminal convention — plain Ctrl+V is a shell's rare "quoted
// insert next char" readline binding) and swallowed every clipboard-read
// failure with `.catch(() => {})`. Users hit plain Ctrl+V from muscle memory
// and got nothing, with no way to tell "wrong key" from "clipboard blocked"
// apart. The fix: a genuine read failure is a menu item / keystroke that
// visibly does nothing rather than silently nothing — see clipboard.ts's
// readClipboard, which pane.ts surfaces via showToast — and plain Ctrl+V
// pastes TOO, but only when `pasteOnPlainCtrlV` opts in (default true;
// see settings.ts). It is not a free win: it costs vim's VISUAL BLOCK mode,
// readline's quoted-insert, and any TUI/agent CLI that wants the raw key —
// review of #370 found the first cost (vim) undocumented and the "every
// terminal emulator already binds it" justification for eating it overstated
// (Windows Terminal does by default; gnome-terminal, iTerm2, kitty, and
// alacritty default to Ctrl+Shift+V precisely to leave plain Ctrl+V for the
// program in the pane). A setting, not an unconditional interception, is the
// same call `Alt+V` made from the other direction (#155, shortcuts.ts) —
// loomux stopped intercepting it once it was shown to steal a key an agent
// pane needed; Ctrl+V gets the option instead of the same unconditional grab.

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
  altKey: boolean;
  code: string;
}

/** Is this keydown a terminal paste? Ctrl+Shift+V always is (the original,
 *  unconditional binding — kept). Plain Ctrl+V is a paste only when
 *  `plainCtrlVPastes` is true — the #370 review's blocking finding: binding
 *  it unconditionally silently steals Ctrl+V from vim's VISUAL BLOCK mode,
 *  readline's quoted-insert, and any TUI/agent CLI that wants the raw key
 *  (the exact failure mode `Alt+V` was deliberately left alone for, #155 —
 *  shortcuts.ts). `settings.ts`'s `pasteOnPlainCtrlV` (default true) is the
 *  opt-out; pane.ts reads it and passes the current value in here on every
 *  keydown rather than this module reading global state, so it stays pure
 *  and testable without a settings singleton.
 *
 *  `!e.altKey` guards a keyboard-layout collision the plain-Ctrl+V case
 *  introduced: on layouts where AltGr (= Ctrl+Alt) + V types a character,
 *  Ctrl+Alt+V would otherwise be swallowed as a paste instead of reaching
 *  the shell as that character. The original Ctrl+Shift+V binding never had
 *  this problem (AltGr doesn't hold Shift), so gate the whole match on it. */
export function isPasteKey(e: PasteKeyEvent, plainCtrlVPastes: boolean): boolean {
  if (e.altKey || !e.ctrlKey || e.code !== "KeyV") return false;
  return e.shiftKey || plainCtrlVPastes;
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

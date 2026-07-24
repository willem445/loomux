// Pure copy/paste keydown gesture decisions for terminal panes (#370).
// DOM-free so node:test can pin the key matching without a browser; pane.ts
// wires the actual keydown handler. (A right-click Copy/Paste context menu
// lived here briefly — removed in the #402 second live-demo round: its paste
// path was unreliable and the human chose not to iterate on a second
// right-click-specific native-event interaction rather than keep debugging
// it. Right-click on a terminal is back to doing nothing, its pre-#370
// state; Ctrl+Shift+C — below — is the supported copy gesture.)
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

/** What a terminal keydown resolves to — pinned as ONE enum, not two
 *  independent booleans, because of a live-demo finding (#402 review): the
 *  DOM layer originally called `isCopyKey`/`isPasteKey` and, on a match,
 *  `return false` from xterm's `attachCustomKeyEventHandler` WITHOUT calling
 *  `e.preventDefault()`. Per xterm's own contract, returning `false` means
 *  only "don't let xterm itself process this key" — it does NOT suppress the
 *  browser's native handling of the same key. For plain Ctrl+V specifically,
 *  the browser's native paste accelerator then fired on xterm's own focused
 *  textarea, which xterm ALSO listens to natively (`handlePasteEvent`,
 *  bound to the DOM `"paste"` event) — so the clipboard text landed twice:
 *  once from our own `pasteFromClipboard()`, once from xterm's untouched
 *  native path. `"copy"`/`"paste"` are the two dispositions that MUST
 *  preventDefault; `"pass"` is the only one that must not — collapsing to
 *  one enum makes forgetting the preventDefault for one of the two, but not
 *  the other, a one-branch typo instead of two independently-fixed call
 *  sites. See pane.ts for the DOM wiring (the preventDefault calls
 *  themselves, and the capture-phase native-`"paste"`-event kill switch that
 *  backstops this regardless of what triggers the browser's native paste). */
export type TermKeyDisposition = "copy" | "paste" | "pass";

export function keyDisposition(e: PasteKeyEvent, plainCtrlVPastes: boolean): TermKeyDisposition {
  if (isCopyKey(e)) return "copy";
  if (isPasteKey(e, plainCtrlVPastes)) return "paste";
  return "pass";
}

// Pure copy/paste keydown gesture decisions for terminal panes (#370).
// DOM-free so node:test can pin the key matching without a browser; pane.ts
// wires the actual keydown handler. (A right-click Copy/Paste context menu
// lived here briefly — removed in the #402 second live-demo round: its paste
// path was unreliable and the human chose not to iterate on a second
// right-click-specific native-event interaction rather than keep debugging
// it. Right-click on a terminal is back to doing nothing, its pre-#370
// state; Ctrl+C/Ctrl+Shift+C — below — are the supported copy gestures.)
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

/** Is this keydown the terminal's EXPLICIT copy gesture? Ctrl+Shift+C —
 *  always intercepted (see `keyDisposition`), copying when there's a
 *  selection and otherwise a harmless no-op. Shift is why it never doubles
 *  as an accidental interrupt: Ctrl+Shift+<letter> sends the identical
 *  control byte a plain Ctrl+<letter> would (Shift doesn't change what a
 *  terminal's Ctrl-modifier maps to), but nobody's muscle memory reaches for
 *  Shift+C to send SIGINT, so eating it unconditionally is safe. */
export function isCopyKey(e: PasteKeyEvent): boolean {
  return e.ctrlKey && e.shiftKey && e.code === "KeyC";
}

/** Is this keydown the terminal's CONDITIONAL copy gesture — plain Ctrl+C?
 *  #402 (third live-demo round): copy only worked via the explicit
 *  Ctrl+Shift+C above, but plain Ctrl+C is the gesture most people reach
 *  for first (mouse-select, then Ctrl+C — the universal convention outside
 *  a terminal), and it did nothing when nothing else was true either. This
 *  matcher alone is NOT enough to decide "copy" — see `keyDisposition`:
 *  plain Ctrl+C is a terminal's actual interrupt key, so it may only
 *  resolve to copy when a selection exists; with no selection it MUST fall
 *  through to the shell as ^C, unconditionally, or SIGINT would be
 *  unreachable from the keyboard whenever anything happened to be
 *  selected. `!e.shiftKey` excludes Ctrl+Shift+C, which `isCopyKey` already
 *  owns (and, unlike this one, is never a fallback interrupt). */
export function isConditionalCopyKey(e: PasteKeyEvent): boolean {
  return e.ctrlKey && !e.shiftKey && !e.altKey && e.code === "KeyC";
}

/** What a terminal keydown resolves to — pinned as ONE enum, not independent
 *  booleans, because of a live-demo finding (#402 review): the DOM layer
 *  originally called `isCopyKey`/`isPasteKey` and, on a match, `return
 *  false` from xterm's `attachCustomKeyEventHandler` WITHOUT calling
 *  `e.preventDefault()`. Per xterm's own contract, returning `false` means
 *  only "don't let xterm itself process this key" — it does NOT suppress the
 *  browser's native handling of the same key. For plain Ctrl+V specifically,
 *  the browser's native paste accelerator then fired on xterm's own focused
 *  textarea, which xterm ALSO listens to natively (`handlePasteEvent`,
 *  bound to the DOM `"paste"` event) — so the clipboard text landed twice:
 *  once from our own `pasteFromClipboard()`, once from xterm's untouched
 *  native path. `"copy"`/`"paste"` are the dispositions that MUST
 *  preventDefault; `"pass"` is the only one that must not — collapsing to
 *  one enum makes forgetting the preventDefault for one branch, but not
 *  another, a one-branch typo instead of independently-fixed call sites. See
 *  pane.ts for the DOM wiring (the preventDefault calls themselves, and the
 *  capture-phase native-`"paste"`-event kill switch that backstops paste
 *  regardless of what triggers the browser's native paste).
 *
 *  `hasSelection` is what makes plain Ctrl+C's copy/interrupt split
 *  possible: it's DOM/xterm runtime state (`term.getSelection()`), not
 *  something derivable from the KeyboardEvent alone, so the caller reads it
 *  once per keydown and passes it in — same discipline as
 *  `plainCtrlVPastes` reading `settings.ts`'s live value. This function is
 *  identical for every pane kind (plain terminal, agent, orchestrator) —
 *  there is no pane-kind branch anywhere in this module or in pane.ts's
 *  keydown wiring, deliberately: see the pane-kind/selection matrix in
 *  pasteflow.test.ts, which pins that a terminal pane and an agent pane
 *  produce the SAME disposition for the same (event, selection) input. */
export type TermKeyDisposition = "copy" | "paste" | "pass";

export function keyDisposition(
  e: PasteKeyEvent,
  plainCtrlVPastes: boolean,
  hasSelection: boolean
): TermKeyDisposition {
  if (isCopyKey(e)) return "copy";
  if (isConditionalCopyKey(e)) return hasSelection ? "copy" : "pass";
  if (isPasteKey(e, plainCtrlVPastes)) return "paste";
  return "pass";
}

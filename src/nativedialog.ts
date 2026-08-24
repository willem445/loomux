// The app's ONE gate on native (OS-owned) dialogs — #1564.
//
// WHAT WENT WRONG. A human clicked Browse on the new-agent repo field, the app
// hung, and then died: `0xc0000005`, an access-violation WRITE to `0x10`, on
// `lock cmpxchg byte [r12+0x10]` with `r12 = 0` — an atomic compare-and-swap
// through a null pointer. The minidump (walked by the human; the full analysis
// is a comment on #1564) puts the crashing frame in app code, called FROM
// `EmbeddedBrowserWebView.dll`'s focus handling, reached through
// `comctl32!CallNextSubclassProc` — our window's subclass chain. A second
// thread was, at that moment, inside `comdlg32!CFileOpenSave::_InitOpenSaveDialog`
// -> `CFileNameComboBox::InitializeControl` -> `ZwUserSetFocus`: the folder
// picker, mid-initialization, taking focus.
//
// WHY A DIALOG'S FOCUS GRAB REACHES US AT ALL. `tauri-plugin-dialog` shows the
// picker with our main window as its OWNER (`set_parent(&window)`) but calls
// `IFileOpenDialog::Show` from a thread it spawns (`pick_folder` ->
// `run_on_main_thread` -> `std::thread::spawn` -> rfd). Windows attaches the two
// threads' input queues for an owner/owned pair, so the dialog thread's
// `SetFocus` is delivered SYNCHRONOUSLY into our main thread's window procedure.
// From there it is all dependency code compiled into loomux.exe: wry's parent
// subclass answers `WM_SETFOCUS` by forcing `ICoreWebView2Controller::MoveFocus`
// back into the webview, and tauri-runtime-wry's `add_GotFocus`/`add_LostFocus`
// handlers run inside WebView2's own focus machinery, each taking two mutexes
// and posting a synthesized window event. Nothing in `src-tauri/src` or `src/`
// registers a WebView2 focus callback or a WndProc subclass, so the frame that
// faulted is not ours to patch — which leaves the INPUT to that path, which is.
//
// WHAT WE OWN, AND WHAT THIS MODULE STOPS.
//
//  1. `main.ts` answered every window `focus` event by pushing focus straight
//     back into the active terminal (`activePane.focus()` -> xterm's textarea).
//     That is the app's only code that INITIATES a focus grab in reaction to a
//     focus change — a positive-feedback edge on exactly the path the minidump
//     names. Harmless when a human alt-tabs; a tug-of-war when the other end is
//     a foreign thread's dialog calling `SetFocus` at the same instant, because
//     WebView2's focus state machine is then re-entered from inside its own
//     focus callback. `reclaimFocusOnWindowFocus` drops OUR half of that pull
//     for as long as a native dialog is outstanding.
//
//  2. Nothing stopped a SECOND picker. A modal dialog disables its owner window,
//     but only once it exists: between the click and the dialog appearing there
//     is an IPC hop, a `run_on_main_thread`, a thread spawn, COM init and — on
//     the machine that crashed — BeyondTrust's `PGHook.dll` in the middle of
//     `CFileOpenSave::Show`. The webview is live and clickable for all of it,
//     and #1564's report is precisely a human watching a click do nothing. A
//     second click used to spawn a second dialog thread against the same owner
//     window, doubling the cross-thread focus traffic through that re-entrant
//     path. `withNativeDialog` admits one at a time.
//
// The suppression is BOUNDED by the promise, not by a clock: `withSubmitLatch`
// releases in a `finally`, so a picker that REJECTS reopens the gate just as a
// picker that resolves does. What holds it shut is one thing only: a request
// that has not settled. Usually that is a dialog on screen. It ALSO covers the
// case where the picker never settles at all — the host wedged, which is what
// #1564 reports — and there the gate stays shut for the session, so Browse does
// nothing until a restart. That is the deliberate trade: a wedged picker is the
// state in which spawning a SECOND dialog thread against the same owner window
// is most dangerous, and it is the state the crash was reported from. A timeout
// would trade the crash back in for the button — and would fire on a human
// browsing folders for two minutes, who is not a fault to begin with.
//
// One latch, read by both halves. The refusal and the focus suppression are the
// same question ("is a native dialog outstanding?") and must never be able to
// answer it differently — CLAUDE.md's one-rule-for-every-input convention — so
// `nativeDialogOpen()` reads the latch that `withNativeDialog` holds, rather
// than a second flag kept alongside it.
//
// DOM-free and reusing `SubmitLatch`/`withSubmitLatch` (panesetup.ts), the
// single-flight mechanism the repo already has, so this file adds a POLICY and
// not a second concurrency primitive. Unit-tested in test/nativedialog.test.ts;
// the end-to-end crash is Windows-native and human-validated (see
// doc/design/native-dialog-focus.md).

// Explicit `.ts`: a VALUE import in a module `node --test` loads off disk.
import { SubmitLatch, withSubmitLatch } from "./panesetup.ts";

/** Process-wide. There is one window and one webview (tauri.conf.json declares a
 *  single window), so "a native dialog is outstanding" is an app-level fact, not
 *  a per-view one. */
const nativeDialog = new SubmitLatch();

/** Whether a native dialog request is outstanding right now — from the click
 *  that asked for it until its promise settles, which spans the gap BEFORE the
 *  dialog appears as well as the time it is up. That gap is the dangerous half:
 *  it is when the webview is still interactive and when the dialog thread is
 *  calling `SetFocus`. */
export function nativeDialogOpen(): boolean {
  return nativeDialog.busy;
}

/** Run `open` only if no native dialog is outstanding; hand a concurrent caller
 *  `null` — which every `pickDirectory` call site already reads as "the human
 *  chose nothing", so a refused second click leaves the field alone instead of
 *  needing a new failure path. */
export function withNativeDialog<T>(open: () => Promise<T>): Promise<T | null> {
  return withSubmitLatch<T | null>(nativeDialog, () => null, open);
}

/** The window-`focus` reaction: reclaim terminal focus, unless a native dialog
 *  is outstanding. WebView2 can come up without keyboard focus, which is why the
 *  reclaim exists at all; a dialog's cross-thread focus grab is the one case
 *  where answering a focus event by grabbing focus back is what hurts. */
export function reclaimFocusOnWindowFocus(reclaim: () => void): void {
  if (nativeDialogOpen()) return;
  reclaim();
}

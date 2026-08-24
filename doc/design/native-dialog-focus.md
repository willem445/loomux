# Native dialogs and the WebView2 focus path

Why `src/nativedialog.ts` exists, what it does and does not claim to fix, and what the
escalation is if it turns out not to be enough (#1564).

## The crash

A human clicked **Browse** on the new-agent repo field after a long uptime. The app hung, then
died before the picker ever appeared. From the minidump (walked by the human; the full analysis
is a comment on #1564):

- `0xc0000005`, an access-violation **WRITE to `0x10`** — `lock cmpxchg byte [r12 + 0x10], cl`
  with `r12 = 0`. An atomic compare-and-swap through a null pointer, on the main thread.
- The crashing frame is app code, called from `EmbeddedBrowserWebView.dll`'s focus handling,
  reached through `comctl32!CallNextSubclassProc` — the window-message subclass chain on our
  main window.
- Another thread was inside `comdlg32!CFileOpenSave::_InitOpenSaveDialog` →
  `CFileNameComboBox::InitializeControl` → `ZwUserSetFocus`: the folder picker, mid-init, taking
  focus. BeyondTrust's `PGHook.dll` was injected into that stack.

No Rust panic, no `breadcrumbs.log` entry beyond the next run's `unclean_prev=true`: the process
was killed before any hook ran.

## Why a dialog on another thread reaches our window procedure

`tauri-plugin-dialog` shows the picker with our main window as its **owner**
(`commands.rs`, `dialog_builder.set_parent(&window)`) but calls `IFileOpenDialog::Show` from a
thread it spawns (`desktop.rs`: `run_on_main_thread(...)` → `std::thread::spawn` → rfd). Windows
attaches the input queues of an owner/owned pair, so the dialog thread's `SetFocus` is delivered
**synchronously** into the owner thread's window procedure — ours.

From there the path is entirely dependency code compiled into `orrerix.exe`:

- `wry`'s parent subclass answers `WM_SETFOCUS` by forcing
  `ICoreWebView2Controller::MoveFocus` back into the webview.
- `tauri-runtime-wry` registers `add_GotFocus` / `add_LostFocus` handlers that run **inside**
  WebView2's focus machinery, each taking two mutexes and posting a synthesized window event.

Nothing in `src-tauri/src` or `src/` registers a WebView2 focus callback or a `WndProc` subclass.
The faulting frame is therefore not ours to patch — but the **input** to that path is.

## What loomux contributed, and what changed

**1. The app pulled focus back, from inside a focus change.** `main.ts` answered every window
`focus` event with `activeGrid().activePane?.focus()` — an xterm textarea focus. That is the app's
only code that *initiates* a focus grab in reaction to a focus change. It is harmless when a human
alt-tabs. It is a tug-of-war when the other end is a foreign thread's dialog calling `SetFocus` at
the same instant, because WebView2's focus state machine is then re-entered from inside its own
focus callback — which is where the minidump faulted. The reclaim is now suppressed while a native
dialog is outstanding (`reclaimFocusOnWindowFocus`).

The reclaim itself stays: WebView2 can come up without keyboard focus, which is what it is for.

**2. Nothing stopped a second picker.** A modal dialog disables its owner window — but only once
it *exists*. Between the click and the dialog appearing there is an IPC hop, a
`run_on_main_thread`, a thread spawn, COM init and, on the machine that crashed, `PGHook.dll` in
the middle of `CFileOpenSave::Show`. The webview is live and clickable for all of it, and #1564's
report is exactly a human watching a click do nothing. A second click spawned a second dialog
thread against the same owner window, doubling the cross-thread focus traffic through that
re-entrant path. `pickDirectory` now single-flights at the seam (`withNativeDialog`); a refused
request resolves `null`, which every call site already reads as "the human chose nothing".

### Three properties worth stating

**One latch, two readers.** The refusal and the focus suppression answer the same question. They
read the same `SubmitLatch` rather than two flags that can drift — the repo's
one-rule-for-every-input convention.

**Bounded by the promise, not a clock.** `withSubmitLatch` releases in a `finally`, so a picker
that *rejects* reopens the gate exactly as one that resolves does. What holds it shut is one
thing only: a request that has not settled.

Usually that is a dialog on screen. It also covers the case where the picker **never settles** —
the host wedged, which is precisely what #1564 reports ("hung, then crashed") — and there the
gate stays shut for the rest of the session: Browse does nothing until a restart. That is the
deliberate trade, and it is the honest cost of this fix. A wedged picker is the state in which
spawning a second dialog thread against the same owner window is *most* dangerous, and it is the
state the crash was reported from. Releasing on a clock would trade the crash back in for the
button, and would fire on a human who is merely browsing folders slowly.

**A policy, not a new primitive.** `SubmitLatch` / `withSubmitLatch` (`panesetup.ts`) is the
single-flight mechanism the repo already has, added for the same class of defect: a second trigger
that skipped the first one's gate.

## What was weighed and declined

- **Null-guard the atomic op in the focus callback.** Not ours — the callback is in `wry` /
  `tauri-runtime-wry`. Vendoring a patch or forking is not warranted before the cheap fix has been
  tried on the machine that crashes.
- **Initialize per-window state before the dialog can open.** Not applicable: `tauri.conf.json`
  declares one window, created at boot and destroyed at exit. There is no create/destroy interval
  in which per-window state is half-built, and the crash came after long uptime.
- **Give the dialog the right owner.** Already done by the plugin.
- **Marshal `Show` onto the thread that owns the window.** This is the textbook Win32 rule and it
  would address the mechanism at its root. It is declined *for now* because it means replacing the
  plugin's picker with our own command that blocks the tao event loop for the entire life of the
  dialog: the failure mode is a wedged main thread, which is worse than the bug; it is GUI-only, so
  neither CI nor an agent can validate it (agents do not build Rust locally — see the
  `ci-validate` skill); and it forks us off a maintained plugin.

  **This is the escalation if the fix above does not hold.** If the human still sees the crash on
  the work PC, the next step is a `#[tauri::command]` that runs `rfd::FileDialog::pick_folder()`
  through `app.run_on_main_thread`, so `Show` is called by the thread that owns the parent HWND —
  with the event-loop-blocking behaviour argued and hand-validated before it ships.

## Validation boundary

The end-to-end race is Windows-native: comdlg32's dialog thread against WebView2's focus
machinery, exposed with `PGHook.dll` injected. CI has no WebView2 focus, no `comdlg32` and no
PGHook, and it cannot be reproduced headlessly at all. **The end-to-end proof is the human
repeating the gesture on the PC that crashed** — the same native-race exemption #903 and #1561
carry.

What is *not* on trust is the app-side rule the fix rests on. `test/nativedialog.test.ts` and
`test/transport.test.ts` pin it: one dialog at a time, a refusal that reads as a cancel, the first
dialog still answering its own caller, the gate reopening on **both** settle paths, the gate shut
from the moment of the request (the gap the report was lost in), and the focus reclaim suppressed
while a dialog is outstanding — with the positive control that it runs otherwise.

**Both halves are pinned at the source too, symmetrically.** A decision being correct says nothing
about the app still routing through it, and both routes are one-line edits the compiler cannot see:
`noUnusedLocals` catches deleting a call, never an honest revert that drops the import with it. So
two source scans stand beside the behavioural tests — one refusing any `.pickDirectory` member
access that is not the one inside the gate, one requiring every window-level `focus` listener in
`src/` to route through `reclaimFocusOnWindowFocus`. Both are default-deny, keyed on the entity
rather than on a binding's name, carry population controls, and state their blind spots where they
are implemented. A suppression one line can walk around is no more a suppression than a gate one
call site can walk around is a gate.

# #360 multiwebview-embedding spike — harness

Throwaway dev harness. Does not ship; not referenced by any build target.
Branch `fix/360-plugin-embed`, committed as a reference alongside the earlier
Phase-0/Phase-0.5 spikes on `spike/360-sandbox-proof`.

## Why this spike exists

The overlay approach shipped by #360 Slice D (`pluginbroker::plugin_open_window`
+ `src/pluginpaneview.ts`) hosts a plugin as a separate, top-level
`WebviewWindow` continuously repositioned/resized over the pane's box. Even
once made borderless (`decorations(false)`, `skip_taskbar(true)`) it is still,
structurally, a second OS window dressed up to look embedded — not a native
pane. The question this spike answers: can Tauri v2's multiwebview API
(`Window::add_child`, behind the `unstable` feature) host the plugin as a
*real* embedded region of the `main` window instead?

## Verdict: VIABLE, with one load-bearing capability-config change the plan
must call out — same shape of prerequisite the Phase-0.5 spike found for
Option B itself.

## Findings, adversarially

### 1. Isolation — the gate (the one that matters most)

**Not viable unmodified. Viable with a required config change, confirmed live.**

Read first, then confirmed empirically:

- `tauri-utils-2.9.3/src/acl/capability.rs:150-157` — `Capability::windows`'s
  own doc comment: *"If a window label matches any of the patterns in this
  list, the capability will be enabled on **all the webviews of that window**,
  regardless of the value of `webviews`. On multiwebview windows, prefer
  specifying `webviews` and omitting `windows` for a fine grained access
  control."* This is Tauri's own documented behavior, not a bug.
- `tauri-2.11.5/src/ipc/authority.rs:439-471` (`resolve_access`) — a command
  is granted if **either** `cmd.webviews` matches the requesting webview's
  label **or** `cmd.windows` matches its *window's* label. A capability that
  grants `windows: ["main"]` (loomux's shipped `capabilities/default.json`,
  unmodified) therefore grants every webview embedded in the `main` window —
  including a plugin child webview — because `add_child` attaches the child
  to the *same* window, so its window label is also `"main"`.

**Confirmed live**, not just from source (harness: `spike_multiwebview.rs` +
`child-webview-adversary.html` + `run-multiwebview-via-cdp.mjs`, driven via
CDP the same way Phase-0/0.5 were):

- **Test A — today's shipped grant leaks.** With `capabilities/default.json`
  exactly as shipped (`"windows": ["main"]`), a child webview added via
  `window.add_child(...)` and labeled `"spike-child"` (granted nothing of its
  own) successfully called `invoke("pty_backend_info")` — a real,
  already-shipped, already-`main`-granted command — and got real backend data
  back:
  ```json
  {
    "route": "invoke(pty_backend_info)",
    "outcome": "SUCCEEDED-i.e.-WINDOWS-GRANT-LEAKED-TO-CHILD-WEBVIEW",
    "detail": "{\"sideloaded_conpty\":true,\"conpty_build\":22621}"
  }
  ```
  This is worse than a residual capability — it is the entire `main-ui`
  permission set (122+ commands: `git_push`, `orch_grant_merge`,
  `ft_write_file`, `spawn_pty`, …) leaking to *any* embedded child webview,
  by design, per Tauri's own documented semantics.
- **Test B — switching the grant to `webviews` fixes it, confirmed both
  directions.** Editing `default.json` from `"windows": ["main"]` to
  `"webviews": ["main"]` (Tauri's own recommended pattern for multiwebview
  windows) and rebuilding:
  - The same child webview is now **denied**, with a real, named, auditable
    error identical in shape to Phase-0.5's Option B denials:
    ```
    pty_backend_info not allowed on window "main", webview "spike-child", URL: local
    allowed on: [webviews: "main", URL: local]
    referenced by: capability: default, permission: allow-pty-backend-info
    ```
  - `main`'s own frame (its initial webview, whose label is also `"main"` by
    Tauri's own convention for a single-webview window) still succeeds:
    `{"sideloaded_conpty":true,"conpty_build":22621}`.

**The prerequisite this spike is naming, the same way Phase-0.5 named "add an
app ACL manifest":** every capability that currently scopes to `windows` (today
just `default.json`) must be rewritten to `webviews` before any plugin is
hosted as a child webview of `main`. This is a **standing landmine**, not a
one-time migration cost: `windows` and `webviews` are silently interchangeable
from a JSON-schema point of view (both are `Vec<String>` glob lists, no lint
distinguishes "meant for a multiwebview app" from "meant for a single-webview
one") — any future contributor who adds a new `main`-scoped capability using
`windows: ["main"]` out of habit (the pattern every *other* capability in this
file, and every Tauri tutorial, uses) silently re-opens full command access to
every plugin embedded in `main`, with no test failure and no error at
run time for the mistake itself — only a working exploit. `plugin.json`'s own
grant (`windows: ["plugin-*"]`) is unaffected either way, since a child
webview's *window* label is never `plugin-*` — but that means the plugin's
own capability would need `webviews: ["plugin-*"]` to grant it its two broker
commands, which is the easy, obviously-plugin-shaped half of this; the
dangerous half is remembering to defend `main`, not the plugin.

### 2. No subframe leak — structurally different from the failed iframe, confirmed live

- `wry-0.55.1/src/webview2/mod.rs:94-178` (`new_as_child` → `new_in_hwnd`) —
  a child `Webview` gets its **own native child `HWND`**
  (`create_container_hwnd`) and its **own `ICoreWebView2Controller`**
  (`CreateCoreWebView2Controller`/`CreateCoreWebView2ControllerWithOptions`).
  This is categorically different from Phase-0's `<iframe>` (a node in one
  webview's own DOM tree, subject to wry's Windows subframe-script-injection
  bug at `wry-0.55.1/src/lib.rs:990`) — a child webview is a second, distinct
  WebView2 controller, the same structural category as the existing
  `WebviewWindow` model, just windowless.
- **Confirmed live**: the child webview enumerated as its **own separate CDP
  target** (`http://localhost:1420/dev-harness/.../child-webview-adversary.html`,
  distinct from `main`'s `http://localhost:1420/` target) — proving it is a
  genuinely separate top-level debugging target/controller, not a subframe of
  `main`'s document. `window.opener` was `null`, same as the WebviewWindow
  case.

### 3. Embedding mechanics — simpler than the overlay-window model

- `Window::add_child(webview_builder, position, size)` positions the child
  **relative to the parent window's own client area** (`LogicalPosition`/
  `LogicalSize`), not absolute screen coordinates — this removes the overlay
  approach's whole `scaleFactor()`/`innerPosition()`/multi-monitor-DPI
  translation step (`pluginpaneview.ts`'s `reposition()`) entirely. Tracking
  a pane's box becomes: call `webview.setPosition()`/`setSize()` with the
  pane's own `getBoundingClientRect()` numbers directly.
- `Webview<R>` (not just `Window<R>`) exposes `set_position`, `set_size`,
  `hide`, `show`, `set_focus`, `set_zoom` — the same primitives
  `pluginpaneview.ts` already calls on a `WebviewWindow`, just on a `Webview`
  handle instead. The hide-when-pane-not-visible logic
  (`pluginwindow.ts`'s `pluginWindowShouldShow`) carries over unchanged.
- No separate OS window at all: a child webview has no title bar, no
  minimize/maximize/close, no taskbar/alt-tab entry, because it was never a
  top-level window to begin with — this is a structural fix for the ORIGINAL
  bug (floating, fully-decorated OS window), not a polish of the overlay
  approach's symptoms.
- Not independently confirmed by screenshot this session (see "What this
  spike did not do," below) — the CDP target enumeration and successful
  `add_child` call are strong indirect evidence (a `HWND`-less/undecorated
  child would not behave differently here either way), but a human should
  visually confirm no title bar/taskbar entry appears, per the verify steps
  any real implementation PR will need anyway.

### 4. `unstable` feature cost

- `tauri-runtime-wry-2.11.4/Cargo.toml:60`: `unstable = []` — the feature
  adds **zero new dependencies**. `tauri/Cargo.toml:130`:
  `unstable = ["tauri-runtime-wry?/unstable"]` — same, a pure cfg-gate
  forwarding to that empty feature. Confirmed via `cargo tree -e features`
  before and after enabling it on this branch: no new crate entries, in
  particular **no new `getrandom`** (the two `getrandom` instances already in
  this repo's dependency tree — v0.3.4 via `tauri`'s own proc-macro/build-dep
  graph, v0.4.3 via `uuid`→`cfb`→`infer`→`tauri-utils`/`tauri-build`'s
  build-dependency graph — are pre-existing, host-side build/proc-macro
  tooling, not linked into the shipped Windows binary, and are unaffected by
  `unstable` either way; confirmed via `cargo tree -i getrandom@<ver>`,
  neither dependency chain touches a runtime dependency of the `loomux`
  binary itself).
- Compiles cleanly (`cargo check --locked -j 4`, this branch) with `tauri`'s
  `unstable` feature enabled.
- `#[cfg(any(test, all(desktop, feature = "unstable")))]` gates `add_child`
  itself (`tauri-2.11.5/src/window/mod.rs:1127-1129`) — desktop-only, exactly
  loomux's target, no mobile-path concern.

### 5. Security posture carries over — same shape as Option B, one caveat

- `on_navigation` is defined once, on the shared `WebviewBuilder` base
  (`tauri-2.11.5/src/webview/mod.rs:284,528`) — available identically for a
  child webview. No redesign needed; the origin-lock predicate
  (`pluginbroker::is_navigation_allowed`) carries over verbatim.
- CSP is unaffected either way — already a `plugin://` scheme-handler-set
  header question (Slice B), not a `WebviewWindow`-vs-`Webview` question.
- **The async-build requirement is unchanged, confirmed from source, not
  just by analogy**: `WebviewBuilder::new`'s own rustdoc
  (`tauri-2.11.5/src/webview/mod.rs:289`) states verbatim *"On Windows, this
  function deadlocks when used in a synchronous command or event handlers...
  use `async` commands"* — the exact wry#583 warning `plugin_open_window`'s
  own doc comment already cites for `WebviewWindowBuilder::build`. `add_child`
  itself blocks on a `channel::recv()` round-trip to the main thread
  (`tauri-2.11.5/src/window/mod.rs:1141-1145`), the identical shape. This
  spike's own `spike_open_child_webview` command is `async fn` for this
  reason; the real implementation must be too.

## What this spike did not do

- No visual/screenshot confirmation that a child webview truly shows no
  title bar/taskbar entry (see point 3) — inferred from the API being
  windowless by construction, not eyeballed.
- Did not spike the pane-resize/move/tab-hide tracking loop end-to-end (only
  a static `add_child` call + one position/size) — expected to be a
  straightforward port of `pluginpaneview.ts`'s existing tracking logic onto
  `Webview` methods instead of `WebviewWindow` ones (see point 3), but not
  itself exercised here.
- Did not spike focus/z-order edge cases (a plugin embedded in `main`
  necessarily shares `main`'s own focus/z-order — there is no "loses focus
  when the app loses focus" question the way a separate `WebviewWindow` has,
  which if anything *removes* an edge case the overlay approach carried, but
  this spike did not exercise it directly).

## Recommendation

**Replace the overlay-window approach with `add_child` embedding**, gated on
one prerequisite the real implementation must budget for explicitly:

1. Rewrite every `main`-scoped capability (today: just `default.json`) from
   `windows: ["main"]` to `webviews: ["main"]` — confirmed live to both (a)
   deny an embedded plugin child webview real backend commands and (b) leave
   `main`'s own frame fully functional. Add a coherence check (mirroring
   `tests/acl_manifest.rs`'s existing role for the app-manifest prerequisite)
   that fails CI if any capability file ever reintroduces a bare
   `windows: [...]` grant scoped to a window that hosts plugin children —
   this is the durable defense against the standing-landmine risk in Finding
   1, not a one-time fix-and-forget.
2. Grant the plugin's own capability via `webviews: ["plugin-*"]` (not
   `windows`), since a child webview's window label is always its parent's
   (`"main"`), never `plugin-*`.
3. Port `pluginpaneview.ts`'s tracking (ResizeObserver + main-window
   move/resize listeners + `pluginWindowShouldShow`) from `WebviewWindow`
   methods to the equivalent `Webview` methods — simpler, since position is
   already relative to `main`'s own client area (no scale-factor/
   multi-monitor screen-coordinate math needed).
4. Keep `on_navigation`, the async-build discipline, and CSP-via-scheme-handler
   exactly as already designed for Option B — none of it changes.
5. A human should visually confirm no title bar/taskbar entry on first real
   build (point 3's uncompleted half).

This is a **narrower, more security-relevant prerequisite than Option B's**
(Phase-0.5 needed one new manifest; this needs an existing, shipped
capability file's scoping key changed and permanently guarded), but the
result is a genuinely native embedded pane, not a window dressed up to look
like one — worth it if the guard in (1) is built and tested, not just
documented.

# Spike: iframe internals-scrub proof (#360)

**Verdict: both candidate mitigations fail. The native `add_child` child-webview
model (Option B, `doc/design/pane-plugins.md`) stays. This branch is a
reference artifact — no PR, not for merging.**

## What was tested

Whether an in-DOM `<iframe sandbox="allow-scripts">` hosting a `plugin://`
pane plugin could be denied `window.__TAURI_INTERNALS__` on this repo's exact
baseline (Tauri 2.11.5 / wry 0.55.1 / WebView2 / Windows 10), as an
alternative to the current native child-webview hosting
(`Window::add_child`). Two mitigations were investigated on top of the
already-known Phase-0 finding (`doc/design/pane-plugins.md`'s "Rejected:
sandboxed opaque-origin iframe") that a naive sandboxed iframe leaks a fully
working `__TAURI_INTERNALS__`.

## Verdicts

1. **M1 — main-frame-only injection: UNAVAILABLE.** Tauri's own
   `WebviewBuilder::initialization_script()` already requests this
   (`for_main_frame_only: true`), but both wry's own doc comment
   (`wry-0.55.1/src/lib.rs:990`) and Tauri's own doc comment
   (`tauri-2.11.5/src/webview/mod.rs:830`) state plainly: "Windows: scripts
   are always added to subframes" regardless of the flag.
   `wry-0.55.1/src/webview2/mod.rs:493-495` confirms it in the
   implementation — the flag is never even read on the Windows code path.
   **Live-confirmed**: a marker script added via the main-frame-only API
   leaked into the plugin iframe (`m1MarkerLeaked: true` in the captured
   probe report).

2. **M2 — document-start scrub: FAILS, and where it appeared to "work" it
   was a timing race, not a designed boundary.** Every property Tauri
   attaches to `__TAURI_INTERNALS__` (`ipc`, `postMessage`, `invoke`,
   `transformCallback`, etc.) is defined via `Object.defineProperty` with no
   `configurable`/`writable` flag (`core.js`, `ipc.js`, `ipc-protocol.js`) —
   deletion is a structural no-op once populated — and a caller-supplied
   init script can never register ahead of Tauri's own
   (`tauri-2.11.5/src/manager/webview.rs:157-224`: ours is `extend`-ed in
   last). In the one live run captured, `__TAURI_INTERNALS__` happened to be
   `undefined` in the plugin frame when the scrub script ran — but the M1
   marker (registered via the *identical* mechanism, in the *same* run)
   *did* leak into that same frame, so nothing in the source explains the
   asymmetry. Most likely a load-order race between wry's sequential init-
   script registration and the iframe's own navigation — exactly the kind of
   unowned, non-deterministic behavior nobody should build a trust boundary
   on, and it does not overturn the design doc's already-cited Phase-0
   finding of full, reliable internals leakage under closely comparable
   conditions.

3. **Residual: an iframe inherits `main`'s entire ACL grant — no per-frame
   boundary exists at all.** Even setting timing aside: an in-DOM iframe has
   no webview label of its own (it shares `main`'s single webview), and
   Tauri's ACL is resolved per-webview, not per-frame. Any command `main`'s
   webview can reach is reachable to any script executing inside it,
   including the plugin iframe, directly via `invoke` — completely
   bypassing the broker's `postMessage` relay the moment internals are
   reachable (which is the norm per (1)/(2), not the exception). The
   broker's core premise — "a plugin never sees `__TAURI_INTERNALS__`" —
   cannot be satisfied for a same-webview iframe on this baseline by any
   mitigation reachable through Tauri's public API.

## Recommendation

Stay on Option B (`Window::add_child`, ACL-gated by webview label) — its
isolation is structural (Tauri's ACL resolver denies before any handler
runs), not best-effort. Keep investing in #391's region-clip fix
(`pluginregion.rs`, `SetWindowRgn`) for the z-order/glitch complaints that
originally motivated this spike.

A genuinely different third path — classifying plugin content as a Tauri
**remote** origin (denied IPC backend-side, per-request, regardless of any
JS-level leak) — was analyzed separately and is **not dead**, but requires
replacing the `plugin://` custom-scheme handler with a real loopback HTTP
listener (any registered custom scheme is unconditionally "local" under
`is_local_url`), trading the scheme handler's zero network exposure for a
port reachable by any local process. Tracked as a dedicated follow-up:
[#395](https://github.com/willem445/loomux/issues/395).

## Reproducing the harness

The spike harness is still in this branch, uncommitted-nowhere-else,
opt-in only:

```
LOOMUX_SPIKE_IFRAME_TEST=1 npm run tauri dev
```

Wait for the second window, titled **"SPIKE: iframe isolation proof
(#360)"**, to open alongside the normal main window. It renders an
in-DOM iframe pointed at the bundled `resource-monitor` example plugin
(served over the real `plugin://`/`http://plugin.localhost/` address
space, no fakes) with two init scripts applied: one testing M1 (a
main-frame-only marker), one testing M2 (a document-start scrub +
live `invoke()` attempt from inside the plugin frame). The result posts
back over `postMessage` to the host page (renders a PASS/FAIL banner) and
is also breadcrumbed to disk so it can be read without looking at the
GUI:

```
%APPDATA%\loomux\logs\breadcrumbs.log   → look for `spike-iframe-report`
```

**Known hazard, read before running:** dev builds share the app identifier
(`dev.loomux.app`) with an already-installed production instance, and both
attach to the *same* WebView2 user-data folder / browser process. Launching
this harness while a production loomux instance is running, and then
hard-killing the dev process tree, can crash the shared browser process and
take production down with it (observed once during this spike). Let the dev
instance exit on its own (close its windows normally) rather than killing
it, or run it when no production instance is up.

## Files this spike added (all spike-only, clearly marked in-code, not
wired into any real codepath)

- `src-tauri/src/spike_iframe.rs` — the two probe commands
  (`spike_probe_marker`, `spike_report_probe`) and the two init scripts
  (M1 marker, M2 scrub-and-probe), plus `maybe_open()` which opens the
  harness window only when `LOOMUX_SPIKE_IFRAME_TEST` is set.
- `src-tauri/src/lib.rs` — `mod spike_iframe;`, a `maybe_open(app)?` call in
  `.setup()`, and the two commands added to `generate_handler!`.
- `src-tauri/src/command_manifest.rs` — the two commands' bare names added
  to `APP_COMMANDS` so `build.rs` autogenerates their ACL permission files.
- `src-tauri/capabilities/spike-iframe.json` — grants the two spike
  commands to the harness window's own webview (`webviews:
  ["spike-iframe-test"]`) only.
- `spike-iframe-test.html` (repo root) — the harness host page: renders the
  iframe, listens for its `postMessage` probe report, does its own control
  `invoke()` to confirm main-frame IPC still works, and relays the combined
  result to the backend for breadcrumbing.

# #360 Phase-0 sandbox spike — harness

Throwaway dev harness. Does not ship; not referenced by any build target.

## What this proves or refutes

The #360 plan (pane plugins) proposes isolating third-party plugin code in a
`sandbox="allow-scripts"` iframe with **no** `allow-same-origin`, relying on the
browser's opaque-origin same-origin-policy boundary to keep plugin code away from
`__TAURI_INTERNALS__` / `invoke` / the 117 unguarded `#[tauri::command]`s. This
harness tests that assumption directly, from inside the real dev webview.

## How to run it

1. `npm run tauri dev` (opens the real GUI window — this is the one case the task
   brief pre-authorizes running it, since the question is genuinely about runtime
   WebView2 behavior that source-reading alone can theorize but not fully confirm).
2. Open devtools on the main window (right-click → Inspect, or F12).
3. Paste the entire contents of `harness.js` into the console, press enter.
4. After ~3s it prints `console.table(results)`. `LEAK-*` outcomes mean that route
   escaped the sandbox; anything else means it held for that route.

## Routes attempted

Direct global access (own frame + `window.top`), prototype-chain pollution,
`window.parent`/`window.opener` DOM reach, a real `invoke()` call through
`__TAURI_INTERNALS__` if reachable (using the harmless, read-only
`pty_backend_info` command), a raw `fetch` straight at Tauri's own
`ipc.localhost` custom-protocol endpoint (bypassing the JS bridge entirely),
dynamic `import()` of `@tauri-apps/api`, `BroadcastChannel` rendezvous, and a
forged isolation-pattern `postMessage` to the parent.

See the findings comment on
https://github.com/willem445/loomux/issues/360 for the verdict, the static
source analysis (tauri 2.11.5 / wry 0.55.1) that predicted the result before
this harness ran, and the recommendation for the trust core.

## Result (recorded run, 2026-07-16)

**LEAKS.** Run against `npm run tauri dev` (WebView2, Windows 10 baseline) via
Chrome DevTools Protocol (`run-via-cdp.mjs` drives `harness.js` headlessly —
no manual devtools paste needed once `--remote-debugging-port` is enabled;
see that script for the exact CDP wiring, including the
`WEBVIEW2_USER_DATA_FOLDER` override needed so a second, already-running
instance sharing the app's default profile doesn't swallow the flag).

- `own-window.__TAURI_INTERNALS__` → **LEAK-PRESENT**. The sandboxed iframe's
  own global gets the full internals object (invoke fn, invoke key, IPC
  pattern) despite `sandbox="allow-scripts"` with no `allow-same-origin`.
- `own-window.ipc` / `own-window.chrome.webview` → **LEAK-PRESENT**. Both the
  wry-level and native WebView2 postMessage bridges are present too.
- `window.top.__TAURI_INTERNALS__` / `window.parent.document` → blocked by
  SOP, as expected (opaque origin correctly stops cross-frame *reflection*;
  it just doesn't stop the frame getting its own copy of everything).
- `invoke("pty_backend_info")` (real command, called through the leaked
  internals) → reached Tauri's Rust-side IPC handler and was rejected with
  `Origin header is not a valid URL` — `tauri-2.11.5/src/ipc/protocol.rs:496`
  does an unconditional `Url::parse` on the `Origin` header; the browser
  sends the literal string `"null"` for an opaque-origin request, which
  isn't a parseable URL. This is an accidental parse failure, not a
  deliberate origin allowlist — it has no security intent behind it.
- Raw `fetch("http://ipc.localhost/pty_backend_info")` (bypassing the JS
  bridge entirely) → reached the exact same handler, rejected only for a
  missing invoke-key header the harness didn't supply. Confirms the
  custom-protocol interception applies to iframe-originated requests
  regardless of origin, exactly as wry's own comment describes
  (`AddWebResourceRequestedFilterWithRequestSourceKinds(..., ALL)`,
  "to allow Shared Workers and iframes to work with custom protocols").
- Baseline: the same `invoke("pty_backend_info")` from the trusted main
  frame resolves cleanly with real backend data — confirming the pipeline
  itself works and the sandboxed frame is reaching the *same* live command
  dispatch, not a dead end.

Net: the plan's Option A isolation boundary does not hold on this repo's
Windows 10 / WebView2 baseline. See the issue comment for the full verdict
and recommendation.

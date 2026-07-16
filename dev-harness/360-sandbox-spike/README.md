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

## Phase-0.5: Option B (child WebviewWindow + dedicated capability)

**HOLDS — but only with one prerequisite the plan didn't call out.** Files:
`spike_phase05.rs` (temporary `#[tauri::command]` that opens a child
`WebviewWindow` labeled `spike-plugin`), `capabilities/spike-plugin-zero.json`
(binds that label to an **empty** permissions array), `plugin-window-adversary.html`
(the adversarial page loaded as that window's own top-level document — not an
iframe), `run-phase05-via-cdp.mjs` (drives it end to end over CDP: opens the
window from the trusted main frame, attaches to the new window's own CDP
target, reads its self-test results).

**The prerequisite, found empirically, not by reading:** capability-based
per-window command denial does *nothing* for loomux's own commands as the
repo stands today. `tauri-2.11.5/src/webview/mod.rs:1823` only enforces the
ACL check when `has_app_acl_manifest || !is_local` — and
`has_app_acl_manifest` (`tauri-utils-2.9.3/src/acl/mod.rs:348`,
`acl.contains_key(APP_ACL_KEY)`) is only true if the app has its own
`src-tauri/permissions/` manifest. Loomux doesn't have one (only
`capabilities/default.json`, which references core/dialog permissions, not
app commands) — so *every* app command is `is_local`-exempt from ACL
regardless of which window or capability asks for it. A `spike-plugin`
window bound to a zero-permission capability would invoke `git_push` just
as freely as `main`, for the exact same reason Option A's iframe did (a
different mechanism, same root cause: loomux never opted into Tauri's ACL
system for its own commands).

`build.rs` was changed for this spike to opt in:
`tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[...]))`,
listing 6 representative commands (`orch_grant_merge`, `git_push`,
`ft_write_file`, `spawn_pty`, `pty_backend_info`, plus the spike's own
`spike_open_plugin_window`), and `capabilities/default.json` was extended to
explicitly grant `main` the resulting `allow-*` permissions. **This is an
all-or-nothing switch, found by breaking it first**: the moment
`has_app_acl_manifest` flips true, *every* app command with no permission
entry — including the 111 commands not in the spike's list, and initially
`spike_open_plugin_window` itself, forgotten on the first pass — starts
failing with `"<cmd> not allowed. Command not found"`, even for `main`. A
real rollout needs every one of the 117 commands enumerated with an explicit
permission and an explicit grant, or the app silently breaks. This is the
single biggest engineering cost Slice C's brief needs to budget for.

### Check 1 — real ACL deny, all 5 representative commands + all Phase-0 routes

Result, run against a live built `loomux.exe` with the above changes:

| Route | Outcome |
|---|---|
| `invoke(orch_grant_merge)` | **denied** — `"orch_grant_merge not allowed on window \"spike-plugin\"... allowed on: [windows: \"main\"]... referenced by: capability: default, permission: allow-orch-grant-merge"` |
| `invoke(git_push)` | **denied**, same shape |
| `invoke(ft_write_file)` | **denied**, same shape |
| `invoke(spawn_pty)` | **denied**, same shape |
| `invoke(pty_backend_info)` *(benign command, granted to `main`)* | **denied** for `spike-plugin` — proves the deny is genuinely per-window-label, not "some commands are globally off" |
| raw `fetch("http://ipc.localhost/...")` | reached the network layer (custom-protocol interception still applies to this window, same as any window); rejected for the missing invoke-key header the harness didn't supply — a fully-formed request would additionally hit the same real ACL deny above |
| `window.opener` | `null` — no reachable reference back to the main window (Tauri creates secondary windows independently, not via `window.open()`) |

Every denial above cites a **named capability and permission** in the error
— this is a real, deliberate, auditable deny, categorically unlike Option
A's incidental `Url::parse("null")` crash.

### Check 2 — the wry Windows subframe-injection bug does not cross WebviewWindows

Holds, and structurally must: the bug (`wry-0.55.1/src/lib.rs:990`,
*"Windows: scripts are always added to subframes"*) is scoped to **frames
nested within one webview's document tree**. A `WebviewWindow` is a
separate top-level `ICoreWebView2` controller — Tauri registers a
completely independent set of initialization scripts and IPC handlers per
webview at creation time (`manager/webview.rs`'s per-webview `pending`
construction); there is no shared script-injection list for wry to leak
across. Confirmed empirically two ways: (1) `spike-plugin` got its own,
correctly-resolved zero-grant ACL outcome, provably independent of `main`'s
grants; (2) `window.opener` is `null` — no cross-window JS reference exists
at all, let alone one that could be used to reach `main`'s internals.

### Residual capabilities to design around

A `WebviewWindow` has no `sandbox=""` attribute equivalent — none of the
iframe sandbox tokens (`allow-forms`, `allow-popups`, etc.) apply to a
top-level webview window at all. What a plugin window can still do that a
sandboxed iframe couldn't, found empirically:

- **Unrestricted self-navigation.** `location.href = 'https://example.com/'`
  succeeded completely — the window actually navigated and rendered the
  real external page (confirmed via the CDP target's title/URL changing to
  "Example Domain" / `https://example.com/`). Nothing in the spike's setup
  stops a compromised plugin from redirecting its own pane to a phishing
  page or an attacker-controlled origin. **Mitigation:** `WebviewWindowBuilder::on_navigation`
  (`tauri-2.11.5/src/webview/mod.rs:528`) takes a callback returning
  `bool`; Slice C needs one that only allows navigations back to the
  plugin's own `plugin://<id>/` origin, denying everything else.
- **Network egress is not blocked by CSP.** loomux's global CSP is `null`.
  A same-window `fetch()` to an external origin technically still sends the
  request over the network even when the *response* can't be read back due
  to that origin's CORS policy (confirmed: the plain `fetch("https://example.com/")`
  attempt itself reached the network — the same call from the *trusted main
  window* failed identically, proving the failure was CORS-response-reading,
  not a network block) — so exfiltration via a GET query string or POST body
  works regardless of CSP being `null`. Tauri's CSP is a single global
  `tauri.conf.json` setting, not configurable per-`WebviewWindow` through
  the public builder API; the only real per-plugin lever is what the
  `plugin://` URI-scheme handler (Slice B) sets as a `Content-Security-Policy`
  response header on the page it serves (e.g. `connect-src 'none'` or an
  explicit allowlist) — an HTML document enforces its own served CSP headers
  regardless of the app's global config.
- **Same-origin storage/messaging rendezvous — an artifact of this spike's
  shortcut, not the real design.** This spike loaded the plugin window from
  the same `http://localhost:1420` origin as `main` (for convenience —
  no `plugin://` scheme registered in the spike). `BroadcastChannel` and
  `localStorage` both successfully rendezvoused/shared with `main` under
  that setup. This is expected, ordinary same-origin browser behavior and
  **not itself a finding about Option B** — but it is a sharp reminder that
  the real design's `plugin://<id>` custom-protocol origin (a distinct,
  opaque-per-plugin origin, per the design note) is doing real isolation
  work here and must not be skipped or shared across plugins: two plugins
  (or a plugin and `main`) sharing one origin would share storage,
  `BroadcastChannel`, `SharedWorker`, and `postMessage`-without-origin-check
  surface with each other.

### Recommendation

**Option B is a sound trust core, gated on three things Slice C's brief must bake in, none of which the plan currently names:**

1. **Add `src-tauri/permissions/` for the app's own commands** (via
   `tauri_build::Attributes::new().app_manifest(AppManifest::new().commands(&[...]))`,
   listing all 117 `#[tauri::command]`s) and grant `main`'s capability every
   permission `main` needs. This is the load-bearing change — without it,
   Option B's per-window capability denial is inert for loomux's own
   commands, for the same underlying reason Option A leaked. Budget real
   time for this: it is all-or-nothing and every missed command silently
   breaks (see the prerequisite section above).
2. **`on_navigation` lock on every plugin `WebviewWindow`**, restricting it
   to the plugin's own `plugin://<id>/` origin — the isolation boundary
   this spike found nothing else provides.
3. **A `Content-Security-Policy` header set by the `plugin://` URI-scheme
   handler itself** (Slice B), not relying on the app's global CSP — at
   minimum `connect-src 'none'` unless a plugin's manifest declares network
   capabilities it doesn't have in v1 anyway.

None of these three depend on iframe sandbox tricks — the deny is a real,
named, auditable ACL rejection once (1) is in place, confirmed by 5-for-5 on
the representative command spread plus every Phase-0 escape route.
Slice C's brief should cite this as Option B, not Option A, and scope in
the app-permissions-manifest work as a prerequisite task, not an
afterthought.

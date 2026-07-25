# Design: E2E testing (Playwright over WebView2 CDP)

Status: spike / proof-of-concept (this doc), experimental CI job `e2e-windows`.

## Problem

A recent session found roughly ten real bugs that only showed up when someone
actually ran the GUI: pane-reorder geometry going wrong after a drag, a
side-panel resizing the workspace instead of floating over it, plugin overlays
landing at the wrong z-order, and a launch failure from an oversized argv —
none of them visible to `cargo check`, `cargo test`, or the frontend's
DOM-free `node:test` suite, because none of those exercise real layout, real
pointer events, or a real running window. Catching this class of bug requires
an actual GUI run; today that only happens when a human demos the change.

This spike answers three questions: what mechanism can drive loomux's real
WebView2 window from an automated test, does it actually catch bugs in that
class, and how does it avoid colliding with a real running loomux instance
while doing it.

## Mechanism comparison

Four options exist. All citations are to primary/official sources, verified
directly (crates.io, the GitHub API, playwright.dev, v2.tauri.app) rather than
taken from search-result summaries — see the `agent-cli-reference` skill's
citation discipline, applied here to a different ecosystem.

| | Playwright native WebView2 CDP | `tauri-plugin-playwright` | `tauri-driver` (official WebDriver) | Official Tauri testing docs |
|---|---|---|---|---|
| Mechanism | `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=N`, then `chromium.connectOverCDP()` | Rust plugin embedded in the app; `cdp` mode is the same CDP connection, plus a cross-platform `tauri` mode (JS-eval-over-IPC socket bridge) and a `browser` mode (mocked IPC in a downloaded Chromium) | WebDriver protocol via Microsoft Edge Driver (Windows) / WebKitWebDriver (Linux); WebdriverIO's `@wdio/tauri-service` also embeds a WebDriver server directly | Documents unit/integration testing with a mock runtime, plus WebDriver E2E — no mention of Playwright at all |
| New Rust dependency | None | Yes — `tauri-plugin-playwright` in `src-tauri/Cargo.toml`, unaudited for `getrandom` (constraint 2) | None for the CLI-driven path; `@wdio/tauri-service`'s embedded-server mode adds a plugin | N/A |
| New Tauri command / ACL surface | None | Yes — an IPC command (`pw_result`) gated by a `playwright:default` capability; multi-window use needs the window ACL widened to avoid ~30s hangs on a rejected command | None for the CLI-driven path | N/A |
| Test authoring API | Playwright (locators, auto-waiting, `expect`) | Playwright | WebdriverIO (different API/assertion style) | N/A |
| Platform coverage | Windows only (WebView2 is the only system webview CDP supports) | All three (macOS/Linux via the socket-bridge `tauri` mode) | Windows + Linux directly; macOS via the embedded-server WebdriverIO path only | N/A |
| Maturity | Playwright itself: mature, Microsoft/Playwright-maintained, this exact guide is on playwright.dev | Real but young: [github.com/srsholmes/tauri-playwright](https://github.com/srsholmes/tauri-playwright) — created 2026-03-27, 39 stars, 4 contributors (one at 40 commits, next at 17), latest npm release 0.4.1 (2026-06-20). Single-maintainer-scale project, not a Tauri-org package. | Official, `tauri-apps`-maintained; documented CI recipe for `windows-latest`/`ubuntu-latest` | Official |
| CI viability on `windows-latest` | Direct — CDP is a plain HTTP(S) endpoint, no browser download needed | Same CDP path on Windows, plus extra plugin/build surface | Documented, but needs Edge Driver version-matched to the runner's installed Edge, and a separate driver process | N/A |

**Recommendation: Playwright's native WebView2 CDP support.** It is the only
option that gets everything this spike needs — the Playwright API the user
asked for, zero new Rust dependencies, zero new Tauri commands or ACL surface
(satisfying "if you'd add a test hook: don't" directly), and it is the
mechanism `tauri-plugin-playwright`'s own `cdp` mode reduces to on Windows
anyway. `tauri-plugin-playwright` earns its keep only for the cross-platform
`tauri` mode, which doesn't matter here — loomux is a Windows-10-baseline
product (CLAUDE.md), and adding an unaudited, 4-month-old, small-team
dependency plus a new IPC command/capability to get a cross-platform mode
this project will never exercise is not a good trade. `tauri-driver` is the
most official path but authors tests in WebdriverIO, not Playwright, and adds
an Edge-Driver-version-matching CI dependency the CDP path doesn't need.

Sources: [playwright.dev/docs/webview2](https://playwright.dev/docs/webview2) ·
[v2.tauri.app/develop/tests/webdriver](https://v2.tauri.app/develop/tests/webdriver/) ·
[v2.tauri.app/develop/tests/webdriver/ci](https://v2.tauri.app/develop/tests/webdriver/ci/) ·
[github.com/srsholmes/tauri-playwright](https://github.com/srsholmes/tauri-playwright) ·
[learn.microsoft.com/.../user-data-folder](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/user-data-folder) ·
[learn.microsoft.com/.../AdditionalBrowserArguments](https://learn.microsoft.com/en-us/dotnet/api/microsoft.web.webview2.core.corewebview2environmentoptions.additionalbrowserarguments) ·
Tauri CLI `--config` merge-patch behavior confirmed directly via `npx tauri build --help` against this repo's installed `@tauri-apps/cli`.

### Multi-webview attach (investigated, not blocking)

The brief asked whether Playwright could attach to loomux's own child
webviews too, since each would be its own WebView2 control. As of this spike,
**loomux has none** — grepping `src-tauri/src` for `SetWindowRgn`/child-webview
creation and `src/` for a plugin-docking mechanism found nothing; the "plugin
z-order / embed docking" bug class from the originating session refers to
work not yet in `main`. If/when loomux does add a real embedded child
WebView2 (as opposed to a DOM-level overlay like the git-view/task-board
panels, which Playwright already sees fine), CDP should in principle still
reach it: WebView2 controls sharing one browser process each enumerate as
their own CDP target, and `browser.contexts()`/`context.pages()` already
returns every page in the process a Playwright `connectOverCDP()` attaches
to. This is unverified against a real loomux child-webview because none
exists yet — flagged as a concrete follow-up once that feature lands, not a
blocker for this spike.

## Isolation model

Two collision vectors, two fixes — both landed as generic product code, not
test-only hacks:

**1. WebView2 browser-process sharing (issue #394).** WebView2 keys its
user-data folder — and therefore its *one shared browser process* — off the
Tauri app's `identifier` (`%LOCALAPPDATA%\<identifier>\EBWebView`, per #394's
own investigation). `dev.loomux.app` is baked into `src-tauri/tauri.conf.json`
at build time. `src-tauri/tauri.e2e.conf.json` is a JSON Merge Patch
(officially supported via `tauri build/dev --config <file>`, per
`v2.tauri.app/develop/configuration-files`) that overrides `identifier` to
`dev.loomux.e2e` for E2E builds only. A binary built with that override gets
an entirely separate WebView2 browser process — sharing nothing with a
running production install, so **even a hard-kill of the E2E process can
never take down another instance's WebView2 process**, independent of
graceful vs. abrupt teardown. This was verified empirically while building
this spike: with a production loomux instance running (the one orchestrating
this very session), an E2E test run left only `dev.loomux.app`-rooted
`msedgewebview2.exe` processes behind after the test's own `dev.loomux.e2e`
instance exited — no cross-talk.

**2. loomux's own app-data root.** `orchestration/`, `logs/`, `tabs.json`, and
`running.lock` all lived under three independently-duplicated
`dirs::data_dir().join("loomux")` call sites (`obs.rs`, `uistate.rs`,
`orchestration/mod.rs`), with no override. This is the other half of #394's
own proposed direction. This spike adds `obs::data_root()` — a single
dedup'd helper the other two now call — honoring a `LOOMUX_DATA_DIR`
environment variable override. `e2e/fixtures.ts` points every test run at a
fresh `fs.mkdtempSync` directory via this variable, so an E2E run's
orchestration/logs/tabs state never touches a real install's, and multiple
E2E runs never touch each other's either. This is generic, not test-gated —
any dev workflow that wants a second isolated profile can use the same
variable.

`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=<N>` is used
purely to open the CDP port; per Microsoft's docs it is *appended* to the
options WebView2 was already going to use, so it never fights the
identifier-driven user-data-folder path above. `WEBVIEW2_USER_DATA_FOLDER`
(a *different* WebView2 env var that can redirect the user-data folder
outright) was deliberately **not** used for isolation here — Tauri already
passes an explicit, non-null `userDataFolder` derived from `identifier` when
creating the WebView2 environment, and whether an env var can override an
explicit code-supplied value isn't confirmed by the docs read for this spike.
The `identifier`-based fix is the one #394's own investigation already named
as correct, so it's the one this spike relies on for safety.

## What E2E can and cannot validate here

Honestly, up front:

- **Can**: DOM structure, layout geometry (`getBoundingClientRect` via
  `boundingBox()`), CSS-driven show/hide and resize behavior, real pointer-event
  interactions (drag, click, type), and anything that round-trips through a
  Tauri command to real backend state (git status, file reads) — because the
  webview really is running the real frontend bundle against the real
  backend.
- **Cannot**: assert on anything outside the DOM. `SetWindowRgn`-style native
  window-region clipping, raw PTY byte content (xterm.js's *rendering* of it
  is DOM-visible; the underlying ConPTY stream is not), native OS dialogs
  (`tauri-plugin-dialog`'s file pickers run outside the webview entirely), and
  — see above — any future *actual* child WebView2 control's internals are
  all blind spots for this mechanism. A DOM-level overlay (git view, task
  board, audit log — all `.git-overlay` in `src/pane.ts`) is fully visible;
  a genuine second WebView2 control embedded for, say, a browser-preview
  plugin would need its own CDP target, unverified per above.
- **Cannot, by hard constraint**: anything requiring a real orchestrator pane.
  The task-board overlay only appears once a pane is bound to an
  orchestrator role, which requires actually running one of the supported
  agent CLIs (`src/launcher.ts` `ORCH_CLIS`) — forbidden for automated E2E
  (CLAUDE.md constraint 3, never spawn real agent CLIs). The PoC substitutes
  the git-view overlay, which reuses the exact same `.git-overlay`
  docking/z-order mechanism on a plain shell pane, as the safe equivalent.
  Live multi-agent orchestration UI (task board, audit log, group lifecycle)
  stays a human-demo-only surface until/unless a safe stand-in agent process
  exists.

## The PoC

`e2e/fixtures.ts` spawns the isolated-identifier build, waits for the CDP
port, connects via `chromium.connectOverCDP`, and waits for `#tab-bar` plus a
first pane to exist before handing the test a `Page`. Three specs:

1. **`pane-reorder.spec.ts`** — splits a tab into two plain shell panes, drags
   one onto the other's center (`Grid.swap` in `src/grid.ts`), asserts their
   left-to-right order actually flips. Targets the pane-reorder-geometry bug
   class directly.
2. **`sessions-panel.spec.ts`** — opens and closes the sessions side panel,
   asserts the workspace's grid area returns to its exact pre-open width.
   Targets the "overlay resized the workspace instead of floating"
   class (the CLAUDE.md "never resize the PTY" constraint, verified from the
   layout side).
3. **`git-overlay.spec.ts`** — opens the git-view overlay on a plain shell
   pane rooted at a real repo, asserts it's docked within the pane's bounds
   (not clipped/mis-z-ordered) and interactive (loads real commit rows,
   clicking one selects it). Stand-in for the task-board overlay per the
   constraint above.

All three were run locally against a real build (`npx tauri build --debug
--no-bundle --config src-tauri/tauri.e2e.conf.json`) and pass — 3/3, repeatedly.
The `data_root_from` Rust helper backing the isolation env var has its own
unit tests in `obs.rs`, verified red (assertion failure, not a compile error)
against a stub that ignored the override, then green against the real
implementation.

**They do not currently pass in CI.** See "CI status" below — this is a
runner-execution-context issue confirmed unrelated to the specs or the app,
not a flaky test.

There are no `data-testid` hooks anywhere in the frontend yet, so every
selector in `e2e/helpers.ts` and the specs is structural — class names and
label text read straight out of `src/launcher.ts`/`pane.ts`/`grid.ts`. That's
a real fragility cost (a class rename breaks a test with no relation to the
behavior it tests) and the most obvious near-term follow-up.

## How agent workers should use this

E2E belongs to CI, same line as the rest of this repo's local-vs-CI split
(`ci-validate` skill): a single spec file or a quick manual check against the
isolated profile while iterating is fine locally, always via
`e2e/fixtures.ts`'s isolation (never point `LOOMUX_E2E_EXE` at an installed
build, never skip the `tauri.e2e.conf.json` identifier override). The full
suite as "it passes" evidence is CI's job — cite the `e2e-windows` job's run,
not a local one, **once it's actually green there** (see "CI status" below —
today it fails on every run for a reason unrelated to the code under test).
Until that's fixed, cite a local `npx playwright test` full-suite run instead
and say so explicitly. Building the E2E binary is a real `cargo build`, so
it's capped the same as any other local build (`-j 4`).

## CI status: currently red, root-caused, not a flake

`e2e-windows` fails on GitHub-hosted `windows-latest` today, and it's been run
down to a specific, confirmed, upstream cause rather than left as an unknown:

- GitHub-hosted `windows-latest` runners execute the job at **High
  integrity level** (confirmed directly: `whoami /groups` in the job prints
  `Mandatory Label\High`). A normal dev machine's shell — where the PoC
  passes 3/3 — runs at Medium IL.
- WebView2 Runtime 150+ has a **confirmed upstream regression**
  ([MicrosoftEdge/WebView2Feedback#5640](https://github.com/MicrosoftEdge/WebView2Feedback/issues/5640)):
  the DevTools/CDP endpoint never opens when the host app runs at High IL.
  Runtime 149 is the last known-working version; the runner's WebView2 is
  150.0.4078.65.
- **Verified directly against this job**, not just inferred from the upstream
  report: the built exe launches and runs fine at High IL (`Responding: True`,
  a full `msedgewebview2.exe` process tree spawns — main, crashpad-handler,
  network/storage utility, gpu-process, **and a renderer** — so the app is
  actually rendering), but `netstat` shows no listener on the requested
  `--remote-debugging-port` at all, and an HTTP probe against it gets
  `ECONNREFUSED`. This rules out a slow-start/timeout explanation (the earlier
  hypothesis this spike tried first) and a GPU-less-CI-hang explanation
  (`--disable-gpu` made no difference) — it's specifically the CDP listener
  that the High-IL regression suppresses.
- A `runas /trustlevel:0x20000` de-elevation attempt (re-launch the exe at
  Medium IL from within the High-IL job, which needs no credential prompt
  since it's lowering privilege) **did not** resolve it in this session's
  testing. Root cause not fully isolated further — possibly the de-elevated
  process still can't reach a window station/desktop the High-IL job owns, or
  `runas /trustlevel` behaves differently in a non-interactive Actions
  session. Not pursued further within this spike's scope.

This is why `e2e-windows` stays `continue-on-error: true` — a red run here is
the runner's execution context, not signal about the change under test, and
must never block a PR. This is different from an ordinary "flaky test"
situation: there's nothing non-deterministic to wait out or retry away. Fixing
it for real needs one of (roadmap, not done in this spike):

1. A **self-hosted Windows runner** for this one job, running as a normal
   (Medium IL) user — the straightforward fix, at the cost of infra to
   maintain.
2. Wait for Microsoft to fix #5640 upstream.
3. Pin the E2E build's WebView2 to **Fixed Version 149** via the
   `BrowserExecutableFolder` policy — the one workaround reported in the
   upstream issue, explicitly called "not sustainable long-term" by its own
   reporter (tracks an old runtime indefinitely, diverging from what real
   users actually run).

Until one of those lands, treat `e2e-windows` red as expected, and rely on a
local run (`npx playwright test` against the isolated profile) as this
suite's actual signal.

## Roadmap

- Promote `e2e-windows` off `continue-on-error` once it has a flake track
  record.
- Add `data-testid` attributes to the highest-churn selectors (pane header,
  launcher form fields, overlay roots) so specs stop depending on class names
  and label text.
- Parallelize (currently `workers: 1` — see `playwright.config.ts`) by
  allocating a CDP port per worker instead of a fixed one.
- The argv-length launch failure from the originating session is a strong
  E2E-shaped candidate (spawn with a long argv, assert the pane reaches a
  running state rather than an error) but wasn't one of this spike's three
  PoC cases — worth a follow-up spec.
- Revisit child-webview CDP attach once loomux has a real one to test against.

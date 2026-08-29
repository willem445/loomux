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
identifier-driven user-data-folder path above. (This env var is also the one
WebView2 Runtime 150+ drops at High integrity level — see "CI status" below —
but that's a CDP-availability problem, not an isolation problem.)

`WEBVIEW2_USER_DATA_FOLDER` (a *different* WebView2 env var that can redirect
the user-data folder outright) is deliberately **not** used for isolation
here, now for a settled reason rather than an open question: it's confirmed
to override an explicit code-supplied value — and, per the same closed
WebView2Feedback#5640 thread, to keep working even at High IL, unlike
`AdditionalBrowserArguments` ("Explicit userDataFolder via
`WEBVIEW2_USER_DATA_FOLDER=C:\WV2Debug\UDF` … Folder was created and
populated by WebView2 in both [elevated and non-elevated] cases"). It's not
used here because using it would erase the one thing `e2e/fixtures.ts`
verifies before touching anything: the identifier-derived UDF path *is* the
observable signal that a launched process is really the E2E build and not a
stale production one (see "Structural isolation verification" below).
Redirecting it away with `WEBVIEW2_USER_DATA_FOLDER` would trade a real
safety check for a different (currently unneeded) isolation property.

### Structural isolation verification

The two mechanisms above are real, but a build config is not self-enforcing:
nothing stops a stale or wrongly-configured build from sitting at the exact
path `e2e/fixtures.ts` launches by default. If that ever happened, the
harness would be driving — and, at teardown, hard-killing — whatever's
actually there, identifier and all, which is precisely the #394 hazard this
whole design exists to avoid.

So the isolation guarantee isn't asserted, it's checked: before
`e2e/fixtures.ts` does anything else with a spawned process — before
connecting Playwright, before any interaction, before any teardown decision —
`verifyIsolatedBuild` inspects the OS process tree (`Get-CimInstance
Win32_Process`, filtered to the exact spawned PID's own WebView2 child) and
confirms its `--user-data-dir` really is rooted at `dev.loomux.e2e`, not
`dev.loomux.app` or anything else. A mismatch — or no WebView2 child
appearing at all within the timeout — throws immediately, before the
`try`/`finally` that would otherwise call `proc.kill()` is even entered. Both
directions were exercised directly while building this: a correctly-built E2E
exe passes verification and the suite runs normally; a plain `tauri build
--debug` (production identifier) sitting at the same path is refused within
the process-tree check — in that test the second instance's WebView2
environment creation didn't even spawn a distinguishable child process
(consistent with #394: a shared UDF means it joined an existing browser
process rather than starting its own), and the harness never touched it —
the live production instance's own WebView2 process tree was confirmed
untouched immediately afterward.

A refusal deliberately leaves the process running and its temp data dir in
place rather than cleaning either up (killing it is exactly the hazard being
guarded against) — so the thrown error names the pid and the dir path
explicitly, rather than leaving a silent orphan. It also distinguishes *why*
no child appeared: a differently-identified (production) instance already
owning the shared browser process, vs. a previous E2E run's browser process
that hadn't finished exiting yet — the second case being a known, narrow
race between specs (WebView2's shared-process model means a not-yet-exited
previous instance can be joined instead of a new one spawning), which
teardown now reduces by waiting for both the host process and its WebView2
child to actually disappear (not just calling `kill()` and moving on) before
the next spec starts.

One more residual, left narrowed rather than fully closed: `verifyIsolatedBuild`
verifies the *app* is ours — the spawned pid's own WebView2 child is rooted
at the E2E identifier — it does not additionally cross-check that the CDP
endpoint the harness then connects to (a fixed `127.0.0.1:<port>`) belongs to
that exact verified process. A foreign listener on the same port would still
be attached to. This fails safely rather than silently — the `#tab-bar`
selector wait gates everything downstream, so a wrong endpoint reads as a
confusing timeout, not a wrong-app drive, and `browser.close()` on a
`connectOverCDP` connection disconnects rather than terminates (never closes
someone else's browser) — but a `/json/version` identity cross-check before
handing back the page would close it properly; not implemented here.

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

  **Partially reopened by event injection (#814).** A surface driven by a
  backend *event* rather than by a bound orchestrator role does not need an
  agent CLI at all: the spec emits that event from the page through
  `plugin:event|emit` — the exact call the bundled `emit()` makes (see
  `@tauri-apps/api`'s `event.js`/`core.js`), permitted by the shipped ACL
  (`core:default` covers `core:event`), so it needs no capability widening and
  no test-only build flag. It round-trips through the backend's real
  broadcaster into the app's real `listen()` handler, so **the whole frontend
  half is genuinely exercised** — handler, presentation module, DOM, CSS, and
  any mirrored chrome — on ordinary plain shell panes. What it does *not*
  exercise is the backend's decision to emit at all (what to send, and when),
  which stays the unit/integration suite's job. State that split in any spec
  that uses this technique, because the technique's whole risk is reading like
  end-to-end proof of a feature whose producing half was never run. It is a
  stand-in for a **human's DOM look**, not for the backend's own tests.

## The PoC

`e2e/fixtures.ts` spawns the isolated-identifier build, waits for the CDP
port, connects via `chromium.connectOverCDP`, and waits for `#tab-bar` plus a
first pane to exist before handing the test a `Page`. Five specs:

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
4. **`tab-reorder.spec.ts`** (added #402, live-demo round 5) — creates a
   third tab, drags the first onto the third, asserts the tab strip's
   rendered order actually changes. Added specifically because a human
   reported "grab works, drop refused everywhere" against `src/tabbar.ts`'s
   tab-strip drag in real use, a symptom the pure `moveTab`/`dropTargetIndex`
   suite (`test/tabs.test.ts`) and two prior rounds of code-level DOM-wiring
   fixes couldn't resolve. See "A native-HTML5-DnD lesson" immediately below
   — this spec is also why the tab-strip drag mechanism itself changed.

5. **`queue-badge.spec.ts`** (added #814) — pushes `orch-queue-depth` readings
   into plain shell panes by event injection (see the "partially reopened"
   note above) and asserts what a human's DOM look was going to: the header
   chip renders with its count/cap/age in the label rather than in a tooltip,
   a stalled queue paints *differently* (compared as computed style, so a rule
   that never matched fails here instead of passing a selector check), a
   minimized pane keeps the count on its dock chip, and — on two- and
   three-column layouts with two chips lit — the badge's own crowding cost stays
   within the box `flex-shrink` cannot touch and it never takes width from the
   pane's drag handle, while the pane's terminal box does not move a pixel
   (constraint 1, from the layout side, the same way `sessions-panel.spec.ts`
   checks it). It asserts the badge's CONTRIBUTION, not the header's absolute
   fit: that header is already over-subscribed by chrome the badge did not add
   (#894), so an absolute assertion would charge this feature for it. It is the
   first
   spec to stand in for a human demo rather than for a bug class, which is why
   its header states exactly which half of the feature it proves.

Specs 1-4 were run locally against a real build (`npx tauri build --debug
--no-bundle --config src-tauri/tauri.e2e.conf.json`) and pass, repeatedly.
The `data_root_from` Rust helper backing the isolation env var has its own
unit tests in `obs.rs`, verified red (assertion failure, not a compile error)
against a stub that ignored the override, then green against the real
implementation.

### A native-HTML5-DnD lesson (#402, tab-reorder)

`tab-reorder.spec.ts` exists because a human reported the tab strip's
drag-to-reorder (#379) as "grab works, drop refused everywhere" in real use —
after the pure index-arithmetic tests already passed and a code-level
DOM-wiring fix (missing `dragenter`/`dropEffect`) had already shipped. Writing
the spec surfaced something more useful than a repro: **neither manual
`DragEvent` dispatch (`element.dispatchEvent(new DragEvent(...))` with a real
`DataTransfer`) nor Playwright's CDP-driven `locator.dragTo()` could ever
reproduce a failure** against `src/tabbar.ts`'s original native
`draggable="true"` HTML5 drag-and-drop — both consistently succeeded, with
*and* without the dragenter/dropEffect fix, across a clean single drag, an
exact-center drop (a genuine tie in the app's own before/after math, not a
failure), and a slow, jittery multi-target drag meant to mimic a real hand.

The likely explanation: CDP's `Input.dispatchMouseEvent` (what
`locator.dragTo()` uses under the hood) injects events at the browser's own
input layer, and Chromium's internal drag-and-drop implementation reacts to
that layer directly — it does not depend on the same native-OS mouse-message
path a REAL hardware drag takes through a Tauri-hosted WebView2 window. If
whatever's broken lives in that native-OS-to-WebView2 initiation step (as
opposed to anything in `src/tabbar.ts`'s own event handlers), CDP-simulated
input structurally cannot see it — manual `DragEvent` dispatch bypasses the
same layer even more directly, going straight to the app's listeners without
any native drag session at all. **Both are excellent tools for testing an
app's own drag-event handling; neither can tell you whether a real user's
native HTML5 drag ever reaches that handling in a WebView2 desktop app.**

The fix sidesteps the question entirely rather than resolving it: it drops
native HTML5 DnD for the tab strip and reimplements the reorder with the same
POINTER-EVENT mechanism (`pointerdown`/`pointermove`/`pointerup`, a drag
threshold, manual hit-testing) that `src/grid.ts`'s pane-reorder drag already
uses — a mechanism this same E2E suite has verified reliable since the
original spike (`pane-reorder.spec.ts`) and that never depends on the browser
initiating anything; the app owns the entire gesture from the first pixel of
movement. `tab-reorder.spec.ts` now uses `locator.dragTo()` too, the same way
`pane-reorder.spec.ts` does, and reliably passes.

**Takeaway for future specs in this repo**: if a real drag-and-drop feature
uses native `draggable="true"`/`dragstart`/`dragover`/`drop`, do not trust a
passing Playwright spec (via either `dragTo()`/`dragAndDrop()` or manual
`DragEvent` dispatch) as proof it works in the real, packaged desktop app —
this harness cannot distinguish "the handlers are correct" from "the browser
ever calls them" for that specific mechanism. Prefer pointer-event-based drag
implementations for anything reorderable in this codebase; they're both more
reliable in practice (per this investigation) and the only kind of drag this
harness can meaningfully validate end to end.

**They do not currently pass in CI.** See "CI status" below — this is a
runner-execution-context issue confirmed unrelated to the specs or the app,
not a flaky test.

There are no `data-testid` hooks anywhere in the frontend yet, so every
selector in `e2e/helpers.ts` and the specs is structural — class names and
label text read straight out of `src/launcher.ts`/`pane.ts`/`grid.ts`. That's
a real fragility cost (a class rename breaks a test with no relation to the
behavior it tests) and the most obvious near-term follow-up.

## The soak lane (#1603, plan #1600 §3 Phase 4.1)

Every spec above is a **shape** test: open something, measure the DOM,
assert a structure. `soak-liveness.spec.ts` is not. It asserts a **liveness**
property — after the app has run for a while against a large corpus with its
poll paths ticking, does a keystroke still reach a pane and does the MCP
still answer? — because that is the property four consecutive hangs (#1564,
#1592, #1595, and the beta6 field report) broke while every shape guard in
the repo stayed green. Plan #1600 §2.2 is the argument; this lane is the
test it asks for.

Two specs, two app launches:

1. **The soak.** Boot against the synthetic corpus, open a plain `cmd` pane,
   idle for `ORRERIX_SOAK_MS`, then assert a keystroke round-trips through
   the pane's child and an MCP `ping` answers, both within
   `ORRERIX_SOAK_BOUND_MS`. Expected to PASS on today's `main` — beta6 fixed
   the poll-batch path — and to stay as regression protection.
2. **The class assertion.** Same corpus, plus a deliberately long registry
   lock hold injected while the app runs, then the same two probes. Expected
   to **FAIL** on today's `main`; plan #1600's Phases 1 and 2 are what make
   it pass. See "The expected failure" below.

### What loads the poll paths, and what cannot be loaded

Stated plainly, because a liveness lane that quietly covers less than it
reads as covering is worse than none:

- **The 4 s tab-strip poll is exercised.** `src/tabbar.ts` arms it at
  construction — its own comment calls it "the app's one app-lifetime
  poll" — and `pollStatus` issues `orch_group_summary` + `orch_group_usage`
  for every **group-bound** tab. A tab is group-bound purely because its
  persisted `groupIds` names a group, so the corpus's `tabs.json` alone puts
  that poll under load: no clicking, and no agent CLI.
- **The 2 s group-view poll is not, and cannot be.** `src/groupview.ts`'s
  timer is armed only while the view is shown, and the view opens only from
  a pane whose `groupBtn` is visible — which `applyOrchIdentity` reveals only
  for a LIVE orchestrator-role pane. That needs a real agent CLI, which
  CLAUDE.md constraint 3 forbids here. Both polls park threads on the same
  registry mutexes, so the mechanism is exercised either way; the per-tick
  fan-out is simply smaller than a real orchestrating session's. Closing that
  gap needs a safe stand-in agent process — the same standing limitation as
  the task-board overlay above — not a change to this lane.
- **Blocking-pool exhaustion is not reachable from the poll path at all, and
  no hold duration or corpus size changes that.** Plan #1600 §1.2 step 4
  describes ticks accumulating parked `spawn_blocking` threads until the
  512-thread pool exhausts. **#1604 single-flights this sweep**: a tick that
  fires while the previous one is still outstanding skips, so at most one
  call is parked however long a lock is held. That is the fix working as
  designed, and it means this lane cannot demonstrate the pane-input half of
  the chain.

  An earlier version of this section did an arithmetic — four bound tabs at
  two invokes every 4 s, so ~2 parked threads per second, so ~180 of 512 at a
  90 s hold — and offered a longer hold or a wider corpus as the way to reach
  saturation. That was true against the base this branch was cut from and is
  **false on the merged tree**; it is recorded here rather than deleted
  because it is exactly the kind of stale arithmetic a reader would otherwise
  reconstruct from the plan.

  What the class assertion still demonstrates is the half #1604 does not
  govern: `orchestration/mcp.rs` spawns a thread per request and each one
  parks on the registry mutex, ungoverned by any single-flight. That is why
  the MCP probe is the one that dies while pane input stays healthy, and why
  the lane's finding survives the fix that removed the other half.

### The corpus, and the store it is deliberately not written into

`e2e/corpus.ts` writes, into the fixture's own throwaway data dir and before
the app is spawned, the install shape every hang report was made against:
orchestration groups with rosters, task files and multi-megabyte
`audit.jsonl` files, plus a large CLI session store behind them.

The session store is the **copilot** one, and that is a constraint rather
than a preference. The Claude half (`~/.claude/projects`) has no production
redirect — `claude_projects_root()` is `dirs::home_dir()` plus a
thread-local seam only reachable from Rust — so seeding hundreds of
synthetic sessions there would mean writing into the operator's real
transcript directory. `copilot_session_state_root()` honours `COPILOT_HOME`,
so the whole store lives inside the same temp dir the fixture already
deletes on teardown. The groups carry `agent_cli: "copilot"` for the same
reason: it is what makes the boot listing's `resumable` check actually
enumerate the synthetic store instead of skipping it.

`e2e/fixtures.ts` grew two hooks for this and nothing else: `seedDataDir`
(which may return extra environment variables, since `COPILOT_HOME`'s value
is not known until the dir exists) and `extraEnv`. Both merge in ABOVE the
two isolation variables the harness owns, so a spec can add a knob and can
never take `ORRERIX_DATA_DIR` or the CDP port away.

### The two probes

**A keystroke reaches the pane's child.** xterm.js renders through the WebGL
addon, so a terminal's contents are not in the DOM and no selector can read
them. Rather than add a debug hook to product code to expose the buffer, the
spec listens to the same `pty-output` event the app's own router consumes,
through the low-level bridge the bundled `listen()` uses underneath — the
emit-direction twin of the #814 technique above, and covered by the shipped
ACL (`core:default` covers `core:event`). That payload is **base64 of the raw
pty bytes**, not text (`src/pty.ts`'s router `atob`s it before handing it to a
pane); a tap that skips the decode matches nothing and reports it as "no
output", which is precisely how a working round trip reads as a dead one.

It types `echo <marker>%RANDOM%` and matches `/<marker>\d+/`. That asymmetry
is the point: the typed text's `%RANDOM%` is followed by `%`, not a digit, so
the echo of the typed line cannot match — only `cmd.exe` expanding it can. A
pane that rendered the keystrokes locally but never reached, or never heard
back from, its child cannot produce a match. (`cmd`, not the launcher's
default PowerShell: PSReadLine redraws the input line as you type.)

The probe runs in **two separately-bounded phases**, because they are two
properties that fail for different reasons and can cost wildly different
amounts of time. `src/ptywrite.ts` keeps one `write_pty` in flight per pane and
chains the next on the previous promise (#65), so every typed character is a
full IPC round trip — and how long that takes has been observed to vary by
orders of magnitude between CI runs of this same spec: once only 19 of 23
characters echoed within 8 s, and on the next run the whole input phase
finished in 38 ms. **That cause is not established, and is deliberately not
guessed at** — guessing from a plausible reading is what §2.3 of plan #1600
says produced beta5 and beta6, and if the slow case recurs it is worth chasing
under that plan in its own right rather than being explained away here.

What follows from it is the split. The *input* phase ends when the typed
marker echoes back and is the one the beta6 report is about; the *answer*
phase is one write and one read after Enter, and is fast whenever it works at
all. Collapsing them into one clock produced a probe that timed out mid-typing
and called it "no output" — a slow input path indistinguishable from a dead
child. The marker is kept short for the same reason.

`ORRERIX_SOAK_BOUND_MS` bounds each phase, and 20 s is deliberately generous:
the failure this lane is about is a probe that NEVER answers, not a slow one.
A latency budget here would be a flake generator wearing an invariant's
clothes.

**The MCP answers.** Over real HTTP, from the Playwright process, not
through the webview — the webview is the half that stays alive in the beta6
mechanism, which is why the window kept painting, so asking it whether the
backend is well is asking the wrong process. `ping` is the cheapest method
and still faithful: every method is authenticated first, and `resolve_token`
locks `by_token`, then `agents`, then `groups`.

The identity comes from `orch_solo_prepare`, which mints a token, registers
the agent and writes its MCP config file **before** the launcher would spawn
anything and independently of whether that CLI is even installed — so
calling it alone yields a valid token and the server's ephemeral port (bound
at `127.0.0.1:0`, and not otherwise discoverable) with no child process
anywhere. Constraint 3 is about spawning agent CLIs; this spawns nothing.

Both probes are bounded by their own deadline rather than by Playwright's
test timeout, because the failure under test is a HANG: an unbounded `await`
would report every red as "Test timeout exceeded" with no statement of which
half died.

### The lock-hold injector, and why it is a file

`src-tauri/src/orchestration/e2ehold.rs` is the one piece of this repo that
deliberately makes the app misbehave. It watches for
`<data root>/e2e-lock-hold.request`, takes the named registry mutex, writes
`<data root>/e2e-lock-hold.state` with `acquired_ms`, sleeps out the hold,
and rewrites the state with `released_ms`.

This design note's own recommendation above is "zero new Tauri commands or
ACL surface", and #814's queue-badge spec turned a test hook down for the
same reason. A command would have been permanent product surface — a name in
`generate_handler!`, an entry in `command_manifest::APP_COMMANDS`, and an
ACL grant — all present in a *release* build even with the body cfg'd away.
A file under the app-data root costs none of that, and it is better for the
test besides: the Playwright process owns that directory, so it can trigger
a hold and read back when the lock was actually taken without going through
the very IPC path whose liveness is under test. A probe the app has to
answer to tell you the app is stuck is not a probe.

It cannot ship enabled, on two independent gates:

1. `#[cfg(debug_assertions)]`. The watcher is compiled only into a
   dev-profile build; the workspace `[profile.release]` does not set
   `debug-assertions`, so a release build keeps cargo's default (`false`) and
   contains an empty `start` and nothing else.
2. An explicit opt-in: even a dev build starts no thread unless
   `ORRERIX_E2E_LOCK_HOLD` is exactly `1`, which the soak spec passes through
   `extraEnv`. `npm run tauri dev` behaves as it always has.

`src-tauri/tests/e2ehold_guard.rs` is what makes those claims checkable
rather than readable: a shape scan asserting every function that can hold a
lock, sleep, spawn or write is gated (with a floor on what the scan found,
so an instrument that stopped matching cannot report "all clean"); a check
that `[profile.release]` has not turned `debug-assertions` back on; a
behavioural test that only the exact string `1` arms it; and a check that
`e2e/liveness.ts` still names the two filenames and the environment
variable, since there is no shared header between a Rust module and a
TypeScript spec and a one-sided rename would produce a soak run with no hold
behind it — green, and meaningless.

### Positive controls

Three, because every one of this lane's assertions has a way of passing
vacuously:

- **The corpus really landed.** The builder returns what it wrote and the
  spec asserts group, session and audit-byte counts against it — otherwise a
  builder that silently failed turns this into a soak against an empty
  install, which passes.
- **The polls really ran.** The app counts its own dispatches, and the spec
  asserts a floor on `orch_*` calls across the soak window. Without it the
  test asserts that an app survives being ignored. The floor is on the TOTAL,
  not on which commands — the group-view batch has been five, then nine, then
  ten, and pinning a count would pin the last incident's shape, which is the
  mistake this lane exists to stop making.

  The counter lives in `src/transport.ts`, not in the spec, and that is a
  correction rather than a preference. Counting from the spec's side meant
  patching `window.__TAURI_INTERNALS__.invoke` — which is a **frozen own
  property** (`writable:false, enumerable:false, configurable:false`,
  measured on the E2E build). Assignment to it is a silent no-op outside
  strict mode, `defineProperty` throws, and a `Proxy` cannot help either: a
  `get` trap is forbidden by the language from returning anything but the
  real value for a non-writable, non-configurable data property. Fighting
  that descriptor would also have been a bet on Tauri's internals keeping
  their present shape. `src/transport.ts` is the ONE module allowed to touch
  Tauri IPC (CLAUDE.md constraint 5), so a counter there sees every dispatch
  by construction — the seam the codebase already had.

  It **ships disarmed**: `__invokeStats("arm")` turns it on, and until then
  `invoke` pays a single null check. Armed, it increments an integer in a map
  keyed by command name, so what it retains is bounded by the command surface
  and not by session length. No IPC surface, no ACL grant, no new command —
  the same shape of devtools instrument `pollgate.ts` already exposes as
  `__pollGateStats()`.
- **And the counter itself is checked**, which is not belt-and-braces: a
  counter that is not counting reports `{}`, bit-for-bit what an app that
  never polled reports. `invokeStats()` therefore reports `armed` alongside
  the counts, and `readInvokeCounts` refuses a disarmed reading outright.
  The spec then calls `assertCounterSeesTheApp` immediately after the
  baseline round trip — which cannot have succeeded without `write_pty` — so
  a blind instrument fails there rather than two hundred seconds later
  wearing a finding's clothes. That is not hypothetical: it is what the third
  CI run on this branch did.
- **The hold really held** — and this one could NOT live inside the test it
  is about. `test.fail()` absorbs every failure in its own test, controls
  included: an injector that silently never took a lock would leave both
  probes measuring an idle app, the test would fail, and Playwright would
  report a healthy expected failure. So the proof is split across two places
  the marker cannot reach. `src-tauri/tests/e2ehold_guard.rs` proves the
  mechanism **differentially** against a real `OrchRegistry` — the same
  public registry read that finishes in milliseconds with nothing held does
  not finish at all during a 1.5 s hold, and then does once it elapses,
  because "it did not finish" is evidence only if the same probe could have
  finished. And a short, non-xfail spec in the same describe proves the
  injector is compiled in and armed **in the build under test**, which is the
  one part a Rust test cannot say. The `acquired_ms`/`released_ms` checks
  still inside the expected failure are diagnostics: they put the reason in
  the report, and are not counted as evidence.

The MCP probe also carries a negative control: a bogus token must be refused
with JSON-RPC `-32000`. Without it, "something answered on 127.0.0.1" would
read as "orrerix's authenticated MCP server answered".

### The expected failure

The class assertion is marked `test.fail()`. It is expected to fail on
today's `main` — that is the point of it — and `fail` was chosen over `skip`
deliberately: the assertion runs at full strength, the E2E job stays green
while the fix is outstanding, and the moment Phases 1/2 land Playwright
reports "expected to fail but passed", telling whoever landed the fix to
flip the marker. A `skip` would have gone quiet instead, and quiet is how a
lane stops being re-armed.

The cost of the marker is that it absorbs its own test's controls — see
**Positive controls** above for where each of them lives instead. The rule
this lane follows is that nothing inside an expected failure is ever cited
as evidence for anything.

**When plan #1600's Phases 1 and 2 land:** delete the `test.fail()` line in
`e2e/tests/soak-liveness.spec.ts` and this paragraph with it. Nothing else
about the spec changes.

**Do not expect CI to tell you.** Playwright's unexpected-pass report lands
inside the `e2e-windows` job, which is `continue-on-error: true` — so when
the fix arrives the report is a line in a log inside a job that still shows
green, and no required check moves. What actually carries the obligation is
#1603 staying open and this paragraph. Whoever lands Phases 1/2 should read
the E2E log rather than the check mark; if `e2e-windows` ever comes off
`continue-on-error`, this caveat goes with it.

### Budget

`ci.yml` sets `ORRERIX_SOAK_MS=180000`, `ORRERIX_SOAK_LOCK_HOLD_MS=90000`
and `ORRERIX_SOAK_BOUND_MS=20000` explicitly, so the cost lives where the
job does. 180 s is ~45 ticks of the 4 s status poll per group-bound tab, and
the hold has to outlast both probes, which need ~70 s worst case at those
bounds. Three launches (the soak, the armed-injector control and the class
assertion) plus warm-ups put the lane at roughly eight minutes of the
`e2e-windows` job, and up to ~14 if the soak spec takes its one retry. Every
knob is an environment variable (`ORRERIX_SOAK_MS`,
`ORRERIX_SOAK_LOCK_HOLD_MS`, `ORRERIX_SOAK_BOUND_MS`, `ORRERIX_SOAK_GROUPS`,
`ORRERIX_SOAK_SESSIONS`, `ORRERIX_SOAK_AUDIT_LINES`), so a long local soak
is a matter of setting one, not editing the spec.

### A follow-up this lane deliberately does not depend on

Phase 0 (#1601) is adding a liveness heartbeat and lock breadcrumbs in
parallel. Nothing here depends on them, on purpose — the lane had to be able
to land on today's `main`. Once they exist, the invoke-counter positive
control and the injector's own state file could both be replaced by reading
the app's real heartbeat, which would be a crisper statement of the same
property and would let the lane distinguish "the GUI thread is stuck" from
"the backend is stuck" without inferring it from which probe died.

## Running it (the two commands)

Build the isolated test binary once, then run the suite (this recipe used to
live in the README; #609 moved it here with the rest of the developer detail):

```sh
npx tauri build --debug --no-bundle --config src-tauri/tauri.e2e.conf.json
npm run test:e2e
```

## How agent workers should use this

E2E belongs to CI, same line as the rest of this repo's local-vs-CI split
(`ci-validate` skill): a single spec file or a quick manual check against the
isolated profile while iterating is fine locally, always via
`e2e/fixtures.ts`'s isolation (never point `LOOMUX_E2E_EXE` at an installed
build, never skip the `tauri.e2e.conf.json` identifier override). The full
suite as "it passes" evidence is CI's job — cite the `e2e-windows` job's run,
not a local one (see "CI status" below for the HKLM policy fix that makes
this hold in practice, not just in principle). Building the E2E binary is a
real `cargo build`, so it's capped the same as any other local build
(`-j 4`).

## CI status

`e2e-windows` has been run down to a specific, confirmed cause rather than
left as an unknown — and the cause is **intentional Microsoft hardening**,
not a bug Microsoft will fix:

- GitHub-hosted `windows-latest` runners execute the job at **High
  integrity level** (confirmed directly: `whoami /groups` in the job prints
  `Mandatory Label\High`). A normal dev machine's shell — where the PoC
  passes 3/3 — runs at Medium IL.
- WebView2 Runtime 150+ **intentionally drops** the `WEBVIEW2_*` env-var and
  HKCU-policy channels for an elevated (High-IL) host process, as a
  local-privilege-escalation hardening measure. This is
  [MicrosoftEdge/WebView2Feedback#5640](https://github.com/MicrosoftEdge/WebView2Feedback/issues/5640) —
  **closed as completed on 2026-07-09**, sixteen days before this doc was
  first written; an earlier version of this doc called it a "confirmed
  upstream regression" and listed "wait for Microsoft to fix it upstream" as
  a roadmap item, which was wrong on both counts and is corrected here. The
  Microsoft maintainer's own closing comment:

  > "In Runtime 150 we added a security hardening for scenarios involving an
  > elevated (High-integrity) host process. To close a local
  > privilege-escalation gap, WebView2 now ignores command-line switch
  > overrides and HKCU policy values when read from an elevated process. When
  > the host is elevated, only HKLM policy and arguments the app passes
  > directly via the API are honored — the user-writable channels
  > (`WEBVIEW2_*` environment variables and HKCU) are intentionally dropped.
  > … This is by design."
  >
  > "Fix — switch to a channel that survives elevation: In app code
  > (preferred): `CoreWebView2EnvironmentOptions.AdditionalBrowserArguments`
  > … Or via HKLM policy: `HKLM\Software\Policies\Microsoft\Edge\WebView2\AdditionalBrowserArguments`"

- **Verified directly against this job**, not just inferred from the upstream
  report: the built exe launches and runs fine at High IL (`Responding: True`,
  a full `msedgewebview2.exe` process tree spawns — main, crashpad-handler,
  network/storage utility, gpu-process, **and a renderer** — so the app is
  actually rendering), but `netstat` shows no listener on the requested
  `--remote-debugging-port` at all, and an HTTP probe against it gets
  `ECONNREFUSED`. Matches the maintainer's description exactly: the switch
  reaches the WebView2 loader (it's still `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`,
  a dropped channel) and is silently ignored, not erroring and not hanging on
  anything GPU-related (`--disable-gpu` made no difference, ruling out that
  earlier hypothesis).
- **Fix applied and confirmed working.** `ci.yml` sets the HKLM policy value
  Microsoft names — `HKLM\Software\Policies\Microsoft\Edge\WebView2\AdditionalBrowserArguments`
  — before launching: a machine-wide registry write on the ephemeral runner VM
  (CI-only, nothing in product code, nothing to clean up since the VM is
  destroyed after the job regardless). The first attempt set it as a plain
  value directly under `...\WebView2`, which had no effect (still
  `ECONNREFUSED`) — `AdditionalBrowserArguments` isn't in Microsoft's
  *published* WebView2 policy reference at all, so the fix comment named the
  policy without its registry shape. Every documented policy in that same
  family (`BrowserExecutableFolder`, `ChannelSearchKind`, `DowngradeVersion`,
  `ReleaseChannel{Preference,s}`) stores itself as a **key**, not a value,
  containing one Value-Name/Value pair per app (name = exe name/AUMID, or
  `*` for all apps). Restructuring the write to match that convention
  (`...\WebView2\AdditionalBrowserArguments` as the key, the app's own exe name (`orrerix.exe`) as the
  value name) fixed it: the CDP port opened, and 2 of 3 specs passed in CI on
  the very next run (the third had an unrelated drag-timing flake, since
  fixed — see `pane-reorder.spec.ts`).
- A `runas /trustlevel:0x20000` de-elevation attempt (tried before the by-design
  root cause was found, back when this looked like it might be a
  window-station/desktop-access issue) did not resolve it — expected in
  hindsight, since de-elevating the *process* doesn't change which policy
  channels WebView2 itself honors.

`e2e-windows` stays `continue-on-error: true` even now that it's green — a
brand-new job earns its way off `continue-on-error` with a track record, not
on day one (see Roadmap). What's now fixed is the roadmap itself: "wait for
Microsoft" was never a real option (the maintainer explicitly won't change
by-design behavior). The HKLM policy is the actual fix; a self-hosted runner
remains the fallback if it ever stops holding up (e.g. a future WebView2
version tightens the HKLM channel too). For the record, a Fixed-Version-149
pin was also investigated and ruled out on two independent grounds, not just
deferred:

- It would not have worked even if a pre-150 build were available:
  `WEBVIEW2_BROWSER_EXECUTABLE_FOLDER` is a `WEBVIEW2_*` environment
  variable, and per the maintainer's own closing comment quoted above,
  that whole channel is dropped at High IL — the exact same restriction
  blocking `AdditionalBrowserArguments`. (An earlier version of this
  section argued the opposite, that `WEBVIEW2_BROWSER_EXECUTABLE_FOLDER`
  "would mechanically work" because it *replaces* rather than merely
  supplements the value passed in code — true as a general statement
  about the override's semantics, verified from
  [the WebView2 environment-variables reference](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/webview2-idl?view=webview2-1.0.3719.77)
  (misattributed in that earlier version to the *Distribution* page,
  which doesn't contain that sentence — corrected here), but beside the
  point once High IL drops the env var before it ever reaches that
  override logic.)
- Independently: Microsoft only keeps Fixed Version downloads for the
  latest and second-latest major releases (confirmed on
  [the Distribution page](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution)),
  and the live download page
  ([developer.microsoft.com/microsoft-edge/webview2](https://developer.microsoft.com/en-us/microsoft-edge/webview2/#download-section))
  offered only 150.0.4078.99 — the same major as the by-design change —
  when checked. There would have been nothing to pin to either way.
- Tauri 2's own config reference has no `webviewFixedRuntimePath` field
  (only `bundle.windows.minimumWebview2Version`/`webviewInstallMode`,
  which govern installing Evergreen, not redirecting to a Fixed Version
  folder) — a Tauri v1 claim that didn't hold up against the v2 docs, moot
  given the above either way.

If a future run goes red again for a CDP-connection reason (not a spec-logic
one), re-check the HKLM policy first — that's the thing actually holding this
up, not "wait and see."

**On the job's per-push cost**: running a full debug `tauri build` plus the
suite costs a few minutes of Windows runner time on every push regardless of
pass/fail. Gating the job to path-filtered triggers (`e2e/**`, `src/**`) or a
schedule was considered as a mitigation for a job that would otherwise be
*guaranteed* red — but with the HKLM fix, a run now produces a real,
actionable result (pass/fail on the actual specs) rather than a foregone
conclusion, so that trade-off no longer applies and the job stays on every
push like the other three.

## Roadmap

- Promote `e2e-windows` off `continue-on-error` once it has a flake track
  record.
- Add `data-testid` attributes to the highest-churn selectors (pane header,
  launcher form fields, overlay roots) so specs stop depending on class names
  and label text.
- Parallelize (currently `workers: 1` — see `playwright.config.ts`). Port
  contention is not the blocker (a fixed port is simplest given `workers: 1`
  has no contention to avoid today, and CI needs one static value to match
  its HKLM policy regardless); the real blocker is that every E2E instance
  shares one Tauri identifier, so concurrent workers would share a WebView2
  browser process with each other. `WEBVIEW2_USER_DATA_FOLDER` is confirmed
  to survive elevation and could isolate workers from each other, but doing
  so would break `verifyIsolatedBuild`'s identifier-based verification (see
  "Structural isolation verification") — parallelizing safely needs a way to
  do both at once, e.g. a distinct identifier per worker.
- The argv-length launch failure from the originating session is a strong
  E2E-shaped candidate (spawn with a long argv, assert the pane reaches a
  running state rather than an error) but wasn't one of this spike's three
  PoC cases — worth a follow-up spec.
- Revisit child-webview CDP attach once loomux has a real one to test against.

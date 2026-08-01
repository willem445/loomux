# Pane plugins: the public contract (#360, Slice A)

*Contract-first. This note ships before any plugin code, and every later slice
(B backend host, C broker, D the `"plugin"` kind, E metrics, F the example
plugin, G template/SDK) builds against exactly what is written here — not
against whatever felt convenient while implementing. A change to the manifest
schema, the capability enum, or the broker envelope after this note lands is a
contract change and needs its own review, the same as any other public
contract in this repo (`AGENTS.md`/CLAUDE.md's definition-of-done rule 5).*

## Why this exists

The human asked for user-installable custom panes (#360): "everyone has their
own requirements for their own development workflow… I want my tool to
support whatever they need in front of them." The naive version of that ask —
let a repo or a user drop in a script that becomes a pane — is a straight
line to the thing this codebase has spent its whole life refusing to be. A
pane plugin is third-party code the human did not audit, running inside the
same process as an orchestrator that can grant merges, push to remotes, and
write to disk. So the shape of this feature is set entirely by one question:
**what can a plugin reach, and what stops it reaching anything else?** That
question is answered once, here, and every other slice inherits the answer
rather than re-deriving it.

## The trust problem, stated exactly

Loomux's whole identity is "visible prompts, audit log, host-enforced
guardrails, never bypass ever" (`doc/marketing-research.md`). Two facts about
the current webview make that identity fragile the moment arbitrary code runs
inside it:

- **Every `#[tauri::command]` is reachable by any script in the webview.**
  At the time of this audit the app registers roughly **117** commands in one
  `generate_handler!` (`src-tauri/src/lib.rs`) — the exact count moves as
  commands are added; what matters is that **none is individually
  permission-gated**. Tauri's capability system gates *plugin* commands
  (dialog, window, …), not app-defined ones. A script with access to
  `window.__TAURI_INTERNALS__` can call `orch_grant_merge`, `git_push`,
  `ft_write_file`, `spawn_pty` — anything — with no capability check between
  it and the command.
- **`tauri.conf.json`'s `csp` is `null`.** No script/connect/frame-source
  restriction exists anywhere in the app today. `withGlobalTauri` is off, but
  `@tauri-apps/api`'s `invoke` reaches the same injected internals from any
  same-origin script.

Today this is safe because of an axiom stated outright in CLAUDE.md
(constraints 5 and 6): **the webview is trusted**, because everything that
runs in it is code the human installed loomux to run. A pane plugin breaks
that axiom on purpose — it is code from someone else, loaded because the
human wanted a feature, not because they audited it. The instant plugin code
shares an origin with `__TAURI_INTERNALS__`, "the webview is trusted" stops
being true, and every guardrail built on top of it (the merge gate included)
stops meaning anything.

**The merge gate specifically does not help here.** The `gh pr merge` PATH
shim (#196/#335) intercepts an agent's own *subprocess* call to the real `gh`
binary. It has nothing to do with IPC: a script calling `orch_grant_merge` or
`git_push` over `invoke` never touches a subprocess, so it never touches the
shim. Grant files are documented as reachable "ONLY through Tauri
commands… agents never mint them" — that promise assumes the only things
minting them are loomux's own trusted frontend modules. An unsandboxed
plugin is exactly the kind of code that promise was never written to survive.

**Conclusion:** a pane plugin that runs as same-origin JS has the same reach
as loomux's own frontend — all ~117 commands, no exceptions. Isolation is not
a hardening pass on top of the feature. It **is** the feature; everything
else in this note exists to give a plugin a box it cannot climb out of.

## One new content kind, not a second pane system

The repo already has the mechanism this needs: a **content pane** (#214,
generalized in #217/#222, see `doc/design/content-panes.md`) is a PTY-less
pane whose content is a view — `startContent()` builds it, `PersistedPane`
captures it, `planPaneRestore` restores it, `tabcounts` excludes it from the
agent counter by kind. `files`, `editor`, `git`, and `workflow` are four
instances of one mechanism, not four mechanisms.

A plugin pane is a **fifth instance of the same mechanism**: one new content
kind, `"plugin"`. It is not a parallel pane system, and it does not get its
own split/dock/drag/maximize/restore/counting logic — it inherits all of
that by joining the same closed unions the other four kinds already sit in
(`PaneKind`/`isContentKind` in `panesetup.ts`, `PersistedPaneKind`/
`CONTENT_KINDS` in `tabstore.ts`, the `buildContentView` dispatch in
`pane.ts`, the restore switch in `panerestore.ts`). Slice D is the only slice
that touches those shared sites, and it touches them the same small,
contained way #217 and #222 each added one kind before.

**Identity: `pluginId`.** A content pane's persisted identity today is
carried in the `cwd`/`file` fields it already has (a root, an open path) —
there was never a reason to add a schema field for `files`/`editor`/`git`,
because "which folder" and "which file" are both paths. A plugin pane's
identity is not a path — it is *which plugin* — so `PersistedPane` gains one
new field: `pluginId: string | null`. This is additive to the existing
persisted-layout schema (`tabstore.ts`), not a version bump: content panes
have never bumped `SCHEMA_VERSION` for a new field, because `decodePane` is
shape-driven and tolerates an absent field as `null` on an older snapshot.
The same tolerance has to hold in reverse for plugins specifically, because a
plugin can be *uninstalled between sessions* in a way a folder or a repo
generally isn't — see **Restore and fail-soft** below.

`pluginId` is the string in the manifest's `id` field (below) — author-chosen,
stable, and the only thing an edge case (a rename, a re-install, a version
bump) is allowed to key on.

## The plugin manifest

A plugin is a folder containing `plugin.json` plus its own assets. The
manifest is the plugin's declaration to loomux of what it is, what version of
the contract it speaks, and what it is asking permission to touch. It is
**not** a place a plugin can expand its own reach — see the capability model
below for why that is load-bearing, not incidental.

```jsonc
{
  "id": "resource-monitor",        // REQUIRED. Stable identity — see below.
  "name": "Resource monitor",      // REQUIRED. Display name only, may change freely.
  "version": "1.0.0",              // REQUIRED. The PLUGIN's own semver, author's concern.
  "apiVersion": 1,                 // REQUIRED. Which broker contract this plugin speaks — see Versioning.
  "entry": "index.html",           // REQUIRED. Relative path inside the plugin folder; served over plugin://.
  "capabilities": ["panel", "metrics.system"], // REQUIRED, may be []. Subset of the closed enum — see below.
  "rootless": true                 // OPTIONAL, default false. See "Rootless plugins".
}
```

**`id` is the identity; `name` is display only** — the exact rule
`doc/design/content-panes.md` states for workflow blocks and that design
document's own precedent in `orchestration/workflow.rs` states for blocks
generally: *n8n keys its graph by a node's display name, so a rename silently
breaks every reference to it.* Here, `id` is what `pluginId` persists, what
the `plugin://` scheme's per-plugin folder jail is keyed by (below — its
*address space*, not necessarily a distinguishable browser origin; see
Install/discovery), and what "is this plugin still installed" is checked
against on restore. It is chosen once by the plugin
author and is not intended to change across the plugin's own versions.
`name` can be retitled in any release without touching a single persisted
pane.

**Required fields fail closed.** A manifest missing `id`, `version`,
`apiVersion`, `entry`, or `capabilities`, or naming an `entry` that resolves
outside the plugin's own folder, or declaring a capability string outside the
closed enum, is **rejected** at discovery/install time with a reason — never
partially accepted, never coerced to a default. This mirrors the workflow
model's "an unknown `kind` is a hard validation error, never coerced"
(`orchestration/workflow.rs`): guessing what an invalid declaration *meant*
is exactly the wrong instinct for a file that is, by definition, untrusted
input.

**Versioning (`apiVersion`).** `apiVersion` is a small integer, bumped only
when the broker's method/event contract changes in a way that isn't
backward-compatible. It is not the plugin's own `version` (that one is the
author's release number and loomux never inspects it). The broker refuses to
load a plugin whose `apiVersion` exceeds what the running loomux build
implements (a newer plugin on an older loomux), and refuses individual
broker methods that were introduced after a plugin's declared `apiVersion` (a
plugin correctly declaring an older contract doesn't get offered a newer
method it never asked for and may not expect). Both refusals are visible to
the plugin as an error on the broker call, not a silent no-op. The enum below
plus the method/event set Slice C implements *is* the wire contract for
`apiVersion: 1`; widening it is a new `apiVersion`, not an edit to this one.

**Rootless plugins.** Most content panes are rooted at a folder the human
picks (a directory for `files`/`editor`, a repo for `git`). A plugin whose
subject isn't a filesystem location at all — the bundled resource monitor
being the motivating case, Slice F — sets `rootless: true` so the install/new
-pane picker skips asking for a root and skips the `ftRootIsDir`-style
existence probe on restore entirely. A rootless plugin's `fs.read` capability
(if declared) is simply unavailable — there is no root to jail reads to, so
the broker's `fs.read` handler has nothing to serve and the manifest
combination `rootless: true` + `fs.read` is rejected at validation, not
silently ignored.

## The capability model

### Why closed, not extensible — the precedent this mirrors exactly

This is not a new pattern for the repo. The workflow-block `kind` field
(`orchestration/workflow.rs`, `doc/design/workflows.md`, #222) already solved
this exact problem for a different kind of untrusted input — a workflow YAML
file a human puts in a repo, that any contributor can edit, that an
`auto_ops` group runs unattended with nobody approving individual tool calls.
Its rule, quoted directly because a plugin manifest is the same shape of
problem:

> **A workflow file can never grant a capability.** `kind` *selects* from the
> closed enum; there is no `read_only: false` escape hatch, no `allow_write`,
> no way to spell a fifth capability class.

A plugin manifest is a repo- or human-installed file authored by someone the
human is choosing to trust *for this one plugin, at this one grant of
capabilities* — no different in kind from a workflow file being authored by
"whoever opened a PR against the repo." The same rule applies verbatim:
**`capabilities` selects from a closed enum; it cannot invent a new one, and
there is no field anywhere in the manifest or the broker protocol that widens
what an entry in the enum grants.** If a plugin needs something the enum
doesn't cover, the enum is wrong and gets a deliberate, reviewed addition —
never a per-plugin escape hatch.

**Why closed instead of an open/scriptable permission system:** an extensible
model (arbitrary IPC method names a plugin can request, or a manifest field
that maps to a raw command name) reduces the review surface to "does this
one plugin's ask look reasonable" — exactly the reasoning that let 117
ungated commands accumulate in the first place. A closed enum means the
question is answered **once, for all plugins, in this file**: the full set
of things a plugin can ever be capable of is the four rows below, forever,
until a human deliberately reviews and ships a fifth.

### The v1 enum

| Capability | Grants | Backed by | Notes |
| --- | --- | --- | --- |
| `panel` | Render into the pane's content box. | Nothing IPC-shaped — a plain hosted window/frame, no broker method behind it. | Implicit: every plugin gets this merely by existing as a pane. Not declared in the manifest's `capabilities` array (there is nothing to opt into). **Host→frame `resize` and `theme` events were planned for this row but are NOT implemented in v1** — the envelope reserves the event names (see Broker contract, below) but nothing sends them; a plugin gets no live signal of pane size or app theme today. Tracked as [#378](https://github.com/willem445/loomux/issues/378), a required follow-up, not a silent gap — a plugin author relying on either event will see nothing arrive. |
| `storage` | A namespaced per-plugin key/value store for view state (window position, last-selected tab, etc). | `uistate.rs`, new keys namespaced by `pluginId` so one plugin can never read or overwrite another's storage. | No cross-plugin read. No shared bucket. |
| `fs.read` | Read files **under the pane's own root only** — path-jailed, no exceptions. | The existing `ft_read_file` command plus `fileedit.rs`'s server-side path choke point, with the pane's root as the jail boundary. | Rejected at manifest-validation time on a `rootless: true` plugin (no root to jail to). No directory listing beyond what `ft_read_file`'s existing surface already permits. |
| `metrics.system` | Subscribe (`metrics.subscribe`/`metrics.unsubscribe`) to a read-only stream of system + per-process resource stats (CPU/RAM). | `procmetrics.rs` (Slice E) — a plain Rust module, not a `#[tauri::command]`; the only way to reach it is through the broker's `metrics.subscribe`/`metrics.unsubscribe` methods, per this note's "never exposed to a plugin except through this one broker capability." | Curated payload — name, pid, cpu%, rss. No cmdline, no paths, no environment. Bounded: capped at 32 processes/tick (`procmetrics::MAX_PROCESSES`), sorted by CPU desc; poll interval clamped to 1–10s (`procmetrics::clamp_interval_ms`) regardless of what a plugin requests — a plugin cannot turn this into an unthrottled process-table dump or a tight polling loop. Pane attribution (mapping a build-child process to the agent pane that spawned it, via the per-pane kill-on-close Job Object `pty.rs` already holds) was scoped for this slice but deferred as a follow-up — see the Slice E note below. |

**Deliberately absent from the enum, so unreachable by construction, not by
policy a plugin could talk its way around:** any filesystem **write**, git,
`gh`, PTY spawn or write, any orchestration/grant command, network access of
any kind. The broker (Slice C) has **no handler function** for any of these —
there is no code path to find a bug in, because there is no code path.

### What a plugin can and cannot do (v1), stated as the threat table

| | Can | Cannot |
| --- | --- | --- |
| Rendering | Draw arbitrary DOM/canvas in its own pane box; burn CPU inside its own sandboxed frame. | Read or manipulate the host DOM outside its own frame. |
| Data | Read files under its own declared root (`fs.read`); persist its own namespaced view state (`storage`); read a read-only system-metrics stream (`metrics.system`). | Write **any** file; read another plugin's storage; read outside its own root; read process command lines/paths beyond name+pid+stats. |
| System reach | Nothing beyond the three capabilities above. | Call `invoke` or reach any of the ~117 app commands directly; spawn or write to a PTY; touch git or `gh`; mint or read an orchestration/merge grant; steer or inject input into an agent pane. |
| Network | Nothing. | Phone home, load a remote resource, or otherwise reach the network — enforced by a restrictive CSP header served on every `plugin://` response, **not** by the iframe `sandbox` attribute alone (which does not restrict network egress). See **Content-Security-Policy on plugin content**, under Isolation. |

### Grant, in v1: auto-granted from the closed enum, not a human decision yet

**Status update — this section previously described install-time human
approval as decided; it wasn't, and the implementation was never changed to
match. Correcting the record:** v1 **auto-grants** whatever subset of the
closed enum a manifest declares and passes validation on — `install_plugin`
(Slice B, `plugins.rs`) copies the folder once the manifest parses and every
declared capability string is in the enum; there is no approval prompt, no
install-time UI step, and no per-capability human decision anywhere in the
code path. The manifest's declared `capabilities` array **is** the grant, the
moment install succeeds.

This is narrower than it sounds, not a hidden hole: the enum itself is
closed and reviewed once, here, and every row in it is already bounded on the
implementation side — `fs.read` is jailed to the pane's own root, `storage`
is namespaced per `pluginId`, `metrics.system` is curated and capped
(`procmetrics::MAX_PROCESSES`, `clamp_interval_ms`), and `panel` grants
nothing IPC-shaped at all. Auto-granting a member of *this* enum is not the
same risk as an open permission model would be — but it is still a human
installing a plugin without being shown, or asked to confirm, which of these
four the manifest is asking for, which is what "grant is a human decision"
promised and did not deliver.

**Install-time capability approval is deferred to
[#377](https://github.com/willem445/loomux/issues/377), and is a REQUIRED
blocker** — before v1 ships to general availability, and before
[#375](https://github.com/willem445/loomux/issues/375)'s native-sidecar work
(which would widen what a plugin can reach and makes an un-reviewed grant a
materially bigger problem than it is today). Shipping without #377 in the
interim is acceptable only because of the bound above: the v1 enum has no
write, no git/gh/PTY/orchestration reach, jailed `fs.read`, namespaced
`storage`, and a curated, bounded `metrics.system` — a plugin auto-granted
its full declared set still cannot reach anything this note's threat table
(above) lists as "Cannot." That acceptability argument does not extend past
v1's enum; widening what any single capability *means* (e.g. turning
`fs.read` into `fs.write`, or adding a new capability class entirely) is
exactly the kind of change flagged in the plan's decision list as needing its
own threat review before it ships — this note is not pre-approving that
expansion, only the four rows above, and #377 is what makes even *those*
four a human's decision rather than the manifest author's.

## The broker contract

**A plugin never calls `invoke`, and never sees `@tauri-apps/api` or
`window.__TAURI_INTERNALS__`.** Its only channel to the host is
`postMessage`, and its only handler on the other end is a **broker** —
hand-written host-side code with one function per allowed operation, never a
generic relay onto Tauri's command surface. "Never raw `invoke`" is the
sentence the whole isolation section (below) exists to make true structurally,
not just by convention: even if a plugin's sandbox were somehow bypassed,
the broker itself has no method that forwards to `invoke`, so there is a
second, independent wall behind the first.

### Envelope shape

Every message crossing the `postMessage` boundary, either direction, is one
of three envelope shapes, each carrying the `apiVersion` the plugin declared
so the broker can apply the right method/permission set without a second
handshake:

```ts
// plugin -> host, expects a reply
interface PluginRequest {
  type: "request";
  id: string;           // correlates the response; plugin-chosen, opaque to the host
  apiVersion: number;
  method: string;       // e.g. "storage.get", "fs.read", "metrics.subscribe"
  params: unknown;
}

// host -> plugin, replying to a request
interface PluginResponse {
  type: "response";
  id: string;            // echoes the request's id
  ok: boolean;
  result?: unknown;      // present when ok
  error?: { code: string; message: string }; // present when !ok
}

// host -> plugin, unsolicited (resize, theme, a metrics tick, …)
interface PluginEvent {
  type: "event";
  event: string;         // e.g. "resize", "theme", "metrics.tick"
  payload: unknown;
}
```

`resize`/`theme` are names this envelope reserves, not events v1 sends —
only `metrics.tick` (Slice E) is actually pushed today. See the `panel`
row above and [#378](https://github.com/willem445/loomux/issues/378).

### Per-message capability + version check

Every `PluginRequest` the broker receives is checked, in this order, before
any handler runs:

1. **Identity check** — `event.source === frame.contentWindow`, compared
   against the specific frame object the broker created for that plugin
   pane. This window-reference identity is the security-bearing check, **not**
   an origin-string comparison: under the recommended sandboxed-iframe model
   (below), every plugin's frame reports `event.origin === "null"` — the
   opaque origin `sandbox="allow-scripts"` without `allow-same-origin`
   produces — so origin strings cannot and do not distinguish one plugin's
   frame from another's. Only the live window reference can. A message whose
   `source` isn't the exact frame object the broker is listening for is
   dropped, not answered, regardless of what `origin` it claims.
2. **`apiVersion` check** — the method exists at the plugin's declared
   `apiVersion`. If not: `error.code = "unsupported-version"`.
3. **Capability check** — the method's owning capability is in the set the
   manifest declared (v1: auto-granted at install on successful validation —
   see "Grant, in v1" above and [#377](https://github.com/willem445/loomux/issues/377)
   for the human-approval step this will gain). If not:
   `error.code = "capability-denied"`.
4. **Params validation** — malformed params (wrong shape, a path escaping
   the jail root, an unknown method name) get `error.code = "bad-request"`.

Only after all four pass does the broker's hand-written handler for that
method run and produce `result`. This is the pure decision the plan calls out
by name — "is method M allowed for granted capabilities C at apiVersion V" —
and it is implemented once, as a pure function the DOM wiring calls, so the
rule cannot be quietly re-implemented (or forgotten) at a second call site.
The same house move as `dirtystate.closeDecision` and `workflowpane.ts`'s
`paneSurface`/`savePlan`: **the rule lives in one pure place; the view only
calls it.**

### Error surface

A denied or malformed request always gets a `PluginResponse` with
`ok: false` and a stable `error.code` (`unsupported-version`,
`capability-denied`, `bad-request`, or a handler-specific code such as
`not-found` for `fs.read`) — never a silently dropped message, and never a
thrown exception that could crash the plugin's frame in a way that looks like
a host bug rather than a permission boundary. A plugin author debugging
"why doesn't `fs.read` work" sees `capability-denied` and knows exactly what
to add to their manifest — the error surface is part of the contract, not an
implementation detail.

## Isolation

**Status: DECIDED (Option B — child `WebviewWindow`).** The Phase-0/Phase-0.5
spikes this section originally gated on have both run. Phase-0 (branch
`spike/360-sandbox-proof`) found the sandboxed opaque-origin iframe **leaks**:
the frame's own global gets a full, working `__TAURI_INTERNALS__` despite
`sandbox="allow-scripts"` with no `allow-same-origin` (full findings:
[#360 comment](https://github.com/willem445/loomux/issues/360#issuecomment-4992640640)).
Phase-0.5 then proved the fallback below — a child `WebviewWindow` bound to a
dedicated capability — **holds**, but only once loomux opts its own commands
into Tauri's ACL system at all (full findings:
[#360 comment](https://github.com/willem445/loomux/issues/360#issuecomment-4992837152)).
That prerequisite is no longer a gap: #363/#366 shipped it (see
`doc/design/acl-manifest.md`) — `src-tauri/build.rs` now builds a real app ACL
manifest, `capabilities/plugin-zero-template.json` is the proven zero-grant
base, and Slice C (this section's implementation) adds the real, populated
`capabilities/plugin.json` a shipped plugin window is bound to. The sections
below are written in the past tense where they describe what was *decided*
and in the present tense where they describe what Slice C *built* — nothing
here is provisional anymore.

> **Amendment (#360 Slice D pivot, `fix/360-plugin-embed` commit e337c95):**
> Slice D's *hosting* mechanism changed after this section was written —
> `WebviewWindow` as a **separate top-level OS window** shipped first (a live
> bug: it rendered as a floating, fully-decorated window instead of embedded
> pane content) and was then replaced with `Window::add_child` (Tauri's
> multiwebview API, the `unstable` feature): the plugin is now a **child
> webview embedded directly in the `main` window**, a real native region of
> the pane, not a second window. This section's *trust-core* reasoning below
> is otherwise unaffected — the same broker, the same capability/apiVersion
> check, the same "no live JS reference between plugin and `main`" property —
> **except for one load-bearing correction the multiwebview spike found**
> (findings comment on #360): a capability scoped via `windows: [...]` grants
> *every webview of that window*, `webviews` field notwithstanding (Tauri's
> own `Capability::windows` doc comment). Since `add_child` attaches the
> plugin to the *existing* `main` window, `capabilities/plugin.json` and
> `capabilities/default.json`'s own `main` grant are now `webviews`-scoped,
> not `windows`-scoped — every place below that says `windows: ["plugin-*"]`
> should be read as `webviews: ["plugin-*"]`. `tests/acl_manifest.rs`'s
> `webview_scope_guard_denies_windows_scoped_leak_to_child_webview` is the CI
> guard against ever reintroducing `windows`-scoping here.

> **Amendment (#380, iframe re-verdict — spike 2):** the `add_child` decision
> above was re-checked against an iframe with two candidate mitigations for
> the Phase-0 leak, since a native child webview carries its own real cost (no
> DOM z-index, the geometry-sync class of bug this PR fixes). Neither holds on
> this repo's Tauri 2.11.5 / wry 0.55.1 baseline: **M1**
> (`for_main_frame_only` scoping the IPC handler to the top frame) still
> leaked live — wry registers the custom-protocol/IPC handler per `WebView`,
> not per frame, so a child frame's `invoke` reaches it exactly as before.
> **M2** (a `document-start` init script scrubbing `__TAURI_INTERNALS__`
> before the plugin's own script runs) loses the race — wry's own internals
> injection is *itself* a `document-start` script, and injection order between
> two `document-start` scripts on the same frame is unspecified, so the scrub
> sometimes runs first (leak closed) and sometimes second (nothing left to
> scrub, but too late regardless since the plugin's inline script can already
> have run by then). Separately, an iframe has no webview label of its own to
> scope a capability to (`Capability::windows`/`webviews` both key off the
> **hosting webview**, `main`), so it inherits `main`'s entire ACL grant
> wholesale — the broker's curated permission set becomes decoration, not a
> boundary. A residual remote-origin loopback path this spike surfaced (a
> `plugin://` response served with a stale/absent CSP being reachable via a
> `main`-origin `fetch` a leaked-internals frame could issue) is tracked
> separately in [#395](https://github.com/willem445/loomux/issues/395), not
> folded into this decision. **Decision: `add_child` stays.** The
> geometry-sync gaps it carries (a moved pane's child webview not
> repositioning, #380) are ordinary bugs with an ordinary fix — not a
> trust-boundary defect — whereas the iframe's leak is load-bearing and,
> per M1/M2 above, has no mitigation that holds on this baseline.

### Rejected: sandboxed opaque-origin iframe

A plugin's `entry` HTML is rendered inside an `<iframe sandbox="allow-scripts">`
— **deliberately without `allow-same-origin`** — served from a `plugin://`
URI scheme (see Install/discovery). Omitting `allow-same-origin` is the whole
mechanism: it forces the frame onto a **unique opaque origin**, which the
same-origin policy uses to block the frame from reaching `window.top`'s
`__TAURI_INTERNALS__`, the host's DOM, or the host's storage — regardless of
what URL scheme served the frame's content. The frame's only channel out is
`postMessage`, to a broker that is the only thing listening.

This is the same model VS Code's own webview extensions use (sandboxed
iframe, `postMessage`, no Node/host access) — a proven pattern at exactly
this scale (view-level plugin code, not a service), not a novel design being
tried for the first time here.

**Rejected outcome (Phase-0, recorded for the historical record — do not
build this):** the assumption above does not hold on this repo's Tauri
2.11.5 / WebView2 / Windows 10 baseline. The sandboxed iframe's own global
gets a fully working `__TAURI_INTERNALS__` — `invoke` function, invoke key,
IPC pattern, all present — despite `sandbox="allow-scripts"` with no
`allow-same-origin`. The opaque origin still correctly blocks the frame from
*reflecting into* `window.top` (SOP does what SOP does), it just doesn't stop
the frame getting its own copy of the internals wry/Tauri inject regardless
of origin. A real `invoke("pty_backend_info")` from inside the sandboxed
frame reached Tauri's IPC handler and was rejected only by an accidental
`Url::parse("null")` parse failure on the opaque `Origin` header
(`tauri-2.11.5/src/ipc/protocol.rs:496`) — not a deliberate boundary, and not
one to build a trust model on. Full route-by-route evidence:
[#360 comment](https://github.com/willem445/loomux/issues/360#issuecomment-4992640640).

### Content-Security-Policy on plugin content

`sandbox="allow-scripts"` stops DOM/storage/IPC reach — it does **not** stop
network egress. A sandboxed frame with no CSP can still `fetch()` an
arbitrary host, load a remote `<img>`/`<script>`, or open a `WebSocket`;
sandboxing and network isolation are two separate guarantees, and only a CSP
served *with the plugin's content* provides the second one. So this is part
of the contract, not a Slice B implementation detail left to chance: **every
response the `plugin://` scheme handler returns MUST carry a restrictive
`Content-Security-Policy` header**, at minimum

- `connect-src 'none'` — no `fetch`/`XHR`/`WebSocket`/`EventSource` to
  anywhere, including loopback;
- `default-src`, `script-src`, `img-src`, and `style-src` bounded to
  `'self'`/`plugin:` — a plugin loads its own bundled assets and nothing
  remote;
- `frame-src 'none'` and `object-src 'none'` — a plugin cannot embed a
  further frame or object to route around the policy.

This header is a property of **what is served**, not of the container
serving it — it rides on the HTTP-shaped response the scheme handler returns
for every asset request, so it applies unchanged whichever isolation
primitive ends up hosting the frame (the recommended iframe or the
`WebviewWindow` fallback below). A Slice B implementation that serves
`plugin://` assets without this header silently falsifies the "cannot phone
home" row in the threat table above, even if the frame's sandbox/isolation
is otherwise perfect — the two mechanisms are independent, and this note
requires both.

### Decided: child webview with scoped capabilities (Option B)

Each plugin gets its own isolated `Webview`, embedded directly into the
`main` window via `Window::add_child` and positioned/sized over the pane's
content box (Slice D's job — out of this slice's scope; see the amendment
above for why this is `add_child`, not a separate `WebviewWindow`), bound to
`capabilities/plugin.json` (`webviews: ["plugin-*"]`), which grants
**exactly** the `plugin-broker` permission set — two commands,
`plugin_broker_request` and `plugin_broker_open_channel` — and nothing else
in the app's command surface. This holds *only* because #363/#366 gave
loomux a real app ACL manifest first (`doc/design/acl-manifest.md`); without
that prerequisite, a zero/curated-permission capability on a secondary window
is inert, for the same root cause the iframe leaked by (see the Phase-0.5
findings linked above). The broker contract, envelope shape, and capability
enum from the sections above are all unchanged by this choice — none of them
depend on iframe-specific mechanics (an opaque `"null"` origin, `sandbox`
semantics); they depend on "the plugin's code cannot reach `invoke` and
cannot reach the host DOM," which Option B satisfies by a different, and in
practice more auditable, mechanism: a real, named, per-window ACL deny
instead of an opaque-origin same-origin-policy wall.

Two more options were weighed and rejected outright, not gated on a spike: a
separate OS process per plugin (right isolation, wrong scale — plugins are
view code, not services) and no isolation at all / manual review (the exact
thing this whole note exists to refuse — every one of the ~117 commands
reachable, including a merge grant).

**Transport: `invoke` + `Channel`, not literal `postMessage`.** The envelope
shapes above (`PluginRequest`/`PluginResponse`/`PluginEvent`) were specified
before Option B was chosen, and describe a `postMessage` bridge — correct for
the rejected iframe model, where `event.source === frame.contentWindow` is a
live, comparable JS reference. A plugin's child webview is a separate
top-level browsing context from `main`'s own document (confirmed originally
against the `WebviewWindow` fallback, Phase-0.5, and again against
`add_child` embedding, the multiwebview spike): `window.opener` is `null`
either way — Tauri creates every plugin webview independently, never via
`window.open()` — so there is no JS reference between it and `main` at all,
and nothing for a literal `postMessage` to target. Slice C's broker
therefore uses Tauri's own IPC as the transport:

- **Plugin → host (request/response):** the plugin calls
  `invoke("plugin_broker_request", { request })`; the command *is* the
  request/response round trip — no second envelope hop needed. The
  envelope's logical shape (`id`/`apiVersion`/`method`/`params` in,
  `ok`/`result`/`error` out) is preserved exactly; only the wire mechanism
  changed.
- **Host → plugin (unsolicited events — reserved for resize/theme, shipped
  for metrics ticks; see [#378](https://github.com/willem445/loomux/issues/378)):**
  a `tauri::ipc::Channel<PluginEventWire>`, opened once via
  `plugin_broker_open_channel`. This is deliberate, not incidental: granting
  a plugin window the app's general `core:event:allow-listen` permission
  would let it call `listen()` for *any* event name emitted anywhere in the
  app — including e.g. `pty-output`, which broadcasts every pane's terminal
  output on one shared event, per `pty.ts`'s own docstring — since Tauri's
  permission gates whether `listen()` may be called at all, not which event
  names it may hear. A `Channel` has no such surface: it is scoped to the one
  invocation that created it, so a plugin can receive only what the broker
  explicitly pushes to *its own* channel.
- **Identity (step 1 of the per-message check):** structural rather than a
  runtime comparison. Only a webview matching `capabilities/plugin.json`'s
  `webviews: ["plugin-*"]` pattern can reach `plugin_broker_request` at all —
  enforced by Tauri's ACL resolver before the broker's own code runs — and
  the broker then looks up that exact webview's registered session by its
  label (unforgeable — Tauri, not the plugin, assigns and reports it via the
  command's injected `Webview` parameter). Note this checks the WEBVIEW
  label, not the window label: under `add_child` embedding every plugin
  shares `main`'s window label, so `windows`-scoping here would defeat the
  identity check entirely (the amendment above). This is at least as strong
  as the iframe model's `event.source` check, and it's the same mechanism
  that makes the zero-permission template's denial real in the first place.

### Residual capabilities a plugin child webview has that a sandboxed iframe wouldn't, and their mitigations

A plugin's child webview has no `sandbox=""` attribute equivalent — none of
the iframe sandbox tokens (`allow-forms`, `allow-top-navigation`, etc.) exist for
a top-level webview window. Found empirically by the Phase-0.5 spike, and
mitigated across Slices B and C:

- **Unrestricted self-navigation.** `location.href = 'https://example.com/'`
  fully navigated the spike's plugin webview to a real external page —
  nothing stopped it. **Mitigation (Slice C):** every plugin
  `WebviewBuilder` (originally `WebviewWindowBuilder`, unchanged by the
  Slice D `add_child` pivot — see the amendment above) is constructed with
  `.on_navigation(...)`, locked to a pure predicate that allows only the
  plugin's own `plugin://localhost/<id>/…` address space — the same one
  Slice B's scheme handler serves (see the `plugin://` scheme bullet below);
  the authority is fixed (`localhost`, or `plugin.localhost` on Windows) and
  `<id>` is checked as the first path segment, never the host — and denies
  everything else, another plugin's own otherwise-valid address included, so
  one plugin's webview can't even navigate itself into impersonating a
  different plugin.
- **Network egress is not blocked by CSP alone; the app's global CSP is
  `null` anyway.** Tauri's CSP is one `tauri.conf.json` setting, not
  configurable per-webview through the public builder API. The real
  lever — as this note's CSP subsection already specified before Option B was
  chosen — is the `Content-Security-Policy` header Slice B's `plugin://`
  scheme handler (`plugins::plugin_protocol_handler`) stamps on every
  response it returns, `connect-src 'none'` included (hardened further with
  `form-action 'none'`/`base-uri 'none'` — sandbox tokens alone don't stop a
  form submission or a `<base>`-tag rewrite either). It does this on
  **every** response, success or denial alike, so a rejected request can't
  be distinguished from an allowed one by a missing header.
- **Same-origin storage/messaging rendezvous is a real hazard the `plugin://`
  origin must actually prevent**, not an iframe-specific concern: two
  plugins (or a plugin and `main`) sharing one origin would share
  `localStorage`/`BroadcastChannel`/`SharedWorker` with each other. This is
  why the `storage` capability is namespaced by `pluginId` **host-side**
  (Slice C's broker, not origin isolation) rather than relying on each
  plugin's `plugin://` origin to keep them apart on its own.
- **Plugin-provided text is untrusted, regardless of transport.** A
  manifest's `name` (and any other author-chosen string) is third-party text
  loomux never audits — it must be treated as data everywhere it's
  surfaced (the pane's tab label, the plugin picker's option text), never
  interpolated into HTML or any other markup a renderer would parse. (Slice D
  originally also passed it as a `WebviewWindow`'s OS window-chrome title;
  the `add_child` pivot has no window chrome to title, so that surface no
  longer exists — see the amendment above and Slice D's own notes below.)

## Install / discovery

- **Install location (recommended — see Open decisions, below):**
  `<app-data>/loomux/plugins/<id>/`, one folder per plugin, scanned on boot.
  This is the recommendation the rest of this section is written against, not
  a settled decision: the plan flags a repo-scoped `.loomux/plugins/`
  (git-shared, exactly like `.loomux/workflow.yml`) as a live alternative for
  a team that wants its plugin roster to travel with the repo rather than the
  machine, and leaves picking one (or supporting both) to the human. A folder
  is a plugin if it contains a `plugin.json` that passes manifest validation;
  a folder that doesn't is skipped, not treated as an error that blocks
  discovery of the others (the same "one bad entry doesn't take down the
  rest" posture the workflow model's audited-and-skipped failure policy
  uses). The install action, the `plugin://` scheme, and the no-marketplace
  stance below are all agnostic to which location (or both) is ultimately
  chosen.
- **Install action:** an in-app **Install plugin…** picker that copies a
  chosen folder into the plugins directory. "Install" means exactly that copy
  — there is no build step, no compilation, no fetch from anywhere. A source
  folder whose manifest fails validation is rejected with the specific
  reason (missing field, unknown capability, bad `apiVersion`, an `entry`
  that resolves outside the folder); nothing is copied on a rejection. A
  source that would itself try to escape the plugins directory (a manifest
  or path crafted to write outside `<id>/`) is refused the same way
  `fileedit.rs`'s path choke point refuses any other traversal attempt.
- **The `plugin://` scheme:** each installed plugin is served from
  `plugin://<id>/...`, resolving strictly inside that plugin's own folder —
  the same server-side path-validation discipline the rest of the app already
  applies to file access (canonicalize, reject anything that escapes). This
  gives every plugin its own **address space for asset serving** (one
  plugin's `entry` can never resolve into another's folder) and is the
  request the CSP header above rides on. It does **not** by itself give each
  plugin a distinguishable *browser origin* under the recommended
  sandboxed-iframe model: every such frame's opaque origin serializes to the
  same string, `"null"`, regardless of which `id` served it (see the identity
  check in the Broker contract, above). Two plugins never bleed into each
  other's `storage` because the broker namespaces storage keys by `pluginId`
  host-side (the capability table, above) — not because of origin isolation.
  (Under the `WebviewWindow` fallback, `plugin://<id>` **does** become each
  window's own real, distinguishable origin — one more reason this note
  leans its guarantees on the CSP and the folder jail, not on origin
  comparison, so the two isolation models stay behaviorally equivalent from
  a plugin author's perspective.)
- **No remote marketplace in v1.** Discovery is local-folder-scan only; there
  is no in-app browse/search/download of plugins from anywhere. Getting a
  plugin onto the machine is entirely the human's own act (copy a folder,
  or use the picker to copy one in) — nothing in loomux fetches plugin code
  from the network on its own.

## Open decisions (pending human veto)

The plan closes with five decisions it names explicitly as the human's call,
not this note's to settle. Recording them here, together, is deliberate: two
of them (isolation model, capability breadth) are already threaded through
the sections above as gated/flagged, but presenting only those two risks
reading the other three as quietly decided. None of the five is closed by
this note — a veto on any one is a targeted edit to the section(s) it names,
not a rewrite of the contract:

1. **Isolation model.** *Recommended:* the sandboxed opaque-origin iframe
   (Isolation, above), pending the Phase-0 spike. *On veto or spike failure:*
   the child `WebviewWindow` fallback, already specified above as a full
   alternative. *Must not ship regardless:* no isolation at all — a plugin
   with unsandboxed reach to all ~117 commands, which this whole note exists
   to refuse.
2. **Install location.** *Recommended:* `<app-data>/loomux/plugins/<id>/`,
   scanned on boot (Install/discovery, above). *Live alternative:* a
   repo-scoped `.loomux/plugins/`, committed and git-shared exactly like
   `.loomux/workflow.yml` — the natural choice if "a team standardizes on
   the same plugin roster" matters more than "a plugin follows me across
   repos." The plan leaves picking one (or supporting both) to the human;
   this note does not foreclose it.
3. **API surface breadth.** The v1 capability enum (`panel`, `storage`,
   `fs.read`, `metrics.system`) is deliberately narrow — no writes, no
   git/gh/PTY/orchestration reach. Widening what an existing capability means
   (e.g. `fs.read` → `fs.write`) or adding a new capability class is a
   per-capability decision with its own threat review; this note pre-approves
   none of that, only the four rows as written.
4. **Bundling the example plugin as installed-by-default.** Slice F's
   resource monitor is planned to ship already installed, so the demo works
   without a manual install step — which also means a default plugin holds
   `metrics.system` from first run. The alternative is shipping it
   *uninstalled* (bundled in the app but requiring the same Install action as
   any third-party plugin). Confirm the turnkey default is actually wanted
   before Slice F ships it that way.
5. **Deferring the pane-kind registry refactor.** This note's design adds
   `"plugin"` to the existing closed unions (One new content kind, above) and
   deliberately does not collapse the four built-in kinds into a general
   registry first. That is the right scope for v1 — confirm the deferral is
   acceptable rather than assumed, since a future registry refactor would
   revisit every union site this note lists.

## v1 non-goals (verbatim from the plan)

Copied here so this note stays the single place both the scope and its edges
are stated, without drifting from what was actually agreed:

> Remote plugin **marketplace/registry** and in-app browse/download; plugin
> **auto-update**; **signed/notarized** plugins; plugins that **write** files
> or reach **git/gh/PTY/orchestration/grant** commands (the capability enum
> simply omits them); **plugin-to-plugin** messaging; plugins contributing
> **non-pane UI** (menu items, toolbar buttons, command palette, keybindings);
> **production hot-reload** (dev-mode reload only); **mac/Linux packaging** of
> the install/asset story (design for portability, build and validate
> Windows). Each is a deliberate follow-up, not an oversight — v1 proves the
> sandbox + capability contract with one real consumer.

## What later slices owe this note

- **Slice B** (backend host — **done**, `plugins.rs`) implements manifest
  parsing/validation (reject-with-reason on any manifest violation,
  path-traversal-proof by construction, never a partial accept),
  local-folder discovery/install under `plugins_root_dir()`, and **owns the
  one registered `plugin://` scheme handler**
  (`plugins::plugin_protocol_handler`, wired in `lib.rs`) — jailed
  per-plugin-folder and carrying the CSP header the
  **Content-Security-Policy on plugin content** section specifies on every
  response, success or denial alike, hardened past this note's floor with
  `form-action 'none'`/`base-uri 'none'`. Tauri allows exactly one handler
  per registered scheme, so Slice C's `plugin_open_window` points the plugin's
  child webview at the URLs this handler serves
  (`plugin://localhost/<id>/<entry>`, `pluginbroker::build_plugin_url`)
  rather than registering a second one. `list_plugins`/`install_plugin` are
  main-only commands (`permissions/sets/plugins.toml`).
- **Slice C** (the broker, the trust core — **done**, this note's Isolation
  section records the decided design) ran the Phase-0/Phase-0.5 spikes;
  implemented the envelope contract, the capability/apiVersion check as one
  pure function (`pluginbroker::check_request` / `pluginprotocol.ts`'s
  `checkCapability`) plus the command wiring around it; `plugin_open_window`
  (which embeds a child webview pointed at Slice B's `plugin://` address
  space and installs the `on_navigation` lock); forwards nothing to raw
  `invoke` from within a plugin webview, ever — a plugin's capability grants
  exactly two commands (`capabilities/plugin.json`,
  `permissions/sets/plugin-broker.toml`). The `metrics.system` capability is
  gated but its data handler is a stub pending Slice E's `sys_processes`-shaped
  backend — the check is real, the numbers aren't yet.

  **`plugin_open_window` must stay an `async fn`.** A live bug found on
  #380's merge gate: as a synchronous command it hit the documented Tauri/wry
  Windows deadlock (`WebviewWindowBuilder::new`'s own rustdoc, wry#583) — a
  sync command runs inline on the same WebView2 UI thread that dispatched the
  IPC call, and `.build()` then can't get that same thread to service the
  window-creation round trip it needs, so the whole app hangs (blank plugin
  surface, frozen main window, nothing closable). `async fn` moves the
  command body onto the async-runtime threadpool instead, leaving the UI
  thread free to answer. **Unchanged by the Slice D `add_child` pivot** (the
  amendment above): `WebviewBuilder::new`'s own rustdoc carries the identical
  warning, and `Window::add_child` itself blocks on the same main-thread
  round trip `WebviewWindowBuilder::build()` did — confirmed from
  `tauri-2.11.5/src/window/mod.rs`'s own source, not just by analogy. This
  isn't testable against `tauri::test::MockRuntime` for the deadlock itself
  (the mock runtime's dispatcher doesn't block the way a real WebView2 UI
  thread does) or without a live Windows GUI, so it has no automated
  regression test for the DEADLOCK specifically; don't revert this to a
  plain `fn` without re-reading wry#583 first. (The ACL-isolation
  consequence of `add_child` — `windows` vs `webviews` scoping — IS
  automated: `tests/acl_manifest.rs`'s
  `webview_scope_guard_denies_windows_scoped_leak_to_child_webview`.)
- **Slice D** (the `"plugin"` kind — **done**) adds exactly one member to
  each closed union this note describes as inheriting the content-pane
  mechanism (`PaneKind`/`isContentKind` in `panesetup.ts`, `PersistedPaneKind`/
  `CONTENT_KINDS` in `tabstore.ts`, `ContentPaneKind`/`buildContentView` in
  `pane.ts`, the restore switch in `panerestore.ts`), adds `pluginId` to
  `PersistedPane` (additive, no schema bump — the same move `file` was for
  #217), and implements the restore fail-soft behavior this note assumes: a
  pane naming a `pluginId` that is no longer installed fails soft to the
  welcome form with a toast, in that one slot, the same way an uninstalled
  git repo already does — it does not throw, and it does not silently drop
  the pane from the layout on the next save.

  **Hosting the webview (rewritten by the `add_child` pivot — see the
  amendment above).** A plugin pane hosts NO DOM content of its own — Slice
  C's `plugin_open_window` embeds a child `Webview` directly into the `main`
  window via `Window::add_child`, not a node this pane's content box could
  contain. `PluginPaneView` (`pluginpaneview.ts`) is the one place that
  positions and resizes that child webview to sit exactly over the pane's
  `.pane-content` box, on every layout change that could move it — a divider
  drag, a split, a tab switch, a maximize elsewhere — via a `ResizeObserver`
  on its own content box. Unlike the original `WebviewWindow` design, there
  is no `onMoved`/`onResized` listener on the main window at all: `add_child`
  positions the webview relative to `main`'s OWN client area (not absolute
  screen coordinates), so a window move changes nothing, and a window resize
  only matters insofar as it resizes the pane's own box — which the
  `ResizeObserver` already watches directly. It hides the plugin webview the
  moment that box collapses to zero size (`pluginwindow.ts`'s
  `pluginWindowShouldShow`) — the SAME zero-size signal `applyFit()` already
  uses to skip a PTY resize on a hidden pane, reused here for a hidden
  *webview* — rather than wiring a bespoke hook into each of
  dock/tab-hide/maximize separately, and closes it (`plugin_close_window` —
  a child webview never fires `WindowEvent::Destroyed`, so an explicit close
  command replaces the window-destroyed cleanup hook the original design
  used) on pane dispose. The geometry (`pluginWebviewRect`) is pure and
  DOM-free (`test/pluginwindow.test.ts`), and simpler than the original
  design's `pluginOverlayRect`: no main-window origin/scale-factor
  translation, since the pane's own `getBoundingClientRect()` is already in
  the exact coordinate space `Webview.setPosition`/`setSize` expect. The
  Tauri/DOM wiring around it is hand-validated, per this repo's convention
  for DOM wiring.

  **The one gap Slice B left for this slice to close.** `list_plugins`
  echoes a manifest's `id`/`name`/`entry`/`capabilities`/`apiVersion`/
  `rootless` but never an absolute install path — nothing needed one until
  `plugin_open_window`'s `root` (for `fs.read`'s jail). Slice D resolves it
  client-side (`resolvePluginRoot` in `pluginpaneview.ts`) by joining Tauri's
  own `dataDir()` with `loomux/plugins/<id>` — the exact formula this note's
  own "Install / discovery" section publishes as the install-location
  contract, computed via Tauri's base-directory resolver rather than
  reimplementing `plugins_root_dir()`'s OS-path logic a second time. If the
  install location decision (open decision 2, above) changes, this is the
  one place that has to follow it.

  **Untrusted text.** A plugin manifest's `name` reaches this slice in two
  places — the pane's tab label and the plugin picker's option text. (A
  third, earlier surface — the `plugin_open_window` `title` passed to the
  `WebviewWindow`'s OS window chrome — no longer exists: the `add_child`
  pivot embeds the plugin with no window chrome to title, so
  `OpenPluginWindowRequest` dropped the field entirely rather than leaving it
  unused.) Both remaining surfaces go through `textContent`/`.value`
  assignment only, never `innerHTML` or template interpolation — the DOM
  auto-escapes, so there's no separate "escaping" step to get wrong.

  **Known, accepted gaps** (documented rather than engineered around — this
  repo's own precedent for a real-but-cosmetic limitation, see
  `content-panes.md`'s "one known, accepted cosmetic gap"; rewritten by the
  `add_child` pivot, which removes one gap entirely and changes the shape of
  another — see the amendment above): a freshly-opened plugin webview that
  starts hidden (opened into a currently-invisible pane) may render for one
  frame at a degenerate 1x1 size before the first `reposition()` call hides
  it (smaller than the original design's equivalent gap, which flashed at a
  full default size in the wrong place, but not fully eliminated — `add_child`
  has no `visible: false` builder option). Multi-monitor DPI is no longer a
  gap at all: `add_child` positions relative to `main`'s own client area, so
  there is no cross-monitor scale-factor math to get wrong (the original
  design's absolute-screen-coordinate positioning is exactly what the
  `add_child` pivot exists to eliminate — see the multiwebview spike's
  findings comment on #360). Z-order versus `main`'s OWN DOM content was a
  gap here too — **CLOSED on Windows by the #391 amendment below.**
- **#391 amendment (folded into #380, `fix/360-native-zorder` — corrected
  root-cause fix superseding the reverted global-hide band-aid, PR #392,
  reverted at d3333b3):** the z-order gap above had a real functional half,
  not just a cosmetic one — a DOM overlay (the sessions sidebar, a modal, a
  context menu) opened over a plugin pane wasn't just rendered behind the
  plugin's native content, it was also unclickable underneath it, because a
  Win32 `WS_CHILD` window (what `add_child` creates) always both paints above
  and swallows every pointer event over its own rect, unconditionally — there
  is no z-index knob for that, and `main`'s own DOM overlays are painted BY
  `main`'s one webview surface, not as separate OS surfaces, so whichever is
  "on top" wins for the whole overlapping rect.

  **Spike: WebView2 composition hosting, rejected.** WebView2 has a
  windowless "composition" mode (`ICoreWebView2CompositionController`,
  backed by `Windows.UI.Composition`/DirectComposition) that would let a
  webview's content be placed as a visual in a host-owned compositor tree
  instead of a native child `HWND`. The underlying COM surface is already in
  `webview2-com`'s own bindings, but `wry` 0.55.1's Windows backend never
  calls into it — it hardcodes the windowed `CreateCoreWebView2Controller`
  path unconditionally, so this is unreachable through Tauri's public API
  without forking `wry`. That fork would not even solve the problem, though:
  composition-hosting the PLUGIN alone only changes whether its visual sits
  above or below `main`'s ENTIRE webview surface as one opaque unit — it
  still can't interleave with content `main` paints INSIDE its own single
  surface without ALSO composition-hosting `main` end-to-end (a full rewrite
  of Tauri's Windows windowing backend, affecting every window in the app,
  still needing the host to drive per-region hit-test routing itself since
  WebView2 doesn't do this automatically for windowless content — the same
  computation the fix below does anyway, just wrapped in DirectComposition).
  Full findings: `src-tauri/src/pluginregion.rs`'s module doc comment.

  **The fix: region-clip the plugin's own HWND.** The same technique
  browsers used for windowed-plugin/DOM coexistence (NPAPI/ActiveX windowed
  plugins had this identical problem). Tauri v2's STABLE `Webview::with_webview`
  API (not `unstable`/multiwebview) hands back the real
  `ICoreWebView2Controller`; `ICoreWebView2Controller::ParentWindow` returns
  exactly the container `HWND` `wry` created for that one embedded webview.
  `pluginregion::plugin_set_occlusion` (main-only, mirroring
  `plugin_open_window`/`plugin_close_window`'s grant) calls `SetWindowRgn` on
  that `HWND` to exclude the rects of every DOM overlay currently covering the
  pane (`overlaystate.ts`'s live per-overlay-rect registry +
  `pluginocclusion.ts`'s pure intersect/translate math, called from
  `pluginpaneview.ts`'s `reposition()` on every overlay open/close, window
  resize, and pane layout change). Both paint AND hit-test fall through to
  `main` in the excluded rects — a real per-region clip, not a global hide.
  ACL isolation is untouched: this only affects OS-level `HWND`
  painting/hit-testing, never the webview's label, capability resolution, or
  the broker's session state — `tests/acl_manifest.rs`'s guards are
  unaffected. No `wry` fork, no new crate: `windows` was already a
  dependency (a second, differently-versioned copy is aliased in as
  `windows061` specifically for this interop boundary — see `Cargo.toml`'s
  comment on why).

  **Cross-platform delta, stated honestly.** Windows-only. The same root
  cause applies on macOS/Linux (`add_child`'s child webview is a peer
  `NSView`/`GtkWidget` there too, and `main`'s overlays are painted inside
  `main`'s own single surface exactly the same way), but a region-clip
  equivalent (`CALayer` masking on macOS, GDK shape/input regions on GTK)
  needs separate, platform-specific native code this PR does not add:
  unverified native GUI code for a platform this workspace cannot build or
  interactively test, and CI's macOS/Ubuntu builds don't exercise the
  overlay-over-plugin scenario either, so a from-scratch implementation would
  ship unverified by construction. This is a documented gap, not a silent
  one — the pre-#391 bleed is unchanged there, not regressed, and is a
  candidate follow-up issue.
- **#380 amendment: the #391 fix shipped, then broke live under the sessions
  sidebar's own open animation.** Third live report on this surface: opening
  the sessions sidebar (`#sessions`'s `width: 0 -> 344px` CSS transition,
  `styles.css`) over a visible plugin pane let the plugin paint back over the
  sidebar, sometimes correcting itself only after an unrelated later event.
  Root cause, proved by reading the exact Tauri/wry versions this repo is
  pinned to (`tauri` 2.11.5, `tauri-runtime-wry` 2.11.4, `tauri-macros`
  2.6.3 — not guessed): `pluginpaneview.ts`'s old `reposition()` called
  `Webview.setPosition`/`setSize` (Tauri's own BUILT-IN webview commands,
  declared `async` — dispatched onto the async runtime's threadpool), then
  separately called this module's `plugin_set_occlusion`. The built-in
  position/size commands' handler (`Dispatch::set_bounds`) checks
  `current_thread().id() == main_thread_id`; called from a threadpool worker
  (not main), it takes the fire-and-forget branch —
  `context.proxy.send_event(...)`, a post to the winit/tao event loop's
  user-event queue — so the awaited JS promise resolved once that message was
  *queued*, not once the window had actually moved/resized.
  `plugin_set_occlusion`, by contrast, is a plain (non-`async`) command, which
  Tauri's macro runs INLINE on whatever thread dispatched the IPC call — for a
  call from `main`'s own webview, the WebView2 UI/main thread itself, via a
  completely separate dispatch path with no ordering guarantee against
  winit's queue. Net effect: occlusion could be computed and applied against
  the OLD size/position while the frontend had already translated the DOM
  overlay's rect into the NEW pane origin, and rapid `ResizeObserver` ticks
  during the sidebar's 240ms transition could apply out of order relative to
  each other, too.

  **The fix: fold bounds into the same synchronous command as the clip.**
  `plugin_set_occlusion` is replaced outright by `pluginregion::plugin_set_frame`
  (renamed in place — the ACL command count stays 128, not a net addition;
  its only caller is updated in the same change). Still a plain, non-`async`
  command (so it still runs inline on the calling/main thread), it now ALSO
  sets the webview's bounds itself via `tauri::Webview::set_bounds` — called
  from this synchronous context, `send_user_message` takes the FAST inline
  branch instead of the fire-and-forget one, so the resize is applied before
  this same function goes on to read the client rect and build the occlusion
  region a few lines later. One IPC round trip, one synchronous sequence, no
  thread hop, no other command able to interleave — atomic by construction.
  This also closes the concurrent-`reposition()`-calls race: a single
  synchronous command is processed by WebView2's IPC dispatch strictly in
  arrival order, so there is no longer a window for an older call's
  now-orphaned write to land after a newer one's. `pluginpaneview.ts`'s
  `reposition()` reads `el`'s rect exactly ONCE per call now (previously it
  was read once but reused across two intervening `await`s, a second, smaller
  staleness gap this also closes) and makes ONE `setPluginFrame` call.

  **Telemetry.** Every `plugin_set_frame` application logs one breadcrumb
  (`crate::obs::breadcrumb`, `"pluginregion"`) — the trigger source the
  frontend passed through (`resize` | `move-notify` | `overlay-open` |
  `overlay-close` | `init`, `pluginpaneview.ts`'s `RepositionSource`), the
  bounds and exclude-rect count applied, and whether the native calls
  succeeded — so a live occurrence leaves diagnostic evidence in
  `breadcrumbs.log` even if this fix turns out to be incomplete.

  **A second, smaller bug found investigating the same trigger.**
  `sessions.ts`'s `SessionBrowser` closed its overlay registry slot the
  instant the `.hidden` class landed on CLOSE, while `#sessions`'s own CSS
  transition was still visually collapsing the sidebar for ~240ms after —
  the mirror image of the open-side bug, on the close edge. Fixed by
  `closeOverlayAfterTransition` (waits for the real `transitionend`, with a
  timeout backstop).

  **#380 residual: the bounds+occlusion fix above didn't fully close the
  open-side bleed.** Live re-report after the atomic `plugin_set_frame` fix
  shipped: expanding the sessions sidebar still left a visible plugin pane's
  native content painted at its PRE-expansion footprint for several seconds
  before correcting — far longer than the sidebar's own 240ms transition, the
  discriminator that this wasn't an animation-timing gap but a genuinely
  stuck state waiting on an unrelated later trigger. Confirmed against
  `breadcrumbs.log`: an `overlay-open` frame is applied at the exact instant
  `toggle()` flips the class (before the transition's first frame), so it
  captures the sidebar at its still-collapsed width — after that single edge,
  nothing re-fired until some unrelated later trigger (another overlay's own
  open/close, a window resize) happened to force a fresh `reposition()`.
  Root cause: registering/releasing the overlay slot on the open/close EDGE
  was never enough by itself for an overlay that keeps moving/resizing WHILE
  open — `overlaystate.ts`'s `poke()` exists precisely for that case
  ("an overlay that moves/resizes while open") but had no production caller;
  every DOM overlay in the app is a fixed-size popover/modal that doesn't
  need it, except this one, an animated docked sidebar.

  **The fix.** `sessions.ts`'s `SessionBrowser` now keeps a `ResizeObserver`
  on `#sessions` itself for its whole lifetime, calling `overlayState.poke()`
  on every tick of its own `width` transition (idle the rest of the time,
  since the observer only fires when the box's size actually changes), plus
  a `transitionend` listener as a final-frame backstop — the same
  belt-and-suspenders reasoning `closeOverlayAfterTransition` already uses
  for this same transition. `poke()` feeds the same
  `overlayState.subscribe` → `reposition(...)` → `plugin_set_frame` path
  every other trigger already uses, so no new command was needed — but a
  poke is now its OWN `RepositionSource`, `"overlay-poke"`, rather than
  folded into `"overlay-open"`: a poke during an animated overlay's
  transition is a per-frame burst, the same frequency class as a `"resize"`
  storm, not the rare discrete edge `"overlay-open"` is. Folding it into
  `"overlay-open"` (an always-logs source) would have reintroduced, from the
  overlay's side, the exact per-frame breadcrumb storm `"resize"`'s gate
  exists to prevent from the pane's own side — so `pluginregion.rs`'s
  `should_log_frame` gates `"overlay-poke"` identically to `"resize"` (logs
  only on an actual exclude change or a native failure). Net effect: every
  plugin pane recomputes its bounds+occlusion continuously across BOTH the
  open and close transition, not just at their edges, with telemetry staying
  state-change-gated exactly as before.
- **#380 round 2: the `overlay-poke` fix above shipped, then proved INERT
  under live re-test.** Fresh telemetry from `breadcrumbs.log` across
  multiple sessions-sidebar toggles: (1) ZERO `overlay-poke` breadcrumbs,
  ever; (2) `exclude` was 0 in EVERY logged state, open or closed; (3) the
  plugin pane's logged bounds at the `overlay-open` edge and the
  `overlay-close` edge were nearly identical ("full width" in both). Read
  together with the human's own eyes ("the workspace visibly shifts/shrinks"
  and the plugin "outgrows its pane" for a while before correcting), this
  round treated the prior diagnosis as unproven rather than trusted it.

  **Root cause 1 — wrong mental model, not a bug in the exclude math.**
  `index.html`/`styles.css` show `#sessions` is `flex: none; width: 344px;
  transition: width` — a genuine flex SIBLING of `#grid-area` inside
  `#workspace { display: flex }`, not `position: absolute`/`fixed`. Toggling
  it changes `#grid-area`'s available width via ordinary flexbox reflow; it
  never occupies the same screen region as a pane it "covers" at any instant
  of its transition (`pluginocclusion.ts`'s intersect math structurally
  cannot produce a rect for two regions that never overlap). So `exclude`
  being 0 in every state (finding 2) was never a bug — every round since
  #391 mis-modeled this ONE panel as a covering DOM overlay (the same class
  as a modal or context menu) when it's actually a push-layout sibling that
  changes plugin panes' own BOUNDS, not what covers them. `sessions.ts` no
  longer registers `#sessions` with `overlayState.open()`/`close()` at all —
  see that file's header comment.

  **Root cause 2 — the reason finding (1) tells you nothing either way.**
  `pluginregion.rs`'s `should_log_frame` gated the two high-frequency
  sources (`"resize"`, `"overlay-poke"`) on `exclude_changed` ALONE. A
  push-layout panel's transition changes BOUNDS, never `exclude` — so every
  one of those frames was structurally invisible to `breadcrumbs.log`
  regardless of whether they were actually being applied. Finding (1) (zero
  `overlay-poke` lines) is therefore NOT proof the mechanism never ran — it's
  proof the log could never have shown it either way. Fixed: `should_log_frame`
  now gates on `bounds_changed || exclude_changed` (`pluginregion.rs`), with a
  red-before-green unit test (`should_log_frame_logs_a_bounds_only_change_
  even_with_an_unchanged_exclude`) encoding exactly this case.

  **Root cause 3 — the actual mechanism behind "corrects several seconds
  later."** Confirmed empirically (a minimal repro of the exact `#sessions` /
  `#grid-area` / `.grid-root` / `.pane` / `.pane-content` / `.pane-plugin`
  flex structure, driven headless in a real Chromium/WebView2-family engine
  via raw CDP — not the loomux app itself, no `tauri dev`): a plugin pane's
  own generic `ResizeObserver` (`pluginpaneview.ts`'s `"resize"` source)
  fires correctly on nearly every animation frame of the sidebar's 240ms
  `width` transition — roughly 15 ticks, each converging to the exact
  correct, live geometry; a `transitionend` + `requestAnimationFrame` read
  lands on the identical final value. The DOM/observer chain was never the
  defect. The defect: every one of those ~15 ticks (plus every other
  trigger — a window resize, an overlay edge) independently fired its OWN
  `plugin_set_frame` IPC round trip, completely unthrottled — `frameUnchanged`
  only skips a call when the geometry is byte-identical to the last one
  already INTENDED, which is never true during continuous motion. A real
  native call (`Webview::set_bounds` + `SetWindowRgn` plus the IPC bridge
  hop) is not free; a burst of ~15-20 of them crammed into a single 240ms
  window drains SLOWER than the observer produces them, so the plugin kept
  applying stale, already-superseded frames well after the CSS transition had
  visually finished — the exact "several seconds" both this file's #380
  entry above and the live #380 round-2 report independently describe.
  `overlay-poke` (the previous fix) made this WORSE, not better: it queued
  ANOTHER redundant per-frame trigger duplicating geometry the pane's own
  `"resize"` observer was already producing, onto an already-overloaded
  channel — which is also why it left no telemetry trace regardless of the
  `should_log_frame` gate above.

  **The fix.** `pluginpaneview.ts`'s `reposition()` is now a thin gate in
  front of the real work (renamed `repositionNow`): it reuses `RefreshGate`
  (`refreshgate.ts`, the SAME single-flight + trailing-coalesce primitive
  `sessions.ts`'s own `refresh()` and `IssuesView`'s refresh loop already
  use) so at most ONE native `plugin_set_frame` call is ever in flight; every
  trigger that arrives while one is running collapses into exactly one
  trailing call, which reads geometry FRESH at the moment it actually runs,
  never a stale value from when it was superseded. A burst of N triggers now
  costs at most 2 native round trips, not N — the visible lag can never
  exceed roughly one round trip, regardless of how many `ResizeObserver`
  ticks, overlay edges, or window resizes arrive while one is in flight.
  `sessions.ts` additionally provides ONE authoritative settle-time recompute
  (`transitionend` + `requestAnimationFrame` → `overlayState.poke()`) once
  its own transition has fully committed, as a final guarantee on top —
  replacing the per-tick `overlay-poke` observer outright (dead code once
  the pane's own `"resize"` tracking already covers "during", per root cause
  1 above) rather than leaving it in place unproven.

  **Expected telemetry signature for the next live test.** A single sessions
  toggle should now produce, in `breadcrumbs.log`, filtering to
  `pluginregion`: an `overlay-open` or `overlay-close` line at the toggle
  edge (always logged), then a small number of `resize`-sourced lines (now
  visible under the bounds-changed gate) with PROGRESSIVELY changing
  `bounds=` values converging toward the sidebar's final width, and finally
  one `overlay-poke`-sourced line whose `bounds=` matches the fully-settled
  layout (pane narrower and shifted right on open, back to full width on
  close) — `exclude=0` throughout every line, which is correct for this
  panel, not a bug. The total `resize`+`overlay-poke` line count for one
  toggle should be small (at most a couple, thanks to the coalescing gate),
  not the ~15-20 an unthrottled burst would have produced. If the user's next
  test shows ONLY the settle (`overlay-poke`) line and no intermediate
  `resize` lines, that means the coalescing gate is collapsing the entire
  burst into its first and trailing calls (expected and fine); if it shows
  NEITHER — silence all the way through, `exclude` aside — that would mean
  `"resize"` truly isn't firing in the live app despite the empirical repro,
  and is the next thing to re-investigate, stated honestly rather than
  assumed away.
- **Slice E** (metrics — **done**, `procmetrics.rs`) exposes `sys_processes`
  -shaped data **only** through the `metrics.system` broker handler — never as
  a command a plugin (or any other webview script) could `invoke` directly.
  `metrics.subscribe` starts a background poll thread, keyed by the plugin
  webview's label, that pushes a curated, bounded `metrics.tick` `PluginEvent`
  over the channel `plugin_broker_open_channel` opened; `metrics.unsubscribe`
  (and `plugin_close_window`'s cleanup, since a child webview never fires
  `WindowEvent::Destroyed` — see Slice D's notes above) stops it. Bounding is
  two pure, unit-tested
  functions — `shape_processes` (sort by CPU desc, cap at `MAX_PROCESSES`) and
  `clamp_interval_ms` (floor/ceiling on the poll cadence a plugin can request)
  — so the DoS-shaped concern in this note's threat table has a concrete,
  tested answer rather than being merely intended. **Deferred, not shipped:**
  attributing a build-child process to the agent pane that spawned it, via
  `QueryInformationJobObject` on the per-pane kill-on-close Job Object
  `pty.rs`'s `assign_kill_on_close_job` already creates (issue #78). Wiring
  that through means exposing pane-to-job-handle lookup out of `pty.rs`'s
  `PtyManager` and threading a pane/group identity into the metrics payload —
  more than the contained addition this slice's brief allowed for scope; a
  plain per-process snapshot ships now, pane attribution is a follow-up.
- **Slice G** (template/SDK/authoring guide — **done**,
  `templates/loomux-plugin/`, `docs/features/pane-plugins.md`) needed no
  addition to the contract above: the template's manifest declares only
  `panel`/`storage` (rootless, so no `fs.read`), and its "hello world" is one
  `storage.get`/`storage.set` round trip. The client SDK
  (`templates/loomux-plugin/sdk/plugin-sdk.js`) is a thin, dependency-free
  wrapper around the two broker commands, not a new capability or method —
  it exists because a plugin has no build step to `npm install
  @tauri-apps/api` through, not because the wire contract needed widening.
- **Slice F** (the example plugin) is the first real *runtime* consumer of
  the contract above; if it needs a capability, method, or event this note
  doesn't already grant, that is this note being wrong, not a shortcut to
  take silently — it comes back here for a reviewed addition.

A bundled example plugin (the resource monitor) lives at
`src-tauri/resources/plugins/resource-monitor/` and ships already installed
(#360 Slice F).

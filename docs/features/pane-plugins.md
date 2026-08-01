---
title: Pane plugins
layout: default
parent: Features
nav_order: 7
---

# Pane plugins
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

---

A **pane plugin** is third-party code that fills a pane's content box, the
same way the built-in File explorer / File editor / Git / Workflow panes do.
It runs in its own isolated window, reaches loomux only through a small,
capability-gated **broker**, and can never touch a shell, git, `gh`, or any
orchestration/merge command — those simply aren't in the broker's vocabulary.
This page is the plugin-author's guide: the manifest, the broker API, what
the sandbox does and doesn't allow, and how to install and open one. The
deeper design rationale (why isolation works this way, the full threat model)
lives in [`doc/design/pane-plugins.md`](https://github.com/willem445/loomux/blob/main/doc/design/pane-plugins.md)
in the repo.

## Start from the template

Don't start from scratch — copy
[`templates/loomux-plugin/`](https://github.com/willem445/loomux/tree/main/templates/loomux-plugin)
from the repo. It's a complete, minimal plugin: a manifest, an `index.html`,
a `main.js` that does one real broker round trip, and the client SDK
described below, already wired together. Its own `README.md` has the
copy/rename/install steps; the rest of this page is the reference for what
you do next.

## The manifest — `plugin.json`

Every plugin is a folder containing `plugin.json` plus its own assets:

```jsonc
{
  "id": "my-plugin",              // REQUIRED. Stable identity — see below.
  "name": "My Plugin",            // REQUIRED. Display name only, may change freely.
  "version": "1.0.0",             // REQUIRED. Your plugin's own semver.
  "apiVersion": 1,                // REQUIRED. Which broker contract you speak — currently always 1.
  "entry": "index.html",          // REQUIRED. Relative path inside your folder, served over plugin://.
  "capabilities": ["panel", "storage"], // REQUIRED, may be []. Subset of the closed enum below.
  "rootless": true                // OPTIONAL, default false. See "Rootless plugins" below.
}
```

- **`id` is the identity; `name` is display only.** `id` is what the
  plugins directory's folder name must match exactly, what the `plugin://`
  address space is keyed by, and what a persisted pane restores against.
  Pick it once and don't change it across your own releases — a rename is a
  new plugin as far as loomux is concerned. `name` can change in any release.
- **Every required field is enforced, fail-closed, with a reason.** A missing
  field, an `entry` that resolves outside your own folder, or a capability
  string outside the closed enum below gets your plugin rejected at
  install/discovery time — never partially accepted, never silently
  coerced to a default.
- **`apiVersion`** is the broker's wire contract version (currently `1`), not
  your plugin's own release number. Declaring a version newer than the
  running loomux build understands gets your plugin refused outright, with
  the reason stated.
- **Rootless plugins** (`rootless: true`) have no filesystem root — pick this
  if your plugin isn't "rooted" at a folder or repo (a dashboard, a monitor,
  anything not reading local files). A rootless plugin cannot declare
  `fs.read` (rejected at validation — there's no root to jail reads to).

## The capability enum — closed, not extensible

`capabilities` selects from a **closed enum**. There is no field anywhere in
the manifest or the broker protocol that grants anything beyond these four
rows, and none will ever let a plugin invent a fifth:

| Capability | Grants | Notes |
| --- | --- | --- |
| `panel` | Render into the pane's content box. | Implicit — every plugin gets this merely by existing. Declaring it in `capabilities` is accepted as a harmless no-op (so the sample above stays valid), but you don't have to list it. |
| `storage` | A namespaced per-plugin key/value store (`storage.get`/`storage.set`). | Your own bucket only — no other plugin, and no host code, can read or overwrite it. |
| `fs.read` | Read files **under your plugin pane's own root only**. | Path-jailed, no exceptions. Unavailable (and rejected at validation) on a `rootless: true` plugin. |
| `metrics.system` | Subscribe to a read-only, bounded stream of system + per-process CPU/RAM stats. | Curated payload — name, pid, cpu%, rss only; no cmdline, no paths, no environment. Capped at 32 processes/tick, polling clamped to 1–10s regardless of what you request. |

**Not in the enum, so unreachable no matter what you write:** any filesystem
*write*, git, `gh`, spawning or writing to a PTY, any orchestration/grant
command, and the network (see CSP, below). The broker has no handler
function for any of these — there's no code path to find a bug in, because
there is no code path.

**Capabilities are auto-granted in v1, not yet a reviewed human decision.**
Declaring a capability in your manifest and passing validation is enough —
installing copies the folder and every declared capability is live
immediately, with no approval prompt shown along the way. Nothing in the
protocol lets a plugin widen its own grant after install, but nothing today
shows the human what they're granting either; an install-time approval step
is planned ([#377](https://github.com/willem445/loomux/issues/377)) and is a
required blocker before general availability. In the meantime this is
bounded by the enum itself — the four rows above are all a manifest can ever
ask for, and none of them reach a write, git/gh/PTY, or the network.

## Talking to the broker

Your plugin never sees `@tauri-apps/api`, `invoke`, or any of loomux's ~120
backend commands. The **only** thing your window can reach is a broker with
exactly two entry points, and your manifest's approved capabilities decide
which of the methods below actually succeed:

| Method | Needs | Params | Result |
| --- | --- | --- | --- |
| `storage.get` | `storage` | `{ key: string }` | The stored value, or `null` if unset. |
| `storage.set` | `storage` | `{ key: string, value: unknown }` | `null` |
| `fs.read` | `fs.read` | `{ path: string }` (relative to your root) | The file's contents. |
| `metrics.subscribe` | `metrics.system` | `{ intervalMs?: number }` | `null` (ack) — ticks then arrive as `metrics.tick` events. |
| `metrics.unsubscribe` | `metrics.system` | `null` | `null` |

Every call is checked, in order, before anything runs: is the method real,
does your declared `apiVersion` cover it, is its capability among the ones
you were granted, are the params well-formed. A denied or malformed call
never throws or silently vanishes — you always get back
`{ ok: false, error: { code, message } }` with a stable code
(`capability-denied`, `unsupported-version`, `bad-request`, `not-found`, …),
so you can tell "I forgot to declare this capability" from "I have a bug in
my params" from "this loomux build is older than my plugin."

Unsolicited pushes (events) arrive the same way, tagged with a name:
`metrics.tick` is implemented today (paired with `metrics.subscribe`);
`resize`/`theme` are reserved on the wire for a future release but nothing
sends them yet.

### The client SDK

Use `templates/loomux-plugin/sdk/plugin-sdk.js` (copied into your plugin
folder already if you started from the template). It's a small,
dependency-free ES module — no bundler, no build step, no network fetch —
that turns the raw protocol into two calls:

```js
import { createPluginClient } from "./sdk/plugin-sdk.js";

const client = createPluginClient({ apiVersion: 1 }); // match plugin.json's apiVersion

// Request/response:
const value = await client.request("storage.get", { key: "greeting" });
await client.request("storage.set", { key: "greeting", value: "hi" });

// Unsolicited events:
const unsubscribe = client.onEvent("metrics.tick", (snapshot) => {
  // Field names are the Rust payload's own — `procmetrics::MetricsSnapshot`/
  // `ProcessSample` are plain `#[derive(Serialize)]` structs with no
  // camelCase rename, so the wire shape is snake_case throughout.
  console.log(snapshot.cpu_percent, snapshot.mem_used_bytes, snapshot.mem_total_bytes);
  for (const proc of snapshot.processes) {
    console.log(proc.pid, proc.name, proc.cpu_percent, proc.rss_bytes);
  }
});
await client.request("metrics.subscribe", { intervalMs: 2000 });
// later: unsubscribe(); await client.request("metrics.unsubscribe");
```

A denied or malformed `request()` call rejects with a `PluginBrokerError`
carrying the same `code`/`message` the broker sent — `try`/`catch` it (or
`.catch()`) rather than assuming success.

**Why a shared SDK, and why this thin:** the raw protocol is simple enough
(one Tauri command for request/response, one `Channel` for events) that a
plugin *could* call `window.__TAURI_INTERNALS__.invoke` directly, but that
means hand-rolling the `Channel` wire format (message ordering, the
`__CHANNEL__:<id>` marker) — exactly the kind of thing worth getting right
once. The SDK is that "once," reproduced faithfully against
`@tauri-apps/api`'s own implementation (see the file's own doc comment) so
it interoperates without a plugin author needing to know any of that.
Nothing about the sandbox depends on using it — it's convenience, not a
trust boundary; the broker re-checks every request's capability and
`apiVersion` regardless of what sent it.

**Why `request()` doesn't take a capability argument.** You might expect
`request(capability, method, params)`. It's just `request(method, params)`
instead — the broker's own method table already knows which capability each
method needs (`storage.get` always needs `storage`, etc.), so passing one
yourself would be redundant information a caller could get wrong. The
capability check happens host-side either way.

## The sandbox — what's not allowed, and why it can't be gotten around

Your plugin's `entry` HTML runs in its **own OS-level window**, bound to a
capability grant that permits exactly the two broker commands above and
nothing else in loomux's command surface — not "nothing you're supposed to
call," but nothing the window is *allowed* to call; a stray
`invoke("git_push", …)` from inside your plugin is rejected before any
loomux code runs, the same way a request for an ungranted capability is.

**No network, ever.** Every response your plugin's assets are served with —
including `index.html` itself — carries a Content-Security-Policy that
blocks all of it: no `fetch`/`XHR`/`WebSocket` anywhere (`connect-src
'none'`), scripts/styles/images limited to your own bundle
(`script-src`/`style-src`/`img-src 'self' plugin:`), no further
`<iframe>`/`<object>` embedding, no form submission, no `<base>`-tag
rewrite. This is why `index.html` in the template has no inline
`<script>`/`onclick` — the CSP would block it, and rightly so, since
`unsafe-inline` would be the same hole as no CSP at all.

**No navigating away.** Your window can only ever show your own plugin's
pages (`plugin://localhost/<your-id>/…`) — not another installed plugin's
address space, and not any external URL.

**Untrusted text is untrusted, always.** Your manifest's `name` field (and
anything else you author) is rendered as plain text wherever loomux shows
it (a window title, a pane label) — never interpolated as markup. Treat any
text you render inside your own plugin the same way if it came from
somewhere you don't fully control.

## Install and open a plugin

There is no remote marketplace and no auto-update in v1 — getting a plugin
onto the machine is a manual, human act:

1. Find (or build) a plugin folder containing a valid `plugin.json`.
2. Copy it into `%APPDATA%\loomux\plugins\<id>\`, where `<id>` is exactly
   the manifest's own `id` field (this is currently the one way in — an
   in-app installer is planned but not wired up yet; a folder whose name
   doesn't match its manifest `id` is skipped, not partially accepted).
3. In loomux, open a new pane, pick **Plugin**, and select it from the list.
   A folder with an invalid manifest simply doesn't show up — it isn't
   surfaced as an error in the picker.

A plugin pane persists like any other content pane. If the plugin gets
uninstalled between sessions, the pane fails soft to the welcome form with a
toast in that one slot — it doesn't crash and doesn't silently drop the rest
of your layout.

## Bundled example

loomux ships (or will ship) a first-party **resource monitor** plugin — a
`metrics.system` consumer showing per-process CPU/RAM — as a live reference
for a plugin beyond the template's hello-world. If it's present on your
build, its folder is a second worked example alongside this page and the
template.

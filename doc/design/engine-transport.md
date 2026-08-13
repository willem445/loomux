# The engine transport seam (#905)

`src/transport.ts` is the only module in `src/` that imports `@tauri-apps/*`.
Everything the frontend asks of its host goes through the `EngineTransport`
object it exports.

## Why a seam, when a convention already said the same thing

CLAUDE.md constraint 5 has always read "the frontend never touches Tauri IPC
directly — every backend capability is a `#[tauri::command]` plus a typed
wrapper". It was true as a *policy* and false as a *description*: fifteen
modules had grown their own `import { invoke } from "@tauri-apps/api/core"`,
because that is the obvious thing to write and nothing stopped it. A rule
enforced only by a reviewer noticing an import line is a rule that degrades one
convenient diff at a time.

So the rule is now a property of the tree, checked by
`test/transport.test.ts`: any `@tauri-apps` import outside `transport.ts` fails
the suite, and the failure names the file.

Two things fall out of that, and both are the actual payoff — the tidiness is
not the point.

**The frontend became mockable.** Before this, a module that reached the backend
could not be exercised outside a webview at all: its `invoke` was bound at import
to a function that throws without `window.__TAURI_INTERNALS__`. That is why every
tested module in `src/` is a *pure* module and why the IPC-touching ones have no
tests. `setEngineTransport(fake)` removes the obstacle: a `node:test` can now
hand any module a recording transport and assert the exact commands, arguments
and event payloads it produces. `test/transport.test.ts` does this to `pty.ts` as
the worked example.

**It is the cut line #888 needs.** The remote-engine proposal (§12) has the
frontend running against a server: today's `invoke`/`listen` become a network
protocol. With the seam in place that is a second `EngineTransport`
implementation, chosen at boot; without it, it is a rewrite of sixteen modules.
Nothing in this module anticipates that transport — there is no protocol here,
no versioning, no reconnect — it is only that the boundary now exists in one
file instead of being spread across the app.

## Shape

```ts
interface EngineTransport {
  invoke<T>(cmd: string, args?: EngineArgs): Promise<T>;
  listen<T>(event: string, handler: (event: EngineEvent<T>) => void): Promise<UnlistenFn>;
  hostVersion(): Promise<string>;
  pickDirectory(opts: PickDirectoryOptions): Promise<string | null>;
  onCloseRequested(handler: (event: CloseRequest) => void | Promise<void>): Promise<void>;
}
```

Two kinds of capability sit in one interface, deliberately:

- `invoke` and `listen` are the **engine** surface — request/response and the
  backend→frontend event stream. A remote transport carries these over the wire.
- `hostVersion`, `pickDirectory` and `onCloseRequested` are **display-side**. A
  folder picker and a window's close button belong to the machine with the
  screen; a remote transport would keep answering them locally.

They are not split into two interfaces because the invariant that can actually
be *enforced* is "one module imports `@tauri-apps`", and an interface with an
exception list is an interface that grows exceptions. The comment above is the
split; the module is the chokepoint. When a remote transport arrives, that is the
moment to decide whether the display half deserves its own object — with the
knowledge of what the wire protocol actually needs, rather than guessing now.

`EngineEvent<T>` is a deliberate subset of Tauri's `Event<T>` (it drops `event`
and `id`): nothing in `src/` reads either, so the narrower shape is what a remote
transport would genuinely have to produce.

## How call sites reach it

Modules import the free `invoke`/`listen`/`pickDirectory` functions, which
forward to whatever transport is installed **at call time**. Capturing the
transport at import instead would make `setEngineTransport` a silent no-op for
every module already loaded.

The free functions also keep the refactor honest: consolidating the seam changed
sixteen import lines and four folder-picker calls, not the ~170 `invoke(...)`
call sites. A refactor that churns every call site is one nobody can review, and
the risk it carries is exactly the risk a *pure* refactor is supposed to avoid.

Imports of this module carry an explicit `.ts` extension, unlike every other
intra-`src/` import. Those are all `import type`, erased before Node resolves
them; this one is a value import, and `node --test` loads `src/*.ts` off disk
rather than through Vite. Without the extension a test of any backend-touching
module dies on `ERR_MODULE_NOT_FOUND` — which would give back the testability the
seam exists to provide. `allowImportingTsExtensions` was already on.

## Interaction with the performance manifest

`test/perfpolicy.test.ts` (#743) scans `src/` for `listen(` call sites and
requires each to carry a string-literal event name, so the STREAMS manifest can
enumerate every backend stream. The seam does not blind it: a subscription still
reads `listen("event-name", cb)` in its own module, with its own literal, and is
extracted exactly as before — only the module that imports Tauri moved.

`transport.ts` itself contains three `listen(` shapes that are *declarations* of
the primitive (the interface member, the local implementation, the exported
forwarder), each taking `event: string`. That count is pinned in
`SEAM_LISTEN_DECLARATIONS` rather than exempting the file, so a fourth has to be
argued instead of inherited.

## Adding a capability

- A new backend **command** needs nothing here: add the `#[tauri::command]`, its
  ACL grant, and a typed wrapper in `pty.ts` or the feature's own bridge.
- A new **Tauri API** (a plugin, another `@tauri-apps/api/*` module) is added to
  `EngineTransport` and to the capability-set assertion in
  `test/transport.test.ts`. That second edit is the prompt to ask whether the
  capability is engine-side or display-side.

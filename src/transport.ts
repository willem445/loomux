// The ONE place the frontend speaks to the engine (#905).
//
// CLAUDE.md constraint 5 has always said "the frontend never touches Tauri IPC
// directly" — but it was a CONVENTION, enforced only by a reviewer noticing an
// `import { invoke } from "@tauri-apps/api/core"` in a diff. Fifteen modules had
// already grown one. This module makes the claim STRUCTURAL: `@tauri-apps` is
// imported here and nowhere else in `src/`, and `test/transport.test.ts` fails
// the build if that stops being true.
//
// The seam is an OBJECT, not a pile of functions, for the reason #888 needs it
// to be: an `EngineTransport` is exactly the surface a remote engine has to
// mirror over the wire, so a WebSocket implementation is a second object rather
// than a second copy of the frontend. Nothing here anticipates that transport —
// this is the local one, byte-for-byte what every call site did before — but the
// cut line is now in one file instead of spread over sixteen.
//
// The second half of the point is testability. Before this, a module that called
// `invoke` could not be exercised at all outside a webview: the import was hard
// wired to a function that throws without `window.__TAURI_INTERNALS__`. With the
// transport installable (`setEngineTransport`), any module can be handed a fake
// and its IPC traffic asserted in a `node:test`.
//
// WHY THE IMPORTS OF THIS MODULE CARRY A `.ts`. Everything else in `src/` imports
// its siblings extensionless, and gets away with it because those imports are
// `import type` — erased before Node ever resolves them. This one is a VALUE
// import, and `node --test` (which loads `src/*.ts` off disk, not through Vite)
// needs the real filename. `allowImportingTsExtensions` was already on; without
// the extension, every future test of a module that reaches the backend dies on
// ERR_MODULE_NOT_FOUND instead of running — which would undo the point.
//
// WHY THE FREE FUNCTIONS. Call sites import `invoke`/`listen` from here rather
// than reaching for the singleton themselves. They forward to whatever transport
// is installed AT CALL TIME (never a captured reference — that would make
// `setEngineTransport` silently a no-op for any module already imported), so the
// indirection is real while the 170-odd call sites stay untouched. A refactor
// that churns every call site is a refactor nobody can review.

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getVersion } from "@tauri-apps/api/app";
import { open as tauriOpen } from "@tauri-apps/plugin-dialog";
// Explicit `.ts`, like every value import a `node --test` module resolves off
// disk. Pure and Tauri-free, so importing it here keeps the one-seam rule.
import { withNativeDialog } from "./nativedialog.ts";

/** Cancels an event subscription. Structurally Tauri's `UnlistenFn`; declared
 *  here so consumers don't need the Tauri types either. */
export type UnlistenFn = () => void;

/** What a subscriber receives. A deliberate SUBSET of Tauri's `Event<T>` (which
 *  also carries `event` and `id`): no call site in `src/` reads either, so the
 *  narrower shape is what a remote transport would actually have to produce. */
export interface EngineEvent<T> {
  payload: T;
}

/** Command arguments — the object form, which is the only one this app uses.
 *  Tauri also accepts raw byte payloads; nothing here sends one. */
export type EngineArgs = Record<string, unknown>;

/** A close-request the app may veto. Structural subset of Tauri's
 *  `CloseRequestedEvent` — `preventDefault()` is the whole contract. */
export interface CloseRequest {
  preventDefault(): void;
}

export interface PickDirectoryOptions {
  title?: string;
  /** Where the picker opens. Absent means the OS decides. */
  defaultPath?: string;
}

/** Everything the frontend can ask of the process hosting it.
 *
 *  Two kinds of capability live here, and the difference matters for #888:
 *
 *   - `invoke` / `listen` are the ENGINE surface — request/response and the
 *     server→client event stream. These are what a remote transport carries.
 *   - `hostVersion` / `pickDirectory` / `onCloseRequested` are DISPLAY-side: a
 *     folder picker and a window's close button belong to the machine with the
 *     screen, and a remote transport would keep answering them locally.
 *
 *  They share one interface anyway, because the invariant this module exists to
 *  make enforceable is "one module imports `@tauri-apps`" — and an exception
 *  list is a rule that erodes. The comment above is the split; the interface is
 *  the chokepoint. */
export interface EngineTransport {
  /** Call a backend command and resolve its reply. Rejects with the backend's
   *  error, unchanged — several callers classify on the error string. */
  invoke<T>(cmd: string, args?: EngineArgs): Promise<T>;
  /** Subscribe to a backend event stream. Resolves once the subscription is
   *  registered — callers rely on that ordering to avoid a listen/emit race. */
  listen<T>(event: string, handler: (event: EngineEvent<T>) => void): Promise<UnlistenFn>;
  /** This build's version, as declared in `tauri.conf.json`/`package.json`. */
  hostVersion(): Promise<string>;
  /** Native folder picker; resolves null when the human cancelled. */
  pickDirectory(opts: PickDirectoryOptions): Promise<string | null>;
  /** Register the app's close gate. See `guardAppClose` in `pty.ts` for the
   *  Tauri contract this inherits (registering one makes JS own the close). */
  onCloseRequested(handler: (event: CloseRequest) => void | Promise<void>): Promise<void>;
}

/** The local transport: today's behavior, and the default. Every method is a
 *  one-line forward — deliberately, so "did the refactor change anything?" is
 *  answerable by reading this object. */
export const tauriTransport: EngineTransport = {
  invoke: <T,>(cmd: string, args?: EngineArgs): Promise<T> => tauriInvoke<T>(cmd, args),
  listen: <T,>(event: string, handler: (event: EngineEvent<T>) => void): Promise<UnlistenFn> =>
    tauriListen<T>(event, handler),
  hostVersion: (): Promise<string> => getVersion(),
  pickDirectory: async (opts: PickDirectoryOptions): Promise<string | null> => {
    // `directory: true` without `multiple` can only yield a single path or null;
    // the `typeof` narrowing is what every call site did inline before, kept
    // here so the seam's own type is honest rather than `string | string[]`.
    const picked = await tauriOpen({ directory: true, ...opts });
    return typeof picked === "string" ? picked : null;
  },
  onCloseRequested: async (
    handler: (event: CloseRequest) => void | Promise<void>
  ): Promise<void> => {
    await getCurrentWindow().onCloseRequested(handler);
  },
};

let active: EngineTransport = tauriTransport;

/** Swap the transport. Returns the one that was installed, so a test can put it
 *  back — and so #888's remote client can fall back to local.
 *
 *  Call it before the module under test issues any IPC. Modules that memoize a
 *  subscription (`ensureOutputRouter`) capture the transport that was live at
 *  first call, which is the correct behavior for a swap that happens at boot and
 *  the reason a test swaps first, too. */
export function setEngineTransport(transport: EngineTransport): EngineTransport {
  const previous = active;
  active = transport;
  return previous;
}

/** The transport in force right now. */
export function engineTransport(): EngineTransport {
  return active;
}

/** Per-command dispatch counts, or `null` while the counter is disarmed.
 *  Disarmed is the shipped state and the only state anything reaches
 *  without an explicit call — see `invokeStats`. */
let invokeCounts: Record<string, number> | null = null;

export interface InvokeStats {
  /** Whether counting is on. False in every build nobody has armed. */
  armed: boolean;
  /** Dispatches since arming, by command name. A snapshot, not the live map. */
  counts: Record<string, number>;
}

/** Dispatch accounting for the app's ONE IPC seam, reachable from devtools
 *  (and from an E2E spec over CDP) as `__invokeStats()` — the same shape of
 *  hand-validation instrument `pollgate.ts` exposes as `__pollGateStats()`.
 *
 *  **Why it is here and not in the test.** The E2E soak lane (#1603) has to
 *  be able to say "the poll paths really ran" before it reads anything into a
 *  liveness result — without that, the test asserts only that an app survives
 *  being ignored. Counting from the spec's side meant patching
 *  `window.__TAURI_INTERNALS__.invoke`, which is a frozen own property
 *  (`writable:false, configurable:false`, measured on the E2E build) that a
 *  Proxy is forbidden by the language from intercepting. Fighting that
 *  descriptor would also have been a bet on Tauri's internals staying
 *  shaped as they are. This module is the answer the codebase already had:
 *  CLAUDE.md constraint 5 makes it the only module that touches Tauri IPC,
 *  so a counter here sees every dispatch every bridge makes, by
 *  construction.
 *
 *  **Why it cannot ship enabled.** It is off until someone calls
 *  `__invokeStats("arm")`. Disarmed, `invoke` pays one null check; armed, it
 *  increments an integer in a map keyed by COMMAND NAME, so the retained set
 *  is bounded by the command surface (~160) and not by session length — INV-8
 *  by construction rather than by a prune. There is no IPC surface, no ACL
 *  grant and no new command: this is a local counter behind a read function.
 */
export function invokeStats(arm?: "arm"): InvokeStats {
  if (arm === "arm") invokeCounts ??= {};
  return { armed: invokeCounts !== null, counts: { ...(invokeCounts ?? {}) } };
}

/** Test-only: forget both the counts and the armed state. Not exposed on
 *  `globalThis` — the unit tests need it so one test's arming cannot decide
 *  another's result, and nothing else has a reason to. */
export function resetInvokeStatsForTest(): void {
  invokeCounts = null;
}

(globalThis as unknown as { __invokeStats?: typeof invokeStats }).__invokeStats = invokeStats;

/** Call a backend command. Resolved against the live transport on every call. */
export function invoke<T>(cmd: string, args?: EngineArgs): Promise<T> {
  if (invokeCounts !== null) invokeCounts[cmd] = (invokeCounts[cmd] ?? 0) + 1;
  return active.invoke<T>(cmd, args);
}

/** Subscribe to a backend event stream. */
export function listen<T>(
  event: string,
  handler: (event: EngineEvent<T>) => void
): Promise<UnlistenFn> {
  return active.listen<T>(event, handler);
}

/** This build's version. Throws if the host can't answer — `appVersion()` in
 *  `pty.ts` is the swallowing variant most callers want. */
export function hostVersion(): Promise<string> {
  return active.hostVersion();
}

/** Native folder picker; null when cancelled — and null, too, when one is
 *  already outstanding: at most ONE native dialog may exist at a time (#1564).
 *  The gate lives on this free function rather than inside `tauriTransport` so
 *  it holds for every transport, a test fake and a future remote one included —
 *  the dialog is display-side, and the machine with the screen has one focus to
 *  lose however the request reached it. See src/nativedialog.ts for the crash
 *  this refuses to keep feeding. */
export function pickDirectory(opts: PickDirectoryOptions = {}): Promise<string | null> {
  return withNativeDialog(() => active.pickDirectory(opts));
}

/** Register the app's close gate. */
export function onCloseRequested(
  handler: (event: CloseRequest) => void | Promise<void>
): Promise<void> {
  return active.onCloseRequested(handler);
}

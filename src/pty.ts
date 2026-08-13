// Thin bridge to the Rust backend: PTY lifecycle + session discovery.
//
// Output arrives on one shared "pty-output" event, demultiplexed here by
// pty id. Payloads that arrive before a pane attaches its handler are
// buffered and flushed on attach, so startup output (shell banners,
// prompts) can never be lost to a listen/spawn race.

import {
  invoke,
  listen,
  hostVersion,
  onCloseRequested,
  type UnlistenFn,
} from "./transport.ts";
import type { ShellKind } from "./panesetup";
import type { CliKnobs } from "./selectorknobs";

export interface SpawnOptions {
  cols: number;
  rows: number;
  /** Working directory; defaults to the user's home dir. */
  cwd?: string;
  /** Command line run through the default shell; omit for a plain shell. */
  command?: string;
  /** Which interactive shell a plain Terminal pane spawns (#194 P2). Ignored
   *  when `command`/`argv` is present (agent panes run through the default
   *  shell). Unknown/absent falls back to PowerShell backend-side. */
  shellKind?: ShellKind;
  /** Structured agent invocation (program + args). When present and its program
   *  resolves to a native executable, the backend spawns it directly as the pty
   *  child — no wrapper shell (issue #78) — falling back to `command` otherwise. */
  argv?: string[];
  /** Extra environment for the pane's child, set on top of the shared pane env
   *  (#83). Agent panes carry the gh-shim PATH + `LOOMUX_GROUP_DIR` here so the
   *  merge gate is enforced; a plain shell omits it and is unchanged. Wire form is
   *  the backend's `Vec<(String, String)>` — a list of `[key, value]` pairs. */
  env?: [string, string][];
}

export interface SessionInfo {
  id: string;
  /** Which CLI's store this row came out of. Mirrors `SessionInfo.source` in
   *  `src-tauri/src/sessions.rs` — a plain string over IPC, so nothing checks
   *  the two sets against each other and a scanner added there without a
   *  widening here is silently mis-handled (#722: an opencode row read as
   *  copilot's) rather than rejected. */
  source: "claude" | "copilot" | "opencode";
  title: string;
  cwd: string;
  modified_ms: number;
  resume_command: string;
  /** Orchestration identity detected from the transcript's loomux
   *  signatures — fallback for sessions predating the durable roster. */
  orch_role?: string | null;
  orch_group?: string | null;
}

export interface PtyExit {
  id: number;
  exit_code: number | null;
  /** True when loomux killed the process itself (pane close, kill_agent). */
  expected: boolean;
}

export interface DirInfo {
  /** Directory, home-abbreviated to `~` for display. */
  cwd: string;
  /** Git branch (or short hash when detached); null when not in a repo. */
  branch: string | null;
}

export const spawnPty = (opts: SpawnOptions): Promise<number> =>
  invoke<number>("spawn_pty", { ...opts });

/** Write into a pane's PTY.
 *
 *  `human` (#518) says whether this data ORIGINATED in a genuine keyboard or
 *  paste event rather than being manufactured by the terminal itself (a query
 *  auto-reply — see `humanorigin.ts`). The backend gates its keystroke-recency
 *  clock on it, so a copilot pane's OSC/DCS chatter can no longer read as a
 *  human typing. Omitted means `true`: an unstated origin behaves exactly as
 *  it did before #518, which is the fail-safe direction. */
export const writePty = (id: number, data: string, human?: boolean): Promise<void> =>
  invoke("write_pty", { id, data, human });

export const resizePty = (id: number, cols: number, rows: number): Promise<void> =>
  invoke("resize_pty", { id, cols, rows });

export const killPty = (id: number): Promise<void> => invoke("kill_pty", { id });

export interface PtyBackendInfo {
  /** True when a modern conpty.dll is sideloaded next to the executable. */
  sideloaded_conpty: boolean;
  /** Effective conhost build for xterm's windowsPty option; 0 off Windows. */
  conpty_build: number;
}

/** Which ConPTY the backend binds to (cached — it can't change at runtime). */
export function ptyBackendInfo(): Promise<PtyBackendInfo> {
  backendInfo ??= invoke<PtyBackendInfo>("pty_backend_info");
  return backendInfo;
}
let backendInfo: Promise<PtyBackendInfo> | null = null;

/** Resolve display name + git branch for a directory the shell reported. */
export const dirInfo = (path: string): Promise<DirInfo> => invoke("dir_info", { path });

/** Discover the Git Bash `bash.exe` path, or null when Git for Windows isn't
 *  installed (#194 P2). The welcome screen uses this to enable/disable the Git
 *  Bash shell kind with a reason before any pane is spawned. */
export const discoverGitBash = (): Promise<string | null> =>
  invoke<string | null>("discover_git_bash");

/** The model knobs loomux can actually set on an agent CLI (#687) — the CLI's
 *  `CLI_CAPS` row, reported verbatim by the backend so the launcher, the workflow
 *  parser and the spawn path can never disagree about what a CLI supports.
 *
 *  This is slice A's deferred caller: the command was registered there and its
 *  wrapper left to the surface that uses it (the launcher's per-role selector and
 *  the workflow pane's block form), rather than shipping an unused one.
 *
 *  Never throws: a capability lookup we couldn't make is not worth failing a form
 *  over. A `null` reply reads as "not known yet" everywhere it lands, which
 *  renders every knob disabled with a reason — the honest, and safe, direction. */
export const agentCliKnobs = (cli: string): Promise<CliKnobs | null> =>
  invoke<CliKnobs>("agent_cli_knobs", { cli }).catch(() => null);

/** Drive a pane's shell to `cd` into `path`. */
export const changeDir = (id: number, path: string): Promise<void> =>
  invoke("change_dir", { id, path });

export const listSessions = (): Promise<SessionInfo[]> => invoke("list_sessions");

/** Record what a solo copilot launch's Autopilot toggle was set to for `cwd`
 *  (#456) — the backend's `scan_copilot` reads this back so a Sessions-tab
 *  resume of a session from that folder can rebuild `--autopilot
 *  --allow-all-tools --allow-all-paths` instead of a bare `--resume`, which
 *  otherwise silently drops the session out of autopilot mode. Called for
 *  BOTH toggle states — see `src-tauri/src/sessions.rs`'s module doc for the
 *  ambiguity rule this feeds (disagreement always resolves to no flags,
 *  never the most recent one). Best-effort; never blocks or fails a launch. */
export const recordCopilotLaunchPosture = (cwd: string, autopilot: boolean): Promise<void> =>
  invoke("record_copilot_launch_posture", { cwd, autopilot });

/** Claude's counterpart to `recordCopilotLaunchPosture` (#457) — keyed by the
 *  session id THIS launch minted rather than a cwd (`sessions.rs`'s
 *  `IntentKey::Session`), since claude always has one. The backend's
 *  `scan_claude` reads this back so a Sessions-tab resume of that exact
 *  session can rebuild its autopilot flags instead of the pre-#457
 *  unconditional bare `--resume`. Best-effort; never blocks or fails a
 *  launch. */
export const recordClaudeLaunchPosture = (sessionId: string, autopilot: boolean): Promise<void> =>
  invoke("record_claude_launch_posture", { sessionId, autopilot });

// ---------- durable UI state: project tabs (#63) ----------
// The tab set persists across launches through the backend (atomic temp+rename
// write, corrupt-file quarantine — see src-tauri/src/uistate.rs), NOT
// localStorage, so it survives a webview data clear and lives alongside the
// app's other durable state. The blob is opaque JSON here; tabstore.ts owns its
// schema (encode/decode + validation). These are the typed IPC wrappers every
// frontend module goes through (CLAUDE.md constraint 5).

/** Load the persisted tab-set JSON, or null on first run / after a corrupt file
 *  was quarantined backend-side (the caller then seeds one default tab). */
export const loadUiTabs = (): Promise<string | null> => invoke<string | null>("load_ui_tabs");

/** Persist the tab-set JSON atomically. Best-effort — callers never block on it. */
export const saveUiTabs = (contents: string): Promise<void> =>
  invoke("save_ui_tabs", { contents });

/** Load the persisted app-settings JSON (#370), or null on first run / after a
 *  corrupt file was quarantined backend-side (the caller then seeds defaults). */
export const loadSettings = (): Promise<string | null> => invoke<string | null>("load_settings");

/** Persist app settings atomically. Best-effort — callers never block on it. */
export const saveSettings = (contents: string): Promise<void> =>
  invoke("save_settings", { contents });

/** Load the persisted SSH connection profiles (#887), or null on first run /
 *  after a corrupt file was quarantined backend-side (the caller then seeds an
 *  empty profile list). Opaque JSON here too — `sshprofile.ts` owns the schema,
 *  and the invariant that it never contains a credential. */
export const loadSshProfiles = (): Promise<string | null> =>
  invoke<string | null>("load_ssh_profiles");

/** Persist SSH connection profiles atomically. Same best-effort contract as
 *  `saveUiTabs`. */
export const saveSshProfiles = (contents: string): Promise<void> =>
  invoke("save_ssh_profiles", { contents });

// ---------- window lifecycle (#219) ----------

/** This build's version, as declared in `tauri.conf.json` / `package.json`.
 *
 *  A wrapper rather than a raw `hostVersion()` call at each site, for the reason the rest
 *  of this module exists: callers get the SWALLOWING contract below, not just the seam. The
 *  workflow pane (#222) stamps it into a workflow file it CREATES (`authored_with:`), so a
 *  file that later misbehaves can say which build wrote it.
 *
 *  Never throws: a version we couldn't read is not worth failing a feature over, and the
 *  callers all treat "" as "don't write the key", which is the honest outcome — an absent
 *  key beats an `authored_with: unknown`. */
export async function appVersion(): Promise<string> {
  try {
    return await hostVersion();
  } catch {
    return "";
  }
}

/** Gate the app's own close (title-bar ✕, Alt+F4, the OS asking it to quit).
 *
 *  `allow()` runs when the window is asked to close and decides whether it may: true
 *  quits, false keeps the app running (the human cancelled). It is `await`ed, so it may
 *  put a dialog on screen — which is the whole point, because until now quitting loomux
 *  with unsaved editor edits threw them away without a word (#219).
 *
 *  Tauri's own `onCloseRequested` contract, and the two things it costs:
 *
 *   - Registering ANY js listener for close-requested makes the Rust side stop closing
 *     the window itself; the JS layer destroys it once our handler resolves without a
 *     `preventDefault()`. So this hook is now the only way loomux exits...
 *   - ...which is why `core:window:allow-destroy` is in the capability set. Without it
 *     that destroy is denied and the app becomes UNQUITTABLE — the failure mode is not
 *     "the guard doesn't run", it's "the ✕ does nothing", so it is called out here.
 *
 *  Destroying is what fires the backend's `WindowEvent::Destroyed` — the PTY kill-all and
 *  the clean-exit sentinel (lib.rs) — so a permitted quit still tears everything down
 *  exactly as before. We put a question in front of the existing path, not a second path.
 *
 *  Lives HERE, as a named capability, so the app's modules keep talking to typed wrappers
 *  rather than to window-lifecycle plumbing (CLAUDE.md constraint 5). The Tauri call
 *  itself is `transport.ts`'s. */
export async function guardAppClose(allow: () => Promise<boolean>): Promise<void> {
  await onCloseRequested(async (event) => {
    try {
      if (!(await allow())) event.preventDefault();
    } catch {
      // FAIL OPEN. A guard that throws must let the close through: trapping the human in
      // an app whose ✕ does nothing is a far worse failure than not asking them about a
      // buffer. (The one thing we lose is the awaited final save — and the fire-and-forget
      // persist on every change means the layout is already durable to within one edit.)
    }
  });
}

// ---------- voice prompt (#58 prototype) ----------
// Push-to-talk mic capture → local whisper.cpp transcription. The backend owns
// the microphone (native WASAPI) so there's no WebView2 getUserMedia permission
// to negotiate; these just start/stop the capture and hand back the transcript.

/** Begin capturing from the default input device. Rejects if there's no mic or
 *  a recording is already in flight. */
export const voiceStart = (): Promise<void> => invoke("voice_start");

/** Stop capturing and transcribe locally; resolves to the recognized text (""
 *  for silence). Rejects if whisper.cpp / the model aren't installed. */
export const voiceStop = (): Promise<string> => invoke("voice_stop");

/** Abort an in-flight recording without transcribing. Idempotent. */
export const voiceCancel = (): Promise<void> => invoke("voice_cancel");

// ---------- output router ----------

type OutputHandler = (data: Uint8Array) => void;

const handlers = new Map<number, OutputHandler>();
const pending = new Map<number, Uint8Array[]>();
let routerReady: Promise<void> | null = null;

function decodeB64(b64: string): Uint8Array {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

/** Idempotent; resolves once the shared output listener is registered.
 *  Must complete before the first spawn to guarantee lossless startup. */
export function ensureOutputRouter(): Promise<void> {
  routerReady ??= listen<{ id: number; data: string }>("pty-output", (event) => {
    const bytes = decodeB64(event.payload.data);
    const handler = handlers.get(event.payload.id);
    if (handler) {
      handler(bytes);
    } else {
      const queue = pending.get(event.payload.id);
      if (queue) queue.push(bytes);
      else pending.set(event.payload.id, [bytes]);
    }
  }).then(() => undefined);
  return routerReady;
}

/** Attach a pane's output handler, flushing anything buffered for it. */
export function attachOutput(id: number, handler: OutputHandler): void {
  handlers.set(id, handler);
  const queued = pending.get(id);
  if (queued) {
    pending.delete(id);
    for (const bytes of queued) handler(bytes);
  }
}

export function detachOutput(id: number): void {
  handlers.delete(id);
  pending.delete(id);
}

export const onPtyExit = (handler: (exit: PtyExit) => void): Promise<UnlistenFn> =>
  listen<PtyExit>("pty-exit", (event) => handler(event.payload));

// ---------- git external-change watcher (#36) ----------
//
// The backend polls the `.git` metadata of every repo with an open pane and
// emits "git-changed" with the pane's pty id when HEAD/index/refs move — i.e.
// a checkout/commit/stage run outside the pane's shell (VS Code, another
// terminal). One shared listener demultiplexes to per-pane handlers, mirroring
// the output router above.

type GitChangeHandler = () => void;

const gitChangeHandlers = new Map<number, GitChangeHandler>();
let gitWatchRouter: Promise<void> | null = null;

function ensureGitWatchRouter(): Promise<void> {
  gitWatchRouter ??= listen<{ id: number }>("git-changed", (event) => {
    gitChangeHandlers.get(event.payload.id)?.();
  }).then(() => undefined);
  return gitWatchRouter;
}

/** Register `handler` to fire when pane `id`'s repository changes on disk. Call
 *  once per pane; use setGitWatch to (re)point it at the current cwd. */
export function attachGitWatch(id: number, handler: GitChangeHandler): void {
  gitChangeHandlers.set(id, handler);
  void ensureGitWatchRouter();
}

/** Start (or repoint) pane `id`'s backend watch at the repo containing `cwd`.
 *  Idempotent and cheap; the backend dedupes same-repo calls, so it's safe to
 *  call on every prompt. A cwd outside any repo drops the watch. */
export function setGitWatch(id: number, cwd: string): void {
  invoke("git_watch", { id, cwd }).catch(() => {});
}

/** Stop watching pane `id` and drop its handler (called on pane dispose). */
export function detachGitWatch(id: number): void {
  gitChangeHandlers.delete(id);
  invoke("git_unwatch", { id }).catch(() => {});
}

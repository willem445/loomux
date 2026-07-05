// Thin bridge to the Rust backend: PTY lifecycle + session discovery.
//
// Output arrives on one shared "pty-output" event, demultiplexed here by
// pty id. Payloads that arrive before a pane attaches its handler are
// buffered and flushed on attach, so startup output (shell banners,
// prompts) can never be lost to a listen/spawn race.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface SpawnOptions {
  cols: number;
  rows: number;
  /** Working directory; defaults to the user's home dir. */
  cwd?: string;
  /** Command line run through the default shell; omit for a plain shell. */
  command?: string;
}

export interface SessionInfo {
  id: string;
  source: "claude" | "copilot";
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

export const writePty = (id: number, data: string): Promise<void> =>
  invoke("write_pty", { id, data });

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

/** Drive a pane's shell to `cd` into `path`. */
export const changeDir = (id: number, path: string): Promise<void> =>
  invoke("change_dir", { id, path });

export const listSessions = (): Promise<SessionInfo[]> => invoke("list_sessions");

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

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
}

export interface PtyExit {
  id: number;
  exit_code: number | null;
}

export const spawnPty = (opts: SpawnOptions): Promise<number> =>
  invoke<number>("spawn_pty", { ...opts });

export const writePty = (id: number, data: string): Promise<void> =>
  invoke("write_pty", { id, data });

export const resizePty = (id: number, cols: number, rows: number): Promise<void> =>
  invoke("resize_pty", { id, cols, rows });

export const killPty = (id: number): Promise<void> => invoke("kill_pty", { id });

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

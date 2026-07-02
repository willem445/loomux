// Thin bridge to the Rust backend: PTY lifecycle + session discovery.

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

/** Subscribe to raw output bytes for one PTY (base64 over the event bus). */
export function onPtyOutput(
  id: number,
  handler: (data: Uint8Array) => void
): Promise<UnlistenFn> {
  return listen<string>(`pty-output-${id}`, (event) => {
    const bin = atob(event.payload);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    handler(bytes);
  });
}

export const onPtyExit = (handler: (exit: PtyExit) => void): Promise<UnlistenFn> =>
  listen<PtyExit>("pty-exit", (event) => handler(event.payload));

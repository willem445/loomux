// Bridge for the live system-resource stream. The Rust sampler emits a
// snapshot on the "system-metrics" event every couple of seconds; the status
// bar just subscribes here.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface GpuMetrics {
  /** Adapter name (tooltip). */
  name: string;
  /** Core utilization, 0–100. */
  util: number;
  /** Dedicated VRAM in use, mebibytes. */
  vram_used_mb: number;
  /** Total dedicated VRAM, mebibytes. */
  vram_total_mb: number;
}

export interface SystemMetrics {
  /** Overall CPU utilization, 0–100. */
  cpu: number;
  /** Memory in use, bytes. */
  mem_used: number;
  /** Total physical memory, bytes. */
  mem_total: number;
  /** GPU stats, or null when no NVIDIA GPU is available. */
  gpu: GpuMetrics | null;
}

export const onSystemMetrics = (
  handler: (m: SystemMetrics) => void
): Promise<UnlistenFn> =>
  listen<SystemMetrics>("system-metrics", (e) => handler(e.payload));

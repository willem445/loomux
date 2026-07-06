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

/** Per-CLI agent usage-limit consumption, emitted on each orchestration
 *  attention scan (see issue #80). Reported by the CLI's own statusline, not
 *  estimated by loomux. */
export interface ClaudeUsageLimits {
  /** Rolling ~5-hour session allowance consumed, 0–100, or null if the live
   *  panes' statuslines don't show it. */
  session_pct: number | null;
  /** Weekly allowance consumed, 0–100, or null. */
  weekly_pct: number | null;
  /** Provenance of the figure (currently always "statusline"). */
  source: string;
}

export interface UsageLimits {
  /** Aggregated across live Claude panes (most-constrained), or null when no
   *  pane exposed a limit readout. */
  claude: ClaudeUsageLimits | null;
  /** Always null: Copilot exposes no local premium-request allowance, so
   *  loomux shows nothing rather than a count with no ceiling. */
  copilot: null;
  /** Human-readable provenance/freshness note (tooltip source). */
  note: string;
}

export const onUsageLimits = (
  handler: (u: UsageLimits) => void
): Promise<UnlistenFn> =>
  listen<UsageLimits>("orch-usage-limits", (e) => handler(e.payload));

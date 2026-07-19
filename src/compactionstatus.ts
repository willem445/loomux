// Compact-nudge lifecycle-panel surfacing (PR #329 round 6) — DOM-free
// derivations over `CompactionStatus`/context usage (orchestration.ts) for
// the group lifecycle panel. Never invents a parallel vocabulary: every
// label here narrates a real backend state-machine phase (see
// `orchestration/mod.rs`'s `compaction_status`) or the cached context-token
// reading, nothing else.

import type { CompactionStatus } from "./orchestration";

/** One line naming the agent's current compact-nudge phase, or `null` when
 *  there's nothing worth a human's attention (`"none"` — no arm, no
 *  in-flight reinjection, no recent lost outcome) so a caller can omit the
 *  row entirely rather than render an empty/idle line every tick. */
export function compactionStatusLabel(status: CompactionStatus): string | null {
  switch (status.status) {
    case "none":
      return null;
    case "armed":
      return `compact ${status.trusted ? "armed" : "armed (unconfirmed)"}`;
    case "awaiting_evidence":
      return `compact awaiting evidence${status.trusted ? "" : " (unconfirmed)"}`;
    case "reinjecting":
      return `re-grounding (attempt ${status.attempt}/${status.max_attempts})`;
    case "abandoned":
      return `compact ${lostReasonLabel(status.reason)}`;
  }
}

/** Longer explanation for the status line's tooltip — the mechanism behind
 *  the short label, not a restatement of it. */
export function compactionStatusTitle(status: CompactionStatus): string | null {
  switch (status.status) {
    case "none":
      return null;
    case "armed":
      return status.trusted
        ? "loomux pasted /compact itself — waiting to observe the pane go busy"
        : "loomux believes a compact started (banner or manual typing) — waiting to observe the pane go busy";
    case "awaiting_evidence":
      return status.trusted
        ? "busy observed — waiting for quiet to resolve"
        : "busy observed — waiting for quiet, then a confirmed token drop or compact_boundary marker before trusting it";
    case "reinjecting":
      return "a reinjection was decided and is waiting on its delivery to confirm, or its next bounded retry";
    case "abandoned":
      return lostReasonTitle(status.reason);
  }
}

function lostReasonLabel(reason: string): string {
  switch (reason) {
    case "arm-timeout":
      return "timed out (no evidence)";
    case "reinjection-abandoned":
      return "re-grounding lost";
    default:
      return reason;
  }
}

function lostReasonTitle(reason: string): string {
  switch (reason) {
    case "arm-timeout":
      return "an arm never reached a busy-then-quiet resolution within the bound — released so a new compaction can arm";
    case "reinjection-abandoned":
      return "a decided reinjection's delivery never confirmed despite retries — released so a new compaction can arm";
    default:
      return reason;
  }
}

/** "ctx 23% (46,120 tok)", or `null` before the first reading (no session
 *  yet, or a non-Claude agent) — a caller omits the badge entirely rather
 *  than render a placeholder. */
export function contextUsageLabel(context: { tokens: number | null; percent: number | null }): string | null {
  if (context.percent == null || context.tokens == null) return null;
  return `ctx ${context.percent}% (${context.tokens.toLocaleString()} tok)`;
}

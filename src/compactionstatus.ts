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
      return `compact ${armedQualifier(status)}`;
    case "awaiting_evidence":
      // Round 10 (#428 follow-up, user-directed): a hook-sourced arm sets
      // `compact_seen_busy` immediately at arm time (see the backend's
      // `compaction_status` doc), so it lands HERE, not "armed", on the
      // very first tick — this is the phase a live user re-test sat in
      // for up to the evidence-poll cadence, and "awaiting evidence"
      // reads as stuck even though the outcome is already decided: a
      // hook told loomux directly that compaction happened, resolution
      // is inevitable, it's just waiting on the poll that consumes the
      // marker. The non-hook cases below are genuinely still waiting on
      // an outcome (busy-then-quiet hasn't resolved either way yet), so
      // their wording is unchanged.
      if (status.source === "hook") return "compact confirmed — finalizing";
      return `compact awaiting evidence${status.trusted ? "" : " (unconfirmed)"}`;
    case "reinjecting":
      return `re-grounding (attempt ${status.attempt}/${status.max_attempts})`;
    case "abandoned":
      return `compact ${lostReasonLabel(status.reason)}`;
    case "acked":
      // #546 (option 3): the evidence source is IN the label, not only in the
      // tooltip. "acked (delivery)" and "acked (activity)" are different
      // strengths of claim — the second proves the agent is alive, not that it
      // read the re-grounding — and a reader skimming the panel must be able to
      // tell them apart without hovering. Same shape as `armedQualifier`'s
      // "(hook-confirmed)" (#417).
      return `re-grounding acked (${status.source})`;
  }
}

/** Longer explanation for the status line's tooltip — the mechanism behind
 *  the short label, not a restatement of it. */
export function compactionStatusTitle(status: CompactionStatus): string | null {
  switch (status.status) {
    case "none":
      return null;
    case "armed":
      if (status.source === "hook") return "a PreCompact/SessionStart hook confirmed this directly — waiting to observe the pane go busy";
      return status.trusted
        ? "loomux pasted /compact itself — waiting to observe the pane go busy"
        : "loomux believes a compact started (banner or manual typing) — waiting to observe the pane go busy";
    case "awaiting_evidence":
      // Round 10: a hook already confirmed the compaction directly — there
      // is no inference left to run and no outcome left undecided, only
      // loomux's own poll left to consume the marker and hand off the
      // (already-decided) re-grounding.
      if (status.source === "hook") return "a hook confirmed this compaction directly — wrapping up the re-grounding handoff now";
      return status.trusted
        ? "busy observed — waiting for quiet to resolve"
        : "busy observed — waiting for quiet, then a confirmed token drop or compact_boundary marker before trusting it";
    case "reinjecting":
      return "a reinjection was decided and is waiting on its delivery to confirm, or its next bounded retry";
    case "abandoned":
      return lostReasonTitle(status.reason);
    case "acked":
      // #546: the tooltip states the residual the label can only hint at. The
      // "activity" wording deliberately says what the signal does NOT prove —
      // an honest system that cannot demonstrate the thing it wants to
      // demonstrate says so rather than letting the reader assume it did.
      return status.source === "delivery"
        ? "loomux's own submit sampler watched the re-grounding's Enter land — the paste reached the box"
        : "the agent called a loomux tool after the re-grounding was sent — that proves it is alive and executing, NOT that it read the re-grounding";
  }
}

/** "hook-confirmed" beats "armed"/"armed (unconfirmed)" (#417) — a human
 *  watching the panel should be able to tell a hook-confirmed compaction
 *  from an inferred/loomux-initiated one at a glance. */
function armedQualifier(status: { trusted: boolean; source: string | null }): string {
  if (status.source === "hook") return "armed (hook-confirmed)";
  return status.trusted ? "armed" : "armed (unconfirmed)";
}

function lostReasonLabel(reason: string): string {
  switch (reason) {
    case "arm-timeout":
      return "timed out (no evidence)";
    case "arm-timeout-with-evidence":
      // Round 7: a PreCompact-only hook arm (no SessionStart wired) can
      // still legitimately time out if the agent's own turn never settles
      // — but hook evidence WAS seen, so "no evidence" would be wrong. A
      // SessionStart-evidenced arm resolves immediately now and never
      // reaches this label at all — see compact_nudge_tick's own doc.
      return "timed out after hook evidence — resolution never observed";
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
    case "arm-timeout-with-evidence":
      return "a hook confirmed this compaction directly, but the pane never settled within the bound to resolve it — released so a new compaction can arm";
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

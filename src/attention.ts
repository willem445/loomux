// Pure presentation mapping for attention-routing (#6): a backend attention
// reason → the label and urgency used to badge a pane. Kept DOM-free so both
// the pane header chip and the minimize-dock chip render it identically, and
// so the mapping is unit-testable.

/** Reasons the backend attention scan emits (see the Rust `AttentionItem`). */
export type AttentionReason =
  | "blocked"
  | "stranded"
  | "waiting"
  | "report"
  | "question"
  | "gate";

export interface AttentionPresentation {
  /** Short glyph+word label shown in the header chip / dock chip tooltip. */
  label: string;
  /** `blocked` and `stranded` are the urgent ones — callers tint them red
   *  rather than amber. */
  urgent: boolean;
}

const LABELS: Record<string, string> = {
  blocked: "⚠ blocked",
  // #496 PR-C: a delivered prompt that was never submitted. Distinct from
  // `waiting` on purpose — a waiting pane is asking something and will keep
  // asking; a stranded one is wedged and stays wedged until an Enter lands.
  stranded: "⚠ stuck prompt",
  waiting: "⚠ waiting",
  report: "✓ reported",
  // #1091 slice D: a pending `ask_human` row this pane (orchestrator-only
  // today) is waiting on. Amber like `gate`, not urgent — it's a decision on
  // the human's own pace, not a wedged pane.
  question: "❓ question",
  gate: "⚑ your call",
};

/** Attention reasons rendered as urgent (red, not amber): the pane is stuck
 *  and will not un-stick itself. Kept as a set so adding a reason is one edit
 *  — `tabroute.ts` mirrors this rule (see its note on why it can't import). */
const URGENT: ReadonlySet<string> = new Set(["blocked", "stranded"]);

/** Whether a fresh `(reason, detail)` reading differs from the one currently
 *  applied to a pane — the identity check `Pane.setAttention` gates its DOM
 *  work on (#1091 slice D review). `detail` participates deliberately:
 *  several reasons carry a live count/status in `detail` while the reason
 *  string itself stays put — `question`'s "N pending question(s)" as N
 *  grows, `gate`'s "task is {status}" as the task moves between gate
 *  statuses without leaving the gate set. A reason-only check would freeze
 *  the tooltip at whatever text first raised the chip; this is why the
 *  earlier version of this check (`reason === this.attentionReason` alone)
 *  was a real defect, not merely an optimization choice. */
export function attentionChanged(
  prevReason: string | null,
  prevDetail: string | null,
  nextReason: string | null,
  nextDetail: string | null,
): boolean {
  return nextReason !== prevReason || nextDetail !== prevDetail;
}

/** Map an attention reason to its label + urgency. Unknown reasons fall back
 *  to a generic non-urgent badge rather than throwing, so a new backend reason
 *  never blanks the UI. */
export function attentionPresentation(reason: string): AttentionPresentation {
  return {
    label: LABELS[reason] ?? "⚠ attention",
    urgent: URGENT.has(reason),
  };
}

/** Whether a pane's attention chip carries its own dismiss control (#825 M1),
 *  and what that control shows. */
export interface AttentionDismiss {
  /** Render the dismiss control at all? */
  dismissible: boolean;
  /** Glyph on the control. Empty when there is none. */
  label: string;
  /** Tooltip on the control. Empty when there is none. */
  title: string;
}

/** The tooltip on the dismiss control. It is deliberately two claims wide: what
 *  the gesture settles (the chip) and what it leaves exactly as it was (the
 *  pane). A human who is allowed to take a warning down on their own say-so is
 *  owed both, or "dismiss" reads as "resolve" and someone walks away from a
 *  genuinely wedged pane. */
const DISMISS_TITLE =
  "Dismiss this alert — it takes the chip down only, it does not unstick the pane";

/** Whether the chip for `reason` on a pane identified by `agentId` offers an
 *  explicit dismiss (#825 M1), and what to draw for it.
 *
 *  **Only `stranded`, and that is the whole rule.** It is the one LATCHED
 *  reason: raised backend-side into `attn_stranded` and held until something
 *  removes it, which for several blocker classes is nothing at all — so an idle
 *  pane can wear a stuck-prompt chip indefinitely. The others are re-derived by
 *  every 3-second attention scan (`waiting`, `gate`) or already released by the
 *  focus ack (`report`, `blocked`), so a dismiss control on them would be a
 *  button whose effect visibly evaporates on the next tick — which teaches the
 *  human that dismissing does not work, the very complaint this exists to fix.
 *
 *  **No agent id, no control.** The backend releases the badge by agent id
 *  (`orch_dismiss_stranded`), so a plain (non-orchestration) pane has nothing
 *  to send; offering the control there would be a click that silently fails. */
export function attentionDismiss(
  reason: string | null,
  agentId: string | null,
): AttentionDismiss {
  if (reason !== "stranded" || !agentId) {
    return { dismissible: false, label: "", title: "" };
  }
  return { dismissible: true, label: "✕", title: DISMISS_TITLE };
}

/** A pane's current attention state, as exposed by `Pane.attention`. */
export interface PaneAttention {
  label: string;
  urgent: boolean;
  detail: string | null;
}

/** How a minimized pane's dock chip reflects its attention state. */
export interface DockChipAttention {
  /** Whether the chip shows the "needs attention" dot/pulse. */
  needsAttention: boolean;
  /** Red (urgent) vs amber pulse. */
  urgent: boolean;
  /** Chip tooltip. */
  title: string;
}

/** Decide how a minimized pane's dock chip mirrors the pane's attention state
 *  (#6 detection surfaced on the #26/#31 dock chip): the dot mirrors the header
 *  chip so minimizing a pane never hides an ask — e.g. an agent parked on an
 *  interactive question (#40) shows the dot even while docked. Pure so the
 *  dock-dot path is testable without a DOM. */
export function dockChipAttention(
  paneName: string,
  attn: PaneAttention | null,
): DockChipAttention {
  if (!attn) {
    return { needsAttention: false, urgent: false, title: `Restore ${paneName}` };
  }
  return {
    needsAttention: true,
    urgent: attn.urgent,
    title: `${attn.label} — ${attn.detail ?? "needs you"} · restore ${paneName}`,
  };
}

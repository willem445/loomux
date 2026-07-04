// Pure presentation mapping for attention-routing (#6): a backend attention
// reason → the label and urgency used to badge a pane. Kept DOM-free so both
// the pane header chip and the minimize-dock chip render it identically, and
// so the mapping is unit-testable.

/** Reasons the backend attention scan emits (see the Rust `AttentionItem`). */
export type AttentionReason = "blocked" | "waiting" | "report" | "gate";

export interface AttentionPresentation {
  /** Short glyph+word label shown in the header chip / dock chip tooltip. */
  label: string;
  /** `blocked` is the most urgent — callers tint it red rather than amber. */
  urgent: boolean;
}

const LABELS: Record<string, string> = {
  blocked: "⚠ blocked",
  waiting: "⚠ waiting",
  report: "✓ reported",
  gate: "⚑ your call",
};

/** Map an attention reason to its label + urgency. Unknown reasons fall back
 *  to a generic non-urgent badge rather than throwing, so a new backend reason
 *  never blanks the UI. */
export function attentionPresentation(reason: string): AttentionPresentation {
  return {
    label: LABELS[reason] ?? "⚠ attention",
    urgent: reason === "blocked",
  };
}

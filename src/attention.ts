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

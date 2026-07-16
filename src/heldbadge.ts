// Pure presentation mapping for delivery-hold badging (#246): a backend
// "prompt delivery is held" reason → the label/tooltip shown on the pane
// header. Kept DOM-free (mirrors attention.ts's split) so the mapping is
// unit-testable without a webview.
//
// Distinct from attention.ts's "needs attention" badge: attention flags a
// pane the HUMAN should look at (an idle prompt, a report, a merge gate);
// this badge flags that loomux is CURRENTLY withholding an outbound prompt
// because it believes the human's own input occupies the CLI's box — a
// different, narrower signal, with its own backend events
// (orch-delivery-held / orch-delivery-held-cleared) so the two never fight
// over the same state.

/** Reasons the backend can hold a delivery for (see the Rust `HeldReason`). */
export type HeldReason = "typing" | "box-occupied";

export interface HeldPresentation {
  /** Short glyph+word label shown in the header chip. */
  label: string;
}

const LABELS: Record<string, string> = {
  typing: "⏸ held: typing",
  "box-occupied": "⏸ held: unsubmitted text",
};

/** Map a hold reason to its header-chip label. Unknown reasons fall back to a
 *  generic label rather than throwing, so a new backend reason never blanks
 *  the badge. */
export function heldPresentation(reason: string): HeldPresentation {
  return { label: LABELS[reason] ?? "⏸ held" };
}

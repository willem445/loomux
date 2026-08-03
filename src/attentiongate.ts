// Whether an attention scan needs to be applied to the UI at all (#743 S5).
//
// The backend re-emits `orch-attention` every 3 s carrying the FULL current
// set, changed or not, and the handler's answer was an unconditional pass over
// every pane of every tab plus a tab-attention recompute — work proportional to
// the whole window, paid 20 times a minute for the entire life of the app,
// almost always to conclude that nothing moved (#743's census, part 2b).
//
// So compare first. The comparison has to cover BOTH inputs the pass reads,
// which is the part that is easy to get wrong:
//
//   1. the payload — which ptys need attention, and why; and
//   2. the pane topology — which tabs exist and how many panes they hold.
//
// Skipping on an unchanged payload alone would strand a badge the first time
// the panes changed under a steady payload, and that case is not exotic: a
// rejoin (or a restored layout) creates panes for agents that are ALREADY in
// the attention set, so every tick after it carries a byte-identical payload
// and the freshly-created panes would sit unbadged until some agent's state
// happened to change. Hence the topology token — cheap for the caller to build
// (a count per tab, no pane walk), and enough to force one pass whenever the
// pane population moved.
//
// What the token cannot see is a pane count that changes twice between two
// ticks in a way that cancels out — one pane closed and another opened in the
// same tab. That is safe for the reason the payload gate is safe: a pane
// created in that window has a pty the backend has never seen, so the tick that
// first mentions it is a payload change.
//
// Pure and DOM-free; the pass itself stays in main.ts (test/attentiongate.test.ts).

/** The fields of an `AttentionItem` the DOM pass actually consumes. */
export interface AttentionLike {
  /** Null for an item bound to no pane — the pass skips those, so they are not
   *  part of what "changed" means here either. */
  pty_id: number | null;
  reason: string;
  detail: string;
}

/** A stable, order-insensitive fingerprint of an attention payload.
 *
 *  Order-insensitive because the backend's iteration order is not a contract
 *  and a reshuffled set is not a change. JSON rather than a joined string so no
 *  `detail` text can forge a different set's fingerprint by containing the
 *  separator — a false "unchanged" would hold a stale badge until the next real
 *  change, which is precisely the bug this gate must not introduce. */
export function attentionSignature(items: readonly AttentionLike[]): string {
  const rows: [number, string, string][] = [];
  for (const it of items) {
    if (it.pty_id === null) continue;
    rows.push([it.pty_id, it.reason, it.detail]);
  }
  rows.sort((a, b) => a[0] - b[0] || (a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0));
  return JSON.stringify(rows);
}

export class AttentionGate {
  /** The `(payload, topology)` pair the last applied pass was for; `null` until
   *  the first pass, so the first scan after startup always applies. */
  private applied: string | null = null;

  /** Whether the caller must run the attention pass, recording this state as
   *  applied when it says yes.
   *
   *  @param topology a token that changes whenever the pane population does —
   *    see the note above on why the payload alone is not enough. */
  shouldApply(items: readonly AttentionLike[], topology: string): boolean {
    const state = JSON.stringify([attentionSignature(items), topology]);
    if (state === this.applied) return false;
    this.applied = state;
    return true;
  }
}

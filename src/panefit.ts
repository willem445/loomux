// Pure, DOM-free core of the pane resize-skip decision, split out so the
// load-bearing "hidden panes never resize their PTY" invariant is unit-testable
// under `node --test` (see pane.ts applyFit for the DOM wiring). Mirrors the
// panerename.ts / panefocus.ts split.
//
// Why this matters (CLAUDE.md constraint 1): a pane hidden with `display:none`
// — the maximized-behind panes (styles.css `.has-maximized`), and now every
// pane in an inactive project tab (#63) — reports a zero client width. Resizing
// its ConPTY then would repaint the whole screen on the Win10 inbox conhost,
// polluting scrollback for no visible benefit. So a zero-width pane must issue
// NO resize, and a same-size fit (ConPTY resize is never free) is skipped too.
// The tab-switch no-resize regression test asserts exactly this predicate.

export interface FitDecision {
  /** Terminal element's clientWidth. Zero when the pane is `display:none`
   *  (inactive tab, or hidden behind a maximized sibling) or not yet laid out. */
  clientWidth: number;
  /** The freshly fitted size as `${cols}x${rows}`. */
  size: string;
  /** The last size actually sent to (and confirmed by) the PTY (`""` before
   *  the first send). Only latches on a *successful* resize (#432 item 3) —
   *  see `pending` below for why a failed one must not leave this stale. */
  sentSize: string;
  /** The backing PTY id, or null before the PTY has spawned. */
  ptyId: number | null;
  /** True while a divider drag (grid split or embed-slot) is coalescing
   *  resizes (#432 item 1). A held tick still fits xterm's own buffer —
   *  `pane.ts`'s `doFit` calls this unconditionally — it just must not reach
   *  the PTY; the drag's `end` handler re-derives the settled size and
   *  flushes once the last hold releases, instead of one
   *  `ResizePseudoConsole` per animation frame for the whole drag. */
  held: boolean;
  /** The size of a `ResizePseudoConsole` call already in flight, or null.
   *  Needed because `sentSize` no longer latches until the call resolves
   *  (#432 item 3): without this, a second fit tick landing before the first
   *  call's IPC round-trip resolves would see `size !== sentSize` still and
   *  fire a redundant duplicate call for the identical size. */
  pending: string | null;
}

/** Whether `doFit` should send a resize to the PTY. False for a hidden
 *  (zero-width) pane — THE invariant that makes tab switching / maximize free of
 *  ConPTY repaints — for a pane with no PTY yet, for a held (in-drag,
 *  coalescing) tick, for a no-op same-size fit, and for a size that already
 *  has a call in flight. */
export function shouldResizePty(d: FitDecision): boolean {
  if (d.clientWidth === 0) return false; // hidden tab / maximized-behind / unlaid
  if (d.ptyId === null) return false;
  if (d.held) return false; // coalescing a drag — the drag's end flushes instead
  if (d.size === d.sentSize) return false;
  if (d.size === d.pending) return false; // identical call already in flight
  return true;
}

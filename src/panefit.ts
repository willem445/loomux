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
  /** The last size actually sent to the PTY (`""` before the first send). */
  sentSize: string;
  /** The backing PTY id, or null before the PTY has spawned. */
  ptyId: number | null;
}

/** Whether `applyFit` should send a resize to the PTY. False for a hidden
 *  (zero-width) pane — THE invariant that makes tab switching / maximize free of
 *  ConPTY repaints — for a pane with no PTY yet, and for a no-op same-size fit. */
export function shouldResizePty(d: FitDecision): boolean {
  if (d.clientWidth === 0) return false; // hidden tab / maximized-behind / unlaid
  if (d.ptyId === null) return false;
  return d.size !== d.sentSize;
}

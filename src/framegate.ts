// P5 (doc/design/performance.md §2) as a reusable module: a rAF dirty-flag
// gate that turns "repaint per batch" into "repaint per frame".
//
// The shape is `FileExplorer.onFilesBatch`'s `ft-files` gate — a boolean plus
// one `requestAnimationFrame` — lifted out so a second stream can adopt it
// without re-deriving it, and so the coalescing itself is unit-testable. The
// gate is what stands between a batch stream and the webview thread: a backend
// walk emitting hundreds of batches costs one paint per frame, not one per
// batch, and the paint cost is a function of the DOM, not of how fast the
// producer streams (#743 S5, INV-3).
//
// The scheduler is injected so `node --test` can drive frames deterministically
// (test/framegate.test.ts). Nothing here touches the DOM.

/** Schedules `cb` for the next frame. Defaults to `requestAnimationFrame`. */
export type FrameScheduler = (cb: () => void) => void;

export class FrameGate {
  private scheduled = false;
  private readonly paint: () => void;
  private readonly schedule: FrameScheduler;

  /** @param paint the (idempotent) repaint to run at most once per frame —
   *    fixed at construction rather than passed per call, so a second
   *    `request()` inside one frame cannot silently discard a *different*
   *    closure and leave the caller believing it was scheduled.
   *  @param schedule frame scheduler; injected for tests.
   *
   *  (Fields are declared and assigned rather than written as TypeScript
   *  parameter properties: `node --test` runs these modules in strip-only mode,
   *  which refuses that syntax — the repo's tests import `src/*.ts` directly.) */
  constructor(paint: () => void, schedule: FrameScheduler = (cb) => void requestAnimationFrame(cb)) {
    this.paint = paint;
    this.schedule = schedule;
  }

  /** Mark the view dirty. The first call in a frame schedules the paint; every
   *  further call before that frame runs is absorbed. */
  request(): void {
    if (this.scheduled) return;
    this.scheduled = true;
    this.schedule(() => {
      // Cleared BEFORE painting, so a batch that arrives while the paint is
      // running schedules the next frame instead of being swallowed by a flag
      // the paint had not got round to clearing yet.
      this.scheduled = false;
      this.paint();
    });
  }

  /** Whether a frame is already booked (for assertions/tests). */
  get pending(): boolean {
    return this.scheduled;
  }
}

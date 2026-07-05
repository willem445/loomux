// Bottom shortcut hint bar behaviour.
//
// The bar lists more shortcuts than fit a narrow window, so it scrolls
// horizontally (the scrollbar itself is hidden in styles.css). A plain mouse
// wheel only emits a vertical delta, which a horizontally-overflowing element
// ignores — so while the pointer is over the bar we translate that vertical
// delta into horizontal scroll. The listener is scoped to #hintbar, so wheel
// events over the terminals or the system-metrics strip are untouched.

/**
 * Pick the scroll amount for a wheel event over a horizontally-scrolling bar.
 * Trackpads emit a horizontal delta directly (deltaX); mice only emit a
 * vertical one (deltaY), which we translate to horizontal. Favour whichever
 * axis the user actually moved (the larger magnitude).
 */
export function wheelToScrollDelta(deltaX: number, deltaY: number): number {
  return Math.abs(deltaX) >= Math.abs(deltaY) ? deltaX : deltaY;
}

/** Wire vertical-wheel-to-horizontal-scroll onto the shortcut hint bar. */
export function initHintBar(): void {
  const bar = document.getElementById("hintbar");
  if (!bar) return;

  bar.addEventListener(
    "wheel",
    (e: WheelEvent) => {
      // Nothing to scroll to (bar fits the window) → leave the event alone.
      if (bar.scrollWidth <= bar.clientWidth) return;
      const delta = wheelToScrollDelta(e.deltaX, e.deltaY);
      if (delta === 0) return;
      bar.scrollLeft += delta;
      // We consumed the wheel as horizontal scroll; suppress the default so
      // it doesn't bubble into a page/ancestor scroll.
      e.preventDefault();
    },
    { passive: false }
  );
}

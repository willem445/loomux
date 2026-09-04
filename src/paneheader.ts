// The pane header's overflow policy (#2191) — DOM-free, so it is unit-testable
// under `node --test` (mirrors overlaysize.ts / layout.ts / resizeburst.ts).
// pane.ts owns every pixel this module reasons about: it measures the header,
// calls `planHeaderFit`, and moves the elements the plan names.
//
// WHY A POLICY MODULE AT ALL. The question "is this header too narrow for its
// icon set?" has three inputs that only ever appear together at runtime (the
// header's box, each control's measured width, which controls are priority) and
// one that is pure state (are we already folded?). Answering it inside a
// ResizeObserver callback would make it untestable *and* put a feedback loop one
// typo away — see the invariant below.
//
// THE FEEDBACK-LOOP INVARIANT. `planHeaderFit` decides from `fullWidth`, the
// width the row would need with EVERY control inline. That quantity is
// deliberately independent of the current fold state:
//   - the title contributes a CONSTANT floor (`TITLE_MIN_W`), never its rendered
//     width, which flexbox shrinks;
//   - `fixedWidth` covers only items that stay in the row either way (the CLI
//     mark, the role badge, the chips, the folder/branch meta items);
//   - a folded control is still LAID OUT (pane.ts's menu is `visibility: hidden`
//     when closed, not `display: none`), so its measured width is the same in
//     both states.
// Folding therefore cannot change the number the next pass reads, and the pass
// after a fold reproduces the same decision. Hysteresis (below) is the second
// line of defence, not the first.
//
// WHY THE MENU IS AN OVERLAY, not a second header row: CLAUDE.md constraint 1.
// A wrapping header would change `.pane-term`'s box, which resizes the PTY and
// repaints the user's scrollback. doc/design/pane-header.md carries the argument.

/** Gap between two adjacent header items (`.pane-header { gap: 6px }`). */
export const HEADER_GAP_W = 6;

/** Floor reserved for the pane name before anything folds. The name is priority
 *  chrome — it is what makes a wall of agents legible, and it is also the pane's
 *  drag handle (`Grid.onPointerDown` refuses to start a reorder from a button),
 *  so it may ellipsise but must never be squeezed to nothing. ~8 characters plus
 *  the ellipsis at the header's 11.5px type. */
export const TITLE_MIN_W = 56;

/** Fallback width for a control whose box could not be measured (a pane whose
 *  whole grid is `display: none` — an inactive project tab). Matches
 *  `.pane-btn { width: 23px }`; only ever reached before the first real
 *  measurement, and a wrong guess there costs one extra pass, not a wrong state. */
export const CONTROL_W_FALLBACK = 23;

/** Dead band between the two thresholds, in px. Folding happens the moment the
 *  full set stops fitting; unfolding waits until there is this much SPARE room.
 *  Sized above one drag frame's typical width delta so a divider dragged slowly
 *  across the threshold settles instead of strobing the icon set. */
export const HYSTERESIS_W = 24;

/** Padding kept between the overflow menu and its container's edges. */
export const MENU_EDGE_PAD = 4;

/** One header control, as the policy sees it. `id` is opaque here — pane.ts maps
 *  it back to the element. */
export interface HeaderControl {
  id: string;
  /** Measured border-box width, px. Non-finite or negative is read as "not
   *  measured" and replaced by `CONTROL_W_FALLBACK`. */
  width: number;
  /** True for a control that must stay inline at every width. */
  priority: boolean;
}

export interface HeaderFitInput {
  /** The header's CONTENT-box width (padding already excluded — a
   *  `ResizeObserver`'s `contentBoxSize.inlineSize`). */
  headerWidth: number;
  /** Total width of the header items that are neither the title nor a control:
   *  the CLI mark, the role badge, the chips, and the folder/branch meta items,
   *  each at its natural (unshrunk) width, with their own gaps included. */
  fixedWidth: number;
  /** Every control the pane currently HAS, in header order. A control that is
   *  not applicable to this pane kind (`hidden`, or `display: none` by a
   *  `.pane.is-content` rule) is not in this list at all. */
  controls: HeaderControl[];
  /** Width of the single overflow button that replaces the folded set. */
  overflowWidth: number;
  /** Current state — the only input that makes this decision hysteretic. */
  folded: boolean;
  titleMinWidth?: number;
  gap?: number;
  hysteresis?: number;
}

export interface HeaderFitPlan {
  folded: boolean;
  /** Control ids that stay in the header row, in header order. */
  inline: string[];
  /** Control ids that move into the overflow menu, in header order. Empty
   *  whenever `folded` is false. */
  overflow: string[];
}

/** A measured px value, defaulting a non-finite or negative reading to `fallback`. */
function px(v: number, fallback = 0): number {
  return Number.isFinite(v) && v >= 0 ? v : fallback;
}

function controlW(c: HeaderControl): number {
  return px(c.width, CONTROL_W_FALLBACK);
}

function foldedPlan(controls: HeaderControl[]): HeaderFitPlan {
  return {
    folded: true,
    inline: controls.filter((c) => c.priority).map((c) => c.id),
    overflow: controls.filter((c) => !c.priority).map((c) => c.id),
  };
}

/** Decide which header controls stay inline and which fold into the overflow
 *  menu.
 *
 *  Folding is ALL-OR-NOTHING over the non-priority set, which is what the ask
 *  describes ("everything else aggregates into a single overflow icon") and what
 *  keeps the menu's contents predictable: a human who has learned where the git
 *  icon lives in the menu finds it in the same place at every narrow width,
 *  instead of it drifting in and out of the row 20px at a time.
 *
 *  Two cases never fold, whatever the width:
 *   - folding would not save room. That covers both the arithmetic case (the
 *     overflow button costs at least as much as the set it replaces — one
 *     foldable control, on a header whose gap makes the swap a wash) and the
 *     degenerate one (nothing to fold, because every control is priority: the
 *     folded row is then the full row PLUS an overflow button standing in for an
 *     empty set, which is strictly wider). An explicit "nothing to fold" early
 *     return was tried and removed — mutating it reddened no test, because this
 *     guard already decides it;
 *   - the header has no measurable width yet (a pane in an inactive project tab
 *     measures 0). Guessing "fold" there would fold every pane in a hidden tab;
 *     the previous state is carried instead and the next real measurement decides. */
export function planHeaderFit(input: HeaderFitInput): HeaderFitPlan {
  const gap = px(input.gap ?? HEADER_GAP_W);
  const titleMin = px(input.titleMinWidth ?? TITLE_MIN_W);
  const hysteresis = px(input.hysteresis ?? HYSTERESIS_W);
  const controls = input.controls;

  const unfolded = (): HeaderFitPlan => ({
    folded: false,
    inline: controls.map((c) => c.id),
    overflow: [],
  });

  const sum = (cs: HeaderControl[]) => cs.reduce((t, c) => t + controlW(c) + gap, 0);

  const base = px(input.fixedWidth) + titleMin;
  const fullWidth = base + sum(controls);
  const priority = controls.filter((c) => c.priority);
  const foldedWidth = base + sum(priority) + px(input.overflowWidth) + gap;

  // Folding that buys nothing is worse than not folding: it hides controls AND
  // leaves the row just as tight. Checked BEFORE the width guard below, so a
  // header with nothing foldable can never be carried into a folded state that
  // would render an overflow button over an empty menu.
  if (foldedWidth >= fullWidth) return unfolded();

  const headerWidth = px(input.headerWidth, -1);
  if (headerWidth <= 0) {
    // Not laid out (or detached). Carry the current state rather than inventing one.
    return input.folded ? foldedPlan(controls) : unfolded();
  }

  // The two thresholds. Both read `fullWidth`, which does not depend on
  // `input.folded` — see the feedback-loop invariant at the top of this file —
  // so the dead band between them is the whole of the hysteresis.
  const shouldFold = input.folded
    ? fullWidth > headerWidth - hysteresis // stay folded until there is SPARE room
    : fullWidth > headerWidth; // fold as soon as the full set stops fitting

  return shouldFold ? foldedPlan(controls) : unfolded();
}

/** Where the overflow menu's left edge goes, in its container's coordinates.
 *
 *  Preference is LEFT-ALIGNED with the trigger, the direction a dropdown is read
 *  to open. When that would run past the container's right edge the menu FLIPS to
 *  right-aligned with the trigger — the overflow button lives at the right end of
 *  the header, so on a pane at the window's right border the un-flipped menu
 *  would hang outside the window. The result is then clamped into
 *  `[pad, containerWidth - menuWidth - pad]`, and a menu at least as wide as its
 *  container goes flush to the left edge (pane.ts caps the menu's `max-width` and
 *  lets it wrap, so this is the degenerate case, not the normal one).
 *
 *  Container coordinates, not viewport ones: the menu is a child of `.pane`,
 *  which is `overflow: hidden`, so "inside the window" is implied by — and
 *  strictly weaker than — "inside the pane". */
export function menuLeftFor(
  anchorLeft: number,
  anchorRight: number,
  menuWidth: number,
  containerWidth: number,
  pad: number = MENU_EDGE_PAD
): number {
  const w = px(menuWidth);
  const box = px(containerWidth);
  const p = px(pad);
  const maxLeft = box - w - p;
  if (maxLeft <= p) return Math.max(0, Math.min(p, box - w));

  const preferred = px(anchorLeft);
  if (preferred <= maxLeft) return Math.max(p, preferred);

  const flipped = px(anchorRight) - w; // right-align the menu with the trigger
  return Math.min(Math.max(flipped, p), maxLeft);
}

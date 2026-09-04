// The pane header's overflow policy (#2191, #2335) — DOM-free, so it is unit-testable
// under `node --test` (mirrors overlaysize.ts / layout.ts / resizeburst.ts).
// pane.ts owns every pixel this module reasons about: it measures the header,
// calls `planHeaderFit`, and moves the elements the plan names.
//
// WHY A POLICY MODULE AT ALL. The question "is this header too narrow for its
// icon set?" has three inputs that only ever appear together at runtime (the
// header's box, each control's measured width, which controls are priority) and
// one that is pure state (which rung are we already on?). Answering it inside a
// ResizeObserver callback would make it untestable *and* put a feedback loop one
// typo away — see the invariant below.
//
// THE FEEDBACK-LOOP INVARIANT. `planHeaderFit` prices each rung of its ladder
// from the width that rung's row would NEED. Every one of those quantities is
// deliberately independent of the stage we are currently in:
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

/** Floor reserved for the pane name while the ladder's top two rungs are in
 *  play. The name is priority chrome — it is what makes a wall of agents legible,
 *  and it is also the pane's drag handle (`Grid.onPointerDown` refuses to start a
 *  reorder from a button) — so it holds this much until nothing else will give:
 *  the `squeezed` rung releases the floor and lets it ellipsise freely, and only
 *  `minimal` takes it out of the row (#2335), where the menu's name row carries
 *  it instead. ~8 characters plus the ellipsis at the header's 11.5px type. */
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

/** The rungs of the fold ladder, widest row first. Each one is reached only when
 *  the one above it no longer fits, and each is kept only if it is STRICTLY
 *  narrower than the rung above (see `planHeaderFit`).
 *
 *   - `full`     every control inline, the pane name on its floor;
 *   - `folded`   the non-priority controls in the menu behind `⋯` (#2191);
 *   - `squeezed` the same row, with the name's floor RELEASED so it ellipsises
 *                as far as flexbox needs — the name gives way before a control
 *                does, which is what "the menu button has priority over the
 *                title" means (#2335);
 *   - `minimal`  the terminal state: every control in the menu, the name gone
 *                from the row, and `⋯` the one thing left standing (#2335). */
export const HEADER_FIT_STAGES = ["full", "folded", "squeezed", "minimal"] as const;
export type HeaderFitStage = (typeof HEADER_FIT_STAGES)[number];

/** How the pane name is treated in the row at a given stage. `"shrink"` is the
 *  same element with no reserved floor; `"hidden"` takes it out of the row
 *  altogether, and the menu grows a rename entry in its place
 *  (`overflowMenuIds`). */
export type TitleFit = "floor" | "shrink" | "hidden";

/** The menu entry that stands in for the pane name once the row has lost it.
 *  Not a `HeaderControl`: it never sits in the header row, so it costs the row
 *  nothing and the policy never has to price it. */
export const RENAME_ENTRY_ID = "rename";

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
  /** How much of `fixedWidth` the chrome CANNOT give up, px.
   *
   *  `fixedWidth` is what the chrome WANTS — every item at its natural, unshrunk
   *  width — and that is the right number for deciding when to FOLD, because
   *  folding is how the row buys the chrome its room. It is the wrong number for
   *  the two rungs below the fold: those are emergency rungs, and by the time
   *  they are in play the chrome has already given way. Pricing them off
   *  `fixedWidth` makes a header with a long queue label or a deep folder path
   *  collapse to `minimal` at a width where the row has room to spare — the pane
   *  name vanishing at 620px because a chip WANTED 180 (#2335, caught by
   *  `queue-badge.spec.ts`'s drag-handle floor going to 0).
   *
   *  It is NOT zero, and an earlier cut of this change defaulted it to zero on
   *  the argument that `styles.css` makes every non-control header child
   *  `flex-shrink: 1; min-width: 0`. That argument is wrong at the boundary:
   *  `flex-shrink` scales a child's CONTENT and cannot touch its border, its
   *  padding, or the row's own `gap`, so each lit chip keeps ~20px whatever the
   *  width. Two of them were enough to push the `⋯` off an 80px pane — the
   *  grid's own `MIN_PANE_PX` — which is #2335 not fixed (rev-final round 2).
   *  `pane.ts`'s `measureHeaderFixed` now measures this per child and passes it.
   *
   *  The default of 0 survives only as the value for a caller that does not
   *  measure, and it fails toward folding LATE rather than early. */
  chromeFloorWidth?: number;
  /** Current stage — the only input that makes this decision hysteretic. */
  stage: HeaderFitStage;
  titleMinWidth?: number;
  gap?: number;
  hysteresis?: number;
}

export interface HeaderFitPlan {
  stage: HeaderFitStage;
  /** True whenever the `⋯` button is in the row (and so the menu is in use).
   *  NOT the same question as `stage !== "full"`: a header can reach `squeezed`
   *  with nothing folded, because releasing the name's floor is worth room on
   *  its own and folding is refused whenever it would buy none. */
  folded: boolean;
  /** Control ids that stay in the header row, in header order. Empty at
   *  `minimal`, where `⋯` is the only control left. */
  inline: string[];
  /** Control ids that move into the overflow menu, in header order. Empty
   *  whenever `folded` is false. */
  overflow: string[];
  /** What the row does with the pane name at this stage. */
  title: TitleFit;
}

/** A measured px value, defaulting a non-finite or negative reading to `fallback`. */
function px(v: number, fallback = 0): number {
  return Number.isFinite(v) && v >= 0 ? v : fallback;
}

function controlW(c: HeaderControl): number {
  return px(c.width, CONTROL_W_FALLBACK);
}

function ids(cs: HeaderControl[]): string[] {
  return cs.map((c) => c.id);
}

/** Decide which header controls stay inline, which fold into the overflow menu,
 *  and what happens to the pane name — as a LADDER of progressively narrower
 *  rows, each reached only when the one above it stops fitting.
 *
 *  THE PRIORITY ORDER, last thing dropped first. It is the whole policy, and
 *  every rung below is one step down it:
 *    1. the `⋯` button — never dropped once it is in the row, because it is the
 *       only route to everything that folded (#2335: at minimum width the old
 *       ladder ran out and dropped it, leaving a header with no reachable
 *       control at all);
 *    2. the priority controls (minimize / maximize);
 *    3. the pane name's readable floor — truncating a name costs legibility that
 *       degrades gracefully (the rest is in the tooltip, and the full name is in
 *       the menu's rename entry), where folding a control costs a whole extra
 *       gesture. So the cheap thing is spent first;
 *    4. the non-priority controls.
 *
 *  Folding within a rung is ALL-OR-NOTHING over that rung's set, which is what
 *  #2191 asked for and what keeps the menu's contents predictable: a human who
 *  has learned where the git icon lives in the menu finds it in the same place at
 *  every narrow width, instead of it drifting in and out of the row 20px at a
 *  time.
 *
 *  THE TOP TWO RUNGS AND THE BOTTOM TWO PRICE THE CHROME DIFFERENTLY, and that
 *  asymmetry is the whole of `chromeFloorWidth` (see its doc). Folding is how
 *  the row buys the chrome the width it WANTS, so `full` and `folded` read
 *  `fixedWidth`. `squeezed` and `minimal` are what happens after the chrome has
 *  already given way, so they read the width it cannot give up.
 *
 *  A RUNG THAT BUYS NOTHING IS NOT ON THE LADDER. Each candidate is kept only if
 *  it is strictly narrower than the last rung kept above it, so:
 *   - a welcome pane, whose only control is its `✕`, never folds — the `⋯` costs
 *     more than the one button it would replace (`OVERFLOW_BTN_W` is deliberately
 *     wider than `.pane-btn`), at `folded` AND at `minimal`, so both are dropped
 *     and its `✕` stays inline and clickable at every width;
 *   - a header whose controls are all priority skips `folded` (there is nothing
 *     to fold) but still reaches `minimal`, where folding them is what buys the
 *     room;
 *   - `squeezed` is dropped when the name floor is already zero.
 *  This subsumes #2191's single "folding buys nothing" guard rather than
 *  replacing it, and it runs BEFORE the width check, so a header with nothing
 *  worth folding can never be carried into a folded state that would render an
 *  overflow button over an empty menu.
 *
 *  A header with no measurable width (a pane in an inactive project tab measures
 *  0) carries its current stage instead of guessing; guessing "fold" there would
 *  fold every pane in a hidden tab. A stage that is not on this pane's ladder is
 *  not carried — it drops to the widest rung, which is the case above. */
export function planHeaderFit(input: HeaderFitInput): HeaderFitPlan {
  const gap = px(input.gap ?? HEADER_GAP_W);
  const titleMin = px(input.titleMinWidth ?? TITLE_MIN_W);
  const hysteresis = px(input.hysteresis ?? HYSTERESIS_W);
  const controls = input.controls;

  const sum = (cs: HeaderControl[]) => cs.reduce((t, c) => t + controlW(c) + gap, 0);
  const fixed = px(input.fixedWidth);
  const floor = Math.min(px(input.chromeFloorWidth ?? 0), fixed);
  const overflowCost = px(input.overflowWidth) + gap;

  const priority = controls.filter((c) => c.priority);
  const foldable = controls.filter((c) => !c.priority);

  const ladder: { need: number; plan: HeaderFitPlan }[] = [];
  /** Add a rung, unless it is not strictly narrower than the one above it. */
  const rung = (
    stage: HeaderFitStage,
    inline: HeaderControl[],
    overflow: HeaderControl[],
    titleFloor: number,
    title: TitleFit,
    folded: boolean,
    chrome: number
  ): boolean => {
    const need = chrome + titleFloor + sum(inline) + (folded ? overflowCost : 0);
    const above = ladder[ladder.length - 1];
    if (above && need >= above.need) return false;
    ladder.push({
      need,
      plan: { stage, folded, inline: ids(inline), overflow: ids(overflow), title },
    });
    return true;
  };

  rung("full", controls, [], titleMin, "floor", false, fixed);
  const folds = rung("folded", priority, foldable, titleMin, "floor", true, fixed);
  // `squeezed` keeps whatever row survived above it and only releases the name's
  // floor — so on a pane where folding buys nothing it is the FULL row with a
  // shrinking name, not a folded one. Reading the fold decision off `folds`
  // rather than re-deriving it is what keeps those two answers one answer.
  // Both of these price the chrome at its FLOOR, not at what it wants — see
  // `chromeFloorWidth`. Descending is then driven by the controls that cannot
  // shrink, which is the only thing that can actually clip.
  rung("squeezed", folds ? priority : controls, folds ? foldable : [], 0, "shrink", folds, floor);
  rung("minimal", [], controls, 0, "hidden", true, floor);

  const headerWidth = px(input.headerWidth, -1);
  const exact = ladder.findIndex((r) => r.plan.stage === input.stage);
  if (headerWidth <= 0) {
    // Not laid out (or detached). Carry the current stage rather than inventing
    // one; a stage this pane's ladder does not have falls back to the widest,
    // which is the "never an overflow button over an empty menu" case.
    return ladder[exact >= 0 ? exact : 0].plan;
  }

  // Which rung the dead band is measured FROM. A stage can leave the ladder
  // while the header is perfectly well laid out — revealing or hiding a control
  // changes which rungs are strictly narrower than their neighbour, so `folded`
  // or `squeezed` can drop out between two passes. Left as -1 that would make
  // every rung look like a sideways move and skip the dead band for that pass,
  // so a divider parked on a threshold flaps once at the moment a chip lights.
  // Fall back to the nearest rung still on the ladder, NARROWER first: it is the
  // closer row to the one being rendered, and erring narrow never clips.
  let current = exact;
  if (current < 0) {
    const from = HEADER_FIT_STAGES.indexOf(input.stage);
    for (let i = from + 1; i < HEADER_FIT_STAGES.length && current < 0; i++) {
      current = ladder.findIndex((r) => r.plan.stage === HEADER_FIT_STAGES[i]);
    }
    for (let i = from - 1; i >= 0 && current < 0; i--) {
      current = ladder.findIndex((r) => r.plan.stage === HEADER_FIT_STAGES[i]);
    }
  }

  for (let i = 0; i < ladder.length; i++) {
    // The dead band applies to WIDENING only: a row folds the moment its set
    // stops fitting, and unfolds only once there is `hysteresis` px of SPARE
    // room, so a divider dragged slowly across a threshold settles instead of
    // strobing. Every rung's `need` is independent of `input.stage` — see the
    // feedback-loop invariant at the top of this file — so the dead band between
    // two rungs is the whole of the hysteresis.
    const widening = current >= 0 && i < current;
    if (ladder[i].need <= headerWidth - (widening ? hysteresis : 0)) return ladder[i].plan;
  }
  return ladder[ladder.length - 1].plan;
}

/** What the overflow menu holds under a given plan, in the order it is rendered:
 *  the folded controls, preceded by the rename entry that stands in for the pane
 *  name at the one stage where the row has lost it. The name leads because that
 *  is where it sits in the header row it is standing in for.
 *
 *  Derived from the plan rather than passed in, so "the row has no name" and "the
 *  menu offers one" are one decision asked once (#2335). */
export function overflowMenuIds(plan: HeaderFitPlan): string[] {
  return plan.title === "hidden" ? [RENAME_ENTRY_ID, ...plan.overflow] : plan.overflow;
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

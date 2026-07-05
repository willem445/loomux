// Pure, DOM-free geometry and ordering helpers for pane drag-reorder. The
// decision logic (which region of a pane the pointer is over, and what
// structural change that implies) lives here so it can be unit-tested
// without a browser; grid.ts owns the DOM mutation it drives.

/** Which region of a target pane the pointer is over.
 *  `center` swaps the two panes in place; an edge re-docks the dragged pane
 *  to that side of the target (a new split). */
export type DropZone = "center" | "left" | "right" | "top" | "bottom";

/** Fraction of each dimension (0–0.5) treated as an edge band. Anything
 *  closer to the middle than this on every side is the center zone. */
export const DEFAULT_EDGE_RATIO = 0.3;

/** Classify a pointer position within a pane of the given size.
 *
 *  `x`/`y` are offsets from the pane's top-left. The nearest edge wins; when
 *  the pointer is at least `edge` of the way in from every side, it's the
 *  center (swap) zone. Ties resolve left → right → top → bottom, which only
 *  matters exactly on a diagonal and is otherwise invisible. */
export function dropZoneFor(
  width: number,
  height: number,
  x: number,
  y: number,
  edge: number = DEFAULT_EDGE_RATIO
): DropZone {
  if (width <= 0 || height <= 0) return "center";
  const left = x / width;
  const right = 1 - left;
  const top = y / height;
  const bottom = 1 - top;
  const nearest = Math.min(left, right, top, bottom);
  if (nearest >= edge) return "center";
  if (nearest === left) return "left";
  if (nearest === right) return "right";
  if (nearest === top) return "top";
  return "bottom";
}

/** A rectangle expressed as fractions (0–1) of a pane's box, used to size the
 *  drag snap indicator. */
export interface FracRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** The region the snap indicator should cover for a given drop zone: the full
 *  pane for a swap, or the half the dragged pane would occupy on an edge. */
export function indicatorFor(zone: DropZone): FracRect {
  switch (zone) {
    case "left":
      return { left: 0, top: 0, width: 0.5, height: 1 };
    case "right":
      return { left: 0.5, top: 0, width: 0.5, height: 1 };
    case "top":
      return { left: 0, top: 0, width: 1, height: 0.5 };
    case "bottom":
      return { left: 0, top: 0.5, width: 1, height: 0.5 };
    case "center":
      return { left: 0, top: 0, width: 1, height: 1 };
  }
}

/** How an edge drop maps onto a split: the split direction and whether the
 *  dragged pane lands before or after the target. `center` is a swap, not a
 *  placement, so it returns null. */
export interface Placement {
  dir: "row" | "column";
  before: boolean;
}

export function zoneToPlacement(zone: DropZone): Placement | null {
  switch (zone) {
    case "left":
      return { dir: "row", before: true };
    case "right":
      return { dir: "row", before: false };
    case "top":
      return { dir: "column", before: true };
    case "bottom":
      return { dir: "column", before: false };
    case "center":
      return null;
  }
}

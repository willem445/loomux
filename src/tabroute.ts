// Pure, DOM-free routing/preview decisions for project tabs (#63), split out so
// they're unit-testable under `node --test` (CLAUDE.md convention).
// The Tauri/DOM wiring that consumes these lives in main.ts (the OrchWiring
// router), tabbar.ts, and workspace.ts.
//
// NB: a tested module can't runtime-import a sibling src module (Node's ESM
// loader won't resolve the extensionless path), so the urgency rule is inlined
// below rather than imported from attention.ts. It is the SAME rule
// attentionPresentation uses — blocked is the one urgent reason — and the pane
// header / dock chip still render via attentionPresentation verbatim (main.ts
// applies pane.setAttention, which uses it). Keep the two in lockstep.

/** A pure description of a tab's split layout for the preview composite (#63):
 *  the split tree with each pane's serialized-HTML viewport at the leaves. Built
 *  by Workspace (which serializes each pane) from Grid's
 *  layoutSnapshot; rendered SAFELY by the tab bar (spans → textContent). */
export type PreviewNode =
  | { kind: "leaf"; weight: number; title: string; html: string; capped: boolean }
  | { kind: "split"; dir: "row" | "column"; weight: number; children: PreviewNode[] };

/** Whether an attention reason is urgent, mirroring attention.ts. */
const isUrgentReason = (reason: string): boolean => reason === "blocked";

// Priority when several panes in one tab need attention: show the most urgent
// reason on the tab chip. Mirrors attention.ts's ordering (blocked first).
const REASON_PRIORITY: Record<string, number> = { blocked: 4, waiting: 3, gate: 2, report: 1 };
const reasonRank = (reason: string): number => REASON_PRIORITY[reason] ?? 0;

/** The slice of a backend AttentionItem this module needs. The real
 *  AttentionItem (orchestration.ts) is a structural superset, so it satisfies it. */
export interface AttnLike {
  /** null for a non-pty item — those can't be routed to a tab. */
  pty_id: number | null;
  reason: string;
}

/** Per-tab attention badge state. */
export interface TabAttn {
  urgent: boolean;
  /** The most-urgent reason among the tab's needing-attention panes, so the tab
   *  chip can show the same label as the pane header (via attentionPresentation). */
  reason: string;
}

/** Fold an attention scan into a per-workspace badge state, keyed by the
 *  pty→workspace routing map. A workspace is urgent if ANY of its ptys is
 *  urgent (blocked), and carries the highest-priority reason among its panes.
 *  Urgency/priority reuse attention.ts's ordering, so the tab badge, pane header
 *  chip, and dock chip all agree. Workspaces with no attention item are absent. */
export function tabAttention(
  items: AttnLike[],
  ptyToWs: Map<number, string>
): Map<string, TabAttn> {
  const out = new Map<string, TabAttn>();
  for (const it of items) {
    if (it.pty_id === null) continue;
    const wsId = ptyToWs.get(it.pty_id);
    if (!wsId) continue;
    const prev = out.get(wsId);
    // Keep whichever reason ranks highest (blocked > waiting > gate > report).
    if (!prev || reasonRank(it.reason) > reasonRank(prev.reason)) {
      out.set(wsId, { urgent: isUrgentReason(it.reason), reason: it.reason });
    }
  }
  return out;
}

/** Whether two attention-state maps are equivalent, so the router can skip a
 *  re-render on the 3-second re-emits when nothing changed. */
export function sameAttention(
  a: ReadonlyMap<string, TabAttn>,
  b: ReadonlyMap<string, TabAttn>
): boolean {
  if (a.size !== b.size) return false;
  for (const [k, v] of a) {
    const w = b.get(k);
    if (!w || w.urgent !== v.urgent || w.reason !== v.reason) return false;
  }
  return true;
}

// ---------- preview HTML sanitizer (#63) ----------
//
// The hover preview renders each pane's viewport from @xterm/addon-serialize's
// `serializeAsHTML`, which keeps spacing and per-run color — but does NOT escape
// cell text, and emits inline `style` attributes. We must therefore treat its
// output as UNTRUSTED (a terminal can print any bytes). The tab bar rebuilds it
// SAFELY: cell text goes in via `textContent` (never innerHTML, so no markup can
// execute), and each run's inline style is filtered here to a small whitelist of
// visual properties with values that can't smuggle a URL / script / expression.
// Keeping the decision pure (no DOM) lets a security reviewer read the whole rule
// in one place and lets `node --test` prove the whitelist and value guards.

/** CSS properties a serialized-viewport span may carry — visual only, nothing
 *  that can load a resource or run code. Anything else is dropped. */
export const SAFE_STYLE_PROPS: ReadonlySet<string> = new Set([
  "color",
  "background-color",
  "font-weight",
  "font-style",
  "text-decoration",
  "opacity",
  "visibility",
]);

/** A value is rejected if it could reach outside pure visual styling: markup
 *  delimiters, a `url(...)` resource load, IE `expression(...)`, or a
 *  `javascript:` scheme. Belt-and-braces on top of the property whitelist and
 *  the fact that we never use innerHTML — a color/opacity value has no business
 *  containing any of these. */
const UNSAFE_STYLE_VALUE = /[<>{}]|url\(|expression|javascript:/i;

/** Parse a raw inline `style` attribute into the whitelisted, value-sanitized
 *  `[property, value]` declarations the preview is allowed to apply. Pure and
 *  DOM-free so it's unit-tested directly; the tab bar just calls
 *  `el.style.setProperty` for each pair returned. Malformed declarations
 *  (no colon, blank property/value) and anything failing the whitelist or the
 *  value guard are silently dropped. */
export function safeStyleDeclarations(style: string | null | undefined): [string, string][] {
  if (!style) return [];
  const out: [string, string][] = [];
  for (const decl of style.split(";")) {
    const idx = decl.indexOf(":");
    if (idx < 0) continue;
    const prop = decl.slice(0, idx).trim().toLowerCase();
    const value = decl.slice(idx + 1).trim();
    if (!prop || !value) continue;
    if (!SAFE_STYLE_PROPS.has(prop)) continue;
    if (UNSAFE_STYLE_VALUE.test(value)) continue;
    out.push([prop, value]);
  }
  return out;
}

/** Bounds how many panes a preview composite serializes (workspace.ts's
 *  PREVIEW_PANE_CAP), so a huge grid degrades to a titled placeholder past the
 *  cap instead of serializing every pane on each ~700ms refresh. Pure + tested
 *  so the cap edge (exactly N serialized, the N+1th capped) can't drift as the
 *  tree traversal changes. Used once per preview build, taken in tree order. */
export class PreviewBudget {
  private remaining: number;
  constructor(cap: number) {
    this.remaining = cap;
  }
  /** True if this pane should be serialized; false once the cap is spent. */
  take(): boolean {
    if (this.remaining <= 0) return false;
    this.remaining--;
    return true;
  }
}

// ---------- preview composite scaling (#63 review) ----------
//
// The hover composite serializes each pane at ITS OWN terminal dims (cols×rows —
// whatever it was last laid out at, or 80×24 if never laid out; a hidden pane is
// never re-fitted, so these differ pane-to-pane). Scaling each pane to fit its
// own cell independently therefore produced wildly different effective font
// sizes across one composite — a pane last laid out full-width shrank to an
// illegible, sub-pixel smear (its background-colored rows collapsing into
// horizontal bars) next to an 80-col pane at readable size. The fix: ONE shared
// scale for the whole composite, so text is the same size everywhere.

/** One mini-pane's measured geometry: the natural (unscaled) size of its
 *  serialized content, and the size of the cell it must fit — both in px. */
export interface PreviewFit {
  contentW: number;
  contentH: number;
  cellW: number;
  cellH: number;
}

/** The single uniform scale every mini-pane in a composite renders at, so glyphs
 *  are the SAME readable size across panes (uniform scale ⇒ aspect preserved, no
 *  squish). Each pane's natural fit is `min(cellW/contentW, cellH/contentH)`;
 *  the composite uses the **median** of those fits, clamped to `[min, max]`.
 *
 *  Median (not min) is deliberately robust to an OUTLIER pane — one serialized
 *  at stale/oversized dims (e.g. last laid out full-width): rather than dragging
 *  the whole composite down to that pane's tiny fit (illegible for everyone), the
 *  composite renders at the typical scale and the outlier simply **crops** to its
 *  cell (cells are `overflow:hidden`) — crop, never squish. Panes that would fit
 *  larger letterbox instead. The `min` floor keeps text off the sub-pixel range
 *  where downscaled background runs smear into bars; below it, panes crop rather
 *  than shrink further. `max` (≤1) never enlarges past the source glyphs. Empty
 *  input → `max`. */
export function compositeScale(fits: readonly PreviewFit[], min: number, max: number): number {
  if (fits.length === 0) return max;
  const scales = fits
    .map((f) => Math.min(f.cellW / Math.max(1, f.contentW), f.cellH / Math.max(1, f.contentH)))
    .sort((a, b) => a - b);
  const mid = Math.floor(scales.length / 2);
  const median =
    scales.length % 2 === 1 ? scales[mid] : (scales[mid - 1] + scales[mid]) / 2;
  return Math.max(min, Math.min(max, median));
}

// ---------- cross-tab pane lookup (the live focus / exit / rename path) ----------

/** Minimal grid surface for locating a pane by pty — the real Grid satisfies it. */
export interface PtyLookupGrid<P> {
  findByPtyId(ptyId: number): P | undefined;
}

/** Find the first workspace whose grid currently holds `ptyId`, scanning the
 *  workspaces' LIVE panes in order. This is deliberately a scan over current
 *  state, not a lookup in a maintained pty→tab side-map: a pane close would
 *  leave such a map stale (nothing removes per-pty entries on individual pane
 *  close), whereas `findByPtyId` always reflects the panes that actually exist.
 *  The scan is O(panes) and only runs on rare orch-focus / rename / pty-exit
 *  events. Pure over the minimal grid surface so the cross-tab routing that
 *  ACTUALLY runs (main.ts `findPaneAcrossTabs`) is unit-tested. Returns the
 *  owning workspace + pane, or null when no open pane has that pty. */
export function findPaneByPty<W, P>(
  workspaces: readonly W[],
  gridOf: (ws: W) => PtyLookupGrid<P>,
  ptyId: number
): { ws: W; pane: P } | null {
  for (const ws of workspaces) {
    const pane = gridOf(ws).findByPtyId(ptyId);
    if (pane) return { ws, pane };
  }
  return null;
}

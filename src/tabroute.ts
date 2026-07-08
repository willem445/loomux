// Pure, DOM-free routing/preview decisions for project tabs phases 3–4 (#63),
// split out so they're unit-testable under `node --test` (CLAUDE.md convention).
// The Tauri/DOM wiring that consumes these lives in main.ts (the OrchWiring
// router), tabbar.ts, and workspace.ts.
//
// NB: a tested module can't runtime-import a sibling src module (Node's ESM
// loader won't resolve the extensionless path), so the urgency rule is inlined
// below rather than imported from attention.ts. It is the SAME rule
// attentionPresentation uses — blocked is the one urgent reason — and the pane
// header / dock chip still render via attentionPresentation verbatim (main.ts
// applies pane.setAttention, which uses it). Keep the two in lockstep.

/** A pure description of a tab's split layout for the preview composite (#63
 *  finding 2): the split tree with each pane's serialized-HTML viewport at the
 *  leaves. Built by Workspace (which serializes each pane) from Grid's
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

/** Where an orch-focus for `ptyId` should move the active tab. Returns the
 *  workspace id to switch to, or null when the pty is already in the active tab
 *  (focus the pane in place) or is unknown to the router (caller falls back to
 *  a cross-tab search). Focus must switch the TAB first, then the pane. */
export function revealPlan(
  ptyToWs: Map<number, string>,
  activeWsId: string | null,
  ptyId: number
): { switchTo: string | null; known: boolean } {
  const wsId = ptyToWs.get(ptyId);
  if (!wsId) return { switchTo: null, known: false };
  return { switchTo: wsId === activeWsId ? null : wsId, known: true };
}

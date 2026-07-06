// Pure view logic for the bottom-toolbar agent usage-limit chip (issue #80).
// Kept separate from the DOM wiring in statusbar.ts so the decision — which
// scope is most-constrained, what the chip and tooltip read, when to grey out
// to n/a — is unit-testable without a browser.

import type { UsageLimits } from "./metrics";

export interface UsageChipView {
  /** True when there's nothing to show; the chip greys out to n/a. */
  na: boolean;
  /** Most-constrained consumed percentage (0 when `na`), for the fill bar. */
  pct: number;
  /** Monospace chip value ("34%" or "n/a"). */
  text: string;
  /** Full tooltip: the session/weekly breakdown plus provenance + freshness. */
  title: string;
}

const NA_TITLE =
  "Claude Code usage limit — no live pane is showing a session/weekly limit " +
  "readout. Claude Code surfaces this only via a limit statusline widget " +
  "(e.g. /usage or ccstatusline). Nothing shown for Copilot: it exposes no " +
  "local allowance.";

/** Build the Claude usage-limit chip view from an aggregated limits payload.
 *  Shows the most-constrained (highest consumed) scope; the tooltip carries the
 *  full session + weekly breakdown and honest, estimated-vs-reported wording
 *  consistent with the #42 cost work. */
export function usageChipView(u: UsageLimits): UsageChipView {
  const cc = u.claude;
  if (!cc || (cc.session_pct === null && cc.weekly_pct === null)) {
    return { na: true, pct: 0, text: "n/a", title: NA_TITLE };
  }
  const s = cc.session_pct;
  const w = cc.weekly_pct;
  // Most-constrained = highest consumed %; that scalar drives the chip + bar.
  const pct = Math.max(s ?? -1, w ?? -1);
  const parts: string[] = [];
  if (s !== null) parts.push(`session ${Math.round(s)}%`);
  if (w !== null) parts.push(`weekly ${Math.round(w)}%`);
  const title =
    `Claude Code usage limit — ${parts.join(", ")} (most-constrained shown). ` +
    "Source: live pane statusline, refreshed each scan; reported by the CLI, " +
    "not estimated. Aggregated across all live Claude panes (they share one " +
    "account).";
  return { na: false, pct, text: `${Math.round(pct)}%`, title };
}

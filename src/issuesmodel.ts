// DOM-free core for the issues view (src/issuesview.ts): filtering, sorting,
// the "labeled for agents" predicate, label add/remove deltas, and new-issue
// validation. Kept pure so it's unit-testable in Node (test/issuesmodel.test.ts)
// without simulating a DOM — the repo's DOM-free-module convention.

import type { GhIssue } from "./issues";

/** Which list the overlay is showing. The PR mode reuses the issue list/filter
 *  mechanics wholesale (see `ListItem`); only the fetch + row actions differ. */
export type ViewMode = "issues" | "prs";

/** The fields the filter/sort need — the common shape of `GhIssue` and `GhPr`,
 *  so both flow through `matchesQuery` / `filterAndSortIssues` unchanged. */
export interface ListItem {
  number: number;
  title: string;
  labels: string[];
  updated_at: string;
}

/** The go-signal labels the GUI can toggle. Mirrors the backend allow-list
 *  (validated there — this list only drives the buttons). `agent-ready` and
 *  `agent-investigate` are the two the plan exposes as one-click actions;
 *  `agent-managed` is owned by a running orchestrator and shown read-only. */
export const AGENT_READY = "agent-ready";
// The repo's label is "agent-investigation" (not the plan text's
// "agent-investigate") — that's what exists and what the backend allow-list
// permits; the other value would be rejected by gh_issue_set_labels.
export const AGENT_INVESTIGATE = "agent-investigation";
export const AGENT_MANAGED = "agent-managed";

/** The labels a human toggles from the issues view to hand an issue to the
 *  orchestrator (apply `agent-ready` to start work, `agent-investigate` to ask
 *  for a plan). Order is the display order. */
export const TOGGLEABLE_LABELS = [AGENT_READY, AGENT_INVESTIGATE] as const;

/** Labels that mean "an orchestrator picks this up" — used to highlight rows
 *  already queued for agents. */
const AGENT_GO_LABELS = new Set<string>([AGENT_READY, AGENT_INVESTIGATE]);

/** True when the issue already carries a go-signal label, i.e. an orchestrator
 *  running on this repo will (or already did) pull it onto its board. */
export function isLabeledForAgents(issue: GhIssue): boolean {
  return issue.labels.some((l) => AGENT_GO_LABELS.has(l));
}

/** Case-insensitive match of `query` against an item's number (with or without
 *  a leading `#`), title, and label names. Empty/whitespace query matches all.
 *  Generic over `ListItem` so issues and PRs share it. */
export function matchesQuery(issue: ListItem, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (q === "") return true;
  const bare = q.startsWith("#") ? q.slice(1) : q;
  if (String(issue.number).includes(bare)) return true;
  if (issue.title.toLowerCase().includes(q)) return true;
  return issue.labels.some((l) => l.toLowerCase().includes(q));
}

/** Filter to items matching `query`, then sort newest-updated first. Stable
 *  for equal timestamps (falls back to descending number) so the order never
 *  jitters between refreshes. Does not mutate the input. Generic over
 *  `ListItem`, so the same call serves the issues and PR lists. */
export function filterAndSortIssues<T extends ListItem>(issues: T[], query: string): T[] {
  return issues
    .filter((i) => matchesQuery(i, query))
    .slice()
    .sort((a, b) => {
      const ta = Date.parse(a.updated_at);
      const tb = Date.parse(b.updated_at);
      // NaN (unparseable timestamp) sorts last rather than poisoning the order.
      const va = Number.isNaN(ta) ? -Infinity : ta;
      const vb = Number.isNaN(tb) ? -Infinity : tb;
      if (vb !== va) return vb - va;
      return b.number - a.number;
    });
}

/** The add/remove delta needed to bring `label`'s presence to `desired`, given
 *  the issue's `current` labels. Only ever touches `label` — every other label
 *  (e.g. an orchestrator's `agent-managed`) is left untouched. Idempotent: a
 *  no-op (already in the desired state) yields empty add and remove lists, so
 *  the view can skip the backend call entirely. */
export function labelDelta(
  current: string[],
  label: string,
  desired: boolean
): { add: string[]; remove: string[] } {
  const has = current.includes(label);
  if (desired && !has) return { add: [label], remove: [] };
  if (!desired && has) return { add: [], remove: [label] };
  return { add: [], remove: [] };
}

export interface NewIssueDraft {
  title: string;
  body: string;
}

export type NewIssueValidation =
  | { ok: true; title: string; body: string }
  | { ok: false; error: string };

/** Validate a new-issue form: a non-empty title is required; the body is
 *  optional. Trims both so trailing whitespace never becomes a "valid" title. */
export function validateNewIssue(draft: NewIssueDraft): NewIssueValidation {
  const title = draft.title.trim();
  if (title === "") return { ok: false, error: "A title is required." };
  return { ok: true, title, body: draft.body.trim() };
}

export type CommentValidation =
  | { ok: true; body: string }
  | { ok: false; error: string };

/** Validate a comment before posting: a non-empty (post-trim) body is required.
 *  Mirrors the backend guard in `gh_issue_comment` / `gh_pr_comment` (which
 *  reject an empty `--body`), so the view fails fast without a round-trip. The
 *  returned `body` is NOT trimmed — GitHub markdown can be leading/trailing
 *  whitespace-sensitive (code fences, indentation); we only gate on emptiness. */
export function validateComment(body: string): CommentValidation {
  if (body.trim() === "") return { ok: false, error: "Write a comment first." };
  return { ok: true, body };
}

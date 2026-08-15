// Bindings to the Rust `gh` backend (src-tauri/src/gh.rs) for the per-pane
// issues view. Everything shells out to the authenticated `gh` CLI — loomux
// stores no token; `gh` inherits the user's existing `gh auth login`.
//
// Per hard constraint 5, view code never touches Tauri IPC directly: it goes
// through these thin, typed wrappers (the src/git.ts precedent). `repo` must be
// the repository ROOT — resolve it once with gitRepoRoot (from ./git), exactly
// as the git view does.

import { invoke } from "./transport.ts";

/** A GitHub issue, as returned by `gh issue list --json`. `labels` is the flat
 *  list of label names (matched client-side — see issuesmodel.ts — because the
 *  orchestrator template warns `gh`'s server-side `--label` filter has returned
 *  empty results for issues that carry the label). */
export interface GhIssue {
  number: number;
  title: string;
  labels: string[];
  /** "OPEN" | "CLOSED" (v1 only ever lists open). */
  state: string;
  /** ISO-8601 timestamp of the last update. */
  updated_at: string;
  url: string;
}

/** A pull request, as returned by `gh pr list --json`. Mirrors GhIssue (same
 *  filter/sort mechanics) plus `head_ref` (the source branch). Read-only in the
 *  view: PRs can be listed, opened, and commented on — never labelled/merged. */
export interface GhPr {
  number: number;
  title: string;
  /** "OPEN" | "CLOSED" | "MERGED" (v1 only lists open). */
  state: string;
  labels: string[];
  updated_at: string;
  url: string;
  /** The PR's source (head) branch name. */
  head_ref: string;
}

/** One comment on an issue or PR (`comments` field of `gh {issue,pr} view`).
 *  Every field is GitHub-authored text — render with textContent ONLY (the #129
 *  no-innerHTML-on-GitHub-data XSS boundary), never innerHTML. */
export interface GhComment {
  /** Commenter login, or null for a deleted/ghost account. */
  author: string | null;
  created_at: string;
  body: string;
}

/** Full detail for an issue or PR (`gh {issue,pr} view --json`). One shape backs
 *  both detail panes. `body` is the markdown description verbatim (also
 *  GitHub-authored — textContent only). */
export interface GhDetail {
  title: string;
  body: string;
  labels: string[];
  state: string;
  author: string | null;
  comments: GhComment[];
}

/** Result of `gh auth status` — drives the empty-state. `login` is the
 *  authenticated account when known. */
export interface GhAuth {
  installed: boolean;
  authenticated: boolean;
  login: string | null;
}

/** The new issue's identity, returned by a create. */
export interface GhCreated {
  number: number;
  url: string;
}

/** Is `gh` installed and is the user authenticated? Runs `gh auth status`.
 *  Never rejects for the "not installed / not logged in" case — those are
 *  reported in the returned struct so the view can render a clear empty-state
 *  rather than a toast. */
export const ghAuthStatus = (): Promise<GhAuth> => invoke("gh_auth_status");

/** Open issues for `repo` (first page, ~50), newest-updated first. */
export const ghIssueList = (repo: string): Promise<GhIssue[]> =>
  invoke("gh_issue_list", { repo });

/** Create an issue from a title + body; resolves to its number and URL. */
export const ghIssueCreate = (
  repo: string,
  title: string,
  body: string
): Promise<GhCreated> => invoke("gh_issue_create", { repo, title, body });

/** Add and/or remove labels on issue `number`. The backend validates every
 *  label against a fixed allow-list (agent-ready / agent-investigation /
 *  agent-managed / agent-hold) and rejects anything else, so a malformed call
 *  fails loudly rather than attaching an arbitrary label. `agent-hold` (#778) is
 *  the human veto under full autonomy — writable from here because applying it
 *  IS the veto gesture. */
export const ghIssueSetLabels = (
  repo: string,
  number: number,
  add: string[],
  remove: string[]
): Promise<void> => invoke("gh_issue_set_labels", { repo, number, add, remove });

/** Full detail (description + comments) for one issue — backs the detail pane. */
export const ghIssueView = (repo: string, number: number): Promise<GhDetail> =>
  invoke("gh_issue_view", { repo, number });

/** Post a comment on an issue. The backend passes `body` as the value of
 *  `--body`, so its content (leading `-`, newlines) is data, not a flag; an
 *  empty/whitespace body is rejected there. */
export const ghIssueComment = (
  repo: string,
  number: number,
  body: string
): Promise<void> => invoke("gh_issue_comment", { repo, number, body });

/** Open pull requests for `repo` (first page, ~50). Read-only in the view. */
export const ghPrList = (repo: string): Promise<GhPr[]> =>
  invoke("gh_pr_list", { repo });

/** Full detail (description + comments) for one PR — same detail pane as issues. */
export const ghPrView = (repo: string, number: number): Promise<GhDetail> =>
  invoke("gh_pr_view", { repo, number });

/** Post a comment on a PR (the one write read-only PR mode allows). Same
 *  discrete-`--body` safety and empty-body guard as ghIssueComment. */
export const ghPrComment = (
  repo: string,
  number: number,
  body: string
): Promise<void> => invoke("gh_pr_comment", { repo, number, body });

/** One issue's lifecycle timestamps for the progress-timeline view (#608).
 *  Distinct from GhIssue, which is the issues-view row (open-state only,
 *  `updated_at` alone): a time axis plots when something *happened*, so this
 *  carries the opened/closed instants. All timestamps are RFC-3339 strings —
 *  parse with `Date.parse`; an unparseable one (see `created_at`) is parked as
 *  "undatable", never plotted at the epoch. */
export interface GhIssueActivity {
  number: number;
  title: string;
  /** "OPEN" | "CLOSED". */
  state: string;
  /** Open time. Empty when gh omitted it — treat as undatable, not as 1970. */
  created_at: string;
  /** Close time; null while the issue is open. */
  closed_at: string | null;
  /** Last-activity time — the key the list is sorted by, and what gives the
   *  precise coverage floor when the list is truncated: nothing omitted was
   *  active more recently than the oldest row here. */
  updated_at: string;
  url: string;
}

/** One PR's lifecycle timestamps. Mirrors GhIssueActivity plus `merged_at` and
 *  the head branch. GitHub closes a PR when it merges it, so a merged PR has
 *  BOTH `closed_at` and `merged_at`: key "merged" off `merged_at` only, or a PR
 *  closed unmerged renders as a merge that never happened. */
export interface GhPrActivity {
  number: number;
  /** "OPEN" | "CLOSED" | "MERGED". */
  state: string;
  title: string;
  created_at: string;
  closed_at: string | null;
  /** Merge time; null for an open PR and for one closed unmerged. */
  merged_at: string | null;
  updated_at: string;
  url: string;
  head_ref: string;
}

/** Issue + PR lifecycle activity for one repo, carrying its own coverage
 *  boundary. Each list is capped at `limit` server-side; when the matching
 *  `*_truncated` flag is set, older activity exists beyond what's here and the
 *  view MUST say so — a timeline that looks complete when it isn't is the
 *  failure this feature is most exposed to. `limit` crosses the wire rather
 *  than being duplicated here so the note can't drift from the query. */
export interface GhActivity {
  issues: GhIssueActivity[];
  prs: GhPrActivity[];
  limit: number;
  issues_truncated: boolean;
  prs_truncated: boolean;
}

/** Issue/PR lifecycle activity for `repo`, most-recently-active first — the gh
 *  half of the progress-timeline view. Read-only. Rejects if `gh` fails, rather
 *  than resolving with half the timeline: a silently absent PR list reads as a
 *  quiet period, which is worse than an error the view can render. */
export const ghActivity = (repo: string): Promise<GhActivity> =>
  invoke("gh_activity", { repo });

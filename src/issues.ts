// Bindings to the Rust `gh` backend (src-tauri/src/gh.rs) for the per-pane
// issues view. Everything shells out to the authenticated `gh` CLI — loomux
// stores no token; `gh` inherits the user's existing `gh auth login`.
//
// Per hard constraint 5, view code never touches Tauri IPC directly: it goes
// through these thin, typed wrappers (the src/git.ts precedent). `repo` must be
// the repository ROOT — resolve it once with gitRepoRoot (from ./git), exactly
// as the git view does.

import { invoke } from "@tauri-apps/api/core";

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
 *  label against a fixed allow-list (agent-ready / agent-investigate /
 *  agent-managed) and rejects anything else, so a malformed call fails loudly
 *  rather than attaching an arbitrary label. */
export const ghIssueSetLabels = (
  repo: string,
  number: number,
  add: string[],
  remove: string[]
): Promise<void> => invoke("gh_issue_set_labels", { repo, number, add, remove });

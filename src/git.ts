// Bindings to the Rust git backend. All commands shell out to the system
// `git` CLI; `repo` must be the repository ROOT (paths git returns are
// root-relative) — resolve it once with gitRepoRoot.

import { invoke } from "@tauri-apps/api/core";

export interface RefInfo {
  name: string;
  kind: "branch" | "remote" | "tag" | "head";
}

export interface CommitInfo {
  hash: string;
  parents: string[];
  author: string;
  /** Committer name — differs from `author` for rebased / cherry-picked
   *  commits, so the row can label who actually committed. */
  committer: string;
  /** Author time, unix seconds. */
  timestamp: number;
  subject: string;
  refs: RefInfo[];
}

export interface BranchInfo {
  name: string;
  kind: "local" | "remote";
  /** True for the currently checked-out branch. */
  current: boolean;
}

export interface FileEntry {
  path: string;
  /** Original path for renames/copies. */
  orig_path: string | null;
  /** One-letter status: M A D R C U. */
  status: string;
}

export interface GitStatus {
  branch: string | null;
  detached: boolean;
  /** True when the repo has no commits yet. */
  empty: boolean;
  staged: FileEntry[];
  unstaged: FileEntry[];
  untracked: string[];
}

export type DiffMode = "worktree" | "staged" | "commit" | "untracked";

/** Repo root containing `cwd`, or null when not inside a git repo. */
export const gitRepoRoot = (cwd: string): Promise<string | null> =>
  invoke("git_repo_root", { cwd });

export const gitLog = (repo: string, limit: number): Promise<CommitInfo[]> =>
  invoke("git_log", { repo, limit });

export const gitStatus = (repo: string): Promise<GitStatus> => invoke("git_status", { repo });

export const gitDiff = (
  repo: string,
  path: string,
  mode: DiffMode,
  hash?: string
): Promise<string> => invoke("git_diff", { repo, path, mode, hash: hash ?? null });

export const gitCommitFiles = (repo: string, hash: string): Promise<FileEntry[]> =>
  invoke("git_commit_files", { repo, hash });

export const gitStage = (repo: string, paths: string[]): Promise<void> =>
  invoke("git_stage", { repo, paths });

export const gitUnstage = (repo: string, paths: string[], emptyRepo: boolean): Promise<void> =>
  invoke("git_unstage", { repo, paths, emptyRepo });

export const gitCommit = (repo: string, message: string): Promise<void> =>
  invoke("git_commit", { repo, message });

export const gitCheckout = (repo: string, refname: string, track: boolean): Promise<void> =>
  invoke("git_checkout", { repo, refname, track });

export const gitDiscard = (repo: string, path: string, untracked: boolean): Promise<void> =>
  invoke("git_discard", { repo, path, untracked });

/** Create a worktree named `name` beside the repo (branch of the same name,
 *  created if needed). The branch is cut from `base` — omit it to cut from the
 *  repo's default branch, fetched fresh from origin (#204), never the primary
 *  checkout's incidental HEAD. `base` is ignored when `name` already exists.
 *  Resolves to the worktree's absolute path. */
export const gitWorktreeAdd = (repo: string, name: string, base?: string): Promise<string> =>
  invoke("git_worktree_add", { repo, name, base: base ?? null });

// -- remote & history ops --

/** Fetch + prune from remotes (no-op on a repo with no remote configured). */
export const gitFetch = (repo: string, remote?: string): Promise<void> =>
  invoke("git_fetch", { repo, remote: remote ?? null });

/** Push the current branch. `setUpstream` publishes it to the first remote and
 *  sets tracking; otherwise a plain push (needs an upstream already set). */
export const gitPush = (repo: string, setUpstream: boolean): Promise<void> =>
  invoke("git_push", { repo, setUpstream });

/** Fast-forward-only pull — fails (never merges) when the branch has diverged. */
export const gitPull = (repo: string): Promise<void> => invoke("git_pull", { repo });

export const gitTag = (repo: string, name: string, hash: string): Promise<void> =>
  invoke("git_tag", { repo, name, hash });

export const gitBranchCreate = (
  repo: string,
  name: string,
  hash: string,
  checkout: boolean
): Promise<void> => invoke("git_branch_create", { repo, name, hash, checkout });

export const gitCherryPick = (repo: string, hash: string): Promise<void> =>
  invoke("git_cherry_pick", { repo, hash });

export const gitRevert = (repo: string, hash: string): Promise<void> =>
  invoke("git_revert", { repo, hash });

export const gitMerge = (repo: string, refname: string): Promise<void> =>
  invoke("git_merge", { repo, refname });

export const gitRebase = (repo: string, upstream: string): Promise<void> =>
  invoke("git_rebase", { repo, upstream });

/** All local and remote-tracking branches (for the checkout menu). */
export const gitBranches = (repo: string): Promise<BranchInfo[]> =>
  invoke("git_branches", { repo });

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
  /** Author time, unix seconds. */
  timestamp: number;
  subject: string;
  refs: RefInfo[];
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

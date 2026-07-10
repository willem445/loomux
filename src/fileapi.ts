// Typed bridge to the Rust file-editor backend (issue #174). Mirrors the
// per-feature wrapper precedent set by `git.ts`/`gh.ts`: every `fileedit`
// capability is a `#[tauri::command]` fronted by a typed wrapper here, and no
// other frontend module calls `invoke` for these. (CLAUDE.md constraint #5
// names `pty.ts`; `git.ts` established a dedicated module per feature, which
// this follows for cohesion — flagged in the PR for the human's call.)
//
// Every command takes a `root` (the pane's live cwd) plus a `rel` path relative
// to it; ALL path safety is enforced server-side (see fileedit.rs).

import { invoke } from "@tauri-apps/api/core";

/** One entry in a directory listing. Symlinks are shown but never expanded. */
export interface FtEntry {
  name: string;
  is_dir: boolean;
  is_symlink: boolean;
  size: number;
}

/** A file's decoded contents plus the hash to echo back on save. */
export interface FileRead {
  content: string;
  hash: string;
  truncated: boolean;
}

export interface WriteResult {
  hash: string;
}

export interface SearchMatch {
  rel: string;
  /** 1-based. */
  line: number;
  /** 1-based character column. */
  col: number;
  line_text: string;
}

export interface SearchOutcome {
  matches: SearchMatch[];
  /** A cap cut the walk short — results are partial. */
  truncated: boolean;
}

export interface SearchOpts {
  case_insensitive: boolean;
  whole_word: boolean;
  /** 0 = backend default (capped at its own ceiling). */
  max_results: number;
}

export interface ChangedFile {
  rel: string;
  replacements: number;
}

export interface SkippedFile {
  rel: string;
  reason: string;
}

export interface ReplaceResult {
  changed: ChangedFile[];
  skipped: SkippedFile[];
}

/** Machine-readable code the backend prefixes onto every error string (before
 *  the first ": "). Kept in sync with the `err(code, …)` calls in fileedit.rs
 *  so the UI can branch (conflict → reload/overwrite; binary/too-large → explain
 *  why a file won't open) without parsing prose. */
export type FileEditError =
  | "conflict"
  | "binary"
  | "too-large"
  | "not-found"
  | "not-dir"
  | "is-dir"
  | "outside-root"
  | "invalid-path"
  | "symlink"
  | "empty-query"
  | "no-match"
  | "io"
  | "unknown";

/** Extract the leading error code from a rejected command's error. Any value
 *  that isn't a known code (including a non-string) collapses to "unknown". */
export function errorCode(e: unknown): FileEditError {
  const msg = typeof e === "string" ? e : e instanceof Error ? e.message : String(e ?? "");
  const code = msg.split(":", 1)[0]?.trim() ?? "";
  const known: FileEditError[] = [
    "conflict",
    "binary",
    "too-large",
    "not-found",
    "not-dir",
    "is-dir",
    "outside-root",
    "invalid-path",
    "symlink",
    "empty-query",
    "no-match",
    "io",
  ];
  return (known as string[]).includes(code) ? (code as FileEditError) : "unknown";
}

/** Human-readable prose part of a backend error (everything after the code). */
export function errorMessage(e: unknown): string {
  const msg = typeof e === "string" ? e : e instanceof Error ? e.message : String(e ?? "");
  const idx = msg.indexOf(":");
  return idx >= 0 ? msg.slice(idx + 1).trim() : msg;
}

export const ftListDir = (root: string, rel: string): Promise<FtEntry[]> =>
  invoke("ft_list_dir", { root, rel });

export const ftReadFile = (root: string, rel: string): Promise<FileRead> =>
  invoke("ft_read_file", { root, rel });

export const ftWriteFile = (
  root: string,
  rel: string,
  content: string,
  expectedHash: string | null
): Promise<WriteResult> =>
  invoke("ft_write_file", { root, rel, content, expectedHash });

export const ftSearch = (
  root: string,
  query: string,
  opts: SearchOpts
): Promise<SearchOutcome> => invoke("ft_search", { root, query, opts });

export const ftReplace = (
  root: string,
  query: string,
  replacement: string,
  files: string[],
  opts: SearchOpts
): Promise<ReplaceResult> =>
  invoke("ft_replace", { root, query, replacement, files, opts });

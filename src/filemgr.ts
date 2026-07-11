// Typed bridge to the Rust file-MANAGER backend (issue #214). Follows the
// per-feature wrapper precedent set by `git.ts` / `gh.ts` / `fileapi.ts` — a
// self-contained feature gets its own wrapper module, and no other frontend module
// calls `invoke` for these commands (the README's "Extension seams" names this as
// the sanctioned alternative to piling everything into `pty.ts`).
//
// Every command takes the pane's `root` plus a `rel` path relative to it. ALL path
// safety is enforced server-side (filemgr.rs): containment, the refusal to act on
// the root itself, and the name rules. The mirror checks in `fileexplorermodel.ts`
// exist to answer the user WHILE THEY TYPE — they are a courtesy, never the
// boundary.

import { invoke } from "@tauri-apps/api/core";
import type { FmEntry } from "./fileexplorermodel";

export type { FmEntry };

/** List one directory under `root`. Returns entries UNSORTED and UNFILTERED —
 *  ordering and the hidden-files filter are product decisions and live in the pure
 *  model (`visibleEntries`), where they're tested. */
export const fmList = (root: string, rel: string): Promise<FmEntry[]> =>
  invoke("fm_list", { root, rel });

/** Create a folder named `name` inside `rel`. Resolves to the new entry's `rel`.
 *  Rejects (never silently no-ops) if the name is taken. */
export const fmNewFolder = (root: string, rel: string, name: string): Promise<string> =>
  invoke("fm_new_folder", { root, rel, name });

/** Create an EMPTY file named `name` inside `rel`. Resolves to the new entry's `rel`.
 *  Refuses to clobber — and, crucially, refuses without truncating: an existing file
 *  keeps its contents. It is not opened afterwards; the user's double-click is what
 *  hands it to their default app. */
export const fmNewFile = (root: string, rel: string, name: string): Promise<string> =>
  invoke("fm_new_file", { root, rel, name });

/** Rename the entry at `rel` to `name`, in place. Resolves to its new `rel`.
 *  `name` is one path segment, so this can only re-label — never move. */
export const fmRename = (root: string, rel: string, name: string): Promise<string> =>
  invoke("fm_rename", { root, rel, name });

/** Delete the entry at `rel`. Resolves to `true` when it went to the Recycle Bin
 *  (recoverable) and `false` when it was destroyed — see `fmDeleteMode`, which the
 *  confirmation dialog uses so it can promise the truth BEFORE the user commits. */
export const fmDelete = (root: string, rel: string): Promise<boolean> =>
  invoke("fm_delete", { root, rel });

/** What a delete will actually do on this platform: "recycle" (Windows — the
 *  Recycle Bin, via SHFileOperationW) or "permanent" (elsewhere, where there is no
 *  bin without a new dependency the getrandom ban makes hard to justify). */
export const fmDeleteMode = (): Promise<{ mode: "recycle" | "permanent" }> =>
  invoke("fm_delete_mode");

/** Hand the file at `rel` to the OS default application for its extension — what a
 *  double-click in Explorer does. Loomux does not open, read, or interpret it.
 *  Directories are refused (navigating into one is the pane's own job). */
export const fmOpen = (root: string, rel: string): Promise<void> =>
  invoke("fm_open", { root, rel });

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
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { FmEntry } from "./fileexplorermodel";
import type { FmCaps } from "./filemenu";
import type { HashAlgo } from "./filehashmodel";

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

/** What this platform can actually do. Probed once per pane and consulted when the
 *  context menu is built, so an item that would always fail is HIDDEN rather than
 *  shown-and-broken, and one that is approximate (Linux reveal) says so. */
export const fmCapabilities = (): Promise<FmCaps> => invoke("fm_capabilities");

/** Show the OS "Open with" chooser for `rel`. Windows only — `fmCapabilities().open_with`
 *  says whether the item should even be offered. */
export const fmOpenWith = (root: string, rel: string): Promise<void> =>
  invoke("fm_open_with", { root, rel });

/** Show `rel` in the OS file manager, with the entry selected (Windows/macOS). On Linux
 *  there is no portable reveal, so it opens the containing folder and selects nothing —
 *  `fmCapabilities().reveal_selects` reports which you are getting. */
export const fmReveal = (root: string, rel: string): Promise<void> =>
  invoke("fm_reveal", { root, rel });

/** One file's hash outcome. Exactly one of `digest`/`error` is set. */
export interface HashResult {
  rel: string;
  digest?: string;
  error?: string;
}

/** One streamed batch of hash results, tagged with the caller's id so batches from a
 *  superseded run (the user navigated away) are dropped. */
export interface HashBatch {
  id: number;
  algo: HashAlgo;
  results: HashResult[];
  done: boolean;
}

/** Hash `rels` under `root` on a worker thread (#214). Returns as soon as the thread is
 *  spawned; results arrive as `fm-hash` events tagged with `id`, and `ftSearchCancel(id)`
 *  stops it — one registry and one cancel command serve the search, the name index, and
 *  this. Never blocks: a directory of large files must not freeze the window, which is
 *  exactly what a synchronous hash command would do (Tauri runs those on the main thread).
 *
 *  Used for BOTH the listing column (many rels) and the Hash → submenu (one rel), so
 *  there is one place hashing can be wrong and it is the tested one. */
export const fmHashStart = (
  id: number,
  root: string,
  rels: string[],
  algo: HashAlgo
): Promise<void> => invoke("fm_hash_start", { id, root, rels, algo });

/** Subscribe to streamed hash batches. Each view filters by its own active id. */
export const onHashBatch = (cb: (batch: HashBatch) => void): Promise<UnlistenFn> =>
  listen<HashBatch>("fm-hash", (e) => cb(e.payload));

/** Hand the file at `rel` to the OS default application for its extension — what a
 *  double-click in Explorer does. Loomux does not open, read, or interpret it.
 *  Directories are refused (navigating into one is the pane's own job). */
export const fmOpen = (root: string, rel: string): Promise<void> =>
  invoke("fm_open", { root, rel });

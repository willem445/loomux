# Design: open-in-editor

Status: implemented (issue #25).

## Problem

Loomux is a terminal multiplexer, not a code editor. Users occasionally need a
real editor (VS Code, Zed, …) to review or modify files in a pane's repository.
Rather than build an editor, add a one-click "open this pane's folder in my
editor" action with a configurable editor command.

## Principles

1. **No shell string, ever.** The backend never builds a command line and hands
   it to a shell. The editor is spawned via a direct argument vector with the
   directory as a single element, so a path (or the configured editor value)
   can never be reinterpreted as shell syntax — no injection surface.
2. **Detached, fire-and-forget.** The editor is a long-lived GUI app that must
   outlive the pane and loomux itself. It is spawned detached (no inherited
   console, own process group) and its handle is dropped without waiting.
3. **Client-side setting.** The editor command lives in `localStorage`
   alongside loomux's other settings — there is no server-side config file.
4. **Fail loud but soft.** Every failure mode (unconfigured, missing folder,
   editor-not-found, spawn error) returns a user-facing string that the
   frontend shows as a transient toast — never a silent no-op.

## Backend (`src-tauri/src/editor.rs`)

`open_in_editor(editor, dir) -> Result<(), String>`:

1. **Validate** — editor non-empty (trimmed); dir non-empty and an existing
   directory.
2. **Resolve** the editor to a concrete executable. An explicit path is used
   verbatim; a bare name is looked up on a freshly-resolved `PATH`, trying the
   name as-is and then with each `PATHEXT` extension. This is what makes bare
   `code` work on Windows, where the launcher is `code.cmd` (a batch shim that
   `CreateProcess` can't exec directly). Resolving to the real `code.cmd` path
   lets Rust's std route it through `cmd.exe` **with proper argument
   escaping** — the [BatBadBut](https://blog.rust-lang.org/2024/04/09/cve-2024-24576.html)
   fix (std ≥ 1.77.2) — preserving guarantee (1) for batch-file editors too.
3. **Spawn** detached with `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` on
   Windows, stdio null, `current_dir(dir)`, and the fresh `PATH`.

The pure pieces (`validate`, `editor_args`, `resolve_program`) are split out so
argument-vector construction, PATH/PATHEXT resolution, and every error path are
unit-tested without spawning a process.

## Frontend

- `src/editor.ts` — the persisted `loomux.editorCommand` setting, the
  `open_in_editor` invoke wrapper, and a small config modal (reusing the
  existing `.agent-dialog` styles).
- `src/pane.ts` — a `</>` button in each pane header (additive; no layout
  restructure, which issue #26 owns). Left-click opens the pane's current
  folder; right-click reconfigures. `Pane.openInEditor()` is also driven by the
  `Alt+E` shortcut.
- `src/toast.ts` — a lightweight transient toast for the non-fatal error
  notices, distinct from `main.ts`'s full-screen fatal banner.

## Why not a shell / `code --wait` / an arg string in the setting

Supporting flags in the editor setting would mean tokenizing a user string into
program + args, which breaks on paths with spaces and reopens the very
quoting/injection questions principle (1) exists to avoid. The setting is a
single program (name or path); loomux always appends exactly the directory. If
per-editor flags are ever needed, add a separate, explicitly-typed args field
rather than parsing one string.

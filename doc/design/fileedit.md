# Design: in-app file editor (`fileedit`)

Status: implemented (issue #174).

## Problem

Loomux is a terminal multiplexer, not an IDE — but when an agent (or you) is
working in a pane, jumping to an external editor for a quick look or a one-line
fix is heavyweight, and the external-editor launcher (`Alt+E`, `editor.rs`) opens
a *whole other app*. #174 asks for a lightweight, in-app file tree + code editor
+ project-wide search-and-replace, available in **every** pane type — agent
panes, the orchestrator, and plain terminals alike.

This is a distinct feature from the external-editor launcher. To avoid confusion
everything new is namespaced `fileedit*` / `FileEditView`; `editor.ts` /
`editor.rs` and `Alt+E` remain the external launcher, untouched. The new overlay
is `Alt+F`.

## Principles

1. **Never resize the PTY** (CLAUDE.md constraint #1). The overlay floats over
   the top of the terminal exactly like the git/audit views (`.git-overlay`), and
   the terminal is shifted *visually* (CSS transform) — the ConPTY grid is never
   resized, so no repaint floods scrollback. Only one overlay is open at a time.
2. **Available everywhere.** Unlike the audit/task/group overlays (gated on an
   orchestration role), the file overlay is ungated: its header button and `Alt+F`
   work in plain terminal panes too. The tree roots at the pane's live `cwdRaw`
   (shell-integration cwd for a terminal, the agent worktree for a worker/reviewer,
   repo root for the orchestrator).
3. **All path safety is server-side** (CLAUDE.md constraint #6). The webview is
   trusted, but paths still cross the IPC boundary, so the backend is the single
   authority: it rejects absolute `rel`, folds `.`/`..` lexically, checks the
   result stays within the root, and refuses to traverse any symlinked component.
   Defense-in-depth, not decoration.
4. **Durable, conflict-aware writes.** Saves are atomic (temp + fsync + rename,
   the #133/#161 pattern) and guarded by a content hash: if the file changed on
   disk since it was opened, the write is refused and the user chooses
   overwrite / reload / cancel. Editing a *running agent's* worktree is legitimate
   but risky, so the view shows a subtle banner and relies on the conflict guard
   rather than blocking.
5. **No new backend crates** (CLAUDE.md constraint #2 — the getrandom/ProcessPrng
   ban). The content hash is a std-only FNV-1a; temp names use `AtomicU64` +
   `process::id()`; search is a pure-`std` walker. Nothing pulls uuid/rand/
   tempfile-in-prod.
6. **Testable core, human-validated chrome.** The decision logic lives in
   DOM-free modules (`filetreemodel`, `fileicons`, `searchresults`, `dirtystate`)
   covered by `node:test`, and the backend logic in `pub fn`s exercised by a Rust
   integration test. The DOM wiring (`FileEditView`, CodeMirror mount, overlay
   geometry, keybindings, WebView2 accelerator behaviour) is validated by hand —
   no DOM simulation in the tests, by house convention.

## Backend (`src-tauri/src/fileedit.rs`)

Six `#[tauri::command]`s, each a thin wrapper over a `pub fn` the integration
test drives directly (search is split into start/cancel for the streaming worker,
#207). Every path-taking one routes through one `safe_resolve` choke point.

| Command | Purpose |
| --- | --- |
| `ft_list_dir(root, rel)` | **Lazy** — one directory per call (expand-on-demand). Dirs-first sort; symlinks flagged but reported non-expandable. |
| `ft_read_file(root, rel)` | Rejects binary (NUL in the first 8 KB or invalid UTF-8) and files > 2 MiB with typed errors; returns content + FNV-1a hash. |
| `ft_write_file(root, rel, content, expected_hash?)` | Atomic durable write; if `expected_hash` is set and the on-disk hash differs → `conflict`, file untouched. |
| `ft_search_start(id, root, query, opts)` | **Streaming, off-thread** (#207): spawns a worker that walks and emits `ft-search` batches tagged with `id`; polls a per-`id` cancel flag. Enumerates via `git ls-files` by default (gitignore-aware), or the full `std` walker when `include_ignored` is set / the root isn't a git repo. Literal / case-insensitive / whole-word; bounded, flags truncation. |
| `ft_search_cancel(id)` | Flip search `id`'s cancel flag (a newer keystroke or `Esc`). Idempotent. |
| `ft_replace(root, query, replacement, files, opts)` | Applies only the confirmed files (the UI previews the search first); re-reads + re-matches + writes each atomically; one bad file is skipped, never a partial write. |

**Errors** are plain strings (house style) but each starts with a stable machine
code (`conflict:`, `binary:`, `too-large:`, `symlink:`, `outside-root:`, …) so the
frontend can branch without parsing prose (`errorCode` in `fileapi.ts`).

**Path safety** (`safe_resolve` → `lexical_normalize` + within-root check +
`ensure_no_symlink`): we deliberately do **not** `fs::canonicalize` (it returns a
`\\?\`-verbatim path Windows toolchains mishandle — the same reason
`pty::lexical_normalize` avoids it). Because we don't canonicalize, a symlinked
component is the one remaining way a lexically-in-root path could redirect
outside, so we walk each component below the root with `symlink_metadata` and
refuse any symlink.

**Search performance & the non-blocking rework (#207).** The v1 `ft_search` was a
*synchronous* command. Tauri runs sync commands on the main (webview) thread, so a
full-tree walk that reads every candidate file froze the whole UI for its
duration — and the 300 ms-debounced auto-search relaunched that walk on every
keystroke. On the main checkout (~19k files once node_modules + the Rust build dir
are counted) an `include_ignored` walk for `"fn "` took **~62 s cold** / ~3 s warm;
even the tracked-only walk was ~1.6 s cold — every millisecond of which was a hard
UI freeze under the old command.

The rework keeps the dependency-free `std` matcher (still getrandom-safe — no
ripgrep/`ignore` crate, which would need a `cargo tree -i getrandom` vet) but
changes the *structure*:

- **Off the UI thread.** `ft_search_start` spawns a worker thread; the walk never
  touches the webview thread, so the UI stays live no matter how big the tree.
- **Cancellable.** Each search carries a frontend-issued id and an `AtomicBool`
  cancel flag in a `SearchRegistry` (Tauri-managed, same shape as `GitWatcher`);
  the worker polls it between files, so a superseded keystroke or `Esc` stops the
  walk promptly instead of letting 62 s walks pile up.
- **Streaming + capped.** Matches are emitted in 256-match batches as they're
  found; the frontend accumulates at most `RENDER_CAP` (2 000) so a 10k-hit search
  can't lock the DOM, and the tree render is throttled to one repaint per frame.
- **Gitignore-aware by default.** Enumeration is `git ls-files --cached --others
  --exclude-standard` (tracked + untracked-unignored) so `.gitignore` is respected
  for free — on that ~19k-file tree the default search walks the 248 tracked files
  in **~160 ms warm**. The **Ignored files** toggle (`include_ignored`) switches to
  the full walk; a non-git root has no `.gitignore` to respect, so the toggle is
  disabled there and the whole folder is always searched (the walk still applies
  the heuristic `node_modules`/`target`/… excludes as a best-effort ignore, and
  always skips `.git`). `git ls-files` is a plain subprocess — still no new crate.

A `regex` mode remains the obvious follow-up.

## Frontend

- **`fileapi.ts`** — typed `invoke` wrappers for the five commands + `errorCode`.
  A per-feature wrapper module, mirroring the `git.ts` / `gh.ts` precedent.
  (CLAUDE.md constraint #5 literally names `pty.ts`; `git.ts` established the
  dedicated-module pattern, which this follows for cohesion — no module calls
  `invoke` for `fileedit` except this one.)
- **Pure, DOM-free, `node:test`-covered:**
  - `filetreemodel.ts` — tree node model, dirs-first sort, a hardened `safeJoin`
    (rejects `..`/separators/absolute), a lazy-child merge that preserves
    expansion across a re-list, and expansion → flattened visible rows.
  - `fileicons.ts` — filename → category → inline 16×16 `currentColor` SVG (no
    icon font/sprite); robust to case, multi-dot, dotfiles, unknown extensions.
  - `searchresults.ts` — group flat matches by file, per-file replace selection,
    confirmed-set + count summaries.
  - `searchsession.ts` — the streaming-search state machine (#207): `accept`
    folds a batch into a session only when its id matches (dropping a
    cancelled/superseded search's late batches — the cancellation guarantee),
    caps accumulation at `RENDER_CAP`, and `enumerationSource` mirrors the
    backend's gitignore-vs-walk choice.
  - `dirtystate.ts` — close-guard and conflict decisions.
  - `eol.ts` — line-ending detect / normalize / re-apply (see "Dirty tracking").
- **`editorwidget.ts`** — the swappable editor seam (see below).
- **`fileedit.ts` (`FileEditView`)** — DOM wiring only: the search box sits ABOVE
  the tree in the left column and drives in-tree hit highlighting (below); lazy
  tree render, editor mount, save / dirty dot / Ctrl+S, the conflict + discard
  dialogs (reusing the `.dlg-*` kit), the folder picker, and the agent-worktree
  banner. Wired into every pane via `pane.ts` (`toggleFileEditView`, unconditional
  header button, added to the close-every-other-overlay blocks + `activeOverlay` +
  `dispose`), `shortcuts.ts` (`Alt+F`), and `main.ts` (dispatch).

## Search + in-tree highlighting

Rather than a separate results panel, the search box lives at the top of the
left column and highlights matching files directly in the tree (a lightweight
take on VS Code): each hit file gets an accent highlight and a clickable
match-count badge (the badge toggles whether that file is in the replace set).
Typing debounces a search so highlights update live; the branches leading to
hits auto-expand so they're visible; clicking a hit file opens it and jumps to
its first match. Opening a hit file also pushes the active query into the editor
(`setHighlightQuery`) so every occurrence lights up *inside* the file, not just
the file in the tree. Crucially this uses an **always-on `ViewPlugin` +
`MatchDecorator`** (class `.cm-wsMatch`), NOT `@codemirror/search`'s
`setSearchQuery` — the latter only paints matches while its find *panel* is open,
so with the panel closed the workspace query lit up nothing. A ViewPlugin's
`decorations` facet renders whenever it's in the config, panel or not, and
`MatchDecorator` is viewport-bounded.

The in-file **Find** button opens a **custom CodeMirror search panel**
(`search({top:true, createPanel})`) — a compact two-row inline-icon find/replace
widget floated top-right (VS-Code-inspired *shape*, but our own colours/borders
and text-glyph toggles `Aa`/`W`/`.*`, not VS Code's icons). It drives CM6's
native search state + commands (`setSearchQuery`, `findNext`/`findPrevious`,
`replaceNext`/`replaceAll`, `closeSearchPanel`) with a live "n of m" count, so
in-file replace edits the buffer through the normal dirty→Save path (hash
conflict guard intact). Its pure logic — the regex build from the toggle state
and the match-count + formatting — is the DOM-free `findwidget` module
(`node:test`-covered); only the panel DOM is human-validated. It opens pre-filled
from the workspace query (which `setHighlightQuery` also seeds into CM's search
state), and is entirely separate from the workspace replace and its snapshot/seq
guards — this panel is in-FILE, CM6-native. The textarea fallback can't highlight
or float a find widget; it degrades to the project search box + jump-to-line
(stated in the PR).

Replace still applies from the *snapshot* the highlights were built with (not the
live inputs), guarded additionally by a monotonic search-sequence id, so editing
the query — or a slow search resolving late — can't make apply diverge from what
was shown (the two-phase preview→apply guarantee). The grouping / count /
selection / first-match / snapshot-currency logic is the pure `searchresults`
model; `filetreemodel.ancestorDirs` (pure, tested) computes the branches to
expand.

## Dirty tracking (line endings)

Files on disk are often CRLF (Windows / git autocrlf) but the editor works in
LF — CodeMirror splits on CR/LF and returns LF from `getValue()`. Comparing that
against the raw CRLF bytes read from disk would flag a freshly-opened file as
"modified" the instant it loads. So dirtiness is compared in an EOL-normalized
space (`eol.textDiffers`), and the file's original line ending is detected on
open and re-applied on save (`eol.applyEol`) so writing never silently rewrites
CRLF↔LF. All pure and `node:test`-covered (`eol.test.ts`), including the
open→no-edit→stays-clean regression.

## Key decision — editor widget: CodeMirror 6 vs `<textarea>`

The editor sits behind a thin `EditorWidget` interface (`mount`/`getValue`/
`setValue`/`onChange`/`setReadOnly`/`focus`/`openFind`/`dispose`). Two
implementations satisfy it:

- **CodeMirror 6 (primary, recommended).** Line numbers, per-language syntax
  highlighting, undo/redo, large-file virtualization, and — directly satisfying
  the issue's "search-and-replace" ask *inside* the open file — the
  `@codemirror/search` panel. It is **lazy-loaded via dynamic `import()`**, so its
  ~40-package dependency tree is code-split by Vite into chunks that load only on
  first overlay open, never in the main bundle. Bundle weight is a non-issue for a
  local desktop webview, which is CM6's only real cost here. Colours use the
  standard **One Dark** theme (`@codemirror/theme-one-dark`), with a modern IDE
  monospace stack layered on for the font (`"Cascadia Code", "JetBrains Mono",
  "Fira Code", …` — no bundled font files).
- **`<textarea>` fallback.** Zero-dependency; used automatically if the CM6 chunk
  fails to load.

**This is the single biggest thing to confirm at review.** CM6 is the first
multi-package vendor dependency in a deliberately lean frontend. The interface is
the seam that keeps flipping to the textarea a one-line change if the human
prefers zero-dep. (Monaco was rejected — heavy, worker-based, awkward outside a
framework.)

Note: `Ctrl+F` is eaten by WebView2's own find accelerator, so the in-file find
is reachable via a header button (the reliable path) as well as `Ctrl+F` with
`preventDefault` — validated live. That is also why the overlay toggle is `Alt+F`,
not a `Ctrl`-combo.

## Out of scope for v1 (call out at demo)

LSP / autocomplete / diagnostics; multi-file tabs (one open file at a time);
git-diff gutters; tree mutation (rename/delete/drag); regex search (v1 is literal
+ case-insensitive + whole-word). Each is an additive follow-up on this seam.

## Tests

- `src-tauri/tests/fileedit.rs` (integration — must link the lib, CLAUDE.md
  constraint #4): `..`/absolute/symlink rejection + nested-legit acceptance;
  UTF-8 read + stable hash; binary + oversize guards; dirs-first listing +
  symlink flagging/non-traversal; atomic create/overwrite + no temp left;
  stale-hash conflict leaves the file untouched; literal/case/whole-word search;
  exclude-dir skipping + empty-query guard + cap-truncation flag; replace applies
  only confirmed files atomically + skips a bad file without a partial write.
  For #207: `git ls-files` respects `.gitignore` by default while the toggle
  includes ignored files (skips if `git` isn't on PATH), and a pre-set cancel
  flag stops the walk before it reads anything (the backend half of "a cancelled
  search yields nothing").
- `test/{filetreemodel,fileicons,searchresults,searchsession,dirtystate}.test.ts`
  — the pure models, including the edge cases (path-join escapes, unknown/dotfile
  icons, zero/all-deselected search, conflict on hash drift) and the #207
  cancellation race (a stale-id batch never lands), render cap, and
  ignored-by-default enumeration pick.

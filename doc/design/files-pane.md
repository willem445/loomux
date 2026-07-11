# Design: the file-explorer pane kind (`files`)

Status: implemented (issue #214).

## Problem

Quoting the issue: *"to avoid needing to open a bunch of separate file
explorers."* The user wants to **park a file manager next to their agents** —
browse a project, open a file in the app that owns it, make a folder, rename
something — without leaving loomux or opening an OS Explorer window per repo.

So #214 adds a **pane kind**: a fourth peer to Agent, Orchestrator and Terminal in
the welcome screen, whose content is a native-style file manager.

## What it is NOT (and the course correction)

The first cut of this PR embedded `FileEditView` — the `Alt+F` in-app editor (lazy
tree + CodeMirror + project search) — as the pane's content. The human's
clarification on the issue was explicit: what's wanted is the **Windows Explorer /
Ubuntu Files equivalent**, double-clicking a file opens it in **the OS default
application for its extension**, and the editor reuse is *"explicitly NOT the
preferred direction"*.

That is a genuinely different product, and the difference is one sentence:

| | Answers | Opens a file with |
| --- | --- | --- |
| **File editor** (`Alt+F`, #174) | *"let me read/fix this source file without leaving the terminal"* | loomux's own CodeMirror |
| **File explorer pane** (#214) | *"get this file into the application that owns it"* | **the OS default app** |

A `.png` should open in your image viewer and a `.pdf` in your PDF reader; loomux
having an opinion about either is the bug. The editor is untouched, still lives
behind `Alt+F`, and remains the right tool for a quick look or a one-line fix.

Everything *structural* from the first cut survives — it was the pane **plumbing**
that was right and the pane **content** that was wrong: the welcome-screen kind,
the PTY-less pane, the restore round-trip, the agent-counter exclusion, and the
fast name search all carry over unchanged.

## The core idea: a pane that is *content*, not a *process*

Every pane before this one was ultimately a PTY with chrome around it. A files pane
has **no PTY, ever** — its content *is* the `FileExplorerView`.

This is less of a leap than it sounds, because #194 already built the machinery: a
welcome (setup-state) pane and a dormant restore placeholder are both panes that
exist, sit in the split tree, and hold no process. `Pane.startFiles()` is the third
member of that family, and it follows the same recipe:

- the terminal is **never opened** (`term.open()` is not called), so there is no
  ConPTY and therefore nothing that could ever be resized — CLAUDE.md hard
  constraint #1 holds by construction rather than by care;
- `.pane-term` stays in the flow, empty, and `.pane-files` covers it exactly the
  way `.pane-welcome` does. The grid, the dock, drag-to-reorder, maximize and the
  divider math are all untouched, because from their point of view nothing is
  unusual about this pane;
- `filesRoot !== null` **is** the kind flag, and it doubles as the pane's `cwd`, so
  "open in editor" (`Alt+E`) and `capture()` both target the folder on screen.

Two latent bugs fell out of the audit and are fixed here for *every* PTY-less pane,
not just the new one: `tryWebgl()` and `serializeViewportHtml()` now bail when
`term.element` is unset. Both previously threw-and-caught on a welcome or dormant
pane whenever its tab was shown or hover-previewed.

## Backend: no new crates

Three capabilities were needed, and the constraint that shaped all of them is
CLAUDE.md #2 — **no getrandom-pulling crates**, because
`bcryptprimitives.dll!ProcessPrng` isn't exported on this project's Windows 10
baseline and the binary then fails to load with `0xc0000139`.

| Need | Obvious dependency | What we did instead |
| --- | --- | --- |
| Open with default app | `tauri-plugin-opener` | `ShellExecuteW` |
| Delete to Recycle Bin | the `trash` crate | `SHFileOperationW` + `FOF_ALLOWUNDO` |
| Enumerate paths for the name search | — | reuses `fileedit`'s walker |

Both Shell APIs come from the **`windows` crate we already depend on** — two extra
feature flags, ~40 lines. `Cargo.lock` is **byte-identical**, so the getrandom
question isn't answered, it's *dissolved*: the dependency graph didn't move.

`ShellExecuteW` also happens to be the *safer* option, not just the cheaper one: it
takes a **path**, not a command line, so unlike `cmd /c start "" <path>` there is
nothing for a shell to re-parse and a filename full of spaces, quotes or ampersands
is inert. (Same guarantee `editor.rs` gets by using argv.)

On macOS/Linux the open is `open` / `xdg-open` (spawned detached, argv), and there
is **no Recycle Bin** without a new dependency. So delete is permanent there — and
`fm_delete_mode` reports which, so the confirmation dialog says *"Permanently
delete"* rather than promising an undo that doesn't exist.

## Path safety

Every command takes the pane's `root` plus a `rel` and routes through
`fileedit::safe_resolve` — the existing, tested choke point (lexical `..` folding,
no absolute `rel`, no traversal *through* a symlink), made `pub(crate)` and reused
rather than reimplemented. A second path-validation implementation is a second one
to get wrong, and **this module deletes things**.

On top of it, two rules of its own:

1. **Names are one segment.** `validate_name` rejects separators, `.`/`..`, the
   illegal Windows characters, and the reserved device names. This is what stops a
   "rename" being a *move*: `../../elsewhere` is refused at the name, with a
   sentence the user can act on, long before it reaches a path.

2. **`delete` and `rename` refuse to act on the root itself** (`resolve_child`).
   This one is worth reading twice, because the obvious implementation is wrong. A
   lexical *"is `rel` empty"* check misses `"."` and `"sub/.."` — but you'd catch
   those by comparing resolved paths. What **neither** catches is `"   "`: Rust's
   `PathBuf` faithfully preserves the trailing spaces, so `root.join("   ")`
   compares *unequal* to `root` — while the **Win32 path layer strips them** and
   operates on the root anyway. The integration test for this caught the first
   implementation sending the pane's own root folder to the Recycle Bin. The guard
   is therefore a check on the path **components** (`has_mangled_component`), before
   they are ever handed to the filesystem, plus the resolved-path comparison.

The webview is trusted, so none of this is strictly load-bearing today. It is
defense-in-depth exactly as constraint #6 asks — and destructive operations sitting
next to agent-facing surfaces are precisely where that stops being theoretical.

### Symlinks and junctions are shown, and otherwise inert

A symlink is **listed** (reported as a link, never as its target, and sorted with
files even when it points at a directory) and **nothing else**. You cannot navigate
*through* it, and you cannot `open`, `rename` or `delete` **the link entry itself**
either: `safe_resolve`'s `ensure_no_symlink` lstats the *final* component too, so
every operation on a link is refused with a `symlink` error.

Refusing the op *on* the link is stronger than it strictly needs to be — deleting a
link is not the same as deleting its target, and the non-Windows arm would in fact
get that right for free (`remove_file` on a symlink removes the link). It is
deliberate anyway, because on Windows the reasoning is different: a **junction**
pointing outside the root is exactly the shape a recursive Recycle-Bin delete would
escape through, and rather than reason about whether `FO_DELETE` follows one, we
never hand it the chance. **The question doesn't get to be asked.** For a feature
whose failure mode is "deleted the wrong thing", that trade is worth an inconvenience.

The inconvenience is real, so it's surfaced honestly rather than leaked as jargon:
the row tooltip says the link is shown but won't be followed, opened, renamed or
deleted, and an attempted op toasts *"Loomux won't delete a symlink — it's shown
here, but it's left alone. Use your OS file manager for links and junctions."* — not
the raw backend `refusing to traverse symlink`, which is both jargon and, for a link
you were trying to *delete*, the wrong verb entirely.

Deleting/renaming the **link itself** (never the target) is a reasonable follow-up.
It needs `ensure_no_symlink` to grow a "final component may be a link" mode, and the
Windows delete to use a link-aware call — not a two-line change, and not what #214
is about.

## What the overlays do — and why they're off

Every pane overlay (git, issues, tasks, audit, file editor) is sized *from the
terminal*: `overlayClamp` measures `termEl.clientHeight`, and `updateTermShift`
reads the live `.xterm-screen` to nudge the cursor row out from under the panel. A
files pane has no terminal, so those measurements are meaningless and a panel would
open into a zero-height box.

Making them work needs a **second sizing model** for overlays that don't assume a
terminal underneath. That is real work and it isn't what #214 is about, so the
overlays are cleanly **off** on a files pane rather than half-working: the buttons
carry a `pty-only` class and are hidden by `.pane.is-files`, and the hotkey path is
answered by `Pane.refuseOverlay()` with an honest toast. The git view over a files
root is the one worth revisiting — the deferral is tracked on the issue
([#214 comment](https://github.com/willem445/loomux/issues/214#issuecomment-4942018258)).

## "Go to file" — the fast file-NAME search

The issue asks for one capability beyond the pane itself: *"an optimized and fast
file search. It does not need to search into files as we already have the file
editor pane that can do that."* So: **paths, never contents** — a jump-to-file box.

**Where the speed comes from** is the split. The content search (#207) walks and
*reads* every candidate file on each query; that's inherent to what it does. A name
search needs none of it. So `fileedit.rs` gained `list_files` — the same enumeration
source as the content search (`plan_enumeration`: `git ls-files` in a git repo so
`.gitignore` is respected for free, the full walk otherwise), with the entire
expensive half deleted: no `open`, no read, no binary sniff, no line scan. The
frontend calls it **once per root**, caches the path list, and every keystroke is an
in-memory rank over that array. Typing costs **zero I/O**.

The enumeration is still off-thread, streamed and cancellable (`ft_files_start` →
`ft-files` events, cancelled through the *same* registry and `ft_search_cancel` —
ids come from one monotonic frontend counter, so they're unique across both streams
and one command serves both). It runs lazily, on first focus of the box.

**Substring, not fuzzy** (v1, by choice). A subsequence matcher demos beautifully
and behaves badly at scale: on a 20k-path repo `pnt` matches nearly everything —
any path with a p, an n and a t in that order — so the *ranking function becomes the
entire product*, and small changes to it reshuffle results unpredictably. Substring
makes the opposite trade: you can always predict what it will and won't find, which
is exactly the property you want when jumping to a file whose name you already know.
Space-separated terms are AND-ed across the path, recovering the useful part of
fuzzy (`pane rest` → `src/panerestore.ts`) without the noise.

The ranking (`src/filematch.ts`, pure + unit-tested) is four rules by weight: an
exact file-name match wins outright; a match in the **name** beats one only in the
**directory**; at the start of a name or path segment beats mid-word; ties break on
the shorter path, then alphabetically — so enumeration order can never leak into
results. One subtlety with its own test: the scorer takes the **best** occurrence of
a term, not the first. `test/panesetup.test.ts` contains "test" in both its
directory and its name, and scoring `indexOf`'s first hit would grade it a directory
match — collapsing the name-beats-directory rule on precisely the paths where it
matters most.

In the **manager**, opening a hit hands the file to the default app and navigates to
its folder with it selected, so a jump leaves you oriented. The same box is also in
the `Alt+F` editor (it's the same enumeration and the same ranking), where opening a
hit opens it *in the editor* — each surface opens things the way that surface opens
things.

Nothing is ever cut silently: the result list is capped (ranking still runs over the
*full* list, so the cap never costs the best hit) and the summary reports the true
match count, the index size, and any backend truncation.

## The pure core

`fileexplorermodel.ts` holds everything decidable, DOM-free and tested:

- **Listing order** — folders first, then case-insensitive + numeric by name, with
  a case-sensitive tiebreak so the order is *total* (a `README`/`readme` pair must
  not visibly jitter between refreshes). A symlink sorts with **files** even when it
  points at a directory: we never follow it, so it isn't one. The backend
  deliberately returns entries **unsorted and unfiltered** — ordering and hiding are
  product decisions, not facts about the disk.
- **Rooted navigation** — `parentRel` returns `null` *at* the root, which is what
  disables the Up button. That bound is what makes the backend's `root` + `rel`
  containment model meaningful rather than decorative: there is no `rel` the UI can
  produce that escapes the root.
- **Formatting** — sizes, and an mtime that takes `now` as a parameter so it's
  deterministic (and so `Date.now()` stays out of a pure module).
- **Selection** — **clamps** at both ends, deliberately unlike the Go-to-file result
  list, which *wraps*. A directory listing is a *place*: holding Down must come to
  rest on the last row, not silently teleport past the file you meant to land on. A
  short result list is a *menu*, and cycling it is correct.
- **The inline-edit state machine** — new-folder and rename are the same
  interaction (an input row with a name in it), so they are one state machine rather
  than two flags that can disagree.

Two cases in the edit validation are easy to get subtly wrong and each has a test:
renaming an entry to **its own name** must be allowed (the entry is in the listing,
so a naive duplicate check rejects it) and is then skipped as a no-op; and a rename
that changes only **case** (`old.txt` → `Old.txt`) is a real rename — it must neither
self-collide nor be skipped.

### The inline check is a *subset* of the backend's, on purpose

`nameError` is a **UI courtesy, not a boundary**: `validate_name` in `filemgr.rs` is
authoritative and re-checks everything at commit. What the inline check adds is an
answer *while you type* for the mistakes people actually make (empty, `.`/`..`, a
separator or other illegal character, a trailing dot), plus the one rule the backend
**cannot** check because it doesn't know the listing — a duplicate sibling name,
case-insensitively, because that's how the filesystems this runs on behave.

The gap is worth naming rather than glossing: the Windows **reserved device names**
(`con`, `nul`, `aux`, `com1`, `lpt9`, …) are *not* checked inline. Nobody types `con`
by accident; the list is long and obscure; and an inline error firing on it would
need a footnote to explain itself. So it's left to the backend, which refuses it at
commit with a toast that says exactly why. Adding it inline would be three lines —
the reason not to is that inline errors should cover the **near-misses**, not
enumerate the trivia. There's a test pinning that `con` passes `nameError`, so the
boundary can't drift silently.

## Session restore

The pane captures `{kind: "files", cwd: <root>}` and restores as a files pane at that
root. **No schema bump**: the root rides in the existing `cwd`, the same shape-driven,
additive move `role` made in #194.5, so `SCHEMA_VERSION` stays at 2 and older files
(which simply never contain a `files` leaf) decode unchanged.

The *sub-folder* you had navigated to is **not** persisted — a restore lands you at
the pane's root. Deliberate: the root is the pane's identity and its containment
boundary, and adding a second persisted field to remember a transient cursor position
isn't worth the schema surface. Navigating back down is two clicks.

If the root is **gone** at restore (deleted, renamed, or on a drive that isn't
mounted), the slot **fails soft to the welcome form** with a toast naming the missing
folder, and the rest of the layout restores around it — a pane rooted at a vanished
directory would render an empty listing and a mystery. A rootless `files` leaf is
*well-formed but unrestorable*, so it decodes (rather than tripping the whole-tree
fail-safe and taking its sibling panes down with it) and is resolved in that one slot.

The root is validated for real at **both** ends — at setup (inline error, cursor back
in the field, exactly what a missing CLI gets) and again at restore. The probe is
`ftRootIsDir()`, which reuses `ft_list_dir` with an empty `rel`: `safe_resolve`
already stats the root and rejects a missing or non-directory path, which is exactly
the question being asked. **No new command.**

## Not an agent

A files pane reports `kind: "files"` to `tabcounts.ts`, which ignores it: the per-tab
agent badge counts agents, and this is a viewer. It reports `live: true` (it *is*
fully functional the moment it exists), which is precisely why `tabcounts` keys off
**kind, not `live`** — a tab full of file explorers must not claim to be running
agents that don't exist. There's a test pinning that.

## Module map

| Piece | File | Role |
| --- | --- | --- |
| Kind + root validation | `src/panesetup.ts` | `PaneKind` gains `"files"`; `planPaneSetup` requires a root (no home fallback — a manager rooted at `~` is never what anyone meant). Unit-tested. |
| The pane | `src/pane.ts` | `startFiles()`, `isFiles`, `workdir`, `refuseOverlay()`, the `liveKind`/`capture`/`tabPaneInfo` arms. DOM-coupled → hand-validated. |
| Placement | `src/grid.ts` | `openFilesPane()` — like `openWelcomePane`, but content instead of a form. Synchronous: there's no process to await. |
| The manager | `src/fileexplorer.ts` | Toolbar, breadcrumb, listing, inline edits, Go-to-file. DOM wiring only. |
| Its pure core | `src/fileexplorermodel.ts` | Listing order, rooted navigation, breadcrumb, formatting, inline-edit validation. Unit-tested. |
| Name ranking | `src/filematch.ts` | Substring + path-segment, best-occurrence, deterministic ties. Unit-tested. |
| Typed bridge | `src/filemgr.ts` | `fm_*` wrappers (per-feature module, the `fileapi.ts` precedent). |
| Backend | `src-tauri/src/filemgr.rs` | list / new folder / rename / delete / open-with-default. Reuses `fileedit::safe_resolve`. Integration-tested. |
| Name enumeration | `src-tauri/src/fileedit.rs` | `list_files` + `ft_files_start` — paths only, no file opened. Integration-tested. |
| Persistence | `src/tabstore.ts` | `PersistedPaneKind` gains `"files"`; the root rides in `cwd`. No version bump. Unit-tested. |
| Restore policy | `src/panerestore.ts` | The `open-files` action (root may be null → caller fails soft). Unit-tested. |
| Counting | `src/tabcounts.ts` | `"files"` in the kind union; never counted. Unit-tested. |
| Shared dialog | `src/modal.ts` | Extracted from `fileedit.ts` when the manager needed the same confirm — one copy, two callers. |

## Deferred (not forgotten)

- **Multi-select, copy/move, drag-and-drop.** The issue's "etc." — v1 is the op set
  the human named. Multi-select is the natural next one, and the op layer is
  shaped for it (`fm_delete` takes one `rel`; taking a list is additive).
- **Git view over a files root** — needs the second overlay sizing model. Tracked on
  the issue.
- **Restoring the sub-folder** you had navigated to (see *Session restore*).
- **Deleting/renaming a symlink itself** (never its target) — see the symlink
  section above; needs `ensure_no_symlink` to grow a final-component-may-be-a-link
  mode.
- **The `fm_*` commands are synchronous**, and Tauri runs sync commands on the main
  thread — so a Recycle-Bin delete of a `node_modules`-scale folder will freeze the
  window until `SHFileOperationW` returns. It matches house style (only `voice_stop`
  is async), and the main thread's STA is genuinely the *right* apartment for these
  shell APIs, so a naive `async fn` would trade a freeze for `CoInitializeEx`
  questions in the worker. Filed as its own follow-up rather than rushed into this PR.

## A small bonus

The welcome form's path field is seeded from the working directory of the pane you
split *from* (its shell cwd, agent worktree, or files root), falling back to the
recent-repo default. It matters most here: a file explorer opened beside an agent
should root at *that agent's worktree*, not at whatever repo you last launched.

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
exist, sit in the split tree, and hold no process. `Pane.startContent()` (which #217
generalized from this pane's `startFiles()`) is the third member of that family, and
it follows the same recipe:

- the terminal is **never opened** (`term.open()` is not called), so there is no
  ConPTY and therefore nothing that could ever be resized — CLAUDE.md hard
  constraint #1 holds by construction rather than by care;
- `.pane-term` stays in the flow, empty, and `.pane-content` covers it exactly the
  way `.pane-welcome` does. The grid, the dock, drag-to-reorder, maximize and the
  divider math are all untouched, because from their point of view nothing is
  unusual about this pane;
- `contentKind !== null` **is** the kind flag (it was `filesRoot`, until #217 made
  this a family of three), and the root doubles as the pane's `cwd`, so "open in
  editor" (`Alt+E`) and `capture()` both target the folder on screen.

Two latent bugs fell out of the audit and are fixed here for *every* PTY-less pane,
not just the new one: `tryWebgl()` and `serializeViewportHtml()` now bail when
`term.element` is unset. Both previously threw-and-caught on a welcome or dormant
pane whenever its tab was shown or hover-previewed.

## Backend: the dependency budget

The constraint shaping every choice here is CLAUDE.md #2 — **no getrandom-pulling
crates**, because `bcryptprimitives.dll!ProcessPrng` isn't exported on this project's
Windows 10 baseline and the binary then fails to load with `0xc0000139`.

**The OS integrations cost nothing.** Every one of them turned out to be an API in the
`windows` crate we *already* depend on — a couple of extra feature flags:

| Need | Obvious dependency | What we did instead |
| --- | --- | --- |
| Open with default app | `tauri-plugin-opener` | `ShellExecuteW` |
| The "Open with" chooser | — | `ShellExecuteW`, `openas` verb |
| Delete to Recycle Bin | the `trash` crate | `SHFileOperationW` + `FOF_ALLOWUNDO` |
| Reveal in the OS file manager | — | `explorer /select,` (argv) / `open -R` / `xdg-open` |
| Enumerate paths for the name search | — | reuses `fileedit`'s walker |

`ShellExecuteW` is also the *safer* option, not just the cheaper one: it takes a **path**,
not a command line, so unlike `cmd /c start "" <path>` there is nothing for a shell to
re-parse and a filename full of spaces, quotes or ampersands is inert. (The reveal's
`/select,<path>` is likewise one argv element handed straight to `CreateProcess`, not a
concatenated shell string.)

**Hashing cost three packages**, and it is the only thing in this feature that did.
`sha2`, `sha1` and `crc` were vetted against the ban before use, and the result is
recorded in `Cargo.toml` and under *Hashing* below: none touches getrandom, `sha2` was
**already in the lock** (a tauri transitive) with its whole dependency chain, and `sha1`
reuses that chain — so the additions are exactly `sha1`, `crc`, `crc-catalog`.

That is a deliberate departure from this feature's otherwise-zero-crate record, and the
reason is judgement, not laziness: hand-rolling SHA-256, SHA-512 and SHA-1 to save three
tiny, pure-Rust, widely-audited packages would trade a *known-correct* implementation for
one that has to earn trust — in exchange for nothing the ban actually asks for.

On macOS/Linux the open is `open` / `xdg-open` (spawned detached, argv), and there is **no
Recycle Bin** without a new dependency. So delete is permanent there — and `fm_capabilities`
reports which, so the confirmation dialog says *"Permanently delete"* rather than promising
an undo that doesn't exist.

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
deleted; the **context menu greys every row action** on a symlink with that reason (the
same courtesy a folder already got — offering six items that all end in the same toast
would be a menu that lies); and an op that does get attempted toasts *"Loomux won't delete
a symlink — it's shown here, but it's left alone. Use your OS file manager for links and
junctions."* — not the raw backend `refusing to traverse symlink`, which is both jargon
and, for a link you were trying to *delete*, the wrong verb entirely.

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
carry a `pty-only` class and are hidden by `.pane.is-content` (was `.is-files`), and
the hotkey path is answered by `Pane.refuseOverlay()` with an honest toast.

> **Answered in #217, by the other road.** You don't overlay a git view onto a
> content pane — you **open a git pane**: the same view as a pane's content, sized by
> the pane's own box (it re-clamps its inner layout against its own element, and
> always did). Same for the editor. The refusal above stands for the surfaces that
> genuinely need a terminal underneath; the ones that never did are now pane kinds.
> See [content-panes.md](content-panes.md).

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

## View parity: the results view is not a second-class row list

**Three rounds running, an affordance was built for the directory listing and quietly
omitted from the Go-to-file results:**

| Round | The bug |
| --- | --- |
| 4 | Rename resolved its target from the *listing's* index even while the results were on screen — so it renamed a file the user could not see. |
| 5 | The fix navigated correctly, but mounted its editor into the still-hidden listing — so *"F2 does nothing"* came back on the very path added to kill it. |
| 6 | The context menu was wired to the listing's rows and not the results' at all — so right-clicking a result got the **webview's** default menu. |

Each fix was correct. Each time, the *next* affordance forgot again. That is not three
bugs; it is **one bug, three times**: the results view kept being an afterthought, and
nothing in the code ever asked *"…and does this work there?"*

So the class is closed twice over — structurally, and declaratively.

**Structurally**, `fileexplorer.wireRowAffordances` is now the *only* place a row's
behaviours are attached (click, double-click, right-click), and **both** views call it. A
new affordance lands in both by construction, not by remembering.

**Declaratively**, `ROW_AFFORDANCES` (in the pure model) lists every row affordance, and
each must either work on a result or **say why not**. The parity test enforces that — and,
crucially, **cross-checks the registry against the context menu's own action set**. Add an
item to the menu without declaring it and the test fails, naming the affordance. That
failure is the only moment anyone is going to think about the results view, so it is
engineered to happen.

Exactly one affordance is legitimately listing-only, and the reason is structural rather
than neglect: the **inline-edit row** exists only in the listing. Ops that open one leave
the filter first (`editMountFor.exitFilter`), so they are still *reachable* from a result —
the editor simply cannot be *hosted* in a list that re-renders on every streaming index
batch, where a focused text input would have its keystrokes eaten.

## The right-click menu

A context menu is the **identity-vs-index trap with a longer fuse**. It is built when you
right-click and acted on *seconds later*, after you have read it, moved the mouse, and
possibly let a streaming index batch re-rank the list underneath it. If it resolved its
target when you *clicked an item*, it would be resolving against a list that has had
every opportunity to change.

So it binds an `OpTarget` — a row's **path** — at **menu-open**, and every action it
fires carries that value (`buildContextMenu`, pure + tested; the first test in
`filemenu.test.ts` asserts that *every* row-scoped action carries the bound target).

Just as importantly, the menu is **a second way to reach the op layer, never a second
copy of it**. `runMenuAction` calls `beginRenameOn`, `beginCreate`, `deleteTarget` — the
same functions the toolbar and the keyboard call. That is why the round-4
rename-from-results fix (clear the filter, navigate, *then* mount the editor) applies to
the menu **for free**, rather than being a rule someone had to remember a third time.

Two honesty rules in the menu's shape:

- An item that is **inapplicable here** stays visible but disabled with a reason — a
  folder has no hash and no "open with". The menu's shape shouldn't shift depending on
  what you clicked, or you never learn where anything is.
- An item that is **unsupported on this OS** is omitted entirely. `fm_capabilities`
  reports what the platform can do, and the menu offers exactly that: no "Open with…"
  outside Windows, and on Linux the reveal item is labelled *"Open containing folder"*
  because that is all `xdg-open` can do — it cannot select the entry, and the label
  refuses to pretend otherwise.

## Hashing

The listing carries a short **SHA-256** per file, and the menu's **Hash →** computes
SHA-256/512, SHA-1, or CRC-32/16/8 on demand.

### It must never block the window

Tauri runs a synchronous command on the **main (webview) thread**. Hashing reads *every
byte* of the file, so a sync `fm_hash(rel)` would freeze the whole window on the first
multi-megabyte row — and a directory of them would freeze it for as long as the directory
took. That is exactly the trap `ft_search` fell into in #207, and this takes the same way
out: `fm_hash_start` spawns a **worker thread**, streams results back as `fm-hash` events
tagged with the caller's id, and polls a cancel flag **between files and between chunks**,
so navigating away abandons a 4 GiB hash immediately rather than "when it finishes". It
reuses #207's registry and `ft_search_cancel` outright — ids come from one monotonic
counter, so one registry and one cancel command serve the search, the name index, and
hashing.

The file is **streamed**, never read into memory: a 4 GiB ISO costs 64 KiB of RAM.

The same worker path serves both the column (many rels) and the submenu (one rel), so
there is exactly one place hashing can be wrong — and it is the one the tests cover.

### The 32 MiB threshold

Opening a directory must never cost you a disk read of every byte in it. A folder of
source is nothing; a folder of ISOs, VM images or datasets is *gigabytes*, and hashing
them unasked would spin the disk for minutes filling a column nobody was looking at.

So files up to **32 MiB** are hashed automatically, and above it the cell shows a
clickable **hash** instead. The number is chosen to sit above essentially all source,
config, images and documents — the things you actually want a checksum of from a file
manager — and below the archive/media sizes where the cost stops being free. A 32 MiB
SHA-256 is ~30–60 ms, so even a directory of 50 files at the limit finishes in a couple of
seconds of *background* work. Nothing is hidden: a big file's hash is one click away, and
the click is the user saying *"yes, spend that."*

### The cache key is (path, size, mtime) — and the size is not redundant

A **stale hash is worse than no hash**, because it looks authoritative. Keying on mtime
alone would serve one after a same-size edit that lands inside the filesystem's mtime
granularity. Size and mtime together make "the file changed" observable without re-reading
it, which is the whole point of a cache. (A digest whose entry is no longer in the listing
is *dropped* rather than cached, since we can no longer observe the size/mtime it would be
keyed by.)

### The dependency gate

The hash crates were checked against CLAUDE.md #2 before being used, and the result is
worth recording:

```
cargo tree -e normal --target all -i getrandom@0.3.4
  → getrandom v0.3.4 └── tauri v2.11.5      (pre-existing — tauri's own, unmoved)
cargo tree -e normal --target all -i getrandom@0.2.17
  → "nothing to print"                       (not in the runtime tree at all)
cargo tree -e normal --target all -p sha2 / -p sha1 / -p crc
  → cfg-if, cpufeatures→libc, digest→block-buffer→generic-array→typenum, crc-catalog
```

**None touches getrandom**, so none imports `ProcessPrng`. And the cost is smaller than it
looks: `sha2` was **already in `Cargo.lock`** (a tauri transitive) with its whole chain, and
`sha1` reuses that chain entirely — so the three crates add exactly **three packages**:
`sha1`, `crc`, `crc-catalog`.

Hashing is not a place to be clever. These are the RustCrypto reference implementations,
and the integration tests check them against the **published FIPS 180-4 vectors** and the
**CRC catalogue check values** — not against themselves, which would prove only that the
code is deterministic. The CRC variants are pinned and *named* in the UI (ISO-HDLC, ARC,
SMBUS), because a bare "CRC-16" is genuinely ambiguous and a user comparing our checksum
against another tool's needs to know which one they are looking at.

## Deleting on a worker thread (#216)

A delete used to run **synchronously on Tauri's main (webview) thread**, because that is
what a `#[tauri::command] fn` does. A Recycle-Bin delete of a `node_modules` is tens of
thousands of shell operations, and for every second of it the *entire window* — every
terminal, every agent — was frozen. Same class of bug as the #207 search freeze, and the
same shape of fix: a worker thread, a streamed completion event, an id.

`fm_delete_start(id, root, rel)` spawns a thread and returns immediately. The thread does
the delete and emits `fm-delete` with `{ id, rel, recycled? , error? }`. That is the
**fourth stream** on the one monotonic id counter the frontend already keeps
(`nextSearchId`), after `ft-search`, `ft-files` and `fm-hash`.

### Why not just `async fn` — the question rev-102 raised

This is the part that made the naive fix wrong, and the reason the issue was filed rather
than patched in round 3.

`SHFileOperationW` is a **Shell/COM** API. Its threading requirement was being satisfied
for us *by accident*: wry `OleInitialize`s the main thread, so the main thread is an STA
and the call was always made from inside a properly initialized apartment. An `async fn`
command runs on Tauri's async runtime — a thread pool whose COM state is nobody's business
and certainly not ours. Offloading naively would have traded a visible freeze for an
invisible apartment bug: the kind that works on the developer's machine and fails on a
user's, or fails only when some other subsystem happens to have initialized the pool
thread first.

So the delete gets a **dedicated OS thread that enters its own apartment**:

```rust
let _com = ComApartment::enter();   // CoInitializeEx(COINIT_APARTMENTTHREADED)
// … SHFileOperationW …
// CoUninitialize on drop — an early return cannot leak the init
```

`ComApartment` is an RAII guard, and the three-way return of `CoInitializeEx` is the whole
reason it is a guard rather than two bare calls:

| `CoInitializeEx` returns | meaning | must we `CoUninitialize`? |
| --- | --- | --- |
| `S_OK` | we initialized the apartment | **yes** |
| `S_FALSE` | already initialized — *but the reference count was still incremented* | **yes** |
| `RPC_E_CHANGED_MODE` | the thread is already an MTA; no reference taken | **no** — uninitializing here would decrement somebody else's count |

`S_FALSE` is the one that gets written as a failure and skipped. It is a *success*: the
reference was taken, and every `CoInitializeEx` that returns it must be balanced. The guard
records `owns_reference` and drops accordingly.

The apartment is entered inside `delete_path`, not at the top of the worker, so the pure
`delete()` function is correct on **any** thread — which is also what lets the integration
tests drive the real COM-initialized path instead of a stub. `delete_works_from_a_fresh_thread_that_has_never_initialized_com`
is the test that would have failed before this change; `repeated_deletes_on_fresh_threads_do_not_unbalance_com`
runs it 25 times, so a leaked or over-released reference surfaces rather than lurking.

The worker needs **no message pump**, and this is a property of the flags rather than luck:

```
FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI
```

`FOF_ALLOWUNDO` is the Recycle Bin itself and stays. The other three are the *no UI at all*
set — no progress dialog, no confirmation, no error box — which is right because the pane
now shows the state itself, and it also means nothing in this call can create a window and
sit waiting for messages we are not pumping. The **double-NUL** invariant on `pFrom` is
unchanged: the path list is NUL-separated and NUL-NUL-terminated, and a worker thread does
not alter that in any way.

### Cancellation: there is none, deliberately

The other three streams are cancellable and this one is conspicuously not. That is a
decision:

* `SHFileOperationW` is **one call**. No cancel handle, no progress sink, no way to ask it
  to stop. (`IFileOperation` + `IFileOperationProgressSink` can — a much larger API surface
  and a different conversation.)
* Even if it could be stopped, a delete cancelled mid-tree leaves a **half-deleted
  directory**: some children in the Recycle Bin, some not, a parent that may or may not
  still exist. That is a worse thing to hand a user than a wait.

So the pane's busy state communicates **"in progress"** — a pulsing row, greyed mutating
buttons, and the status line naming the file — and offers no Cancel it could not honour. It
does not register a flag in the cancel registry either: *a flag nobody polls is a lie told
to the next reader.* Closing the pane does not cancel it; it merely stops anyone listening,
which is what closing a pane means.

### What the busy state blocks — and what it must not

Modelled in `fileexplorermodel.ts` (`PaneOp`, `opBlockedReason`), pure and tested:

| while a delete is in flight | |
| --- | --- |
| delete, rename, new folder, new file | **blocked**, with a reason naming the file |
| navigate, hash, open, open-with, reveal | **live** |

The second row is the load-bearing one. Blocking navigation or hashing would reintroduce
the freeze one layer up — just implemented in TypeScript instead of on the main thread.
They read; they don't write; they touch nothing the delete owns. You can browse away while
a big tree deletes, and the completion event refreshes the listing **only if you are still
in the directory it happened in** (`parentRel(rel) === this.rel`) — otherwise re-listing
would refresh a place the user didn't ask about.

The guard is applied at the top of each *op* (`beginCreate`, `beginRenameOn`,
`deleteTarget`, `commitEdit`), not on the buttons — because the toolbar, the keyboard and
the context menu are three doors into the same op layer, and guarding one door is guarding
none. The context menu greys its mutating items from the *same* pure rule, so the two
cannot drift.

The busy marker on the row is attached in `wireRowAffordances` — the one place a row's
behaviours are wired — and declared in `ROW_AFFORDANCES` with `results: true`. A result row
for a tree the shell is currently walking must not look idle and clickable just because the
user typed into the filter box.

### Why rename / new file / new folder stay synchronous

#216 says "the `fm_*` ops", so this needs answering precisely rather than by omission.

They are **single metadata operations** — one `rename(2)`/`MoveFileW`, one `mkdir`, one
`create`. They are bounded by a single directory-entry write and complete in microseconds
regardless of how large the thing they act on is: renaming a 40 GB directory is exactly as
fast as renaming an empty one, because nothing is copied or walked. There is no tree to
recurse and therefore no freeze to remove. Moving them to workers would add an event round
trip, a busy state and an id per op, and buy nothing — the async machinery here is not free
of cost, it is a response to a specific unbounded operation.

Delete is the outlier precisely *because* it recurses.

### The non-Windows arms

`std::fs::remove_dir_all` on a huge tree freezes just as thoroughly as the shell does, and
`trash`-less platforms take the permanent-delete path. Both run on the same worker: the
threading is in `fm_delete_start`, above `delete()`, so every platform gets it. Only the COM
apartment is Windows-specific (`#[cfg(windows)]`), and it is entered below the split, where
it applies.

### Failure

`SHFileOperationW` returns its **own** result codes, not `GetLastError` ones — a fact that
is easy to get wrong and produces confidently mistranslated messages. `describe_delete_failure`
maps the ones a user can act on (`0x20` "open in another program", `0x05`/`0x78` access
denied, `0x10000` "the Recycle Bin refused it", `0x85` disk full, …) and keeps the raw hex
for anything else rather than inventing a meaning. The event carries the translated string;
the pane toasts it and re-lists, because a failed delete leaves the entry where it was and
the rows on screen must agree with the disk.

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

### Ops bind to a row's IDENTITY, never to a selection index

This is the shape of a bug the human hit in the demo, and it is worth recording
because it is the kind that comes back.

The explorer can be showing **one of two different row sets**: the directory
**listing**, or the Go-to-file **results**, which replace it while a query is active.
The listing has a `sel` index; the results have their own `gotoSel`. Ops resolved
their target from the *listing's* index — unconditionally, even while the listing was
hidden and the results were the thing on screen.

So with a filter active:

- **Rename** bound to an invisible row in a list nobody was looking at, then rendered
  its editor into the **hidden** listing. Visibly: *"clicking rename does nothing."*
- Clear the filter and the editor was suddenly sitting on **a different file** — the
  listing's own selection.
- **Delete** had the identical defect, and its toolbar button was never disabled while
  filtered. Nobody hit it in the demo, but it would have deleted a file the user could
  not see. Delete is not recoverable with an *"oh, that did nothing"*.

The fix is structural, not a patch on rename. Ops now resolve an **`OpTarget`** — a
row's *identity*, its path — from the view the user is **actually looking at**, at the
moment the op is invoked (`activeTarget` in the pure model, unit-tested). Once
captured, that value is immune to everything that happens to the lists afterwards: a
streaming index batch re-ranking the results, the filter clearing, a refresh
reordering the listing, the user sitting on a confirm dialog. **An index is a position
in a list that may not even be on screen; a path is the file.**

### …and the editor can only mount where it can be SEEN

Fixing the target was only **half** the bug, and the other half then got built a second
time — by the very code path added to fix the first half. This is worth recording
plainly, because the second occurrence was not a slip; it was a consequence of thinking
the problem was solved.

The inline-edit row exists **only in the directory listing**. Mount it while the
Go-to-file results are on screen and it lands inside a `display:none` list: the row
never appears, and its focus call no-ops. The user sees nothing happen. That is the
*same visible symptom* — "F2 does nothing" — from a *different cause*.

The new rename-from-results path captured the right target, navigated to the right
folder… and never cleared the query. `render()` ends in `refreshGoto()`, which
recomputes "are we filtering?" from the (still non-empty) search box and re-hides the
listing. Editor into a hidden list. Bug, again.

So the rule is not a call to `exitFilter()` remembered at each call site — that is what
was forgotten. It is stated **once, in the pure model** (`editMountFor`), where it is
asserted:

> **An inline editor may only mount in the listing, so any op that opens one must first
> make the listing the visible view — and that is true even when the target is already
> in the folder being browsed**, because it is the *query* that hides the listing, not
> the folder. ("Only exit the filter if we also have to navigate" is a
> reasonable-looking fix and a wrong one; there's a test named for it.)

Belt and braces, `renderList` **self-heals**: a rename edit whose row it did not render
is dropped. Without that, the edit state would sit there with no input to type in and no
Escape to press, while `onListKey` swallowed every key. It should never fire — but it
makes the whole class unreachable rather than merely fixed.

Two consequences worth stating:

- **Rename from a search result** clears the filter, navigates to the file's folder,
  selects it, and opens the editor there. The op still acts on the row you invoked it
  on — that's the identity capture — and now you can *see* it doing so. (Hosting the
  editor inside the results list instead would put a focused text input in a list that
  re-renders on every streaming index batch, which would eat keystrokes.)
- **Creating** a folder or file also leaves the filtered view first, because the new
  entry lands in the directory being browsed and its editor row lives in the listing.

### A target whose row isn't there

`mountBlocker` answers "can this target's row actually be rendered?", because two
perfectly ordinary situations say no:

- **It's hidden.** The Go-to-file index reaches files the listing hides — on macOS/Linux
  that is *every tracked dotfile* (`.gitignore`, `.github/…`), on Windows every
  hidden-attribute file. Renaming `.gitignore` from a search with **Hidden** off is a
  normal thing to want. So the op turns Hidden **on** for you, and says so in a toast:
  refusing would be perverse (you can see the file, right there in the results), and
  silently sprouting dotfiles would be its own small mystery.
- **It vanished** between capture and mount — an agent deleted it while you were picking
  it out of the results. Say so, and drop the edit.

Either, left unhandled, mounts no editor *and* leaves the edit state set with nothing to
escape from — which is the keyboard-deadening the self-heal above also guards.

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
| The pane | `src/pane.ts` | `startContent()` (`startFiles()` before #217), `isContent`, `workdir`, `refuseOverlay()`, the `liveKind`/`capture`/`tabPaneInfo` arms. DOM-coupled → hand-validated. |
| Placement | `src/grid.ts` | `openContentPane()` (`openFilesPane()` before #217) — like `openWelcomePane`, but content instead of a form. Synchronous: there's no process to await. |
| The manager | `src/fileexplorer.ts` | Toolbar, breadcrumb, listing, inline edits, Go-to-file. DOM wiring only. |
| Its pure core | `src/fileexplorermodel.ts` | Listing order, rooted navigation, breadcrumb, formatting, inline-edit validation; `activeTarget` (which view an op resolves against), `editMountFor` (the view state an editor needs before it can mount), `mountBlocker` (whether the target's row can be rendered at all), and `ROW_AFFORDANCES` (the view-parity registry). Unit-tested. |
| Name ranking | `src/filematch.ts` | Substring + path-segment, best-occurrence, deterministic ties. Unit-tested. |
| Typed bridge | `src/filemgr.ts` | `fm_*` wrappers (per-feature module, the `fileapi.ts` precedent). |
| Menu model | `src/filemenu.ts` | What the context menu contains, what's enabled, and **what it acts on** (target bound at menu-open). Unit-tested. |
| Menu renderer | `src/contextmenu.ts` | Placement (flips to stay on screen), submenus, Esc/click-away. Generic — takes `MenuItem[]`. |
| Hash policy | `src/filehashmodel.ts` | Auto-hash threshold, the (path, size, mtime) cache key, digest formatting. Unit-tested. |
| Backend | `src-tauri/src/filemgr.rs` | list / new folder / **new file** / rename / delete / open-with-default / **open-with chooser** / **reveal** / capabilities. Reuses `fileedit::safe_resolve`. Integration-tested. |
| Hashing backend | `src-tauri/src/filehash.rs` | SHA-256/512, SHA-1, CRC-32/16/8 — streamed on a worker thread, cancellable via #207's registry. Tested against published vectors. |
| Name enumeration | `src-tauri/src/fileedit.rs` | `list_files` + `ft_files_start` — paths only, no file opened. Integration-tested. |
| Persistence | `src/tabstore.ts` | `PersistedPaneKind` gains `"files"`; the root rides in `cwd`. No version bump. Unit-tested. |
| Restore policy | `src/panerestore.ts` | The `open-files` action (root may be null → caller fails soft). Unit-tested. |
| Counting | `src/tabcounts.ts` | `"files"` in the kind union; never counted. Unit-tested. |
| Shared dialog | `src/modal.ts` | Extracted from `fileedit.ts` when the manager needed the same confirm — one copy, two callers. |

## Deferred (not forgotten)

- **Multi-select, copy/move, drag-and-drop.** The issue's "etc." — v1 is the op set
  the human named. Multi-select is the natural next one, and the op layer is
  shaped for it (`fm_delete` takes one `rel`; taking a list is additive).
- ~~**Git view over a files root**~~ — **done in #217**, not by growing a second
  overlay sizing model but by making the git view a **pane kind** of its own (and the
  editor too). See [content-panes.md](content-panes.md).
- **Restoring the sub-folder** you had navigated to (see *Session restore*).
- **Deleting/renaming a symlink itself** (never its target) — see the symlink
  section above; needs `ensure_no_symlink` to grow a final-component-may-be-a-link
  mode.
- **Cancelling a delete** — not deferred so much as *declined*; see *Deleting on a
  worker thread* for the argument, which is that `SHFileOperationW` cannot be stopped
  and a half-deleted tree is worse than a slow one.

## A small bonus

The welcome form's path field is seeded from the working directory of the pane you
split *from* (its shell cwd, agent worktree, or files root), falling back to the
recent-repo default. It matters most here: a file explorer opened beside an agent
should root at *that agent's worktree*, not at whatever repo you last launched.

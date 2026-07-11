# Content panes: the editor and the git view as pane kinds (#217)

A **content pane** is a pane that *is* a surface rather than a process. No shell,
no CLI, no PTY — the pane's content is a view, permanently.

#214 built the first one (`files`, the file manager). #217 adds the two the human
actually asks for most: `editor` (the #174 tree + code editor + #207 search) and
`git` (the #208 git view). Both surfaces already existed as **overlays** inside a
terminal pane, behind `Alt+F` and `Alt+G`, and both overlays are unchanged.

```
                overlay (Alt+F / Alt+G)            pane kind (#217)
  where         floats over a terminal             fills a pane's content box
  sized from    the TERMINAL's height              the PANE's own box
  closes with   Esc / ✕                            the pane's ✕ (nothing to close back TO)
  root          view-local (browsing must not      the PANE's root — it adopts a re-root,
                disturb the terminal underneath)   so restore reopens what was on screen
  lifetime      as long as you hold it open        as long as the pane exists
```

Everything else is the same code. There is no second editor and no second git view.

## Why a pane and not "a bigger overlay"

The overlay is right for a *look*: you can see the shell underneath, and `Esc`
takes it away. It is wrong for a *station* — a git graph you want beside the agent
while it works, an editor you want to keep open on the file you're reviewing. With
only an overlay you either toggle it in and out all day, or you surrender a whole
terminal pane to something that never needed a terminal.

So: same surface, hosted by a pane. The pane system already had the machinery —
a welcome pane and a dormant restore placeholder are both PTY-less panes in the
split tree, and `files` proved a third kind could be pure content. `startContent()`
is that family's general case.

## The sizing generalization (the "second sizing model")

#214 deferred "the git view over a files pane" because *every* pane overlay is
sized from the terminal: `Pane.overlayClamp` measures `termEl.clientHeight`, and
`updateTermShift` reads the live `.xterm-screen` to keep the cursor visible under
the panel. With no terminal, an overlay opens into a zero-height box. That deferral
was real, and it is what this note answers — by the other road.

The thing worth noticing is that **the git view itself never needed a terminal**.
Its inner layout (graph | diff over the changes strip, both dividers) has always
re-clamped against `this.el`'s *own* live size, via its own `ResizeObserver` —
that is how a divider drag redistributes space inside the overlay without ever
touching the PTY. What assumed a terminal was the **container**, in `pane.ts`, not
the view.

So the generalization is a container, not a layout engine:

- **overlay path** — unchanged, byte for byte. `.git-overlay` floats over
  `.pane-term`, height clamped from the terminal, cursor shift and all.
- **pane path** — `.pane-content` is a plain box filling the pane below the header.
  The view is `flex: 1` inside it (all three already were), so it fills the box,
  and its existing `ResizeObserver` re-clamps its sub-panes whenever the box
  changes — a divider drag, a split, a maximize, a window resize. No PTY exists,
  so nothing here can resize one.

`GitView`/`FileEditView` gained exactly one hook each to tell the two hosts apart:
`embedded` (drop the ✕ and the `Esc`-to-close — there is nothing to close back to)
and, for the editor, `onRootChanged` (an overlay keeps a re-root view-local; a pane
adopts it). That is the whole fork.

> The `FileEditView` `embedded` / `onRootChanged` pair is not new: it was built and
> reviewed in PR #215 round 1, then reverted with that round when #214's pane became
> a file *manager* instead. It is resurrected here, where the editor-as-pane is the
> actual ask.

## What a content pane still is

Everything a pane is. It splits, docks, drags, maximizes, minimizes to a chip,
restores, and renames — because the grid sees a normal `Pane` and the PTY-less
kinds differ only in what fills the content box. The chrome that describes a
*shell* is hidden (`.is-content`): the folder chip cd's a shell, the branch chip
opens the git overlay, and the overlay buttons need a terminal to measure. What
stays is what still means something.

Two rules the CSS enforces, both learned the hard way:

- Hide the chip **items** (`.pane-meta-item`), **never `.pane-meta`** — that box is
  the header's flex spacer, and `display: none`-ing it collapses the pane's whole
  button cluster to the left of the header while every other kind keeps it right.
- The empty, never-opened `.pane-term` stays in the flow, and `.pane-content` covers
  it — the same trick `.pane-welcome` uses, and the reason no grid/dock/drag path
  needs a special case.

## Unsaved buffers: where the work can be lost, and what asks

An editor pane is the first pane kind where **loomux itself owns unsaved work** —
and it turns out the Alt+F overlay always did too, silently. So the question is not
"does the new pane guard its buffer" but **every way a buffer can die**:

**1. The pane closes** — header ✕, dock-chip ✕, `Ctrl+Shift+W`. One path:

```
header ✕ / dock chip ✕ / Ctrl+Shift+W
   └─► Pane.requestClose()          ← one-shot `closing` latch
          └─► Pane.confirmClose()   ← the editor PANE's buffer, or the Alt+F OVERLAY's
                 └─► FileEditView.canDiscard()
                        └─► closeDecision(dirty)          [dirtystate.ts]
          └─► (only if allowed) host onCloseRequest → grid.closePane()
```

Anything calling `grid.closePane` directly bypasses the guard — exactly the bug the
dock chip had in #214 (rev-100), and why the routing is stated once, in one method.
Two things this got wrong first time and now doesn't:

- It guarded only the *pane* editor. A terminal pane holding a dirty **Alt+F overlay**
  is just as real, and closing it disposes that view just as finally. `confirmClose`
  takes whichever editor the pane has.
- The guard is **async** (a modal) while the app's shortcut handler is capture-phase on
  `document`: a second `Ctrl+Shift+W` while the dialog is up re-entered and stacked a
  second dialog for the same pane, whose second answer re-entered `closePane` on an
  already-disposed pane. Hence the one-shot `closing` latch, released on a decline.

**2. The tab closes**, disposing every pane in it. A per-pane modal is no use in a
synchronous bulk teardown, so the tab bar asks the way it already asks about something
irreversible: **arm, then confirm** — the same two-step the ✕ of an orchestration tab
(which kills live agents) has always used. `Workspace.hasUnsavedWork()` reports, never
prompts, and the ✕'s tooltip names what is at stake ("will end its agents **and**
discard unsaved edits") rather than only the half it used to know about.

**3. The root moves under the open file.** `FileEditView.pickRoot()` re-points the
tree — and `openRel` is *relative to the root*. Carrying it across a re-root silently
re-binds the buffer to a different file: with `notes.md` open under `C:\A` and the root
moved to `C:\B`, `Ctrl+S` writes A's text to `C:\B\notes.md`, and the conflict dialog
then offers to overwrite a file the human never opened. So a re-root asks about unsaved
edits first (cancelling leaves everything as it was), then **closes the buffer** and
drops the search state, whose hits are paths under a root that is no longer on screen.
The trap predates #217 — it sat in the overlay — but #217 makes a re-root a first-class,
persisted operation on a pane, which is what turns it from obscure into reachable.

**Known gap: app quit** does not ask. The whole-app teardown has no per-pane
interrogation today (nor did it before this feature), and adding one means intercepting
the Tauri window-close event — a different feature, and one that can wedge the quit if
it goes wrong. Stated here rather than quietly left.

The corollary, in `panerestore.ts`: **the buffer is never persisted.** The layout
records where the pane was rooted and *which file it was showing* — a path, re-read
from disk — never what was typed into it. A snapshot that quietly preserved unsaved
text would make the layout file a second copy of the user's work and would undercut the
very guards above, whose whole point is that the human was *asked*.

## Open in file editor pane (from the file browser)

Right-click a row in a file-explorer pane → **Open in file editor pane**: an editor
pane opens beside the browser, rooted where the browser is rooted, with the clicked
file open. On a folder the item reads **Open folder in editor pane** and roots the
new pane at that folder (an editor pane is rooted at a directory, so this is the
same action with nothing to open in it — the label says so rather than pretending a
folder can be edited).

It is the in-app counterpart to **Open**, which hands the file to the OS default
app. Both belong: a `.png` belongs in an image viewer; a `.ts` belongs here.

Three things it obeys, none of them optional:

1. **Declared in `ROW_AFFORDANCES`.** The registry + parity test (#214) force every
   row affordance to state whether it works on a **Go-to-file result**. `edit-pane`
   does, for the same reason every other command does — the action carries the row's
   *path*, not its index.
2. **Bound at menu-open** (`OpTarget`), like every other menu action. A context menu
   is built now and clicked seconds later, by which time a streaming index batch may
   have re-ranked the list underneath it.
3. **The browser doesn't move.** No navigation, no cleared filter, no lost selection.
   Opening a file elsewhere is not a reason to move the list you opened it from.

The pane can't reach the grid itself (it doesn't know which tab it's in), so it asks
its host — `PaneEvents.onOpenEditorPane` — exactly as a welcome pane asks for a split.

## Validation, and what "real" means per kind

A content pane's *only* input is its root, and a pane rooted at nothing has no
content — so unlike a terminal it does **not** fall back to home. The pure rule
("a path was given") lives in `panesetup.ts`; the *reality* check is I/O and lives
in the form, because it differs per kind:

| Kind | Probe | Failure |
| --- | --- | --- |
| `files`, `editor` | `ftRootIsDir` — is it a readable directory? | Inline error in the welcome form, focus back on the field |
| `git` | `gitRepoRoot` — is it inside a git work tree? | Inline `Not a git repository: …` |

`gitRepoRoot` accepts any directory *inside* a work tree (the view resolves the top
level itself), which is the honest bar: pointing a git pane at a subfolder of your
repo should just work.

The same asymmetry shows up on restore, and matters more there: a folder can still
exist and no longer be a repo. So the git pane is re-probed with `gitRepoRoot`, not
a directory check, and — like the other content kinds — fails soft to the welcome
form **in that one slot** with a toast, leaving the rest of the layout intact.

But the two ways that probe can fail are **not** the same, and treating them alike is
a data-loss bug in slow motion. `gitRepoRoot` returning `null` is git's own answer:
*not a repo* — fail soft. `gitRepoRoot` **throwing** is a tooling failure: git isn't on
`PATH` this boot, the path is unreadable, a network share hasn't woken up. That is a
fact about the environment, not about the repo — and failing soft on it would replace
every git pane with a welcome form *and* drop the recorded repo from the next layout
save, losing the path for good over a transient hiccup. So a throw keeps the pane: the
view itself says "git was not found on PATH", and ↻ recovers it when the environment
does.

## What a git pane does NOT do: refresh on focus

The obvious idea — refresh the git view whenever the pane gains focus, since it has no
shell prompt to drive it — is wrong, and the reason is worth recording. A refresh
rebuilds the changes strip wholesale (`renderWorking` → `replaceChildren`), and that
strip contains the **commit-message textarea**. Refreshing on focus would mean:
alt-tab to your browser to copy an issue title, come back, and the commit message you
were halfway through typing is gone.

The overlay never had this problem because its only refresh trigger is a shell prompt —
which cannot arrive while you are typing into the overlay. A pane has no prompt, so it
refreshes on **open**, after **its own actions**, and on the **↻ button**: explicit and
safe rather than implicit and destructive. (Auto-refresh on external repo changes would
need the backend git watch, which is keyed by PTY id — and a git pane has no PTY.)

## Not agents

`tabcounts` keys the agent count on **kind**, not on `live`. All three content kinds
report `live: true` (they *are* functional the moment they exist), which is exactly
why: a counter keyed off `live` would render a tab of viewers as a tab of running
agents. Adding a kind to the union is all it takes to stay excluded — by
construction, not by remembering.

## Files touched

| File | What |
| --- | --- |
| `panesetup.ts` | `editor` / `git` kinds, their plans, `isContentKind` (pure) |
| `pane.ts` | `startContent()`, `ContentPaneKind`, `requestClose()`'s latch + `confirmClose()`, `hasUnsavedWork()`, `onOpenEditorPane` |
| `grid.ts` | `openContentPane()` (was `openFilesPane`) |
| `fileedit.ts` | `embedded` / `onRootChanged` hooks, `canDiscard()`, `dirty`, `openPath()` / `openPathRel`, the re-root buffer reset |
| `tabbar.ts` / `workspace.ts` / `tabs.ts` | a tab holding unsaved edits closes behind the arm-and-confirm |
| `gitview.ts` | `embedded` hook (the ✕ + `Esc` fork). Its layout needed nothing. |
| `fileexplorer.ts` + `filemenu.ts` + `fileexplorermodel.ts` | the `edit-pane` affordance, declared and bound |
| `tabstore.ts` / `panerestore.ts` / `tabcounts.ts` | the two kinds through the restore + counting paths; the editor's open `file` (a path, not a buffer) |
| `styles.css` | `.pane-content` / `.is-content` (generalized from `.pane-files` / `.is-files`) |

No backend changes: `ft_list_dir` and `git_repo_root` already take a root, and both
new panes are built from commands that existed. `Cargo.lock` is untouched.

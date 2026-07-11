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

## Unsaved buffers: one close path

An editor pane is the first pane kind where **loomux itself owns unsaved work**.
That makes the close question real: a header ✕, a dock-chip ✕, or `Ctrl+Shift+W`
must not silently discard a buffer the human typed into.

There is therefore exactly one human-initiated single-pane close path:

```
header ✕ / dock chip ✕ / Ctrl+Shift+W
        └─► Pane.requestClose()  ─► host onCloseRequest
                                        └─► Pane.confirmClose()  ─► FileEditView.canDiscard()
                                                                        └─► closeDecision(dirty)   [dirtystate.ts]
                                            └─► (only if allowed) grid.closePane()
```

Anything that calls `grid.closePane` directly bypasses the guard — which is exactly
the bug the dock chip had in #214 (rev-100), and why the routing is stated once, in
one method, instead of re-derived per affordance. The *decision* ("dirty means ask")
lives in `dirtystate.closeDecision` and is shared with the editor's own `Esc`/✕, so
the two cannot drift; `test/dirtystate.test.ts` pins each consumer against it.

Automatic closes — a PTY exiting, a group ending, a tab disposing, app shutdown —
deliberately do **not** come through here. They are bulk operations with their own
semantics, and turning each into a per-pane interrogation is a different feature.

The corollary, in `panerestore.ts`: **the buffer is not persisted.** The layout
records where the pane was rooted, never what was typed into it. A snapshot that
quietly preserved unsaved text would make the layout file a second copy of the
user's work and would undercut the very guard above — the point of which is that
they were *asked*.

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
| `pane.ts` | `startContent()`, `ContentPaneKind`, `confirmClose()`, `onOpenEditorPane` |
| `grid.ts` | `openContentPane()` (was `openFilesPane`) |
| `fileedit.ts` | `embedded` / `onRootChanged` hooks, `canDiscard()`, `openPath()` |
| `gitview.ts` | `embedded` hook (the ✕ + `Esc` fork). Its layout needed nothing. |
| `fileexplorer.ts` + `filemenu.ts` + `fileexplorermodel.ts` | the `edit-pane` affordance, declared and bound |
| `tabstore.ts` / `panerestore.ts` / `tabcounts.ts` | the two kinds through the restore + counting paths |
| `styles.css` | `.pane-content` / `.is-content` (generalized from `.pane-files` / `.is-files`) |

No backend changes: `ft_list_dir` and `git_repo_root` already take a root, and both
new panes are built from commands that existed. `Cargo.lock` is untouched.

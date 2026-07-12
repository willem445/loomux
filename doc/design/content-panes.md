# Content panes: the editor and the git view as pane kinds (#217), and the workflow pane (#222)

A **content pane** is a pane that *is* a surface rather than a process. No shell,
no CLI, no PTY — the pane's content is a view, permanently.

#214 built the first one (`files`, the file manager). #217 adds the two the human
actually asks for most: `editor` (the #174 tree + code editor + #207 search) and
`git` (the #208 git view). Both surfaces already existed as **overlays** inside a
terminal pane, behind `Alt+F` and `Alt+G`.

#222 adds a fourth, `workflow` — the first with no overlay ancestor and the first whose
subject is loomux itself: the repo's `.loomux/workflow.yml`. Its own section is below;
everything in this note up to it applies to it unchanged, which is the point of the kind
being a kind.

**What "the overlays are unchanged" means, exactly** — because it is exact for one of
them and deliberately not for the other:

- **`Alt+G` (git): unchanged, byte for byte.** The only fork is `embedded`, which the
  overlay host doesn't set. Nothing else about it moved.
- **`Alt+F` (editor): unchanged in every way the pane kind is responsible for** — same
  overlay, same terminal-derived sizing, same `Esc`/✕, same view-local root (an overlay
  does **not** adopt a re-root; only a pane does). But it **inherits every unsaved-buffer
  fix** the editor-as-pane forced into the open, and inherits them on purpose (#217 the
  first two, #219 the rest):
  1. a re-root now closes the open buffer (asking first if it's dirty) instead of
     leaving it bound to a path under the *old* root;
  2. closing a pane that holds a dirty overlay buffer now asks, instead of discarding it
     silently;
  3. quitting the app with a dirty overlay buffer now asks;
  4. a pane whose process dies — or whose group ends — no longer disposes a dirty overlay
     buffer with it;
  5. its **Discard** actually discards, instead of hiding the buffer and asking again.

  Every one of those bugs was *always* in the overlay; the pane kind only made them
  reachable enough to notice. They are fixed for it rather than fixed only for the new
  pane, because a guard that covers the new hole and leaves the older, likelier one open
  is not a guard — it is a claim. See *Unsaved buffers* below.

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

- **overlay path** — its SIZING is unchanged, byte for byte: `.git-overlay` floats over
  `.pane-term`, height clamped from the terminal, cursor shift and all. (Not a claim
  about the overlays' *behavior* — `Alt+F` inherits two fixes; see the top of this note.)
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
"does the new pane guard its buffer" but **every way a buffer can die**. There are six,
and every one of them routes through the same pure gate (`dirtystate.closeDecision`):

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

**4. The app quits** (#219 — this was the stated gap; it is now the design). Quitting
loomux used to discard every dirty buffer without a word. The close is now gated:

```
title-bar ✕ / Alt+F4 / the OS asks the app to quit
   └─► pty.guardAppClose()                    ← Tauri's onCloseRequested; the close waits
          └─► unsavedBuffers()                ← EVERY tab (hidden too), every pane
                 └─► Workspace.bufferReports() → Pane.bufferReport() → the editor's or
                                                 the Alt+F overlay's buffer
          └─► quitDecision(dirty)             ← the SAME closeDecision gate  [dirtystate.ts]
                 ├─ "close"   → flushTabs() → quit, silently
                 └─ "confirm" → one modal listing every buffer
                        ├─ Quit anyway → flushTabs() → quit
                        └─ Cancel      → preventDefault(); the app stays, buffers intact
```

Three choices worth defending:

- **One consolidated ask, not a save prompt per buffer.** A human quitting with six dirty
  files does not want six dialogs; they want to know six files are dirty and decide once.
  A chain of modals is how you train someone to hammer Enter through them — which is the
  opposite of what a guard is for. The dialog *lists* what is unsaved (tab · pane — file,
  with Alt+F overlays marked as such, since "which pane is that in?" is the entire
  difficulty of the overlay case), and offers **Quit anyway** / **Cancel**. Cancel leaves
  everything exactly as it was, so the human can go save.
- **Nothing unsaved → no dialog.** A confirm that fires when there is nothing to lose is
  a confirm people stop reading.
- **The session snapshot is flushed on the way out — but not at any price.** Persistence
  is fire-and-forget everywhere else (a failed write just waits for the next change); a
  quit is the one moment there is no next change, so the quit path *awaits* the write. The
  #194 restore still brings the layout back — including from a "Quit anyway".

  That await is then **raced against a 1.5s deadline** (`withDeadline`), and on expiry the
  close proceeds anyway. Failing open on a *throw* — which the guard does — is not enough:
  a promise that HANGS never throws, so a stalled disk or a wedged IPC would leave the
  human with a ✕ that does nothing. The trade is deliberate and one-sided: a possibly-stale
  snapshot costs at most one edit's worth of layout (the fire-and-forget write is never
  further behind than that, and it is *layout*, not content), while an unquittable app
  costs everything and cannot be recovered from inside the app. **A stale snapshot beats a
  window that won't close.**

The mechanics: `guardAppClose` (in `pty.ts`, with the rest of the Tauri surface) wraps
Tauri's `onCloseRequested`, which holds the close while our handler runs and destroys the
window unless we `preventDefault()`. Destroying is what fires the backend's
`WindowEvent::Destroyed` — the PTY kill-all and the clean-exit sentinel in `lib.rs` — so
a permitted quit tears down exactly as it did before. We put a question in front of the
existing path; we did not add a second one.

Three ways this hook can go wrong, and what each costs — all three land on the same side,
because **a window that won't close is the worst outcome available here**:

| Failure | Guarded by | Why that way |
| --- | --- | --- |
| The permission is missing | `core:window:allow-destroy` in the capability set | Registering a JS close-requested listener stops Rust from closing the window itself. Without the permission, the JS destroy is denied and the ✕ silently does **nothing**. |
| The guard throws | fail **open** — the close proceeds | Not asking about a buffer is recoverable; an unquittable app is not. |
| The final save hangs | the 1.5s `withDeadline` race | The fail-open catch cannot help: a promise that never settles never throws. |

And one re-entrancy guard: the confirm is async, so a second ✕ (or Alt+F4, or an impatient
double-click) fires `onCloseRequested` again while the dialog is up, and would stack a
*second* quit dialog whose answer races the first's. A `SubmitLatch` — the same one-shot
latch the welcome form's submit (#194 P1) and `Pane.requestClose` use — refuses the
duplicate: the ask that is already on screen owns the decision. Cancel `release()`s it (a
later ✕ must ask again); "Quit anyway" `finish()`es it (the window is going away; admit
nothing more).

**5. A process dies, or a group ends.** Both are *automatic* teardowns — nobody clicked
"close this pane" — and both used to dispose a pane holding a dirty `Alt+F` buffer.

The rule, stated once in `dirtystate.keepOpenOnExit` and obeyed by both reapers: **an
automatic teardown never destroys a buffer.** A pane whose process exited stays open if
it holds unsaved edits, exactly as a crashed command pane already stayed open to show its
output — and its exit banner says *which* reason, because a pane that outlives its process
for an invisible buffer otherwise just reads as a bug ("why didn't this close?"), and the
buffer it is protecting stays invisible, which is how it gets lost anyway.

Group-end is the same rule, and the distinction it turns on is worth naming: ending a
group *is* a deliberate, confirmed act — but what it deliberately destroys is **agents**,
not the human's half-written file. The two only got conflated because they live in the
same pane. The agent is already dead by the time the frontend reaps it, so keeping the
pane costs nothing; a toast says how many stayed and why, and closing one later asks like
any human close.

So the full picture: **automatic paths keep; human paths ask.** No path discards silently.

**6. "Discard" now discards.** The overlay used to answer *"Discard unsaved changes?"* by
hiding itself and keeping the buffer — press `Alt+F` again and the edits were back, still
dirty, and the next close asked the same question. A Discard that discards nothing is a
dialog that lies, and a second ask is how people learn to click through the first one. The
yes-branch now reverts the buffer to the last-saved snapshot (`dirtystate.discardEdits` —
trivial on purpose: it is where the rule is *stated*, so the view cannot quietly
re-implement "discard" as "hide"). It also fixes a case nobody had noticed: discarding in
order to open another file, when that open then *failed*, used to leave the discarded
edits sitting there. (Hiding a view without dropping its buffer is a legitimate thing to
want — it is just not "discard", and it would need its own affordance and its own word.)

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

## The workflow pane (#222)

A fourth content kind, and the first one whose subject is loomux itself: `.loomux/workflow.yml`
— the repo's **agent workflow**. Which blocks a run may use (a planner, a worker, three
focused reviewers), what each is (prompt / profile, model, agent CLI), the **edges** between
them, and the **merge gate**. Committed, so it is shared with everyone who clones the repo
(the #51 requirement, restated by the human on #222).

It is a pane and not an overlay for the same reason the git view is: it is a **station**,
not a look. You keep it open beside the orchestrator while you tune the roster.

### The file is the source of truth — the GUI is a view over it

This is the whole design, and it is a decision with a body count behind it. **OpenAI's Agent
Builder — the flagship GUI-canvas-as-source-of-truth — shipped in Oct 2025 and is being shut
down in Nov 2026, with the migration path being *back to code*.** Meanwhile LangGraph Studio,
Temporal's UI and GitLab's CI editor all deliberately make the GUI a **debugger/visualiser
over a text-defined graph** — Studio cannot edit topology at all. Kestra states the rule we
follow outright: *"even if you use the UI to modify a workflow, the platform is still
generating and updating the YAML definition under the hood."*

So the pane holds ONE buffer — the YAML — and three views over it:

```
  roster + property form   an edit here SERIALIZES the model back over the buffer
  raw YAML (textarea)      an edit here RE-READS the model from the buffer
  derived graph            READ-ONLY. It cannot write. It cannot corrupt the file.
```

`workflowmodel.ts` is the pure half (parse → validate → derive → serialize) and holds every
rule; `workflowview.ts` is DOM. That split is the house convention (`taskboard` ↔ `tasksview`)
and it is what lets the validation pass — the part that actually earns the feature — be
unit-tested without simulating a DOM.

**The one rule the sync has to obey: while the YAML does not PARSE, the form is disabled.**
A form edit serializes the model back over the buffer, so serializing a model we only half
understood would silently destroy the broken text the human is in the middle of fixing. A
syntax error therefore disables the form and says why (the YAML tab and the findings strip
stay live, which is where the fix happens). Every *other* kind of breakage — an unknown kind,
a dangling edge — still renders, as a stub with a finding, because **a block you cannot see is
a block you cannot repair**; refusing to open a file you can't fully understand is ComfyUI's
#1 import-failure class.

### Advisory edges, enforced gates — and they must not look alike

An **edge is advisory**: it declares the intended path. The orchestrator still schedules, and
that is deliberate (#222 §2g) — its mergeability judgment is the thing that makes it good, and
a static DAG would re-encode that as conditional sprawl. A **gate is enforced**: the backend
refuses `gh pr merge` until every reviewer it names has recorded a PASS.

One of those two can stop a merge and the other cannot, so the graph draws them differently:
a solid arrow into a block, versus a dashed amber connector into a dashed gate box that is not
shaped like a block at all. A picture that rendered them identically would be a picture that
lies about which half of the file has teeth.

### What the validation pass is for

Every workflow tool surveyed for #222 skipped this. Flowise, Langflow and Dify all discover a
dangling reference at RUN time; Dify will happily *publish* a workflow whose node isn't even
installed. The pass is pure, cheap, and it is the difference between "your workflow failed
after spawning two agents" and "block `rev-perf` doesn't exist — the merge gate names it":

| Finding | Why it can't wait for a run |
| --- | --- |
| unknown `kind` | A workflow may define any *persona*; it may never define a *capability*. `kind` picks one of the four closed classes and inherits its structural guarantees. |
| unknown `cli` | loomux can only spawn what it can spawn. |
| duplicate / malformed `id` | The id is the identity. Two blocks sharing one makes every edge naming it ambiguous. |
| edge to a nonexistent block | The dangling-reference class Dify ships. |
| gate names a nonexistent block, or one that isn't a reviewer | **A gate that could never open.** Only a reviewer records a verdict. |
| threshold > reviewers | Same thing, arithmetically. |
| isolated / unreachable block | A *warning*, not an error — edges are advisory, so this is a workflow that still runs. It is just almost certainly a fan-out you forgot to wire. |

A cycle is **not** a finding: worker ⇄ reviewer is the rework loop, and it is how loomux
actually works. What *is* a finding is a graph with nowhere to start.

### Two rules the file keeps, both earned from someone else's scar

- **`id` is the identity; `name` is display only.** n8n keys its graph by the node's *display
  name*, so a rename silently breaks every edge and expression pointing at it — a bug class its
  own maintainer calls *"far from perfect."* Here the id is immutable once created (the form
  disables the field and says why), and a rename touches nothing else.
- **No coordinates in the semantic file.** Dify, ComfyUI and Langflow all embed x/y, so nudging
  a node churns the logic diff. The graph here is *derived* (layered by longest path from the
  entry blocks), so there is no layout to store at all — and if one is ever drawn by hand, it
  goes in `.loomux/workflow.layout.json`, never in the workflow.

The **canonical formatter** follows from the same concern: fixed key order per block, edges
grouped by source, references ordered by the roster — so a save produces a legible `git diff`
rather than a reshuffle. Blocks keep their *authored* order, which is the one place a stable
sort would do harm: the roster reads top-to-bottom, and re-sorting it on every save would churn
the very diff the formatter exists to keep readable. Unknown keys (from a file written by a
newer loomux) are preserved verbatim across the round-trip — an older pane must not silently
strip a field the user's backend depends on.

**One emitter, quoting for the strictest context.** The formatter serves both block context
(`name: …`) and *flow* context (`reviewers: [a, b]`, an unknown key's array), and in flow
context `, [ ] { }` are structural. The emitter therefore quotes any value containing one,
even where block context wouldn't need it. This is not fastidiousness: with the flow
characters left out, an `allow: ["Bash(gh pr view --json title,body)"]` re-read as *two*
entries and a `tools: ["fmt{x}"]` re-read as `null` — on an ordinary form edit, because every
form edit re-serializes the file. A quote that wasn't strictly necessary costs a character; a
quote that was missing costs the user's data. (Found in review, rev-5 F1.)

**`authored_with:`** — an optional top-level key naming the loomux that *created* the file
(§4's "record the loomux version that authored it", the Langflow `last_tested_version`
lesson). Written exactly once, when the pane creates a new workflow; on an existing file it
rides the unknown-key bag and round-trips verbatim. Deliberately *not* restamped on every
save: it records who authored the workflow, not who last looked at it, and a version line that
churned on every model-name tweak would be noise in a file whose whole point is a readable
history. **Sub-PR 1: this key is optional and pass-through — a validator should tolerate it,
not require it.**

### v2: the canvas edits the file (and the empty state was lying)

The human demoed v1 and asked for three things. Two were bugs wearing a UX complaint, and one
reversed a decision — which is what a demo is *for*.

**The empty state was lying, twice.** It said *"No workflow in this repo yet"* for a repo that
had one, and it offered to create a file it could not create. Two independent causes, both
reproduced against the backend before either was touched:

1. **Every read failure was treated as "there is no file".** Only `not-found` means that. The
   ordinary way for a Windows user to produce a workflow file is from PowerShell — whose `>`
   and `Out-File` write **UTF-16**, which is not valid UTF-8, which the backend correctly
   reports as `binary`. That landed in the empty state behind a toast that had already gone,
   and then invited the human to *create a starter over the top of a file the pane had refused
   to show them*. There are now two states: **start** (there is no file — a front door) and
   **error** (the file is there and we can't read it — which says why, offers Retry, and offers
   nothing that writes).
2. **The create path could never have worked.** `ft_write_file` writes atomically (temp file +
   rename) and does **not** create parent directories, so writing `.loomux/workflow.yml` into a
   repo with no `.loomux/` — i.e. every repo that has never had a workflow, which is precisely
   the repo the create button exists for — failed with a raw io error. The pane now ensures the
   directory first, via `fm_new_folder` (#214's "New folder" — no new backend command; an
   "already exists" failure *is* the success case, so it is swallowed and the write is left to
   be the thing that reports a real problem).

   Between the two, the pane both mis-reported an existing workflow as absent **and** could not
   create the one it offered to create — which is exactly what "it says there's no workflow even
   though there is one" feels like from the outside.

A third, found while fixing them: a **BOM** made a perfectly good file look broken. The reader
took U+FEFF as part of the first key, so `version: 1` arrived as a key named `﻿version` and the
pane reported `version-missing` against a file the human could see was correct — and the
character is invisible, so nothing in the error could have led them to it. Stripped in the pure
parser, with a regression test.

**The start surface** replaces the page of nothing: a strip at the top of the pane with one line
of what a workflow is, the roster the button is about to write, and a **Create workflow** button
that scaffolds a real, commented, valid file (`scaffoldWorkflowText`) — then lands them in the
canvas on it. A commented scaffold is how every config-as-code tool worth using introduces
itself, and it costs one string. (The comments do not survive a canonical re-serialize; that is
the honest trade of having one canonical shape, and it is why the scaffold is offered at
*creation* rather than being something the formatter tries to preserve.)

**The graph is now editable**, which reverses v1's read-only decision (§2f/Q6). The reasoning
behind that decision was *"a canvas that can corrupt the file is worse than no canvas"* — and it
is answered rather than abandoned:

```
  drag a node        → .loomux/workflow.layout.json          (never the workflow)
  drag port → node   → connectBlocks()   → canonical YAML    (the pure model, same as a form edit)
  click edge, ✕      → disconnectBlocks() → canonical YAML
  + Block            → asks for the ID   → addBlock()
  Delete             → removeBlockAt()   → takes its edges and its gate seat with it
```

Every gesture goes through the pure model and out through the same canonical formatter as
everything else, so **the canvas cannot express anything the YAML can't**, cannot write a
position into the semantic file, and cannot invent an identity. It is a second way to *edit* the
file, not a second source of truth — which was the whole content of the original objection.

Three commitments the canvas keeps, each because someone else broke it:

- **It asks for the id.** Dify mints `node_1720794829558`; n8n keys its graph by the *display
  name*, so a rename silently breaks every edge pointing at it. Here the id is asked for once,
  validated as you type (a malformed or duplicate id cannot be confirmed at all, so it never
  becomes a finding to decode later), and immutable thereafter. The name stays display-only.
- **Positions are a different file.** `workflowlayout.ts` owns `.loomux/workflow.layout.json` —
  keyed by block id, which is only safe *because* ids are immutable; pruned on save, so a
  deleted block doesn't leave a coordinate behind forever; and treated as disposable, because
  nothing in it is anyone's work. A layout that is missing or corrupt is **recomputed**, never
  reported: a broken `workflow.yml` is a problem the human must see, a broken layout is a picture
  we can redraw. A drag is therefore *not* unsaved work and does not gate a close — a dialog
  asking whether to save the fact that you nudged a box is a dialog that teaches people to click
  through dialogs.
- **The geometry is pure.** Hit-testing, edge routing and placement are arithmetic, so they are
  in `workflowlayout.ts` with `test/workflowlayout.test.ts` around them, DOM-free — the alternative
  is validating a canvas by dragging things and squinting. The DOM layer is left with nothing to
  get wrong but the wiring. (The edge hit-tolerance is why an edge is clickable at all: it is a
  1.5px line, and nobody hits that.)

And the gate is still not a node you can drag or wire. It is not a block — it is a *rule about*
blocks — and making it draggable would imply it can be rewired, which is the single most
important thing about it that isn't true.

### Comments, and the save that eats them

A form or canvas edit re-serializes the **whole workflow from the model**, and the model does not
carry comments — it cannot, because it is a workflow, not a document. For a file loomux wrote,
that costs nothing. For a file a **human** wrote, the comments are frequently the most valuable
lines in it: this repo's own `.loomux/workflow.yml` is 126 lines of which **60 are comments**
explaining the roster and the `.github/agents/` convention. One dragged edge and a `Ctrl+S` used
to take all 60, silently, and hand the human a whole-file diff to discover later.

Three things follow, and the order matters:

1. **The pane says so, once, before it does it.** `rewriteImpact` (pure, in `workflowpane.ts`) is
   the signal: *are we about to write canonical text over a file that was not canonical?* If so
   the save asks, naming what is lost ("the comments on 60 lines will be dropped"), with **Cancel
   as the default** — the only dialog in this pane where the affirmative is not the focused
   button, because it is the only one asking about work that is not recoverable. Once per file,
   not once per save: a human who has said "yes, canonicalize it" has said it, and re-asking on
   every `Ctrl+S` is how you train someone to stop reading the question.
2. **A file already in canonical form saves silently** — anything loomux itself wrote. A confirm
   that fires when nothing is at stake is a confirm people click through, and then they click
   through the one that mattered.
3. **The YAML tab is unaffected**: it saves exactly what you typed, comments and all. The
   rewrite is a property of *re-serializing from the model*, not of saving — so the guard keys on
   the **reformat**, never on the comment count alone. That distinction is not fussiness: a form
   or canvas save *always* reformats (a commented file is never canonical, and the model always
   emits canonical text), so a comments-only warning could only ever fire on text the human typed
   themselves — i.e. it would fire exactly when they had just deleted a comment on purpose, to
   tell them they were about to delete a comment. A dialog that explains your own keystroke back
   to you is how a guard becomes noise, and noise is how the guard that matters gets clicked
   through.

**The test used to claim the opposite, and could not fail.** It asserted that a save "does not
churn the file" while comparing the canonical form against *itself* — never against the bytes on
disk. It now asserts the truth (the shipped file is **not** canonical; a save rewrites it) and
then asserts the guard. A test that cannot fail is worse than no test: it is a claim with a green
tick next to it.

Comment-preserving serialization is the real fix and it is a genuine feature — round-tripping
comments through a structure the form can still rewrite. It needs its own design and its own
review, so it is **filed as a follow-up** rather than smuggled in behind a bug fix.

### The three questions the view was never allowed to answer

Review found the v2 pane getting three things wrong, and they had the same shape: the *view* was
deciding something that is a **rule**. Rules live in `workflowpane.ts` now — pure, tested, stated
once — the same move `dirtystate.ts` makes for the editor.

| The rule | What the view did instead |
| --- | --- |
| `paneSurface` — which surface to show | Showed *"no workflow in this repo yet"* for a file that was **there** and merely unreadable, and then offered to create one **over the top of it**. |
| `savePlan` — how a save may write | A **create** wrote with a null expected hash, which the backend reads as *write unconditionally*. A workflow that arrived while the pane sat on its start surface (an agent wrote one, a `git pull` brought one in) was **destroyed**, with a green "Saved" toast. |
| `layoutPruneIds` — what the layout may forget | Pruned the layout file against the **unsaved buffer**, so deleting a block (without saving) and then dragging another one wrote the deletion to disk *before the human had made it*. |

The save fix is the one worth naming: a create now **claims the path atomically** with
`fm_new_file` (which is `create_new(true)` — "create, but only if it isn't there", one syscall,
no TOCTOU window) and then writes against the claimed file's own hash. So even the sliver between
the claim and the write is an ordinary conflict-guarded write, and the *only* code path left that
can overwrite a workflow is a human answering **Overwrite** in the conflict dialog — which is an
answer to a question, not a save plan. `src-tauri/tests/workflowfile.rs` pins both halves of why
that works, at the layer where the behaviour lives.

And one more, in the module whose whole job is to be the hostile-input-proof half of the canvas:
a block whose id is **`constructor`** is a perfectly legal workflow (the validator reports zero
findings), but `positions["constructor"]` on a plain object literal returns the *inherited* `Object`
function — truthy, so the canvas read `{x: undefined, y: undefined}` off it, and `NaN` reached the
SVG's width and height. The canvas did not render, for a valid file, keyed by an id that can never
be changed. The position table now has **no prototype** and is read through `Object.hasOwn`.
Tightening the +Block dialog would not have fixed it: an id can arrive from a hand edit, the YAML
tab, or an agent, and none of those pass through a dialog. Fix the lookup, not the instance.

### The rules were right and the screen disagreed: `hidden` doesn't hide

Everything above was true, tested, and shipped — and the pane still failed in the demo, in three
ways at once. It rendered its **three mutually exclusive surfaces simultaneously**: a *"Can't read
.loomux/workflow.yml"* banner, the *"Start a workflow"* front door, **and** the workflow itself,
loaded, in the roster, badged **valid**. It said it could not read a file it had plainly just read.
And pressing **Create workflow** — the button from the start surface, sitting live on top of a
loaded workflow — scaffolded straight over it. Exactly the data-loss class the claim-then-write
fix above was written to close.

They are one bug, and it is not in any of the rules. `render()` picks one surface with
`paneSurface` and sets `hidden` on the other two, correctly. **`hidden` was doing nothing.** The
UA stylesheet's `[hidden] { display: none }` lives in the *user-agent origin*, and every author
declaration outranks it — origin is decided before specificity is ever consulted. The pane's
surfaces are `.wf-start`, `.wf-body`, `.wf-findings`, all `display: flex`. So the attribute was set,
and the element stayed on screen, and the code that "hid" it had no way to find out.

Read the three symptoms again with that in hand and they collapse into it:

- **All three surfaces at once** — none of them was ever hidden.
- **"Can't read .loomux/workflow.yml"** — the *static title* of an error surface that had never
  been shown and never been hidden. The read never failed. There was no error. (The detail line
  under it was blank, which is the tell: `errorTextEl` is set from `loadError`, and `loadError`
  was `null`.)
- **Create overwrote the workflow** — the button was visible over a loaded workflow, so it was
  pressable, so it was pressed. And *every guard below it worked*: the pane had read the file and
  held its hash, so `savePlan` returned an ordinary `guarded-write`, the hash **matched** (nothing
  else had touched the file), and the backend wrote what it was told. Claim-then-write never armed,
  because claim-then-write is what happens when the pane believes there is **no file** — and here
  it knew there was one. There was no missing refusal downstream. The button should not have been
  pressable.

The attractive theory was a **root/cwd mismatch** — the read probe resolving one root, the write
another. It is wrong, and `src-tauri/tests/workflowfile.rs` now contains the experiment that killed
it: the process cwd pointed at a decoy repo of identical layout, the root in every spelling Windows
hands over (backslashes, trailing separator) against the frontend's forward-slash `rel`. Read and
write resolve the same absolute file, every time. A successful read *proves* the probe worked — the
pane could not have shown a valid workflow otherwise — which is the deduction that ends the theory:
a "Can't read" banner over a file that read fine cannot be a read failure.

**Two fixes, and only one of them is in the pane.**

`[hidden] { display: none !important; }`, once, at the top of `styles.css`. `!important` and not
another per-class `[hidden]` companion — the file had **nine** of those, added one bug at a time,
and the workflow pane's seven elements are what it cost to keep rediscovering the trap by hand. An
author-origin `[hidden]` carries the same specificity as any single class, so without `!important`
the winner is decided by **source order**: by whether the next person to write `display:` happens to
write it below that line. `test/hiddenrule.test.ts` models the cascade and asserts the invariant
over the whole stylesheet — *if the code hides an element, the element goes away* — so the next
`display: flex` on a toggled class fails a test instead of a demo.

And `createAllowed` (`workflowpane.ts`): a create is permitted on the **start surface and nowhere
else** — the same single decision that draws the button — and `scaffold()` refuses if it is called
anywhere else. Because the start surface is *by definition* "no file, empty buffer", every create it
permits is a `claim-then-write`; the two rules cannot drift apart into a create that overwrites. The
lesson is the one the CSS taught: **visibility is not a safety property.** The pane's defence
against scaffolding over a workflow was that the button was *supposed to be invisible*, and a
stylesheet was all it took to make that false.

### The rest is the pattern #217 already set

The workflow file rides in the persisted `file` field the editor pane added; `cwd` carries the
repo. Restore probes the ROOT (`ftRootIsDir`) and deliberately not the file: a repo whose
`.loomux/workflow.yml` doesn't exist is not a broken pane, it is a pane with nothing in it yet
— and it opens on an empty state offering to create one. Reads and writes go through the same
hash-guarded `ftReadFile`/`ftWriteFile` the editor uses, so an **agent rewriting the workflow
it is running under** is a conflict the human resolves, not a silent overwrite. And the view
implements the same `dirty` / `canDiscard()` / `bufferReport()` contract the editor does, so
every guard above — pane close, tab close, app quit, a dead process — covers it by joining one
list (`Pane.unsavedHolder()`) rather than by four more remembered call sites.

No backend commands were added: `ft_read_file` / `ft_write_file` already took a root and a
relative path, and the workflow file is just a file.

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
| `styles.css` | `.pane-content` / `.is-content` (generalized from `.pane-files` / `.is-files`); `.dlg-list` for the quit confirm |
| `dirtystate.ts` (#219) | `dirtyBuffers` / `quitDecision` / `dirtyBufferLines` (who is holding what, and may we quit), `keepOpenOnExit` (does a dead pane stay, and why), `discardEdits` (discard means discard) — all pure, all node:tested |
| `pty.ts` / `main.ts` (#219) | `guardAppClose` (the Tauri close hook, kept on the one Tauri seam) + the quit guard and its awaited `flushTabs` |
| `orchestration.ts` (#219) | group-end keeps a pane holding unsaved edits, and says so |
| `workflowmodel.ts` (#222) | the pure half: the YAML subset, the schema, the canonical formatter, the pre-run validation pass, the derived graph — all node:tested |
| `workflowview.ts` (#222) | the DOM: roster + property form, raw YAML, the **editable canvas**, findings strip, save/conflict, the start + error surfaces — and the same `dirty` / `canDiscard` / `bufferReport` contract the editor has |
| `workflowlayout.ts` (#222 v2) | the canvas's pure half: `.loomux/workflow.layout.json`, placement, hit-testing, edge routing — all DOM-free, all node:tested |
| `modal.ts` (#222 v2) | `promptModal` — one line of text, validated on every keystroke (the affirm button is disabled while the id is bad), so a new block can be ASKED for its id instead of being given a generated one |
| `workflowpane.ts` (#222 v2) | the pane's pure DECISIONS — which surface it shows, how a save is allowed to write, what the layout file may forget. Three rules the view used to hold itself, and got wrong. Plus `createAllowed` (#222 live fix): a create is permitted on the **start surface and nowhere else**, so it can never be reached over a workflow that is already there |
| `styles.css` → `[hidden] { display: none !important; }` (#222 live fix) | app-wide, one line: an author `display:` rule out-ranks the UA's `[hidden]` **by origin**, so `el.hidden = true` was silently ignored on all seven of the workflow pane's toggled elements — the pane drew its three exclusive surfaces at once, and the "Create workflow" button sat live over a loaded workflow. Also fixes the group view's budget meter, which never obeyed its own `Off ⇒ hidden` |
| `test/hiddenrule.test.ts` (#222 live fix) | models the cascade over the real stylesheet and asserts the invariant *if the code hides an element, the element goes away* — for every class in the file, not just today's seven victims |
| `src-tauri/tests/workflowfile.rs` (#222 v2) | pins the two backend facts the create path rests on: a null-hash write clobbers (why a create must never use one), and `new_file` refuses atomically without truncating (why claiming the path fixes it) |
| `filemenu.ts` / `fileexplorer.ts` / `fileexplorermodel.ts` (#222) | the `workflow-pane` row affordance — declared, and offered only on a `.yml`/`.yaml` row |
| `launcher.ts` (#222) | the `Workflow` kind in the welcome form's picker (one option, one plan, one probe — the same directory probe files/editor use) |

No backend changes: `ft_list_dir` and `git_repo_root` already take a root, and all
three earlier panes are built from commands that existed. The workflow pane (#222) adds
none either — `ft_read_file` / `ft_write_file` already take a root and a relative path,
hash-guard included, and a workflow file is just a file. `Cargo.lock` is untouched.

# The right-side dock: git, files and editor, following the active pane (#1020 item 6)

Status: implemented on `integration/ui-redesign`. Issues: #1020 item 6 (the ask
this builds), #934 (the original ORCA-sidebar direction), #1018 (the integration
PR the human demos).

The ask, from the human's live demo of #1018: *"right sidebar hosting the
built-in git / file-explorer / file-editor, optionally open, auto-loaded to the
active pane's current directory."* #934 says the same thing earlier and adds the
reference — ORCA's right sidebar, a strip of view-switch buttons over one docked
panel — plus one requirement worth quoting because it shapes the whole design:
*"the sidebar auto-updates to the directory of whichever pane is currently
highlighted/focused."*

## The one structural decision, and why it is not negotiable

**The dock is an overlay. It is `position: absolute` inside `#workspace`, and it
occludes panes rather than displacing them.**

The obvious implementation is the one the app already has an example of, and
that example is a warning rather than a precedent. `#sessions` — the left
session browser — is an in-flow flex sibling of `#grid-area` at `width: 344px`
with a `0.24s` width **transition**. Opening it therefore shrinks the grid,
which resizes every terminal in it, *on every frame of the animation*. That is
precisely the continuous, chrome-driven PTY resizing CLAUDE.md constraint 1
exists to refuse, and `doc/design/ui-redesign.md` §X10 names it explicitly as
the mistake a right-hand rail must not repeat.

So the dock is out of flow. An absolutely-positioned child of a flex container
is **not a flex item**, so it cannot move `#grid-area` by a pixel: no pane's
`ResizeObserver` fires, no `applyFit()` runs, no ConPTY is resized. The
constraint holds *by construction* rather than by care — there is no code path
that could regress it, because there is no code path that touches the grid at
all. The only stylesheet change to the existing layout is `position: relative`
on `#workspace`, which establishes a containing block and moves nothing.

**The cost is real and is stated rather than hidden: an open dock covers the
right-hand panes.** Three things pay for it:

- it defaults **closed**, so nothing is covered before anyone asks;
- it toggles in one click from the top bar, so reclaiming the space is cheap;
- it may never cover the whole workspace — `DOCK_TERM_RESERVE_PX` (240px) is
  reserved out of every width clamp, the same idea as `overlaysize.ts`'s
  `TERM_RESERVE_H` on the other axis, and for the same reason: an overlay that
  can cover its own host entirely is a way to lose the app.

### Why not the per-pane embed engine, which already docks things to a right edge

`doc/design/embedded-panels.md` builds exactly that — up to three views docked
left/right/bottom **inside one pane**, with real dividers that **do** resize the
PTY, deliberately, on the argument that docking is a discrete user-initiated
split rather than chrome tax. It is a good mechanism and it is not this one.
Two differences, either of which is decisive:

- **Scope.** An embed belongs to one pane and dies with it. The thing asked for
  here is a property of the *app*: one panel that keeps showing one folder while
  the human clicks through four panes across three project tabs, and that
  survives the pane it happens to be following being closed.
- **Trigger.** An embed's resize is paid for by an explicit "put this here"
  gesture. The dock re-points itself every time the active pane changes — which
  is passive, frequent, and exactly the trigger the constraint targets. A dock
  built on the embed engine would resize terminals on every focus change, which
  is the one thing this feature must never do.

They coexist without interacting: the dock builds its own view instances, and a
pane's Alt+G / Alt+F overlays and embed slots are untouched.

## Following the active pane

Two triggers, one debounced pull, one decision:

- **`Grid.setActive`** gained an `onActive` callback, threaded through
  `Workspace` to `main.ts`. It is hung off `setActive` rather than
  `PaneEvents.onFocus` because focus is only one of the ways the active pane
  moves — closing a pane and inheriting its neighbour, finishing a drag,
  `moveFocus`, opening a pane and toggling maximize all reach `setActive`
  directly, and a dock wired to focus alone would sit on a stale folder after
  every one of them. It fires inside the existing same-pane early return, so
  re-focusing the pane you are already on stays free.
- **`tabs.onChange`**, because switching *project tabs* changes the active pane
  with no grid's `setActive` firing at all: `applyActive` focuses the incoming
  tab's already-active pane, and `setActive` early-returns on it. Without this
  second trigger the dock keeps showing the previous tab's repo — plainly
  broken, and invisible until you have two tabs open.

Both funnel into one trailing-edge debounce (250ms). A human walking the grid
with Alt+arrow fires `setActive` per keystroke, and only where they stop
matters. Both also **pull** the active pane's cwd rather than trusting the pane
the event fired for — a background tab's agent exiting reshuffles that tab's
active pane too, and that must not yank the dock away from what the human is
looking at.

The decision itself is `decideFollow` (`sidedockmodel.ts`), and it carries two
rules worth naming:

**A pane with no local cwd never blanks the dock.** An SSH pane reports no local
directory at all — `Pane.onCwdReported` refuses OSC 7 outright for one, because
the path names a folder on the *far* end — and a welcome pane has none yet.
Clicking one of those is not a request to empty the sidebar, so the dock keeps
the last real root it had.

**A closed dock does no work.** The action is `park`: the root is recorded, and
not one view is constructed, refreshed, or measured. Opening the dock re-asks
the same question with the parked root, which is the only way a parked value is
ever redeemed — there is no second entry point to keep in sync.

### What is *not* followed: a `cd` inside the pane you are already on

The dock follows **which pane is active**, not the live cwd of one pane. Typing
`cd ../other-repo` in the focused terminal does not move the dock.

This is the brief's own boundary (`Grid.setActive` is named as the trigger) and
#934's wording ("whichever pane is currently highlighted/focused"), and it is
left where it is rather than quietly widened. It is also not free to add: there
is no event for a cwd change today — `Pane.onCwdReported` assigns `cwdRaw` and
calls a 500ms-throttled `signalDirRefresh` — so following it means a new pane
event and a second throttle interacting with this one. Worth doing if the human
asks at demo; not worth smuggling into this PR.

## Hosting the three views, and the one thing that made it interesting

All three are already host-parameterized, and all three have the same shape:
`new XView(host)` → append `view.el` → `view.show()` → `view.dispose()`. The
root is **pulled** through a `getCwd()`/`getRoot()` callback. `GitView` and
`FileEditView` are constructed with `embedded: true`, which drops their own ✕
and Escape-to-close binding: the dock owns closing, and a second close
affordance inside a panel that already has one in its header is how the #361
demo found a dead empty rectangle.

**None of the three exposes a public setter for its root.** `GitView` and
`FileExplorerView` re-read the callback on their next refresh;
`FileEditView` latches the root on its first `show()` and never re-reads it.
Only one operation is correct for all three, so `decideViewSync` uses it
uniformly: **dispose and reconstruct**. That is also the only thing that drops
the caches a re-root would otherwise strand — `FileExplorerView` invalidates its
go-to-file index and content hashes on its own picker path only, so a view
re-rooted any other way would answer Go-to-file from the previous repo.

**And that is what makes the editor a design problem rather than a third case.**
Reconstructing a `FileEditView` throws its buffer away. Doing so because the
human clicked a different *pane* would destroy work they never agreed to lose,
which is exactly the rule #219 exists to state. So:

- a dirty editor returns **`hold`**: it stops following, keeps its file, and
  says so in a notice naming the folder it is still showing;
- it resumes on the next sync after it goes clean — saving or discarding both
  reach that — which is why the dock re-asks `decideViewSync` on **every tab
  activation** and not only when the root moves;
- **only the active tab's view is ever synced.** An inactive tab's view is left
  exactly as it was, which is what makes a hidden dirty editor safe and what
  stops a root change from rebuilding three views nobody is looking at. Each
  catches up when its tab is next selected.
- **closing the dock disposes nothing.** Closing is hiding: it must not destroy
  the editor's buffer, and it should not throw away a loaded git log either.

### The dock's editor is the one buffer holder outside every pane

The app-quit guard sweeps tabs, then panes (`main.ts`'s `unsavedBuffers`). The
dock's editor is in neither, so the sweep cannot reach it, and a quit that
misses a holder silently destroys it. `SideDock.bufferReport()` is concatenated
into that sweep deliberately, and `DirtyHost` gained a fourth value,
`"sidedock"`, so the confirm can say where to go look. Its line does **not**
name a pane — inventing one would point the human at a place they cannot go —
and instead carries the folder the dock was pointed at, which is the
disambiguator a tab name provides for every other line.

The other #219 paths need nothing: pane-kill and tab-close route through
`Pane.unsavedHolder()`, and the dock is not a pane's holder, so neither can
destroy its buffer in the first place.

### A quirk inherited, checked rather than assumed

`GitView`'s sub-divider sizes (`loomux.gitview.graphW`, `loomux.gitview.changesH`)
are **global** localStorage keys, shared by every instance — so the dock is now
a third consumer alongside a pane's overlay and a git content pane. This is
benign, and it was verified rather than hoped: `relayout()` re-applies the
*stored* value clamped to the live container with `persist: false`, so hosting a
git view in a 420px dock never writes the clamped-down width back. Only a real
divider drag persists. A wide pane's preference survives the dock, and vice
versa.

## Persistence

One localStorage key, `loomux.sidedock`, holding `{open, tab, width}` — the
`loomux.*` UI-chrome convention `agents.ts`, `editor.ts` and `gitlayout.ts`
already use, not the backend settings file, which is for durable app/session
config.

`decodeDockPrefs` is total and **field-wise lenient**: a malformed `tab` costs
the human their tab choice and nothing else, while `open` and `width` survive.
That is `tabstore.decodePane`'s leniency applied at a smaller scale, for the
same reason — record-wise rejection silently discards a whole preference on the
next boot after a stray hand-edit or a version that wrote one extra field. A
persisted width is clamped on the way in as well as out, so a pref written on a
wider monitor cannot restore a dock that covers the app.

## Colour, and the two channels on one row

The tabs sit on two of the three colour channels at once
(`doc/design/ui-redesign.md` §The three colour channels), on different
properties, which is what keeps them readable as different questions:

- the **active tab's underline** is `--accent` — the one interaction colour, and
  "the active tab" is one of the four positions the brief lists for it;
- each tab's **icon** carries its own identity dye, through the registry's
  documented role mapping and never a hue picked here: `git-graph` is `vcs`
  (lime), `folder-open` is `workspace` (cyan), `file-pen` is `source` (amber) —
  the same three questions the tabs themselves answer.

The `hold` notice is **achromatic on purpose**. The honest dye for an
unsaved-edits warning is `--state-attention`, and that token is reserved to the
agent *state* positions (the warp thread, the status chip, the state dot);
reaching for a different hue merely because it is permitted is the "it needed a
slightly different blue" failure maintainability rule 2 refuses. It says it with
primary ink and a hairline instead.

**No shadow.** `--shadow-float` is a 40px soft shadow and its penumbra would
fall on a live WebGL terminal canvas — the documented way to make this app slow
(`doc/design/performance.md`) and what maintainability rule 5 refuses. The dock
separates the way principle 1 says surfaces should: elevation plus a hairline.

## The resize grip

The dock is width-draggable, persisted on release only. This is cheap in a way
an embed slot's divider is not: there is no terminal on the other side of the
drag, so the gesture moves nothing but the dock's own box and touches no PTY at
all. The drag goes through `startDragSession` (so it cannot strand state on an
Alt-Tab-away mid-drag) and applies the same `.resizing` →
`content-visibility: hidden` treatment to the hosted views' heavy lists that the
embed slots and floating overlays already use — same mechanism, same class name,
extended to three more list classes.

## Deliberately out of scope

- **A tasks tab.** #934's sketch includes one, and #1020 item 6 — the ask this
  actually implements — lists git, file-explorer and file-editor only. A tasks
  tab is a small addition on top of `DOCK_TABS` (`test/sidedockmodel.test.ts`
  pins the set at three, so one arriving without the wiring is noticed), but it
  is not what was asked for here, and `TasksView` is gated on a pane's
  orchestration group in a way an app-level dock has no answer for yet.
- **A keyboard toggle.** Every free chord has to clear the
  `agent-cli-reference` check first — a `Ctrl+Shift+` binding is withheld from
  every terminal pane, so taking one steals it from whatever CLI is running,
  with no escape hatch — and that check is a doc read this change did not do. A
  dock nobody can toggle from the keyboard is a missing convenience; a dock that
  eats an agent's binding is a defect.
- **Following a `cd` within the focused pane** — see above.

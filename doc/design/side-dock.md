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
  re-focusing the pane you are already on stays free. **Every workspace has a
  grid and therefore gets this callback, so the handler is gated on
  `followsPaneChange(w.id, tabs.activeTabId)`** — only the foreground tab's
  pane changes may move the dock (below).
- **An active-tab change**, because switching *project tabs* changes the active
  pane with no grid's `setActive` firing at all: `applyActive` focuses the
  incoming tab's already-active pane, and `setActive` early-returns on it.
  Without this second trigger the dock keeps showing the previous tab's repo —
  plainly broken, and invisible until you have two tabs open. There is no
  active-tab event to subscribe to, only the tab-**set** listener
  `tabs.onChange`, so the dock filters it through `isActiveTabChange` (below);
  subscribing to it raw is a defect, not a shortcut.

Both funnel into one trailing-edge debounce (250ms). A human walking the grid
with Alt+arrow fires `setActive` per keystroke, and only where they stop
matters. Both also **pull** the active pane's cwd rather than trusting the pane
the event fired for, so the dock can never hold a stale snapshot of a value that
moves.

**That pull is why the gating matters, not a substitute for it.** An earlier
revision reasoned the opposite way — the dock reads the active pane itself, so
surely it does not matter which workspace's event woke it — and that is exactly
backwards. Reading the *right* pane at the *wrong moment* is the entire defect:
the cwd is live, so any uncaused wake-up can adopt a directory change the human
made long ago. Both gates below exist because a follow's *timing* is as
load-bearing as its *target*.

The decision itself is `decideFollow` (`sidedockmodel.ts`), and it carries two
rules worth naming:

**A pane with no local cwd never blanks the dock.** An SSH pane reports no local
directory at all — `Pane.onCwdReported` refuses OSC 7 outright for one, because
the path names a folder on the *far* end — and a welcome pane has none yet.
Clicking one of those is not a request to empty the sidebar, so the dock keeps
the last real root it had.

**A closed dock does nothing at all** — not even bookkeeping. `decideFollow`
returns `none` outright, `followActivePane` arms no timer, and no view is
constructed, refreshed or measured. It deliberately keeps **no pending root**:
opening the dock runs the same decision against the *live* cwd, which is
strictly more accurate than replaying a root that was current several minutes
ago.

An earlier revision did park a root, and it was wrong twice over: it recorded
the root on the *closed* call, so the reopen saw `dockRoot === paneCwd` and
returned `none` — the redemption actually happened as a side effect of
`syncActiveView` building from the already-set field — and the `adopt` its own
test witnessed was therefore a state the implementation could never reach (#1097
rev-767 N3). Dropping `park` makes every state this function can return
reachable from the real flow.

### The trigger is the whole correctness argument

A follow re-reads the active pane's **live** cwd. That is right for the two
signals above and wrong for anything else, because the cwd moves continuously
(OSC 7 rewrites it on every prompt) while those signals do not.

This is exactly where the first revision was broken (#1097 rev-767 B1). It
subscribed to `tabs.onChange` directly — which is a tab-**set** listener, not an
active-tab one: `emit()` also fires from `renameTab`, `setColor`, `moveTab`,
`closeTab`, `setTabAttention` (every time a background agent's attention flips)
and `touch()` (orch-channel traffic). So a `cd` the human typed and that was
correctly ignored at the time would be silently adopted **later**, at whatever
unrelated moment some other tab's chip happened to change: the file explorer
rebuilt out from under them, a clean editor file closed, and *whether it
happened at all* depended on background activity. Nondeterministic following is
worse than either pure choice.

`isActiveTabChange(prev, next)` is the fix and it is pinned in
`test/sidedockmodel.test.ts`: the dock compares tab ids rather than trusting the
event, and every other emit source leaves the id alone.

**There were two doors onto that defect, and the first fix closed only one.**
The other is `Grid.setActive`'s own callback, which is wired **per workspace** —
every project tab has a grid, so every project tab gets one. A *background* tab
opening or closing a pane (an agent finishing, a delegate spawning, a group
resuming) calls `setActive` on the survivor, and an ungated handler would then
re-read the *foreground* pane's live cwd and adopt a stale `cd` — the identical
user-visible failure, arriving through a different event, and equally dependent
on whether some other tab's agent happened to be busy.

`followsPaneChange(workspaceId, activeTabId)` closes it, and the `Workspace` is
already passed to the callback, so the gate is one comparison. Both predicates
are pinned by mutation: restoring either defect reddens the suite.

The general rule, worth stating once because it is what both fixes have in
common: **a follow re-reads a value that moves, so every signal that can fire
one has to be justified — reading the right pane is not the same as reading it
at a moment the human caused.**

### What is *not* followed: a `cd` on its own

The dock re-reads the active pane's folder **only when the active pane or the
active tab changes**. Typing `cd ../other-repo` in the focused terminal does not
move it; nothing else does either, until you click somewhere.

The precise consequence, stated because it is the honest version of "does not
follow a `cd`": if you `cd` in pane A, click pane B, then click back to pane A,
the dock lands on A's *new* folder — the cwd is read fresh at the moment of a
signal, never snapshotted at spawn. That is deterministic and human-caused,
which is the property that matters; what the dock refuses is moving at a moment
nobody asked for.

Following a `cd` *as it happens* is the brief's own boundary (`Grid.setActive`
is named as the trigger) and #934's wording ("whichever pane is currently
highlighted/focused"), and it is left where it is rather than quietly widened.
It is also not free to add: there is no event for a cwd change today —
`Pane.onCwdReported` assigns `cwdRaw` and calls a 500ms-throttled
`signalDirRefresh` — so following it means a new pane event and a second
throttle interacting with this one. Worth doing if the human asks at demo; not
worth smuggling in here.

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

### Liveness: the git tab refreshes, the other two do not

A view built once and only reparented is a **snapshot**, and for git that reads
as a bug: commit in the very pane the dock is following, and the graph would
still show the repo as of whenever the tab was built (#1097 rev-767 N2).

So `Hosted` carries an optional `refresh()`, called when the active tab is
selected, when the dock opens, and on any follow signal that resolves to `none`
(same folder — the "clicked back after committing" case). Only git implements
it, via `GitView.notifyPrompt()`: the same throttled (500ms) call `Pane` already
drives from OSC 7 for its own instance, and a no-op unless the view is visible,
so a closed dock still costs nothing.

The explorer and the editor deliberately have **no** `refresh`. For them a
reload means re-navigating to the root or rebuilding the tree, which throws away
the human's place in it — a destructive operation that belongs behind their own
explicit refresh affordances, not on a signal they did not ask for. The
asymmetry is the point: a refresh is only free where it is free.

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
next boot after a stray hand-edit or a version that wrote one extra field.

**A persisted width is bounded on the way in to `[DOCK_MIN_W, DOCK_MAX_W]` and
no further** — `decodeDockPrefs` has no live window width, so it cannot apply
the workspace reserve, and it does not pretend to.

### Where the reserve is actually enforced, and why it moved

`.sidedock { max-width: max(280px, calc(100% - 240px)) }`, in the stylesheet.

The first revision applied the reserve **only on the drag path**
(`clampDockWidth(…, workspaceEl.clientWidth)`), which left three ways to get a
dock that covers the entire grid: boot, a restore from persistence, and any
window resize after the drag. Drag to 900px on a wide monitor, then shrink the
window toward the app's own 640px `minWidth`, and the dock is still 900px over a
~640px workspace — every pane occluded, with 0 of the promised 240px delivered
(#1097 rev-767 B2).

CSS closes all three at once, with no listener to forget and nothing that could
reach a PTY — which matters more here than saving code, because a JS re-clamp
would mean a `resize` handler running next to the one subsystem this whole note
exists to keep away from the grid. `max()` preserves the documented narrow-window
degradation: below the reserve the minimum wins, exactly as `clampDockWidth`
already decided.

`clampDockWidth` keeps the reserve too, so the number that gets *persisted* is
sane rather than merely rendered sane. That makes the two constants a mirror, so
`test/sidedockmodel.test.ts` reads the rule off disk and fails if the
stylesheet's copies of `DOCK_MIN_W` and `DOCK_TERM_RESERVE_PX` ever drift — the
same both-ways pinning `theme.test.ts` applies to the palette, and the reason
duplicating two numbers is safe here.

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

# Filtering the board as a tree, and where view state lives (#1270)

The board that prompted #1152 carried 400+ rows. Sinking and the cleared archive
answered *"most of this is finished"*; they do not answer *"where is the auth
thing"* or *"show me the blocked stories"*. #1270 adds the tree-view controls
that do — collapse-all/expand-all, four filter families, and a search box — and
makes the collapse state and the filters **durable**, which is the part with a
design decision in it.

Read alongside `board-order-and-archive.md` (#1152), whose argument about what
is board data and what is not this note continues and partly amends, and
`task-hierarchy.md` (#958/#1156) for the containment model everything here
projects.

## Filtering a tree is not `board.filter(pred)`

Three rules, all in `taskboard.ts`'s filter section, all pinned in
`test/taskboard.test.ts`.

### 1. A match keeps its ancestor chain

A matching row renders, and so does every container above it, flagged
`BoardRow.context` so the view can draw it as scaffolding rather than as a hit.

Without this, `kind=story` returns a flat list and the containment the whole
#1156 hierarchy model exists to show is gone from the one view that shows it —
the human is handed six stories with no way to tell which feature each belongs
to. Dimming them, rather than styling them like hits, is what stops a filtered
board reading as *"every epic matched too"*.

Descendants of a match are deliberately **not** pulled in. An epic matching
`kind=epic` renders alone; what is inside it is named by its `done/total` chip,
not by dragging forty rows onto a screen the human just asked to narrow.

### 2. An active filter overrides collapse, and never mutates it

While any family is armed, `visibleRows` ignores the collapsed set entirely.

This is not merely the conventional tree-view behaviour (though it is that — a
search that finds a row and then hides it inside a folded container is worse
than no search). Under rule 1 it is the only coherent option: a kept container
either has a kept descendant, in which case it MUST expand to show it, or is a
match whose whole subtree was filtered out, in which case folding it changes
nothing. **Collapse has no observable effect while a filter is active.**

So the per-row chevron and ⊟/⊞ render *inert* rather than as dead clicks, and
the stored set is untouched — clearing the filter restores the exact shape the
human left. The same test asserts both halves, because "we did not mutate it" is
only checkable by re-rendering with the same set afterwards.

### 3. AND across families, OR within one

`kind ∈ {epic, feature}` AND `status ∈ {blocked}` AND the title or id contains
`auth`. An empty family constrains nothing; it never means "match nothing".

Two smaller decisions inside that:

- **`unlabelled` is a first-class chip.** A row with no `kind` is legal and
  permanently exempt from the ladder (#1156), so without a chip it would be the
  one class of row the level filter cannot name. An empty-string `kind` reads as
  unlabelled too (`||`, not `??`) rather than becoming an invisible fifth class.
- **`kindFilterChoices` derives its tail from the board.** `ladderRule` exempts
  an out-of-vocabulary kind on purpose (CLAUDE.md constraint 8 — orrerix must not
  require a methodology), so a hand-edited `tasks.json` may legitimately carry
  `saga`. A fixed chip row would leave such a row matching neither a ladder level
  nor `unlabelled` — it *is* labelled — and reachable only by clearing the level
  filter entirely.

### Where the archive and the filter meet

An archived row cannot match while the archive is off screen. Clearing is board
data (#1152) and filtering is a view; the two compose rather than overriding each
other, so a hit inside the archive does not drag the archive back on screen
behind the human's back — 👁 is what does that, and the same needle finds it once
they click it.

There is no hole under that: `clearedIds` only archives a row whose whole subtree
is cleared too, so an archived container can never sit above an un-archived
match.

### One rule per input

`buildSieve` takes `archived` as a parameter and `visibleRows` computes it once,
which is why there is **no exported `filterSieve(board, filter, attention,
showCleared)`** for the view to call. Two callers each passing `showCleared` to a
different place is exactly the asymmetry CLAUDE.md's one-rule-per-input
convention is about: the two would disagree precisely where they differ, and the
bug would live in the gap.

The same reasoning puts the `attention` id set on the *view* side. The pure
module never learns what a question or a demo gate is — it receives an opaque
`ReadonlySet<string>` — and the view derives it from `boardMarker`, the same rule
the ❓/👀 marker chips are drawn from, so the toggle and the chips cannot
disagree about which rows are waiting on the human.

## Where the durable view state lives

**`boardprefs.json`**, an app-global sibling of `tabs.json` / `settings.json` /
`sshprofiles.json` under the app data dir, holding one record per group;
`src/boardprefs.ts` owns the schema and `uistate.rs` stores the blob opaquely.

### Not on the task

#1152 put `cleared_ms` on the task and argued the line this sits on the far side
of: *"I have acknowledged this item and want it out of my working set"* is a
human-authored decision about the work item, so it is board data by the same test
`status` is. Collapse and filters are not that. Putting them on the task would
make every chevron click an audited board write handed to the orchestrator, for
a fact about one human's screen.

### Why the drift objection does not carry over

#1152 rejected a task-id-keyed sidecar partly because it **can drift** — delete a
task and its id lives on in the set. That objection is real and it does not bite
here, for a structural reason rather than a promise:

- **A stale id in a collapsed set is inert.** It names no container, so it
  collapses nothing. A stale `cleared_ms` sidecar entry would have *hidden a live
  row* — a wrong answer, not a no-op.
- **It is already self-healing.** `retainExisting` prunes the set to live rows on
  every board refresh, so the next save writes the dead ids out.

#1152's other objection — that a sidecar splits the audit story — does not apply
either, because there is no audit story to split: nothing here is auditable, by
design.

### Why a sibling file and not a key in `settings.json`

The reason #887 gave for `sshprofiles.json`: a multi-entry keyed structure with
its own lifecycle does not belong inside a flat bag of app-wide scalars, and
keeping them apart keeps both schemas simple.

### Why not the group dir

It would have tied the record's lifetime to the group's, which is genuinely
nicer. It was not worth what it costs: a per-group file means a group id
reaching a path, i.e. a new `#[tauri::command]` taking a group id, parsing it at
the boundary and joining it through `group_dir_at` — new surface on the one
constraint in this codebase with a source-scanning guard behind it (CLAUDE.md
constraint 6), spent on a view preference.

**As an app-global blob the group id is a JSON map key and never a path**, so
constraint 6's surface is untouched. The lifetime problem it trades for is
bounded by an LRU instead (below), and the worst case is that a board opens at
its defaults.

### Bounded by construction

Keyed by group, the file would otherwise grow forever — one record per group ever
opened, long after the group is gone. Nothing on the frontend can know that it is
gone (asking the orchestration registry would make a view preference depend on a
live backend read at save time), so `encodeBoardPrefs` keeps the **50 most
recently touched** groups and drops the rest.

Eviction at the *write*, not the read: a build that only ever loaded would let
the file grow on disk however small the in-memory map was, and the encoder is the
one function every write goes through. Falling off the end costs one board its
folds and filters — the pre-#1270 behaviour, not the loss of anything a human
authored.

### A new filter family is a key, not a migration

The persisted `filters` object is keyed by family. Adding one — a sprint filter
over #1272's `sprint`, a filter over #1273's `links` — is a new key plus one
clause in `matchesFilter`; no version bump, no migration.

**Both of those landed on `main` while this change was in review, and the seam
held as designed**: they added `sprint` and `links` to the board MODEL (on the
task, which is where assignment belongs — the same line #1152 drew) and neither
opened a second per-group view-state store, which is the collision this change's
plan comment flagged in advance. Neither ships a filter, so the extension point
is still unspent. That is only true in *both* directions if a build
that does not know a key hands it back unchanged, so `decodeBoardPrefs` keeps
unknown families verbatim and `encodeBoardPrefs` writes them back **before** the
validated ones, where they cannot shadow a family this build owns.

The corollary for whoever builds sprint grouping: **sprint *assignment* is board
data and belongs on the task, like `status`; sprint *view state* belongs in this
record.** A second per-group UI-prefs store keyed the same way would be the drift
#1152 warned about, arriving through a different door.

### Nothing is published before the file has been read

The blob is ONE file for every group, so a save built from a store that was never
read publishes an empty map as the whole truth. That is not hypothetical: it is
what the first version of this change did, and it would have destroyed up to
`MAX_GROUPS` other groups' collapse sets and filters, silently, on nothing worse
than a cold start plus a fast click (#1270 review B1).

`BoardPrefsStore` holds the ordering, in `boardprefs.ts` with injected IO rather
than in the view — the invariant IS an ordering between two async calls, so
there is no single value to assert about, and a race parked in DOM wiring is a
race nobody can test. Precedent for the shape: `CoalescingRefresh`
(`refreshgate.ts`).

- Every `write` awaits the read.
- A read that **failed** declines the write outright rather than treating "I
  could not look" as "there was nothing there".
- That failure is **not latched** — the next gesture retries, so one transient
  rejection does not disable persistence for the life of the view.
- `read` answers `null` for an unreadable file, which a caller must not collapse
  into `defaultGroupView()`. Adopting defaults there would show an expanded,
  unfiltered board and then let the next gesture save that over what the human
  actually left.

### A live gesture beats the file

`loadPrefs` runs once and adopts nothing if the human has already changed the
view in this window. The disk copy is what they left last session; a chevron
clicked in this one is newer, and adopting the file over it would look like the
click was ignored.

Saves are debounced (400ms) and fire-and-forget — the `persistTabs` contract: a
failed write just means the last gesture is not durable until the next one, and
the store keeps the newer value so the next gesture re-offers it. The one
exception is `dispose`, which flushes a pending save, for the reason `flushTabs`
awaits the quit path: closing the board is the commonest way a session ends, and
there is no next gesture to retry on.

## What the count chip says now

The board has carried a `done/total` chip on every container since #958. What it
could not say is whether any of those children are **on screen** — `3/7` reads
identically whether all seven rows are underneath it or none are, which is
exactly what makes collapse-all unpleasant to use.

`BoardRow.shownKids` (how many of a row's direct children the projection actually
rendered) closes that: the chip picks up a dashed outline and an extended tooltip
whenever `shownKids < total`, naming the cause — folded up, or hidden by the
filter. The numbers themselves are unchanged and still the orchestrator's own
`children`/`children_done`, because the human's board and `list_tasks`
disagreeing about a count they both display would be a defect. An outline rather
than a colour, for the same reason: nothing about the *work* changed, and this is
a note about the view.

`shownKids` is counted off the rendered set after the walk, not re-derived from
the three rules that decide a row's fate (collapse, the archive, the filter), so
it cannot disagree with what is actually on the screen.

## What is not here

**Keyboard navigation of the tree** was on #1270's candidate list and is tracked
separately as **#1314**. Its cost is not in the pure module: a roving-tabindex
focus model has to compose with the multi-select tickboxes, inline title editing,
the four pickers, the request-changes modal, the two-click delete confirms and
`shortcuts.ts`'s global keybindings — all DOM wiring, which this repo validates by
hand. That is a large, low-coverage surface, and bolting it onto a change whose
value is a testable pure projection would have made both harder to review.

What this change leaves in place for it, so the split costs nothing:
`visibleRows` already returns the rendered rows **in display order** with
`depth`/`hasChildren`/`collapsed`/`shownKids` — the sequence arrow-key movement
has to walk, derived and tested; `containerIds` names every foldable row; each
row carries the `data-item-id` anchor `drainFocus` already scrolls to; and
collapse is durable per group, so a fold made from the keyboard persists like a
clicked one with no new persistence work. The open question #1314 records is what
left/right should mean while a filter is armed, since folding is inert then.

**Nothing here resizes a PTY.** The control strip is a flex child of
`.tasks-view`, inside the overlay (or the embed slot) the board already occupied;
it adds no sibling to `#grid-area` and moves nothing in the layout. Hard
constraint 1.

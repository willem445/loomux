# Per-pane notes and orrerix's own sessions log (#2116)

Each agent CLI keeps its own session log, and the sessions browser lists
sessions by scanning those (`session-index.md`). Orrerix never writes one. This
note covers the *other* half — the things orrerix knows and the harness does
not: the name the human gave a pane, and the notes they wrote about the session
running in it.

## The store

`sessionlog.json`, in the app data dir, a fifth sibling of `tabs.json`,
`settings.json`, `sshprofiles.json` and `boardprefs.json`. `uistate.rs` stores
it the way it stores those four — atomic write, quarantine on a file that is not
JSON at all, an opaque string it never parses further — and
`src/sessionlog.ts` owns the schema.

### Why there, and not somewhere else

| Alternative | Why not |
| --- | --- |
| A key inside `settings.json` | The #887/#1270 argument: a multi-entry keyed structure with its own lifecycle does not belong in a flat bag of app-wide scalars. |
| The orchestration group directory | A basic (non-orchestration) pane has no group. A note would then have two homes depending on how its pane was started, and the commonest pane would have none. |
| The harness's own session files | Not ours to write. A note is orrerix's record, and a harness that rewrites its transcript must not be able to destroy it. |
| `localStorage` | Must survive a webview data clear — the same argument that moved the tab set out of it in #63. |
| Reusing `orch_session_roles` / `SessionRoleInfo` for the pane name | Orchestration panes only. A basic pane has no roster row, which is exactly the gap this fills. |

### Schema v1

```json
{
  "v": 1,
  "sessions": {
    "<harness session id>": {
      "cli": "claude",
      "pane_name": "w: #2116 notes",
      "cwd": "C:/Projects/loomux",
      "created_ms": 0,
      "updated_ms": 0,
      "notes": [{ "id": "…", "text": "…", "created_ms": 0 }]
    }
  }
}
```

**The key is a harness session id, and it is a JSON map key — never a path.**
`session_log_path()` takes no argument and joins one constant file name, so
hard constraint 6's single-assembly-point rule is untouched by this file and by
the two commands behind it. Nothing here is validated as a path segment because
nothing here becomes one.

**Passthrough at three levels.** Unknown keys at the top level, on a record, and
on a note all round-trip verbatim (`unknownTop`, `SessionRecord.unknown`,
`SessionNote.unknown`). Without that, opening an older build once would silently
delete whatever a newer one had recorded. The unknown bag is spread **first** in
the encoder, so a future key can never shadow one this build owns.

**Version.** `v: 1`. A future version number is read anyway rather than refused
— every field is validated per-key regardless, so the worst a newer file can do
is contribute keys this build ignores and preserves. Refusing it would throw
away state a downgrade could otherwise hand straight back.

**Tolerance.** A blob that is not a log yields an *empty* log; a malformed
record or field is dropped rather than invalidating the file. A record whose
`notes` is malformed decodes to an empty list and **keeps the record**: the pane
name is still worth having, and dropping the record would take its passthrough
with it. A note with no id or no text is dropped — it can neither be rendered
nor deleted, so keeping it would put an undeletable blank row in the human's
list.

### The cap, and why it has two tiers

`MAX_SESSIONS = 500`, evicted at encode. Without a cap the file grows forever:
a group mints a session per delegate and a fresh one on every rejoin, so a
machine that has run a few fleets accumulates thousands of records for panes
nobody named and nobody wrote about.

Eviction is **two-tier** (`evictionRank`): a record carrying notes ranks ahead
of every record carrying none, and within a tier the most recently *updated*
survives. The asymmetry is the point — what an unnoted record holds is a
remembered pane name, and losing it drops the row back to its transcript title,
which is the pre-#2116 list. What a noted record holds is something the human
wrote, and it is recoverable from nowhere else.

`updated_ms` is stamped only when a field actually **changed**. A boot that
re-records twenty restored panes therefore writes nothing at all, and cannot
reshuffle the eviction order of the records the human wrote on.

### What losing the file costs

Notes and recorded pane names. Nothing else. Every session still lists — the
scan reads the harness stores — and every row falls back to its transcript
title. The file is never the source of truth for *which sessions exist*.

## The two commands

`load_session_log` / `save_session_log`, in `uistate.rs`: thin `async` fns over
the blocking pool, copied from `load_board_prefs` / `save_board_prefs`.

- The contract is an **opaque JSON string** in both directions. The backend
  validates "is this JSON at all" and nothing more.
- A file that is present but not JSON is **quarantined** — renamed aside to
  `sessionlog.corrupt.json` — and `None` is returned, so the frontend degrades
  to an empty log while the evidence survives for inspection.
- Writes are ordered by the same per-path ticket every other state file uses
  (`write_atomic_seq`): an older save landing after a newer one is dropped, not
  written.
- No agent, MCP tool, or registry code reads or writes this file. It is not
  agent-facing in any direction.

The frontend reaches them through `loadSessionLog` / `saveSessionLog` in
`src/pty.ts`, which call the `EngineTransport` seam — constraint 5, unchanged.

## `SessionLogStore`: the rule the schema cannot express

A save publishes the **whole blob**, so it must never run against a store nobody
has read. The store starts with an empty map and fills it when the load
resolves; a write that beats that would serialize the empty map as the entire
file and silently destroy up to `MAX_SESSIONS` other sessions' notes, with no
error anywhere because every individual step succeeded.

So, exactly as `BoardPrefsStore` does (CLAUDE.md's multi-tenant whole-file
rule):

- every write `await`s the read first;
- a read that **failed** declines the write (`declined-unread`) rather than
  treating "I could not look" as "there was nothing there";
- the failure is **not latched** — the next write retries the read, so one
  transient IPC rejection does not disable persistence for the session;
- passthrough is merged off the record just re-read, never off anything a
  caller supplied: a caller cannot lose what it never carries;
- a failed **save** keeps the newer value in memory, so the next gesture
  re-offers it.

It is a copied pattern rather than a shared base class. The two stores have
different record shapes and different mutation verbs, and a shared abstraction
would be a third thing to test.

## Session-id learning, and the pending residual

A note is keyed on the harness session id. Some panes do not have one yet.

`session-id-learning.md` names the three ways a pane acquires an id, and
`Pane.adoptSessionId` is the single choke point every **late-learned** one
passes through — #440 option B's post-start matcher and #1563's
`orch-session-learned` both adopt through it. An id known at spawn (`claude`,
which mints it on the command line) never goes through `adoptSessionId` at all:
that pane records directly.

So:

| When the id is known | What happens to a note |
| --- | --- |
| At spawn (the `--session-id` line) | Written straight onto the session record. |
| Later, via `adoptSessionId` (#440 option B, or #1563's `orch-session-learned`) | Held in memory against `Pane.key`, then moved onto the record by `SessionLogStore.rekey`, appended in order onto whatever a resumed session already carries. |
| Never (the watcher timed out) | Stays pending for the life of the window. |

`rekey` writes **once**, and clears the pending list only after the write is
accepted — a `declined-unread` leaves the notes where a later attempt can still
find them. A second `rekey` for the same pane is then a no-op, which is what
makes the call safe from a site that may fire more than once.

**The dialog's target is a getter, not a value** (#2116 review B1). The one
thing that moves a target moves it while the overlay is open: the pane learns
its id, `rekey` moves the pending notes onto the session record and empties the
pending list. A target captured at open would then point at an emptied pane key
— the overlay showing its "notes here are ephemeral" empty state at the exact
moment they became durable, and every note added afterwards filed as pending
against a pane `adoptSessionId` will never re-key again. `NoteDrafts.migrate`
carries the half-typed note across with it, because the draft book is keyed on
the target too.

That is also what keeps the residual below as narrow as it claims to be: with a
live target, a note written *after* the id is learned reaches the session
record. Only notes written strictly *before* it are exposed.

**The residual, stated rather than hidden: a pending note does not survive an
app restart.** It is in memory only, because there is nothing durable to key it
to — `PersistedPane` has no stable per-pane key. This is reachable only for a
copilot/opencode pane written to before its first prompt (claude ids are minted
at spawn; an orchestration pane's id arrives within the watcher window). The
notes dialog says so on a pending record. Persisting them would need a stable
key on `PersistedPane` — a `tabs.json` schema widening plus `tabstore` decode /
encode / tests — which was rejected for v1 and is the named follow-up.

**Which CLIs that is, stated as a property rather than a list.** The exposed
set is *every harness whose session id is not on its own launch line* — the
ones that mint after boot and reach `adoptSessionId` through #440's reconciler
or #1563's `orch-session-learned`. At the time of writing that is copilot and
opencode; claude and pi (`doc/design/pi.md`) pre-mint a UUID and pass it as
`--session-id`, so their panes are never pending at all. Written as a property
because the list has already moved once: pi arrived while this change was in
review, and a sentence naming three CLIs would have gone quietly false rather
than simply out of date.

## The sessions tab: two controls, not one

The human asked for an explicit "my own sessions" ⇄ "orchestration sessions"
split. #1592 already had a toggle that hid delegate rows. These are different
questions and they **compose**:

| Control | Question it answers | Where it lives |
| --- | --- | --- |
| **Mode** (`SessionMode = "mine" \| "orchestration"`) | Whose sessions am I looking at? | `partitionSessions`, step 1 |
| **The #1592 delegate toggle** | Within an orchestration, do I want the worker/reviewer rows too? | `partitionSessions`, step 2 — inside `orchestration` only |

`partitionSessions(sessions, roleOf, mode, showDelegates)` runs both in one
pass. The mode picks the population — `mine` keeps rows with no recorded
orchestration identity at all, `orchestration` keeps the rest — and the delegate
rule then acts inside `orchestration`. In `mine` there are no delegates by
construction, so the toggle is inert there and `delegateToggleLabel` returns
`null`.

`hidden` counts **only** what the delegate rule held back. A row the mode
excluded is not hidden, it is somewhere else — one click away on a control that
names where — so counting it would put a number on a toggle that cannot reveal
it.

Both rules are stated as **properties**, never as role lists: a row belongs to
an orchestration when it has a recorded role *at all*, and within one it is a
delegate unless the role is `orchestrator`. A workflow file naming a new role
must not silently move its sessions into the human's own view.

**Rejected:** folding the mode into a third state of the #1592 button. It
conflates two questions, and the human asked for an explicit control.

**Chosen, with a consequence worth naming:** the Orchestrations section
(#1563 — the primary restart surface for a recorded *group*) renders only in
`orchestration` mode. The alternative was to show it always, which would keep
that surface in a view the human asked to be "my own sessions". The user docs
say where it went.

## What is bounded, and what is not

The cap bounds the number of RECORDS. Two things it does not bound are stated
here rather than left to be discovered, since neither is visible from the
two-tier argument above.

**A noted record CAN be evicted — past 500 of them.** The two tiers guarantee
that names are shed before notes, not that notes are never shed: the 501st
most-recently-updated *noted* record is dropped at the next encode, silently.
At 500 noted sessions on one machine that is far past any working set this was
designed for, and the alternative — an unbounded file — is worse. It is a real
loss of something the human wrote, so it is written down.

**Notes per record are not capped at all**, and the whole blob is re-serialised
and fsynced on every note add, every delete and every identity change. One
long-lived session accumulating notes is therefore the resource question the
record cap does not answer: at ~1,000 notes near the 2,000-character cap the
blob is a couple of megabytes, sorted and written on the webview's own turn.
Not a problem at plausible sizes, and deliberately not fixed with a second cap
here — a cap that silently drops the human's oldest notes is a worse failure
than a slow save, and the honest fix if this ever bites is incremental
persistence, not eviction.

## What the dialog does when a write does not land

The store's outcome is READ, not discarded, and the two failure shapes are told
apart — `noteWriteFeedback` in `notesmodel.ts` owns which is which, so the
wording and the give-it-back decision are testable without a DOM.

| outcome | what actually happened | what the dialog does |
| --- | --- | --- |
| `saved` | on disk | nothing |
| `pending` | held in memory against `Pane.key`, deliberately | nothing — the empty state already explains it |
| `unchanged` | a no-op (unreachable from a non-pristine draft) | nothing |
| `failed` | the note IS in memory and on screen; the save missed | says the note is not saved yet, and does NOT hand the text back — that would leave a note beside a copy of its own text, and re-submitting would duplicate it |
| `declined-unread` | **nothing was recorded anywhere** — the store returned before mutating, because it has never read the file | hands the text back into the box and says so |
| `threw` | the promise REJECTED, so nothing is known | hands the text back and says to check the list first — at worst the human sees a note *and* its text, which is visible and one Escape from fixed, where not handing it back can lose the note outright |

A subscriber that throws cannot reach any of this: `emit()` runs inside
`publish()`, before the save, so an exception escaping it would reject the whole
write on a path the caller's `.then` cannot see — the note in memory, nothing on
disk, no message. Listeners are isolated and reported individually
(#2116 review premortem 1).

The list itself distinguishes "no notes" from "I could not look", which is the
same rule one level up: while `store.loaded` is false the overlay says the list
may be incomplete rather than asserting the session has no notes
(#2116 review N1).

The last row is the one this section exists for. The obvious wiring — clear the
box, fire the write, ignore the result — loses that note outright: it reaches
neither disk nor the list, the box has already been emptied, every individual
step succeeded, and nothing anywhere says a word. The text is only handed back
if the human has not started typing something else in the meantime; their newer
text outranks the one being restored.

## Where the button is, and when a record is written

**The Notes button** sits in the pane header, `pane-btn pty-only`, and shows on
an agent pane only. Three gates, and only the first is a CSS class:

1. `pty-only` — the stylesheet hides the class on `.is-content`, so files /
   editor / git / workflow panes never show it.
2. a **harness** must be running (`facts().harness !== null`). A plain shell has
   no agent session, so a note would have no key at all.
3. **not an SSH pane.** It may well have a CLI at the far end — `facts()`
   reports one — but that session lives on the remote machine while this store
   is per-local-machine. Same reason the store is not in a group dir.

Gates 2 and 3 cannot be classes: both are read off the launch line and can
change on a respawn, so `syncNotesBtn` re-reads them wherever `spawnCommand` is
set. It reads through `facts()` rather than re-deriving the harness, so this and
the Agents tab cannot disagree about what a pane is running.

The pane does **not** hold the store. The button raises
`PaneEvents.onOpenNotes` and the host opens the overlay — the same shape
`onOpenEditorPane` uses, and for a sharper reason here: giving every `Pane` a
handle on a single-file multi-tenant store is precisely the shape the
read-before-write rule exists to protect against. The note COUNT is pushed the
other way, `setNotesCount`, from the store's own change event.

**When a record is written:**

| Trigger | Why it is the right hook |
| --- | --- |
| `onGridChanged` (open, close, rename, re-root) | The one event that fires whenever the set of panes or a pane's persisted identity changes. This is what records a claude pane, whose session id is on its own launch line and so never passes through `adoptSessionId`. |
| Once when `booting` flips false | The whole restore runs under the `booting` guard, so `onGridChanged` fires for none of it. One sweep is what gives a restored session its recorded pane names. |
| `onRecordChanged` (a human rename) | Deliberately **not** `notifyExited`'s `setName(name + " · exited")`, which does not go through this event. The log records what the human called the pane, never a lifecycle suffix. |
| `onSessionIdentified` (a late-learned id) | Re-key first, then record — `rekey` creates the record if it is absent, so recording first would be two writes where one does. |

Sweeping every pane on every grid gesture is cheap *by construction, not by
luck*: `record` compares the three identity fields and writes **nothing** when
they are unchanged, so a boot that re-records twenty restored panes costs twenty
map lookups and no IPC.

## The recorded pane name on a session row

A session row already shows the transcript title. The recorded pane name is an
**addition** for the case where the human called the pane something of their
own, so `paneNameLine` returns `null` — the caller renders no line at all —
rather than a placeholder or an empty line. The fallback *is* the title.

It suppresses three cases, and the third is a judgement rather than an
observation:

1. no recorded name (a session predating `sessionlog.json`, or one nobody has
   opened a pane on since);
2. a name equal to the title, which would print the same words twice;
3. a name equal to the one a Sessions-tab restore **mints** —
   `restoredPaneName`, i.e. `"<cli> · <title>"`.

Case 3 is worth stating because it is not what the acceptance criterion asked
for literally. Every pane opened by clicking a row in this very list carries
that auto-name, so without the clause the commonest row on the page grows a
second line restating its own title with a CLI prefix. The line exists to show
what the human *chose*; an auto-name is not that. The cost, accepted: a human
who renames a pane to exactly its auto-name sees no line.

Comparison is on the trimmed strings and **case-sensitive**: a human who
renames `worker` to `Worker` has renamed it, and this is a report of what they
wrote rather than a guess at what they meant.

## The notes chip, and what a row is shaped like

The second half of the acceptance criterion is "a way to open that session's
notes", live session or dead. That is a chip on the row, opening the same
overlay the pane header opens — one dialog, two entry points, and `openNotes`
was written to take whichever target it is given.

**The chip is on every row**, including a session with no notes. A chip that
appeared only once notes existed would be a way to *read* them and no way to
write the first one, and the sessions tab is where a human reviews a session
they are not sitting in.

**What it says is `notesChipLabel`, and the middle state is why it is a
function.** `SessionLogStore.notesCount` answers `0` for a session with no notes
**and** for a file nobody has read yet — its own doc says so and forbids the
collapse. So:

| State | Number | Tooltip |
| --- | --- | --- |
| `loaded` false — not read, or the read rejected | none | says the file has not been read |
| read, no notes | none | says there are none, and names the gesture |
| read, `n > 0` | `n` | agrees with the number, singular at 1 |

A `0` in the first row would be the chip asserting that a session with notes on
disk has none — the same silent-loss shape the overlay's own "could not read the
notes file" line exists to avoid, one surface over. `SessionBrowser.refresh`
therefore also awaits `ensureLoaded`, so the first render already knows: without
it the store is read lazily by whatever opens the overlay first, and every chip
would sit in the unread state until then.

**A row is a wrapper, not a button.** Restore and notes are two independent
actions, so `.session-row` is a plain `div` holding two sibling `<button>`s. A
button nested in a button is invalid HTML and leaves a keyboard user with a
control they cannot reach separately. The hover/press feedback moved from
`.session-item` onto the wrapper in the same change: on the item alone the chip
would slide out from under the row it belongs to, and on both it would double.

**What the browser is given is a read port, not the store.** `SessionNotesHost`
is four reads, a subscription and `openNotes`. `SessionLogStore` is the window's
single handle on a multi-tenant file whose whole safety rule is that a write is
published only from a handle that has read it, so a second thing able to publish
it is exactly the shape that rule exists to prevent. `main.ts` keeps the writer
and supplies the overlay; `SessionLogStore` satisfies the read half
structurally, so the adapter carries no state of its own.

Two of those reads are **scalars** — `notesCount` and `paneName` — and not
`get(id)?.…`. `get` returns `cloneRecord`, a fresh object per note on the
record; a row reads one number and one immutable string, on every render, and
notes per record are uncapped. Reading through `get` would make that
O(total notes across shown rows) short-lived objects per gesture. `paneName` is
`notesCount`'s sibling in every respect, including answering `undefined` for an
unknown session and for an unread store alike.

**The list re-renders off `store.onChange`**, skipped while the Sessions tab is
not the visible one — `leftpanel.ts`'s `onShow` calls `refresh()`, which renders,
so a change that lands behind the Agents tab is picked up on return and a pane
rename does not rebuild a list nobody is reading. Either way it replaces the
children of a list inside the fixed-width `.sessions-inner` column: no layout
column moves, so no path here reaches a PTY resize (hard constraint 1).

### Focus across a rebuild

That second trigger is one a human can reach *while reading the list*: a note
written from a pane header rebuilds every row under them. A keyboard user
standing on one then loses the element they were on and focus drops to `<body>`,
so the next Tab restarts traversal from the top of the document — the defect
#2259 fixed on the Agents chip strip, in the shape this list has. That strip
keys and reuses its elements and hands focus on before removing one; this list
replaces all of them, so the handoff is *decided* before the rebuild and
*applied* after it.

`refocusAfterRender(held, shownSessionIds)` is that decision, pure and tested.
Three outcomes, and the first is the one that must not be got wrong:

| held focus | outcome | why |
| --- | --- | --- |
| not in the list | `none` | A render fires on a pane rename and on any grid gesture, so the human is usually typing in a terminal. Nothing is stolen, ever — including when the list is empty, where the temptation to "just focus the search box" is strongest. |
| on a row still shown | `row`, same control | The chip and the restore button are different controls on one row; landing on the wrong one is landing in the wrong place. |
| on a row now gone | `search` | A note write can drop a row out of the current search and a mode switch drops many. The search box is where a keyboard traversal of this panel starts, so it is somewhere to carry on from rather than somewhere focus fell to. |

Keyed by **session id**, off `data-session-id` on the wrapper, never by
position: the list is re-sorted on every refresh, so an index would land the
human on whichever row moved into their slot. The caller resolves the decision
to an element, because a control it names can be absent — a row has no chip when
the browser was built without a notes host — and falls back to that row's
restore button and then to the search box, so every branch lands somewhere real.

**The overlay's target is a constant getter here, unlike the pane header's.**
A pane's target is live because a not-yet-identified pane can learn its session
id under an open overlay and `rekey` empties the pending list. A row named a
session **id**; that id is what the notes belong to for as long as the dialog is
open, and nothing can move it.

## Public-contract changes

| # | Contract | Status |
| --- | --- | --- |
| 1 | `sessionlog.json` — schema v1, key, cap, eviction tiers, passthrough | Landed (slice C) |
| 2 | `load_session_log` / `save_session_log` — opaque string, quarantine on corrupt | Landed (slice C) |
| 3 | `PaneEvents.onSessionIdentified` — fires at most once per pane per adopted id, from `adoptSessionId` only (the spawn-time id path records directly and never fires it). `PaneEvents.onOpenNotes` alongside it. | Landed (slice D) |
| 4 | `partitionSessions(sessions, roleOf, mode, showDelegates)` — internal; tests | Landed (slice E1) |
| 5 | `localStorage` key `loomux.sessions.mode`, decoded totally, default `mine` | Landed (slice E1) |
| 6 | `paneNameLine(paneName, title, source)` — internal; tests | Landed (slice E2's pure half) |
| 7 | `notesChipLabel(count, loaded)` — internal; tests | Landed (slice E2) |
| 8 | `SessionNotesHost` — the read port `SessionBrowser` takes; internal | Landed (slice E2) |
| 9 | `.session-row` wraps `.session-item` on every session row, carrying `data-session-id` — internal DOM/CSS shape | Landed (slice E2) |
| 10 | `SessionLogStore.paneName(sessionId)` — a scalar read beside `notesCount` | Landed (slice E2) |
| 11 | `refocusAfterRender(held, shownSessionIds)` — internal; tests | Landed (slice E2) |

## Where the pieces live

| Piece | Where | Notes |
| --- | --- | --- |
| Schema, cap, eviction, the store | `src/sessionlog.ts` | DOM-free, injected IO. `test/sessionlog.test.ts`. |
| Note text rules, ordering, empty-state wording | `src/notesmodel.ts` | DOM-free. `test/notesmodel.test.ts`. Split out so the dialog and the store cannot disagree about the cap. |
| The file, the two commands, the sibling-path guard | `src-tauri/src/uistate.rs` | Inline `#[cfg(test)]` tests; ACL registration in `tests/acl_manifest.rs`. |
| Typed IPC wrappers | `src/pty.ts` | `loadSessionLog` / `saveSessionLog`. |
| Mode + delegate composition | `src/sessionfilter.ts` | Pure. `test/sessionfilter.test.ts` pins all four crossings plus the negative control. |
| The mode control | `src/sessions.ts` | DOM wiring, hand-validated. Inside the fixed-width `.sessions-inner` column, so it moves no layout column (hard constraint 1). |
| The notes overlay | `src/notesdialog.ts` | DOM wiring, hand-validated. Built on the `.launcher-overlay` / `.agent-dialog` kit, not on `modal()` — that is a button-confirm, and this needs a live list with a per-row delete. |
| The unsubmitted draft | `NoteDrafts` in `src/notesmodel.ts` | DOM-free. The editor is a VIEW of the book, seeded on open and written on every `input`, so a re-render cannot eat what the human was typing. |
| The pane-name line | `paneNameLine` in `src/sessionmeta.ts` | Pure. `test/sessionmeta.test.ts`. |
| The Notes button, and the re-key wiring | `src/pane.ts`, `src/main.ts` | DOM wiring, hand-validated. Reads `Pane.facts()` / `Pane.key` from #2122 slice A. |
| What the notes chip says | `notesChipLabel` in `src/sessionmeta.ts` | Pure. `test/sessionmeta.test.ts`. |
| Where focus goes after the list is rebuilt | `refocusAfterRender` in `src/sessionmeta.ts` | Pure. `test/sessionmeta.test.ts`. The DOM read (`document.activeElement`) and the element resolution stay in `sessions.ts`. |
| Pane name and notes chip ON a session row | `src/sessions.ts`, `src/main.ts` | DOM wiring, hand-validated. The read port is `SessionNotesHost`; `main.ts` keeps the store and supplies the overlay. |

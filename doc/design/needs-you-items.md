# Needs-you items — the thing waiting on the human, as a record

*#1151. This note covers slice A: the item entity, the per-group registry, the
board lifecycle mapping, the clear-completed watermark, and the trusted Tauri
commands. The MCP raise surface (slice B) and the panel rework (slice C) extend
this note as they land. It is the companion to
[human-questions.md](human-questions.md), which owns the other half of the same
panel.*

## The problem

Before this, the NEEDS-YOU panel's DEMOS tier was a **pure projection** of
`tasks.json`: every row whose status was demo-gated (`prototype`,
`human-testing`) became a panel entry, and that entry *was* the task. It had no
record of its own.

Three things follow from that, and all three are the complaint:

- **No lifecycle.** There is no "I have seen this". The only close-out available
  is a board move, so acknowledging a demo and *deciding* about it are the same
  gesture — and a human who has looked but not decided has nowhere to put that.
- **No identity and no timestamps.** Nothing says who asked for the look, when,
  or why this one is urgent and that one is not. The panel could sort by board
  order and nothing else.
- **No way to ask about anything that is not a board row.** "What do you think of
  this direction?" has no status to park in, so it was not askable at all.

## The shape

**A needs-you item is a first-class record that LINKS a task rather than being
one.** `needs-you.json` lives in the group dir beside `questions.json` and
`tasks.json`; `needsyou::Item` owns *who raised it, when, what for, and
open/resolved*. Everything else — `demo_path`, `pr`, `assignee`, whether Proceed
is available — is **joined live from `tasks.json` at render**.

That split is the whole design. Snapshotting a task's fields onto an item would
create a second record about board state that starts drifting the moment the
board moves, which is exactly the shape slice C's projection was written to
avoid. The item owns the *ask*; the task keeps owning the *facts*.

```
needs-you.json   ── item n-3 ──link──▶  tasks.json ── t-12
  who asked                              title, status, pr, demo_path,
  when                                   assignee, canProceed
  what for
  open / resolved (+ by whom, when)
```

### Why not fold it into `questions.json`

`questions.json` is a shipped public contract with a purpose-built trust
boundary — a closed `AnswerSource`, two boundary tests, a documented "every agent
may ask, no agent may ever answer" argument. And *answering* a question is a
different power from *resolving* an item: an answer settles a decision the human
was asked and releases the work waiting on it, while a resolve says "I have
looked". Entangling them would widen the one surface #946 spent three layers
keeping narrow, and would mean migrating a live file that is holding decisions
nobody has made yet.

So the panel becomes **one surface over two registries** rather than one registry
plus one projection. `question` is deliberately not an item `kind`.

### Why not "attention"

`AttentionItem` is already the pane-chip scan's type, with a frontend mirror in
`src/attention.ts`. Calling this "attention" would make every reader
disambiguate. The module keeps its own vocabulary behind `needsyou::`, the way
`humanq::` does, for the same reason.

## Public contracts this ships

### `needs-you.json`

An array of item records in the group dir. Every field past the required core
carries `#[serde(default)]`, so a file written by an older build loads.

| field | notes |
| --- | --- |
| `id` | `n-1`, `n-2`, … — minted off the file's own high-water mark |
| `kind` | `demo` \| `feedback` — a closed set |
| `raiser` | agent id, or `board` for an auto-raised item |
| `text` | what to look at / what is wanted back (cap `ITEM_TEXT_MAX`, 2000) |
| `task` | the linked board row. **Required for `demo`**, optional for `feedback` |
| `urgency` | `normal` \| `high` — the same `humanq::Urgency`, reused not re-declared |
| `status` | `open` \| `resolved` |
| `created_ms` | |
| `resolved_ms`, `resolved_by`, `resolution` | present once settled |

**`task` is required for a demo and optional for feedback** (#1151 decision D4).
The panel opens the board row to show what to run, so a demo with nothing linked
is a demo nobody can reach; a feedback ask can legitimately precede any row.

**`urgency` is the same type questions use**, imported rather than cloned. The
panel unions items and questions into one list and sorts it urgency-first, so two
identical enums would be two spellings of one word that the sort then has to
reconcile.

**`status` has two states, not three.** A withdrawal and a board move are
*resolutions with a different `resolved_by`*, not statuses of their own — unlike
`humanq::Status`, which does carry a separate `withdrawn`. The asymmetry is
deliberate: a question's terminal states differ in whether the human's decision
was ever obtained, which every reader needs; an item's do not — nobody decided
anything, the row is closed, and the provenance lives in `resolved_by` for
whoever cares exactly.

**Ids are legible, not opaque.** `n-{highest + 1}`, read off the file exactly as
`q-N` and `t-N` are. Constraint 2 (no getrandom-based crates) is satisfied
because no crate is involved, and unpredictability would buy nothing: the id is
never a capability — the only surfaces that can act on one are trusted — and it
*is* quoted in a pane notice, where legibility is load-bearing. Ids are never
reused: retention only ever drops rows below the high-water mark that produced
them.

**Reads are loud about a bad file.** `OrchRegistry::needs_you` treats an absent
file as empty and *every other failure as an error*, `questions()`'s posture for
its reason: every mutation is a read-modify-write of the whole file, so a read
that answered "no items" for a file it merely failed to parse would let the very
next raise overwrite it. (`orch_needs_you_list` still shows an empty list — it
has no error channel and its caller renders one.)

**Retention.** Resolved rows are capped in the file at `RESOLVED_RETAINED` (20),
oldest-*raised* out first; the audit log keeps all of them. Open rows are **never**
pruned at any count — `OPEN_MAX` (32) bounds them by refusing new raises instead,
loudly, because reaching it means things are being queued for a human faster than
any human works through them.

### The `needs-you-cleared` marker

A file in the group dir containing a decimal ms timestamp: the clear-completed
watermark. The panel hides any **settled** row (a resolved item, an answered or
withdrawn question) stamped at or before it.

**Clearing is not deleting, and not a row mutation.** `clear_needs_you` never
opens `needs-you.json` at all. That is what makes "clears the UI, persists on
disk" structural rather than a promise, and it is why an open row can never be
affected: there is nothing in the path that could touch one. The choice survives
a restart, which is why it is a marker file rather than session state — the
`set_notify` precedent, under the same `marker_io` lock so two clears cannot land
their writes in the opposite order to the stamps they minted.

The rejected alternative was a `cleared: true` flag written onto settled rows.
That mutates two files' contracts (items *and* questions) where one watermark
suffices, and it turns "hide what I have seen" into a write against the file
holding what the human has *not* seen.

**Unparseable reads as `0`, deliberately the opposite fail direction from the
items file.** A misread items file that came back empty would let the next write
destroy open items; a misread watermark that comes back `0` shows a settled row
the human had already cleared. One loses a record, the other costs a click — so
the watermark fails toward showing *more*, never toward hiding.

### Tauri commands

| command | set | notes |
| --- | --- | --- |
| `orch_needs_you_list(group_id)` | orch-read | items + `cleared_ms`, in one round trip |
| `orch_needs_you_resolve(group_id, id, note?)` | orch-control | the human's close-out |
| `orch_needs_you_clear(group_id)` | orch-control | stamps the watermark, returns it |

All three parse `group_id` at the boundary through `command_group` (CLAUDE.md
constraint 6), like every sibling command. Membership is enforced by *which file
was read*: each group's items live in its own group dir, so another group's id is
simply absent and gets the same refusal an id that never existed does.

`orch_needs_you_list` returns rows **and** the watermark together, rather than
leaving the panel to fetch two things: it hides settled rows against the
watermark, so two reads would let it render this second's rows against last
second's stamp and flash back a row the human had just cleared. It returns the
whole file, uncapped, for `orch_questions_list`'s reason — retention already
bounds it, and a cap whose size the caller cannot see is the silent truncation
the rest of this feature refuses.

**It is a pure read.** It writes nothing, takes no lock, and must stay that way:
§5.4 of `remote-engine-protocol.md` classifies it `viewer`, and that tier is
defined as "cannot write a file". The upgrade migration used to run here; see
[the migration](#the-one-shot-migration) for why that was wrong twice over.

### What an agent reads

**Never a stored `Item`.** `project_list` — the projection behind slice B's
`list_needs_you` — returns `needsyou::AgentItem`, an explicitly enumerated
struct built field-by-field from `&Item` with **no `..` spread and no derive**.
That is the point of it: adding a field to `Item` must not be able to put that
field on an agent surface by itself. Whole-struct serialization onto an agent
surface is the class that failed on #1160, and this struct is what it costs to
pre-empt it.

| shown | withheld |
| --- | --- |
| `id`, `kind`, `raiser`, `text`, `task`, `urgency`, `status`, `created_ms`, `resolved_ms`, `resolved_by` | `resolution` — the human's verbatim close-out note |
| `had_resolution` — that a note exists | |

`resolved_by` is shown deliberately: an orchestrator must be able to tell "the
human looked" from "the board moved on" from "I withdrew this myself", which is
the whole reason those three tags stay distinguishable.

**Withholding `resolution` diverges from the `humanq` precedent, where the shared
`list_questions` returns the human's verbatim answer — and the divergence is the
decision, argued in full in
[human-questions.md](human-questions.md)'s "items vs. questions" comparison table.**
In one line: text written *to* an agent reaches it, text written *about* the
human's own queue is not broadcast. An answer is instructions addressed to the
asker and work is held pending it; a resolution note is the human annotating
their own queue while clearing it, the note already reaches the orchestrator's
pane through `resolve_notice`, and the only thing declined is putting it on a
read every delegate may call. Reviewed and settled as-is: coming out of #1160,
"the other registry is wider" is not a reason to loosen an agent surface when the
other registry's payload is functionally different.

The assertion that polices this is on the **serialized** form, not the struct: a
field present in Rust but absent from the wire would pass a field-by-field check
and still leak.

### `orch-needs-you-changed`

Emitted from `write_needs_you`, the single mutation point — so, the single
notification point, the `orch-tasks-changed` / `orch-questions-changed` shape. The
panel is the listener (slice C).

### Audit actions

`needs-you-open`, `needs-you-resolve`, `needs-you-withdraw`, `needs-you-clear`,
and `needs-you-reject`. The reject line carries the op and the reason
(`unknown-item`, `already-resolved`, `invalid-resolution`, `raise-refused`,
`unreadable`, `unwritable`), which is what makes a refusal — or a best-effort
hook that could not do its job — visible rather than merely silent.

## The resolve boundary

An item is settled three ways, and they are deliberately not one operation:

| how | `resolved_by` | who can do it |
| --- | --- | --- |
| the human acknowledges | `webview` | the trusted Tauri command only |
| the raiser takes it back | `withdrawn:<agent>` | the raiser, through MCP (slice B) |
| the board moves on | `board:<new-status>` | the lifecycle hook |

**There is no MCP resolve, and `ResolveSource` is a closed enum supplied by the
entry point.** Resolving is the human clearing their own attention queue — the
same no-self-served-gate boundary answering a question has, and the same
structural enforcement: `orch_needs_you_resolve` hard-codes
`ResolveSource::Webview` rather than taking a `source` string, so "resolve as the
human" has no spelling, and no `call_tool` arm reaches the method. An agent that
wants its own ask gone has `withdraw_attention`, which settles it *visibly* as a
withdrawal.

That is why the board's auto-resolve and an agent's withdraw are **not**
`ResolveSource` variants. Both are weaker settles that write their own tags;
giving either one a `ResolveSource` would let it be mistaken for a human's
acknowledgement, which is the one thing this column exists to keep unambiguous.

**No settle ever deletes a row.** A withdrawn or board-resolved item stays on
disk so a human who was mid-look can see what happened to it.

**Resolving does not move the task.** It clears the attention row; the board keeps
whatever status it had. Proceed and Request-changes stay the board actions they
always were.

### The resolve notice

`[loomux] the human resolved needs-you item n-N (t-M): <note>`, delivered through
the ordinary `deliver_to_orchestrator` path — **only when the resolve carried a
note**. A note-less resolve is the human tidying their own queue, and a pane
delivery per tidy is noise the orchestrator pays for on every turn.

Both the note and the task ref are untrusted text entering a `[loomux]` line. The
human is trusted; the pane still cannot tell one line from another, so an
embedded newline would forge a second line reading as its own legitimate notice.
The task ref is worse — nothing validates the string an ask attached to its item,
so it is raiser-controlled. Both go through `sanitize_gh_text`; only the id is
loomux-built, and it is emitted first so the cap trims the note's tail rather
than the attribution.

**A delivery failure never fails the resolve.** The item is settled durably
either way, and a cold orchestrator finds it through the list. The registry is
the record; the notice is only a notification.

## The board lifecycle mapping

**One hook, in `OrchRegistry::upsert_task`.** Every status transition already
funnels through it — `proceed_task`, `request_changes`, the board overlay and
every MCP `upsert_task` — so hanging the mapping there is what makes it
impossible to move a task into or out of the demo gate without the human's queue
following.

- **Into** `DEMO_GATED_STATUSES` → auto-raise a `demo` item, `raiser: "board"`.
- **Out of** it → auto-resolve that task's open demo item as
  `board:<new-status>`.

`DEMO_GATED_STATUSES` lives in `mod.rs` beside `MERGE_GATE_STATUSES`, mirroring
`src/taskboard.ts`'s `DEMO_STATUSES` the way `ensure_at_merge_gate` mirrors
`canApprove` — a backend copy rather than a read of the frontend's, because the
hook runs where no frontend exists. It is owned by the board and not by
`needsyou` for the reason `taskboard.ts`'s own comment gives for owning
`DEMO_STATUSES` rather than letting `decisions.ts` own it: which statuses park a
task is a fact about the board, and the registry is a consumer of it.
`the_backend_demo_gate_set_matches_the_boards` scans the TS declaration and
compares, so the two spellings cannot drift.

**The hook keys on the TRANSITION, not on the write.** A note appended to a task
that is already parked, an assignee edit, a shuffle between `prototype` and
`human-testing` — none of those crosses the boundary, so none of them raises or
resolves anything. That is what keeps it one human-visible row per parking rather
than one per board edit. `request_changes` is the concrete case: it appends a
note and leaves the status alone, so a demo item stays open through it, and
leaves the queue when the work actually moves.

**No duplication by construction** (#1151 decision D2). The dedupe lives in one
pure function, `needsyou::admit`: a `demo` raise for a task that already has an
open demo item returns the existing row rather than a second one, whether the
hook, the migration or an explicit `request_attention` asked. Dedupe runs *before*
the cap, so a duplicate raise stays idempotent even on a full queue — otherwise
the hook would start failing exactly when the backlog is worst. Feedback asks are
never deduped: two agents can legitimately want an opinion on one row.

**A deduped raise keeps the EXISTING row's text and discards the new ask's**, so
`admit` returns a `Raised { item, fresh }` rather than a bare item. For the hook
that bit is uninteresting; for a caller with an author to answer it is the
difference between "registered" and "there was already one of these, and your
words were not stored". Slice B's `request_attention` must say which.

**Both halves are best-effort against the board.** The board write has already
landed and cannot be unwound, so a raise refused by the cap, or an items file
that will not read, is audited as a `needs-you-reject` rather than turned into an
error that would make a successful board move look failed.

### The one-shot migration

A board already holding demo-gated rows when this ships has made its transitions
already, so without this every in-flight demo would silently vanish from the
panel on the release that adds the panel's own record. `migrate_demo_items`
synthesizes the missing rows.

**It is a migration, not a reconciliation, and that distinction is the design.**
The first cut ran it on every read and deduped on open rows only. That was
actively wrong, because resolving a demo item deliberately does *not* move the
task: the task was still demo-gated one refresh later, so the next read minted a
replacement row under a new id. The human's close-out came back — and again on
every subsequent resolve, and the same for an agent's withdrawal. A read that
reconciles the item file against the board can only ever fight the human.

Framed as a migration the question is well posed — "did this group exist before
the registry did?" — asked once, answered once, never changing. Two mechanisms
enforce it:

1. **The `needs-you-migrated` marker**, written even when nothing was added
   (`already considered` and `found nothing to do` are the same answer for every
   run after the first). Without it, every group resume would re-run.
2. **`Dedupe::EverRaised`** — for the migration, *any* demo row for the task,
   settled or not, is proof the registry has already seen it. A raise uses
   `Dedupe::OpenEpisode` instead, because there a settled row is a closed episode
   and a re-parked task genuinely deserves a new one. The two scopes are a named
   enum rather than a bool precisely so the difference cannot be lost again.

**They are belt-and-braces, not one guard and one decoration — and the precise
claim is worth stating, because the loose one ("both are load-bearing") is what
this paragraph first said and it is not true.** Either alone prevents the
resurrection in the ordinary case; each covers a case the other does not:

- Remove `EverRaised` alone and the ordinary case still holds, because the marker
  means the migration is never reconsidered. What breaks is the case where the
  marker is *absent while items already exist* — reachable, not hypothetical: the
  unreadable-file branch returns without writing the marker, and the group stays
  live and keeps raising through the hook, so the next load re-runs the migration
  against a file that already holds settled rows.
- Remove the marker alone and the ordinary case still holds too, because
  `EverRaised` reads the settled row as accounted for. What breaks is cost and
  noise, not correctness: every resume re-reads and re-considers the whole board.

Each guard has a test that isolates it, and the isolation is *measured* rather
than argued — the first draft of this paragraph asserted a split the runs did not
show, which is the failure mode the "run the mutation and see which tests redden"
rule exists for:

- `the_migration_does_not_resurrect_a_settled_row_even_without_its_marker`
  isolates `EverRaised`: it deletes the marker itself, so the marker neuter is a
  no-op for it, and it reddens on the `EverRaised` neuter **alone**.
- `the_marker_stops_the_migration_reconsidering_a_board_that_changed_later`
  isolates the marker, on the one observable `EverRaised` cannot mask — a board
  that gains a demo-gated row by a legacy path *after* the first load is
  deliberately not picked up, because the migration answered once and the hook
  owns everything since.

The two whole-flow resurrection tests redden only with the read path restored as
well, so they evidence the composite behaviour rather than either guard alone;
that is stated here rather than glossed, because a red evidences the assertion it
reached and no more.

**It runs at group load**, beside the pause / notify / autonomy marker re-seeds
it resembles — never on a read. That is what keeps `orch_needs_you_list` an
honest `viewer`-tier command: a peer holding only read rights must not be able to
drive a file write, an event emission and audit growth by polling. A group whose
panel can be open has been through that load path this session, so the trigger is
not weaker than the read was.

Re-parking is not this mechanism's job and never was: the transition hook already
gives a task that leaves and re-enters the gate a fresh row, which is pinned
separately.

## Concurrency

`needs_you_lock` serializes every read-modify-write of the file, a leaf of its
own like `questions_lock`.

**One documented nesting: `tasks_lock` → `needs_you_lock`, never the reverse.**
`upsert_task` keeps the board lock across the hook so that a task's status and
its demo item cannot settle in opposite orders under two racing transitions — an
in-then-out landing as out-then-in would leave an open demo item on a task that
is no longer parked, which is precisely the stale row the auto-resolve exists to
prevent. Nothing takes `tasks_lock` while holding `needs_you_lock`: the migration
reads the board through `tasks()`, which is a lock-free file read, and does so
*before* acquiring the items lock, so the nesting cannot cycle.

Also taken under the guard, both leaves and both the same nesting `tasks_lock`
already has: `AUDIT_LOCK` on the refusal paths, and the app-handle mutex on every
successful write (the event emit). The resolve path's success audit and its pane
delivery both happen **after** the guard is dropped — an audit write is cheap, a
delivery enqueues.

## Alternatives rejected

1. **One registry absorbing questions.** Migrating a live public contract, and
   entangling `resolve` with the answer trust boundary. See above.
2. **Keep demos as a projection, bolt a "dismissed ids" list beside it.** That is
   a second record *about task state* — the drift shape the projection was
   written to avoid — and it still gives items no identity, no raiser and no
   timestamps.
3. **`cleared: true` flags on settled rows** instead of a watermark. Mutates two
   files' contracts where one marker suffices.
4. **New task statuses for attention.** The exact model being rejected: it makes
   the ask a property of the work item again, which is where this started.

## What slice A does and does not reach

**Reaches:** the entity, the registry and its caps, the lifecycle hook and
one-shot migration, the watermark, the three Tauri commands and their ACL grants, the
audit actions, the event.

**Does not reach, and is not silently missing:**

- **The MCP surface** (`request_attention`, `withdraw_attention`,
  `list_needs_you`) — slice B. Until it lands, `raise_needs_you` and
  `withdraw_needs_you` have exactly one caller each: the board hook, and nothing.
  The registry methods exist and are tested; no agent can reach them yet.
- **The panel** — slice C. `orch-needs-you-changed` has no listener until then,
  which is cheaper than a second visit to the write path later. The DEMOS tier
  keeps rendering its projection in the meantime, so nothing regresses: the items
  accumulate correctly behind it and the panel starts reading them when C lands.
- **The sort** (#1151 decision D1, urgency-pinned then newest-first) is the
  panel's, not this projection's. `project_list` returns open rows in raise order
  and a newest-first resolved tail; sorting here would put a second, weaker
  ordering in the way of the union sort C has to do anyway.

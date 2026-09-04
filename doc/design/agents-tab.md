# Agents tab — the pane state model (#2122)

The Agents tab (#2122) and the pane Notes rows (#2116) both want to say, in one
word, what each pane is doing. This note is the contract they read: what a pane
projects (`Pane.facts()`), how that projection becomes a state
(`deriveAgentState`), and — the part that is a judgement rather than a
derivation — how far each harness can be trusted when the answer is
`turn-done`.

Slice A shipped the foundation: `src/paneactivity.ts`, `src/agentrows.ts`,
`Pane.key` / `Pane.facts()` / `Pane.noteRosterIdle()`, and the tests. There was
no UI in it. Slice B — the tab host, the view that renders these rows, the
spinner and the badge — is the last section of this note.

## The projection: `Pane.facts()`

`src/pane.ts` holds every fact already, as a dozen scattered getters. A view
reading them one by one would be coupled to `Pane`'s shape and impossible to
test without a DOM, so `facts()` hands over one plain-data reading and
`agentrows.ts` decides what it means. `pane.ts` owns *where the facts come
from*; `agentrows.ts` owns *what they add up to*.

`facts()` carries the same contract `tabPaneInfo()` does — no geometry, no IPC,
no timer — so it is safe on a hidden tab, on every row, once a second.

Two fields are worth their own paragraph.

**`key`** is minted from a module counter in the `Pane` constructor
(`pane-1`, `pane-2`, …). It is per-window and never persisted: its only job is
to let a view diff its rows across a re-render. `ptyId` cannot do that (it
changes on every respawn) and the pane name cannot either (the human renames
panes). Nothing needs it to survive a restart, and a persisted key would be a
schema.

**`harness`** is `agentCli ?? sshDefaultCli ?? null` — read off the launch line,
never branched on a CLI name to produce a name (#722/#841). A CLI neither of
those recognises shows up as `null`, not as somebody else's identity.

## The activity reducer, and why `atPrompt` is a latch

`src/paneactivity.ts` is a DOM-free per-pane state machine with an injected
clock. It reads no screen, opens no PTY probe and issues no IPC; every input is
a signal the pane already receives.

| input | wired from | what it means |
| --- | --- | --- |
| `noteOutput(bytes, nowMs)` | `Pane.acceptOutput`, per chunk | this pane produced output |
| `noteHumanInput(nowMs)` | `Pane.markFirstInput` and `Pane.markHumanInput` | the human typed or pasted |
| `noteAttention(reason)` | `Pane.setAttention` | the backend's current attention reason |
| `noteRosterIdle(idle)` | the tab-strip poll, via `Pane.noteRosterIdle` (slice B) | the roster's own idleness reading |

The design decision is the `atPrompt` latch. The backend's `waiting` attention
reason already means "parked at a prompt": `attention_tick` /
`plain_pane_attention` raise it when output has been quiet for
`ATTENTION_QUIET_MS`, a prompt shape sits on the masked tail, and no keystroke
landed inside `ATTENTION_RECENT_INPUT_MS`. It covers plain panes too, keyed
`pty:N`. But it is **acked on focus** (`Pane.acknowledgeAttention` →
`attn_waiting_ack`), so reading it directly would flip a still-parked pane back
to `working` the instant the human clicks it — and a click is not evidence that
the agent resumed.

So a `waiting` sighting latches, and the latch clears only on evidence
independent of the signal that set it:

1. **human input** — the human typed, so the turn is theirs;
2. **at least `ACTIVITY_FLOOR_BYTES` of output inside one contiguous burst** —
   the pane is painting something bigger than an idle repaint. A burst is chunks
   arriving less than `ACTIVITY_WINDOW_MS` apart; the first chunk after a longer
   gap starts the count over.

Both clears are bounded and signal-independent, which is what
`.orrerix/lessons.md` requires of any suppression driven by a fallible signal:
neither depends on the attention scan noticing anything, so the latch can never
be held on by the scan going quiet.

`ACTIVITY_FLOOR_BYTES` is **2048**, duplicated from the backend's
`DEFAULT_IDLE_ACTIVITY_FLOOR_BYTES`, where the number was measured (a full idle
Claude Code input-box repaint is ~164 bytes —
`src-tauri/tests/fixtures/attention/idle-input-box.txt`). `ACTIVITY_WINDOW_MS`
is **4000**, the backend's own `ATTENTION_QUIET_MS` — so the gap the latch is
cleared over is the gap it was set over. Both are duplicated rather than
plumbed for the reason `DOCK_TERM_RESERVE_PX` is, and both are pinned against
the Rust literals by `test/paneactivity.test.ts`, which reads the sources off
disk so the copies cannot drift silently.

**`ACTIVITY_WINDOW_MS` is a gap bound, not a fixed window**, and the difference
is worth being exact about. Chunks arriving closer together than 4 s keep
topping up one count for as long as they keep coming, so a pane repainting
steadily at, say, 3.9 s intervals *does* cross the floor after enough repaints
and clears the latch. That is the intended reading — a terminal painting without
pause for a minute is not a parked one — and it fails toward `working`, never
toward a turn-done claim that is not true. It is also **not** the backend's own
floor rule, which is a different instrument for a different job:
`idle_output_is_activity` compares total growth between two scan ticks. The two
are named together here so a reader does not take them for one mechanism.

**The clock can go backwards, and the burst rule handles it explicitly.** `Pane`
feeds the reducer `Date.now()` — wall-clock, so an NTP correction or a laptop
resume moves it. That source is deliberate: `lastOutputMs` and
`lastHumanInputMs` are reported to consumers and must stay comparable with
`firstInputMs`, which is wall-clock throughout `pane.ts`. What a plain
`now - last > BOUND` would do, though, is read a *backward* jump as a negative
gap — "still the same burst" — so sub-floor repaints would accumulate across the
jump and eventually clear a latch that should have held. So a non-monotonic
reading is treated as a burst **boundary** instead, which fails toward holding
the latch: the pane keeps reading `turn-done`, the state the human is being
asked about, rather than silently declaring work that never happened.

**Residual on the floor, because the Rust side is not a bare const.** The
backend's floor is a live-tunable guardrail knob
(`Guardrails.idle_activity_floor_bytes` / `set_idle_activity_floor`), so a group
that raises its own floor runs a value this copy does not read, and the pin
above keeps the two *defaults* in step rather than the two live values. Nothing
plumbs it: that would be a wire read per pane per second for a number nobody has
yet moved. The divergence is bounded and fails in the safe direction — a
frontend floor below the backend's clears the latch earlier, so the pane reads
`working`, which is the ladder's honest "no evidence of a prompt", never a
turn-done claim that is not true.

Two consequences worth stating outright:

- **The human-input clear hangs off `markFirstInput` AND `markHumanInput`**, the
  two functions in `pane.ts` that already own the single answer to "what counts
  as human input". Reading it off one and not the other would be a bypass
  exactly the width of the IME composition path. Neither is `term.onData` —
  that is #440 B2-R's structural guarantee, and it is why copilot's boot-time
  OSC/DA replies (#179) cannot un-park a pane here.
- **Nothing resets on respawn, deliberately.** `lastHumanInputMs` is a pane
  fact, not a process one (the same reasoning `humanOrigin` carries). The latch
  needs no reset either: a respawned agent CLI repaints far more than the floor
  on boot, which is clear (2); and a respawned plain shell that comes back
  sitting at a prompt is correctly still `atPrompt`.

## The ladder

`deriveAgentState(facts)` is a precedence ladder — the first rung that decides
wins, and each rung is a strictly more urgent claim than the one below it.

| # | state | decided by |
| --- | --- | --- |
| 1 | `dead` | not alive, not dormant, not welcome |
| 2 | `dormant` | a restore placeholder |
| 3 | `held` | `held !== null` — loomux is withholding a delivery (#246) |
| 4 | `attention` | the reason is urgent per `attention.ts` (`held-dialog`, `blocked`, `stranded`) |
| 5 | `question` | the reason is in `attention.ts`'s `DECISION_REASONS` (`question`, `gate`) |
| 6 | `reported` | the reason is in `attention.ts`'s `REPORT_REASONS` (`report`) — waiting on the ORCHESTRATOR, not on a human decision (#2367) |
| 7 | `turn-done` | the reason is `waiting` **or** the latch is set |
| 8 | `idle` | output under the floor, AND — orch pane: the roster says idle; other panes: never prompted, ever |
| 9 | `working` | everything else |

It takes **no clock**. Everything time-dependent — whether the output window has
lapsed, how many bytes are in it — is already resolved by
`PaneActivity.snapshot(nowMs)` at the moment `facts()` was called. (The plan's
sketch carried a `nowMs` parameter; it would decide nothing here while reading
as though it did, and `noUnusedParameters` would refuse it.)

Two rungs need their reasoning stated, because both are places where an
obvious-looking simplification is wrong.

**`dead` outranks a stale `waiting`.** The scan's last word about a process that
has since exited is not news.

The trap under that rung is that **"no PTY" is four different things**, and only
one of them is a failure: a welcome form and a dormant placeholder have not
started one yet, a **content pane** (files, editor, git, workflow) needs none at
all and is live the moment it exists, and a dead pane had one and lost it.
`facts().alive` is therefore `tabPaneInfo().live` — the repo's single existing
answer to "is this pane live", which already draws that distinction — and not a
fresh `ptyId !== null && !exited`, which is the same expression for a *terminal*
pane and calls every content pane dead. `facts()` reads `kind` and `alive` from
one `tabPaneInfo()` call for that reason: two rules asking one question is how
the two drift apart.

**`idle` is one shared condition plus one that differs by pane kind.** The
shared half is the floor: **a pane painting above `ACTIVITY_FLOOR_BYTES` is not
idle, whatever else is true of it.** It is hoisted out of both branches rather
than written into one, because a guard that reads a signal on one arm and not
its sibling is a bypass exactly the width of that asymmetry — the first draft of
this rung read the floor on the orchestration arm alone, and an unattended
non-orchestration agent pane (`main.ts`'s resume-agent, fresh-agent and
plain-session-restore all open one with a command and no `orchGroup`) therefore
read `idle` for its entire working run. That was #2195 review finding B1.

The half that differs does so because the available evidence differs. An
orchestration pane has the roster's `idle_since_ms`, which means "the reaper
would call this idle / it holds no assignment" — explicitly *not* "parked at a
prompt" (#2089), which is why it feeds this rung and never `turn-done`. A pane
the roster does not cover has one fact left once the floor has been applied:
nobody has ever prompted it. That is a **pane-lifetime** fact, not a per-process
one — `PaneActivity` is constructed once per `Pane` and `respawnFresh` resets
`firstInputMs` while deliberately leaving `lastHumanInputMs` alone — so the rule
is "never prompted, ever", not "never prompted this process". Applying that
basic rule to an orchestration pane would report every unattended worker as
idle, which is the normal case.

**`working` is the default, and its honest reading is "no evidence of a
prompt"** — not "measured to be busy". Anything the ladder cannot place lands
here, including an attention reason the backend adds tomorrow.

## Per-harness trust for `turn-done`

`turn-done` is the rung that makes a claim about the *agent*, so how far it can
be trusted varies by CLI. The generic prompt scan runs for all of them; the
column below is what is known beyond that.

| harness | trust | why |
| --- | --- | --- |
| claude | trusted | `waiting` fires on the idle input box; `src-tauri/tests/fixtures/attention/idle-input-box.txt` is the measured shape |
| copilot | trusted after the first turn | boot OSC/DA replies are excluded backend-side (`classify_human_input`, #496) and by the latch's input rule; its permission TUI is covered by the `question` rung |
| opencode | trusted, with one unmeasured edge | same generic scan. Whether its footer repaints exceed the 2048 floor is **unmeasured** — the floor is the guard, and if they do exceed it the pane reads `working` rather than something false |
| gemini / codex / custom | generic only | no per-CLI marker; `working` is the default reading |
| (any) | never | the roster's `idle_since_ms` feeds `idle` only — it is not an at-prompt signal (#2089) |

**Upgrade path, recorded and not built.** An exact `Stop`-hook signal exists for
orchestration panes, because loomux writes their hook settings; a basic pane's
command line may not be rewritten (`session-id-learning.md` option C). A backend
`at_prompt` field would be exact too, but it widens `AttentionItem` or adds a
push — a wire change for a fact the frontend can latch from signals it already
has. Either is the answer if the latch proves noisy in practice.

## What the rows carry

`toAgentRow(facts, notes)` projects one pane into the row both views render:
`key`, `name`, `harness`, `group`, `agentId`, `role`, `state`, `notes`. `notes`
is the count slot #2116 fills, and `null` there means "not loaded", which is a
different claim from `0`. `sortRows` orders by state urgency then by name, so
the order inside one state is stable as states change around it;
`matchesFilter` backs the filter chips; `needsYouCount` counts the two states a
person must act on (`attention`, `question`) and is the badge number — `held` is
loomux's own doing and clears itself, a `reported` pane waits on its
orchestrator rather than on the human (#2367), and a finished turn is not
something anyone is blocked on.

`tab` (#2371) is the tab the reading was taken from, and it is the one field on
`PaneFacts` the pane does not derive — see the next section.

## Grouping and order (#2371)

The list groups its rows under the tab they live in, with a header carrying the
tab's name, and offers the human a choice of which group comes first.

### The tab is supplied by the caller, not derived by the pane

`PaneFacts.tab` is a `TabRef` — `{ id, title, index }` — and `Pane.facts(tab)`
takes it as an argument. That is a fact about the object graph rather than a
shortcut: a `Workspace` owns a `Grid` and a `Grid` owns its panes, with no
back-reference the other way, so a `Pane` genuinely cannot answer "which tab am
I in".

The one caller that needs the answer already has it. The Agents view's `facts()`
dep reaches every pane *by* walking `tabs.tabs`, so it is holding the workspace
— and its position in the strip — at the moment it asks. Passing it in keeps
`facts()`'s contract intact (no geometry, no IPC, no timer, safe on a hidden
tab) where a lookup would have to walk the whole tab set once per pane to
recover something the enumeration already knew. Every other caller — the pane
Notes rows (#2116), `main.ts`'s focus walk — passes nothing and gets `null`,
which is a complete answer for a caller that groups nothing.

`index` is the strip position, and the header reads `title`. The two are
separate on purpose:

- **Groups are KEYED on `id`.** The human may legally name two tabs the same
  thing, and a rename must re-label a group rather than split or merge one. A
  title-keyed grouping shows two headers for one tab during the tick where a
  rename has reached one pane's reading and not another's.
- **Groups are ORDERED on `index`, never on `title`.** The strip order is an
  arrangement the human made by dragging (`dropTargetIndex`, #379). Alphabetical
  order would silently reshuffle the whole list on a rename, which is a list
  reordering itself in response to something that was not about order.

### `groupRows(rows, order)` — grouping is unconditional, `order` picks the first group

`groupRows` is a pure projection in `agentrows.ts`: rows in, `[{ tab, rows }]`
out, no DOM and no clock. Two decisions are worth stating.

**Grouping is always on.** The headers are what make the fleet read by where it
lives, so they are not something you have to switch an order to see — and making
them unconditional is what gives `"state"` a *defined* group order instead of an
accidental one. `order` decides only which group you read first; `sortRows`
orders the rows inside every group, in both orders, so "most wants you" never
means something different inside one tab than inside another.

| `order` | group order | tie |
| --- | --- | --- |
| `"state"` | the group holding the most urgent row first (`rows[0]`, which `sortRows` has already put there) | strip order |
| `"tab"` | strip order outright (`TabRef.index`) | — |

**A tab with no rows produces no group, by construction.** The buckets are built
from the rows, so a tab this call never saw cannot appear — there is no filter
step that might miss one. `visibleGroups` applies the filter chip *before*
grouping, which is the same rule read from the other end: a tab whose every row
was filtered out loses its header too, with no second mechanism to keep in step.

**The headerless group** collects rows whose reading named no tab. It renders no
header — there is nothing to call it, and an invented "Other" would be a claim —
and the two orders treat it differently on purpose: under `"tab"` it has no
strip position, so it goes last; under `"state"` it is ranked like any other
group, because a headerless group holding a wedged pane is still a wedged pane
and burying it under a tab whose worst row is `idle` would hide urgency for the
sake of tidiness. No production caller produces one today — `main.ts` always
names a tab — so it is a defensive case, pinned rather than assumed.

### The order is one `localStorage` key, and deliberately not a `BoardPrefsStore`

`src/agentorder.ts` holds the choice under one key. It is **not** built on
`BoardPrefsStore`, and the reason is what that class is for: `boardprefs.json`
is ONE file shared by every group, so a save publishes every other group's
record and must never run against a handle nobody has read (#1299). This key
holds one scalar belonging to one viewer. There is no in-memory handle that
could be stale and no other tenant in the key to lose, so the invariant is
satisfied structurally — wrapping a synchronous single-value API in an async
read-before-write store would ship the *shape* of that protection with none of
the hazard it protects against.

What does apply is guarding every access. A private window, cleared site data,
or a browser refusing storage makes the accessor itself throw, and the unit
tests run under `node --test` where `localStorage` does not exist at all — so a
read that cannot happen answers the default and a write that cannot happen is
dropped. An unrecognised stored value also reads as the default *without being
rewritten*: scrubbing a word a newer build stored would lose the human's choice
the moment they opened an older build.

### Rendering: two keyed maps, one flat walk

`AgentsView` walks the sequence the groups spell out — header, its rows, next
header — placing each element after the previous one only when it is not already
there. That is the rule the ungrouped list already followed and it is what keeps
a re-order cheap: a group whose rows did not move inside it costs one
`insertBefore` for the header and none for the rows, so the working spinner's
CSS animation is never taken back to frame 0.

Headers are keyed by tab id and rows by pane key, in **two** maps, because their
lifetimes differ: a row survives its tab's header disappearing (the human moved
the pane) and a header survives every one of its rows being replaced. Both are
keyed rather than rebuilt for the reason the filter chips are — a subtree the
keyboard is standing in, rebuilt once a second, drops focus to `<body>`.

**No PTY resize, on any of it.** The panel is `#sessions`, which is in flow
already; nothing here changes its width, and switching order or filter moves
elements inside `.sessions-inner` only. Constraint 1 is untouched.

### The row's agent-type mark

`agentRowMark(row)` is one call — `agentMark({ knownCli: row.harness })` — and
that is the whole design. `harness` is the CLI loomux already knows this pane
runs (`agentCli` off the launch line, or an SSH profile's declared far-end CLI),
which is exactly `agentMark`'s `knownCli` input, so a CLI added tomorrow shows up
as itself. A `harness === "claude" ? … : …` here would be the #722/#841 defect:
the fourth CLI silently inheriting the third one's badge. `null` falls out of the
resolver's own rule rather than being a case here — a row with no harness has no
launch line to read, and a row of `?` badges over every terminal is noise dressed
as information.

**Residual.** `AgentRow` does not carry remoteness, so an SSH pane whose profile
declares no `defaultCli` reads `harness: null` and draws nothing, where the pane
*header* draws the neutral "remote — agent CLI unknown" badge for the same pane.
Both decline to name a CLI; the header is the surface that can afford to explain
why, and the row's identity line already says what the pane is.

## Slice B: the tab, the view, and the two things it is wired to

Slice B ships the UI: `src/leftpanel.ts` + `src/leftpanelmodel.ts` (the tab
host), `src/agentsview.ts` + `src/agentsviewmodel.ts` (the rows),
`src/spinner.ts` (the working glyph), `src/rosteridle.ts` (the strip reading),
and a `TabBar.onStrip` hook. Nothing in the state model above changed; this is
what reads it.

### A tab, and why that is the whole argument

CLAUDE.md constraint 1 permits exactly two in-flow panels, `#sessions` and
`.sidedock`, and a third would need the argument `doc/design/side-dock.md` and
`doc/design/xterm-resize-reflow.md` describe before it may exist. Two tabs
inside one panel need none, and the reason is mechanical rather than
rhetorical: the panel's width is bound to one boolean — is it open — and a tab
switch does not touch that boolean. So a switch moves no column, autosizes
nothing, and reaches no `fit()`.

That is stated as a transition function rather than as a comment.
`toggleTarget(state, requested)` in `leftpanelmodel.ts` is the only thing that
decides where a toggle lands, `LeftPanel.sync()` is the only thing that touches
`#sessions`'s `hidden` class, and `test/leftpanelmodel.test.ts` enumerates every
crossing of {open, closed} × {same tab, other tab} — including the one that
matters, *switching tabs on an open panel never changes visibility*.

**The `#sessions` CSS rules are untouched**, and that is load-bearing rather
than tidy: `test/resizeburst.test.ts` reads the panel's 240 ms width transition
off the stylesheet to derive the app's frame budget, so a change to the width or
the transition changes a number that test asserts against `FIT_MAX_WAIT_MS`.
Everything slice B adds sits *inside* `.sessions-inner`, which was already a
fixed-width flex column. The one structural move is that `.sessions-inner` now
belongs to `LeftPanel` rather than to `SessionBrowser`: the browser is handed a
body inside it, and `visible` / `toggle` / `hide` move to the panel, because
they were always answers about the panel and there are now two views that would
each have needed a copy.

### Two refresh triggers, one ticker, and what each is for

| trigger | why it exists |
| --- | --- |
| `tabs.onChange` | a pane opened, closed, moved or was renamed anywhere in the window |
| `PaneEvents.onRecordChanged` | a rename, which does **not** reach `tabs.onChange` (#214) |
| `applyAttention` (inside its gate) | four of the eight rungs are decided by the reason that pass just wrote |
| `TabBar.onStrip` | the roster reading landed |
| the 1 s ticker | the two inputs that move with no event behind them |

The dock's own `tabs.onChange` subscription is deliberately *filtered* to real
active-tab changes, because following it unfiltered was a defect (#1097
rev-767 B1) — it re-reads the active pane's live cwd. This one is deliberately
**unfiltered**, and the two are not in tension: a rename, a background tab's
pane closing and an attention flip are all things this view should redraw for,
and a redraw here is a walk over open panes and a keyed diff, with no live read
of anything.

The ticker exists because two inputs change with no event: the output burst
`PaneActivity` accumulates (a pane that *stops* painting has to be noticed to
stop reading as `working`), and the roster's reading, which lands on the strip
poll's 4 s cadence. It is armed by `LeftPanel`'s `onShow` for this tab and
cleared by its `onHide` — so it is off whenever the panel is closed *or* the
Sessions tab is the selected one — and it is gated on window visibility within
that scope through `pollgate.ts`, because component scope and visibility are
different questions (performance.md §3 INV-4). It is declared in
`test/perfpolicy.test.ts`'s TIMERS manifest, which refuses an undeclared
`setInterval` outright. A tick carries no IPC at all.

**The badge does not depend on the ticker.** `AgentsView.refresh()` derives the
count and returns before rendering when the tab is closed, so an attention flip
moves the number on a closed panel without paying for a render — which is what
makes the count useful unopened.

### The roster reading rides the strip poll that already exists

The `idle` rung needs `idle_since_ms`, which arrives on
`StripViewPayload.groups[g].summary.agents[]` — the payload of the tab strip's
own 4 s poll, and the app's single `orch_strip_view` read
(`doc/design/polled-views.md`: one read per strip). So `TabBar` gained an
`onStrip(cb)` subscription rather than this feature gaining a poll: the number
is on the wire either way, and a second poll would double the app's standing IPC
for it.

Two details of that hook are decisions, not incidentals.

- **It fires only on a read that RESOLVED.** The strip's failure path returns
  early, so a tick the backend refused leaves every pane's previous reading in
  place — the same stale-but-true rule the badges themselves follow, rather than
  handing out a fabricated empty roster.
- **It fires LAST**, after `recordSweepSuccess` and the render, so a subscriber
  that throws cannot cost the sweep its witness or leave the badges unpainted.

`rosterIdleFor` (`src/rosteridle.ts`) is the lookup, and it is a module rather
than four inline branches because three separate absences all have to answer
`null` rather than `false`: the group may not be in the payload, its summary may
be a refusal, and the agent may not be in the roster. `null` means *the roster
does not cover this pane*, which the ladder reads as no evidence; `false` would
be a positive claim that the agent holds work, derived from a lookup that found
nothing. Both land on `working` today, so honouring the distinction costs
nothing and is the difference between an honest reading and a lucky one. The
reading is also `idle_since_ms !== null`, never a truthiness test: it is a
unix-ms timestamp and `0` is a legal one.

Every pane is told, including panes with no orchestration identity —
`rosterIdleFor` answers `null` for those. Telling only *some* panes would leave
one that lost its group binding wearing its last reading forever.

### The spinner is a sprite

`src/spinner.ts` emits one inline `<svg>` whose inner `<g>` is
`SPINNER_FRAMES × SPINNER_CELL` units wide; the viewBox is one cell, and
`styles.css` walks the strip with a `steps(8, jump-end)` translate of exactly one
cell per step. So there is no per-frame DOM work at all — the issue rules that
out in as many words — and the animation is a compositor transform.

Three shapes were considered.

| candidate | verdict |
| --- | --- |
| a braille-glyph `content:` keyframe animation | rejected: on Windows braille falls back to Segoe UI Symbol, whose baseline and advance drift against the UI stack, and a `content:` glyph cannot be dyed per state |
| a per-frame DOM swap | rejected: per-frame churn on a list that can hold every pane in the window |
| one SVG sprite stepped by CSS | shipped: deterministic geometry, one element, `currentColor` |

It is **not** in `src/icons.ts`. That registry is vendored Lucide pinned
verbatim by `test/icons.test.ts`, which would refuse a hand-drawn entry —
correctly.

The geometry is pinned from both sides: `test/spinner.test.ts` asserts eight
distinct frames, a solid head with a strictly fading tail, a head that walks one
ring position per frame and returns to its start after a full turn, and that
every dot sits at a whole-pixel position inside its own cell. The stylesheet's
`-40px` and `steps(8)` are the same arithmetic written once more, and the
comment above them says so.

`prefers-reduced-motion: reduce` stops the animation on frame 0. The row's state
**word** is what carries the meaning either way, which is why the word sits
beside the glyph rather than being replaced by it.

### Colour: which channel each mark is on

A row's left edge and its state cell are **state** positions, so they take
`--state-*` tokens — that is what channel 1 is for (`styles.css`'s own note).
`held` and `idle` are achromatic there by design: a stopped agent carries no
dye, and is separated by its word and its edge rather than by a hue. The
selected filter chip and the selected tab are **interaction** positions, so they
take `--accent`: "the one you picked" is not an agent state. The needs-you badge
is a state position again — it counts exactly the two rungs a person must act
on — so it is attention-dyed.

`.agents-chip.active` is an accent *background*, which
`test/theme.test.ts`'s default-deny guard refuses unless argued; it is listed
there as an on-state, the same class as the task board's own filter chip.

### Residuals, stated

- **A hold masks a needs-you count until the next attention change.** `held`
  outranks `attention` and `question` on the ladder, so a pane that is both held
  and blocked reads `held` and is not counted. When the hold clears, the count
  should rise — and the delivery-held events do not drive a refresh, so with the
  panel closed the badge can lag until the next attention *change* (the 3 s scan
  re-emits an unchanged set, which `AttentionGate` correctly drops). Opening the
  panel repaints immediately, and the ticker covers it while open. Not wired,
  because widening `OrchWiring` for a badge that is already correct within one
  attention change is a wire change for a lag nobody has measured.
- **The list is this window's open panes.** An agent with no pane open, or a
  pane in another orrerix window, is not here; the session browser is.
- **An agent-initiated reveal of a hidden pane is deliberately partial.**
  `focus_agent` on a pane that is docked or behind a fullscreen sibling makes
  it the active pane and focuses it, but leaves it hidden — constraint 1 will
  not let an agent resize a PTY to uncover it. The human's own Agents row
  reveals it fully, and the pane's attention badge is what points them at it.
  Swapping the fullscreen to the target instead was considered and rejected on
  the same ground as the human case: it is a layout change nobody asked for,
  and here nobody human asked for anything at all.
- **No keyboard chord.** Out of scope for this slice — a new chord needs the
  `agent-cli-reference` sweep `doc/design/side-dock.md` describes, and the two
  buttons plus the tablist cover the gesture.

## Reveal, not focus (#2365)

A row click used to be three steps — switch to the pane's tab, `Grid.setActive`,
`Pane.focus()` — and all three are blind to the two states that make a pane
invisible while its PTY stays bound.

**Maximize.** `Grid.toggleMaximize` lifts the target's element out of the split
tree into a top layer under `.grid-root` and `styles.css` hides everything else
with `.grid-root.has-maximized > :not(.maximized) { display: none }`. So
`setActive` on a pane behind a maximized sibling toggles a class inside a
`display:none` subtree, and `term.focus()` on a hidden textarea is a browser
no-op. The row click *ran*, correctly, and did nothing anyone could see.

**The dock.** A minimized pane is out of the tree entirely, so the same two
steps have no element to act on — and `toggleMaximize` refuses a docked pane
outright (`if (!this.leaves.has(pane)) return`), which means a pane the blind
`setActive` had made *active* from the dock could not even be maximized back
into view.

Every surface that says "go to this pane" was one of these three-step copies:
the Agents row (`deps.focus`), `orch-focus` → `OrchWiring.focusPty`, and the
Sessions tab's live-group copy, which said "focus its orchestrator pane instead
of resuming it" and offered nothing to click at all. They are now one function,
`revealPane` in `main.ts`, over one rule, `revealPlan` in `panefocus.ts`.

### Where each half lives, and why

The rule is DOM-free and in `panefocus.ts` — already the module that owns
focus/maximize decisions — so all twelve crossings of
`{tab active} × {docked} × {maximized: self | other | none}` are pinned under
`node --test` rather than hand-validated. `Grid` executes the steps it owns (it
is the holder of the dock and the lifted element); `main.ts` executes the tab
step, because it is the only holder of `tabs`. No layer is crossed that is not
crossed today.

### Decision: reveal EXITS fullscreen rather than swapping it

When a *different* pane is maximized, the reveal drops fullscreen and returns to
the layout the human already knows. The alternative — swapping the fullscreen to
the target — was rejected: it keeps the human in a mode they did not choose for
this pane, and it hides whatever they *had* maximized with no visible cause,
which is the same class of silent disappearance this issue is about. Exiting is
one step back to a layout that is on screen and reversible with one
`Ctrl+Shift+M`.

Revealing the maximized pane **itself** leaves fullscreen alone
(`maximized: "self"` emits no `exit-maximize`). A human who maximized a pane and
then clicked its own Agents row is already looking at it; yanking them out would
be a layout change nobody asked for. That is a negative control in the tests,
not just a branch.

A dock restore stands in for the exit: `Grid.restore` already exits fullscreen
on its way in, so emitting both would be one redundant relayout — and under
constraint 1 a redundant relayout is a redundant PTY fit.

### Constraint 1: who asked decides whether a PTY may be resized

**This section said something false in round 0 and round 1, and the correction
is the interesting part.** It read: *"`exitMaximize` and `restore` are the same
discrete-human-click class … No new fit is added, no passive or continuous
trigger reaches one."* The first half is true of the two surfaces that clause
was written about — the Agents row and the Sessions Focus button. It was false
of the third surface this same section lists three paragraphs above:
`orch-focus` → `OrchWiring.focusPty`.

`orch-focus` has no human on its path at all. It is emitted from exactly one
place, `Registry::focus_agent`, reached from exactly one place, the
`focus_agent` MCP tool — advertised to every orchestrator in its own template
and gated only by `require_orchestrator`. So rewiring `focusPty` to a reveal
handed an unprompted agent tool call the ability to drop the human out of a
fullscreen they chose, or pull a pane they had docked back into the split tree.
Both genuinely resize a PTY — `toggleMaximize`'s own doc says the maximized
pane *"alone issues one debounced fit"*, `restore`'s says re-attaching
*"triggers a single genuine fit"*, and a restore re-seats the pane beside the
active one so its new siblings fit too. That is precisely the trigger class
constraint 1 bars.

**The repo had already adjudicated this exact case, seventy lines away.**
`shouldPreserveMaximize` (#155, `src/panefocus.ts`) exists because an
orchestrator-driven *spawn* used to collapse the human's fullscreen: *"the
human is watching one pane full-screen and an agent spawning in the background
must not yank them back to the grid."* `Grid.placeLeaf` honours it. The reveal
path re-opened the same question and had no equivalent answer.

So `revealPlan` takes a `humanInitiated` bit, and the two structural steps —
`restore-from-dock` and `exit-maximize`, the only two that resize anything —
are gated on it:

- **A human gesture** (Agents row click, Sessions **Focus** button, the
  Mine-row click's reveal) may exit a fullscreen and un-dock a pane. This is
  the discrete-human-click class constraint 1 sanctions, through the existing
  `resizeburst` coalescing, so no *new* fit is added there.
- **An agent-initiated reveal** (`orch-focus`) gets `switch-tab` /
  `set-active` / `focus` and nothing structural. The agent can still say which
  pane it wants attention on — the tab comes forward, the pane becomes the one
  receiving keystrokes — and it cannot resize a thing. `switch-tab` is not
  gated because it moves no PTY.
- **The plain case** — a visible pane in the tab already showing — touches no
  layout at all whoever asked: its plan is exactly `["set-active", "focus"]`,
  which is what the app did before.

The bit is a **required** parameter on `revealPlan`, `Grid.reveal` and
`revealPane` rather than an option with a default, so a future caller fails to
compile instead of silently inheriting the permissive answer.

Pinned by three tests, and the negative control is the load-bearing half: for
each of the twelve states, the agent plan must contain neither structural step
**and** the identical state asked for by a human must still contain the one it
is owed. Asserting only the agent half would pass equally well against a
`revealPlan` that had stopped emitting structural steps for anybody, which is
the regression the pair exists to tell apart; a population control asserts that
8 of the 12 human crossings really are owed one, so the control cannot go
vacuous. Deleting the gate reddens exactly the three agent tests; inverting it
reddens those three plus the two human ones.

### The maximize trigger is INFERRED, not observed

This is the honest limit of the diagnosis. Nothing in the report or the logs
records a maximize; what the reading establishes is that no frontend path
reachable from a row click can *remove* a pane (`Grid.closePane(pane, false)`
has three callers, none of them reachable from a click, and `captureLayout`
walks every leaf), and that fullscreen is the app's **only** mechanism for
hiding a pane while leaving its PTY bound. So maximize is the inferred trigger
because it is the only candidate the code admits — not because anyone saw it
happen.

The fix does not depend on that inference being right. It makes every reveal
path handle *both* hiding states, so whichever one was in play, the pane comes
back. Deciding the next occurrence by log rather than by reading would need a
`ui_breadcrumb` command recording maximize/minimize/close-without-kill; that is
a new public command and was deliberately left out of this slice.

### Deviation from the plan: `liveSessionAction` is role-gated

The plan for this slice specified `liveSessionAction({ groupLive, paneInWindow })`
and wired it into the Sessions **Mine**-row click for every orchestration-routed
row. Implemented literally, that regresses a live worker rejoin: the backend
refusal it stands in for is role-gated —
`if record.role == "orchestrator" { if record.group_live { return Err(…) } }`
in `src-tauri/src/orchestration/mod.rs` — so a worker or reviewer row in a live
group is **rejoined**, not refused. With a 2-input rule, clicking a live
worker's row would have revealed the group's *orchestrator* pane instead of
rejoining that worker.

So the rule takes a third input, `isOrchestratorRow`, and
`groupLive && !isOrchestratorRow → "resume"`. The crossings go 4 → 8 and the
live-worker case is pinned as a negative control over both pane placements. The
rule stays in the pure module rather than becoming a condition at the `main.ts`
call site, which is this slice's own boundary argument: a rule the backend
enforces should not live in untested DOM glue.

`explain` (a live group whose orchestrator pane is in no window here) exists so
the app never calls into a refusal it can already predict — the round-trip could
only come back with advice the human has no way to act on.

### Review round 1: two corrections, and one precondition that stays a comment

**`revealPane` is two steps, not three.** It used to end on a `pane.focus()` of
its own, after `Grid.reveal` had already run the plan's `focus` step — a second
focus of the same terminal. Harmless, and precisely the redundant step this
section claims the design does not emit, so the claim and the code disagreed.
The trailing call is gone. The deletion is safe because the plan *always* ends
in `focus`: `no plan ever contains a removing step` asserts
`plan.slice(-2) === ["set-active", "focus"]` on all twelve crossings, and
deleting `plan.push("focus")` reddens it along with three others.

**`Grid.reveal` has a precondition, and it is a comment rather than a guard.**
It passes `tabIsActive: true` unconditionally — the grid cannot know the answer —
so its plan omits `switch-tab`. A caller that has *not* put the pane's tab on
screen therefore runs every step under `display:none` and has re-entered exactly
the blindness this whole section is about, with nothing red to say so.
`revealPane` is the only caller and it switches tabs first.

A source-scanning guard was written for that and **withdrawn**, which is worth
recording because the reason generalises. `reveal` is an ordinary method name in
this tree — `EditorWidget.reveal(line, col)` (`src/editorwidget.ts`), and
`src/filemenu.ts` — so a textual check keyed on the call shape cannot separate
`Grid.reveal` from the others: run against the real tree it refused two
known-good files on its first pass. Anchoring it on `.grid.reveal(` instead
would have passed, but `Grid.allGrids()` hands out bare `Grid` values, so the
evasion it left open is the one a new caller is most likely to take. A guard
whose green is an accident is worse than no guard, so the precondition is
carried in prose — here and on the method — and the residual is stated rather
than implied.

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
| 5 | `question` | the reason is in `attention.ts`'s `DECISION_REASONS` (`question`, `gate`, `report`) |
| 6 | `turn-done` | the reason is `waiting` **or** the latch is set |
| 7 | `idle` | output under the floor, AND — orch pane: the roster says idle; other panes: never prompted, ever |
| 8 | `working` | everything else |

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
loomux's own doing and clears itself, and a finished turn is not something
anyone is blocked on.

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
- **No keyboard chord.** Out of scope for this slice — a new chord needs the
  `agent-cli-reference` sweep `doc/design/side-dock.md` describes, and the two
  buttons plus the tablist cover the gesture.

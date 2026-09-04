# Agents tab — the pane state model (#2122)

The Agents tab (#2122) and the pane Notes rows (#2116) both want to say, in one
word, what each pane is doing. This note is the contract they read: what a pane
projects (`Pane.facts()`), how that projection becomes a state
(`deriveAgentState`), and — the part that is a judgement rather than a
derivation — how far each harness can be trusted when the answer is
`turn-done`.

Slice A ships the foundation only: `src/paneactivity.ts`, `src/agentrows.ts`,
`Pane.key` / `Pane.facts()` / `Pane.noteRosterIdle()`, and the tests. There is
no UI in it. The view that renders these rows, the tab host, the spinner and the
badge are slice B and extend this note.

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

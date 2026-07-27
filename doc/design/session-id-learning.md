# Learning a session id loomux didn't mint (issue #440)

[session-restore.md](session-restore.md) covers the restore *policy*: given a
recorded `sessionId`, an agent pane auto-resumes; given none, it restores
**dormant** with a Start button. This note is about the other half — how that
`sessionId` gets recorded in the first place when loomux itself never minted
it, and why "dormant" was showing up for sessions that plainly existed.

## The bug

`launcher.ts` mints `--session-id <uuid>` and records it, but only for a
launcher-built claude launch (`!plan.isCustom && program === "claude"`). Any
other path to a live claude/copilot pane — most concretely, a **custom
command** the human typed themselves — produces a pane whose `sessionId` stays
`null` forever. The CLI mints its own id, writes a perfectly good transcript,
and loomux never learns it. Two more paths fed the same trap: `startFromDormant`
(clicking **Start** on a dormant card) dropped whatever id the fresh session
ended up with, and `restoreSession`'s plain-session path (a session restored by
hand from the Sessions sidebar) opened the pane holding the id and never
recorded it — so a session restored *by hand* came back dormant on the *next*
boot anyway. All three are the same defect (nothing records an id loomux
didn't mint) hitting three different entry points into a live pane.

## Design options considered

Three ways loomux could learn an id it didn't mint, in increasing order of
mechanism cost:

**A — read it off the command line, when it's already there.** A custom line
carrying `--session-id <id>` or `--resume <id>` *names its own session*. Per
the [CLI reference](https://code.claude.com/docs/en/cli-reference):

> `--session-id` — "Use a specific session ID for the conversation (must be a
> valid UUID)"
>
> `--resume` / `-r` — "Resume a specific session by ID or name..."
>
> `--fork-session` — "When resuming, create a new session ID instead of
> reusing the original (use with `--resume` or `--continue`)"

So a line is exact and free to read — **except** when `--fork-session` also
appears, in which case the id on the line is *not* the id the process ends up
with, and adopting it would record a wrong id (worse than none: a wrong id
silently resumes into the wrong transcript next boot instead of honestly
staying dormant). `panerestore.ts` already had an unguarded version of this
extractor (`sessionIdFromCommand`, used for orchestration capture, whose
backend-built command lines never carry `--fork-session`); this issue adds a
guarded sibling (`adoptableSessionId`) for the human-owned-command-line case,
refusing exactly the one input the CLI reference says would lie.

**Doesn't cover:** a bare `claude` custom line with no session flag at all —
there's nothing on the line to read.

**B — post-start reconciliation against `listSessions()`.** After a null-id
agent pane has been live and produced output, re-run the same background scan
the Sessions sidebar's own prefetch already performs and match by CLI + cwd +
"modified after this pane spawned." The issue's own suggested shape, and the
only option that covers the bare-custom-line case A can't. Heuristic by
nature — see **Ambiguity policy** below for how that's kept honest.

**C — a `SessionStart` hook.** Per the
[hooks reference](https://code.claude.com/docs/en/hooks), the hook's input
payload provably carries `session_id`, `transcript_path`, and `cwd` on every
firing (including `source: "startup"` for a brand-new session) — exact,
race-free, and would need no scan or heuristic at all. **Rejected as the
default** for what it would cost: loomux would have to either write to
`~/.claude/settings.json` / the project's `.claude/settings.json` (mutating
config the human owns, and firing for every claude session on the machine,
not just loomux's) or append `--settings <path>` to the human's command line —
the exact objection that already bars minting a session id onto a custom line
(#440's acceptance criteria: "rewrites a command line the human owns" is not
acceptable). It would also need a phone-home channel (the hook writes
somewhere; loomux watches it) that doesn't exist today, and a symmetric answer
for Copilot's own hooks system that hasn't been designed. **Recorded here as
the upgrade path** if B's heuristic proves too fuzzy in practice — the
docs prove it would work, the cost is in the plumbing, not the concept.

**D — parse the CLI's own startup output.** No documented contract: the CLI
reference describes session-id printing only for `--bg` background sessions,
not interactive mode. **Rejected outright** — nothing to build on.

### Chosen: A + B, layered

A is exact wherever it applies and costs nothing; B is the only thing that
covers a bare custom line. Both CLIs are covered symmetrically — the backend
scanner and the Sessions sidebar already treat Copilot sessions as resumable
(`sessions.rs` builds `copilot --resume <id>` the same as it does for claude),
so restricting learning to claude-only would leave the identical trap open for
Copilot and not actually close the issue.

## Where the timing is genuinely unverified

The docs are **silent** on exactly when a session's transcript file first
appears on disk relative to the process starting. This repo's own observed
behavior (#194 BUG-1: a `--resume` against a session that was *"never
prompted"* fails with "No conversation found...") is consistent with "no
transcript until at least one prompt" — but that is a labeled *observation*
against this codebase's own prior investigation, not a documented contract.
Option B's design does not depend on the exact instant: it's a lazy re-scan
triggered by the pane having received its first HUMAN INPUT (not merely
produced output — see **Review hardening, rounds 2–3** below for why that
distinction matters), retried on a slow interval rather than assumed to
succeed on the first pass — so nothing breaks if the transcript in fact
appears earlier or later than that observation suggests.

## Ambiguity policy: refuse, never guess

`sessionreconcile.ts`'s `planSessionAdoption` is the automatic half of option
B. Two same-CLI panes open on the same folder — a legitimate setup, e.g. two
customer claude sessions in one repo — can produce a session that matches more
than one pane, or a pane that matches more than one session. The two candidate
policies:

- **Refuse on any contested match** (chosen). A pane with more than one
  surviving candidate, or a session claimed by more than one pane, adopts
  nothing — for *any* pane the ambiguity touches, not just a coin flip between
  them.
- **Newest wins.** Simpler, but silently cross-wires a pane onto a
  conversation history that isn't its own, with no signal to the human that it
  happened — they'd only find out by reading something that isn't theirs.

Refusal was chosen because the failure modes are asymmetric: a refused match
costs nothing beyond staying exactly at today's status quo (a dormant pane,
which the D2 card below already gives a human-driven way out of), while a
wrong automatic match is a silent, undetectable data-integrity error. The
matcher (`test/sessionreconcile.test.ts`) pins this in both directions —
one session claimed by two panes refuses for both, and one pane with two
candidate sessions refuses for that pane — computed once across the whole
batch so no pane's position in the input list can change another pane's
outcome.

The `dormantResumeCandidate` lookup that feeds the **D2** card (below) is
*not* subject to this refusal: it surfaces the newest match to a **human**,
who clicks it or doesn't. Newest-wins there isn't a guess loomux is making on
the human's behalf — it's the literal meaning of "resume the most recent
session in this folder," read and confirmed by the person who clicked it.

## Review hardening, rounds 2–3 (B1/B2/B2-R/B3)

A first review pass (head `6c5a776`) confirmed the *contested*-match refusal
above is sound, then found three ways the matcher could still adopt a WRONG
session through *uncontested* inputs — i.e. paths that never reached the
refusal logic at all because nothing was there to contest against. All three
fixes move strictly in the refusal direction (narrower acceptance), never the
reverse — the same asymmetry argument that motivated refuse-over-guess above:
a missed adoption costs nothing the D2 card doesn't already cover; a wrong one
is silent and undetectable.

**B1 — no slack of any kind on the eligibility boundary.** The matcher
originally allowed a session modified up to 5 seconds before the pane's own
eligibility instant, reasoned as clock-skew tolerance. It isn't one: the
eligibility timestamp and a transcript's `modified_ms` are both read off the
same OS clock, at a point strictly before the CLI could have written that
transcript — so nothing legitimate ever lands earlier, and any slack can only
ever admit a *foreign, pre-existing* session into the candidate set. Concrete
failure the slack enabled: close a claude conversation in a folder, then open
a fresh custom `claude` pane in the same folder within the slack window — the
dead session becomes the fresh pane's sole, uncontested, silently-adopted
match. Fixed by removing the slack outright (not shrinking it — see
`sessionreconcile.ts`'s `planSessionAdoption` doc comment); the boundary is
now a strict `>=`.

**B2 — eligibility gated on first INPUT, not first output.** A claude/copilot
TUI produces output — its banner — within about a second of spawn, long
before any transcript exists (the "never prompted → no transcript" fact
above). Gating reconcile-candidacy on "has produced output" therefore left a
pane eligible for its ENTIRE idle-before-first-prompt lifetime — minutes to
hours — with provably no transcript of its own to be found, during which any
single unrelated same-CLI/same-cwd session modified after spawn (a sibling
terminal in that repo, a same-folder pane closed moments earlier) became a
sole, uncontested false match; refusal-on-contest cannot fire when there is
nothing to contest against. Fixed by gating on `Pane.firstInputAt` — the
timestamp of the human's first keystroke/paste into the CURRENT process
(`src/pane.ts`, set once per spawn) — instead of `hasReceivedOutput`. This
collapses the exposure from the pane's whole idle lifetime down to the
genuinely narrow window right after a prompt is sent, in which the pane's OWN
transcript is *also* usually about to appear (turning a same-window collision
CONTESTED, and therefore refused, rather than uncontested and silently
adopted).

**Accepted residual (named, not silently left implicit, per review):** a small
race remains even after B2 — a foreign session in the same folder could be
modified in the seconds between this pane's first prompt and its own
transcript first landing in a `listSessions()` scan, and if that foreign
session is the sole candidate at scan time, it is adopted. This is inherent to
option B as specced (a heuristic scan, not an exact per-process signal — see
option C above for the exact-but-costlier alternative); it is not eliminated,
only narrowed from "the pane's entire unprompted lifetime" to "a few seconds
around its first prompt." The periodic re-scan means an uncontested miss at
one pass can still self-correct... but an uncontested WRONG adoption at one
pass is not retried or revisited — same as any other adoption, it is treated
as settled. No slack/epsilon was added to narrow this further (matching B1's
reasoning: `firstInputAt` is recorded in the renderer before the input even
reaches the PTY, so the pane's own transcript can only be written strictly
after it — a tolerance window here would only ever widen acceptance, the
forbidden direction).

**B2-R (review round 3) — `onData` is not a human-input signal; `firstInputAt`
was sourced from it anyway.** B2's own implementation set `firstInputMs` from
`term.onData`, reasoning that "the pane already routes every keystroke"
through it. That's true, but incomplete: `onData` fires for **everything**
the terminal sends toward the PTY, not only human input — xterm's internal
`wasUserInput` flag gates only scroll-to-bottom and xterm's own private
`_onUserInput` event, never the public `onData` fired below it (verified
against the bundled `@xterm/xterm/lib/xterm.js`: `CoreService
.triggerDataEvent` always ends in an unconditional `this._onData.fire(e)`).
Data that reaches `onData` with no key pressed includes:

- **OSC 10/11/4 color-query replies and the `ESC[>q` version reply** — per
  #179's own investigation, exactly what GitHub Copilot sends at boot,
  and xterm answers automatically.
- **Primary/Secondary Device Attributes replies** (`CSI c` / `CSI >c`) —
  a capability query many TUIs issue at startup, CLI-agnostic.
- **Focus reports** (`ESC[I` / `ESC[O`) once focus reporting is enabled.

This is **the same mechanism as #179**, recurring one layer up: there, the
backend's box-occupancy tracker misread Copilot's boot queries as typed
content; here, B2's `firstInputMs` would do the same thing to the
reconciler's eligibility gate. For copilot specifically (and any claude build
that probes at boot), `firstInputMs` collapsed back to roughly spawn time —
silently reopening the exact uncontested-window B2 was built to close, for
the one CLI already on record for triggering it.

**Fix: `term.onKey` + the app's own paste sites, never `onData`, for this
specific purpose.** `onKey`'s event payload is `{key, domEvent}` (verified in
the bundled lib) — it can only fire from xterm's own keydown handler bound to
a REAL `KeyboardEvent`, which nothing auto-generated ever has. `Pane.ts` marks
`firstInputMs` from `term.onKey` and from both `this.term.paste(...)` call
sites (`pasteFromClipboard`, `pasteToTerminal` — loomux owns paste entirely,
#402, so these are the only two paste vectors) instead. `onData` still does
its original job (forwarding to the PTY) — it's just no longer read as a
human-input signal.

**Explicitly rejected: pattern-filtering `onData` for escape sequences.** A
denylist that tries to recognize "this looks like an auto-reply" against
`onData`'s stream would be a denylist against an OPEN set of possible query/
reply shapes — and missing one is exactly how #179 happened the first time.
Structural exclusion (a different event, not a filter on the same one) is the
only fix that doesn't reopen the same class of gap it's meant to close.

**Reuse check (asked explicitly, confirmed rather than assumed):** the
backend already has a "did a human actually type this" notion —
`classify_human_input` (`src-tauri/src/orchestration/mod.rs`) — built for
#179 itself, for the steering-box-occupancy tracker. It is NOT reused here,
and shouldn't be: it operates on raw bytes already written to the PTY and
classifies them by **pattern-matching and skipping known escape-sequence
shapes** (bracketed-paste markers, OSC/DCS query-reply bodies, CSI sequences)
— exactly the denylist-against-an-open-set approach just rejected above, and
it exists on that side of the fence because the backend has no access to
xterm's own event model at all, only the byte stream `write_pty` receives.
The frontend has something strictly better available — `onKey`'s structural
guarantee — so reaching for the backend's approach here would be trading a
precise, source-guaranteed signal for a heuristic one for no reason.

**Verified, not assumed — including where verification stopped.** Probed
directly against the installed `@xterm/headless` 6.0.0 (`test/xterm-
humaninput.test.ts`): a `CSI c` / `CSI >c` Device Attributes query written
into a terminal auto-replies via `onData` with zero `onKey` involvement —
`@xterm/headless` has no `onKey` at all (confirmed: absent from its
prototype), a DOM-keyboard-event-only feature that is simply unreachable
without a real `KeyboardEvent`, which is the exact property the fix relies
on. The copilot-specific OSC 10/11/4 shapes #179 names could **not** be
reproduced in this harness: `@xterm/headless` has no renderer/theme service,
so color-resolution queries silently produce no reply regardless of `theme`/
terminator options (checked empirically across several variations, not
assumed from a single failed attempt) — a real, different limitation of the
headless package from the "no onKey" one, and distinct from either being a
flaw in the fix itself. DA is used as the CLI-agnostic, verified stand-in for
the same underlying fact (an auto-reply reaches `onData` with no human
input); the copilot-specific shapes are covered instead by a hand-validation
item run against the real app, where xterm has an actual renderer to answer
color queries from.

**B3 — a `--fork-session` line must never acquire a LEARNED id either.** The
spawn-time guard (option A's `adoptableSessionId`) already refused to adopt an
id a fork-carrying line names for itself. But this PR's new attach points —
the reconciler and the D2 button — could each independently give such a pane
a DIFFERENT, freshly-learned id, which is just as wrong: per the CLI
reference, `--fork-session` mints a NEW id on every resume, so any id attached
to that line is stale on the pane's very next restart, and (D2 specifically)
the button would spawn `… --fork-session --resume <id>`, which forks to a
third id at click time — recording something that looks authoritative while
being wrong immediately. Two fix shapes were possible: strip `--fork-session`
from the rewritten command (making the resume/fresh commands deliberately
NOT fork, overriding what the line asked for), or exclude such panes/records
from ever acquiring a learned id at all, leaving them honestly
unrecorded/dormant-eligible. **Chosen: exclusion** — overriding the human's
explicit `--fork-session` intent by silently dropping the flag felt like a
second instance of the exact objection that already bars rewriting a command
line the human owns (option A's design above); leaving the pane dormant (with
its plain Start button, or nothing extra from D2) is the honest degradation,
consistent with every other refusal in this design. `Pane.hasForkSession`
(reused from `panerestore.ts`'s new `hasForkSession`, which — fixed in the
same round — scans BOTH `command` and `argv` rather than whichever is
non-empty) excludes such panes from `reconcileCandidates` in `main.ts`, and
the D2 enrichment checks the same predicate on the dormant record's
command/argv before ever offering the button.

## D2: the dormant card, when reconciliation comes up empty (or hasn't run yet)

A pane can still end up dormant with no recorded id — reconciliation hasn't
had a chance to run yet, or found more than one candidate and correctly
refused. Per the issue's D2 acceptance criterion, the dormant card doesn't
just give up: `dormantResumeCandidate` checks the pane's recorded cwd + CLI
against the same session list, and if there's a match, the card offers a
second button — **"Resume last session"** — alongside plain Start, naming the
match (CLI, title, age, folder) so the human is choosing it, not being
silently defaulted into it. A folder with no candidates keeps today's
Start-only wording.

This lookup runs **after** the card is already rendered, off the existing
background session-list prefetch (`sessions.ts`'s own boot-time `refresh()`)
— never gating the pane's own open on a fresh scan. See **Boot-path
boundary** below.

## Boot-path boundary (#342)

#342 deliberately made restore stop *waiting* on a `listSessions()`
precheck — that scan, over a machine's full Claude/Copilot session history,
can itself take seconds, and gating every restored pane's open on it defeated
the point of an optimistic restore. Nothing in this issue's fix reopens that:

- The reconciler's triggers are (a) chained off the *existing* sidebar
  prefetch — a promise that was already running in the background regardless
  of this issue — and (b) a periodic timer thereafter, both of which run
  **after** `restoreSessionTabs` has already returned and every pane is open.
  `restoreSessionTabs` itself never awaits the reconciler.
- The reconciler is a no-op (a cheap in-memory filter, no I/O) on every check
  where no null-id, prompted, non-forking agent pane exists — the ordinary
  case once a boot's panes have all resumed cleanly.
- The D2 lookup is resolved asynchronously per dormant card, strictly after
  `openActionPane` has already returned the live pane object to the grid.

## What's out of scope

- **Minting `--session-id` onto a custom command line.** Explicitly barred by
  the issue: it rewrites a line the human owns, the same objection that ruled
  out option C's `--settings` approach above.
- **Orchestration (`dormant-group`) restore.** Its session ids are backend-
  built and already parsed via the unguarded `sessionIdFromCommand`
  (`orchestration.ts`, #194.5) — a different, already-correct path this issue
  doesn't touch.
- **Any backend change.** `list_sessions` already returns every field this
  fix needs (`id`, `source`, `cwd`, `modified_ms`, `resume_command` —
  `src-tauri/src/sessions.rs`).

## Where the pieces live

| Concern | File | Notes |
| --- | --- | --- |
| Guarded line-extraction (option A) | `src/panerestore.ts` — `adoptableSessionId`, `hasForkSession` | `--fork-session`-aware sibling of the existing unguarded `sessionIdFromCommand`; `hasForkSession` scans both `command` and `argv`, shared by A's guard and B3's exclusion below. Unit-tested. |
| Learn-on-spawn fallback | `src/pane.ts` — `start()` / `respawnFresh()` | Falls back to `adoptableSessionId` when the caller doesn't already know the id. |
| First-input timestamp (B2, corrected B2-R) | `src/pane.ts` — `firstInputMs` / `firstInputAt` / `markFirstInput()`, called from `term.onKey` and both `term.paste()` sites — NEVER `term.onData` | Reset to null on every `start`/`respawnFresh`; survives a respawn's listener reuse. `test/xterm-humaninput.test.ts` pins the underlying xterm fact this depends on. |
| Fork exclusion (B3) | `src/pane.ts` — `hasForkSession` getter | Reads the live pane's own recorded command/argv via the shared helper above. |
| Post-start matcher (option B) + D2 lookup | `src/sessionreconcile.ts` | Pure, DOM/IPC-free — `planSessionAdoption` (refusal-biased, strict no-slack boundary) and `dormantResumeCandidate` (advisory). Unit-tested. |
| Reconciler wiring | `src/main.ts` — `reconcileSessionIds`, `reconcileCandidates` | Two triggers (prefetch-chained one-shot + periodic), single-flight, throttled, boot-path-safe; candidacy gated on `firstInputAt` + `!hasForkSession`. |
| D2 card | `src/main.ts` — the `dormant-agent` case of `openActionPane`, `addDormantCardAction` | Renders synchronously; enriches asynchronously; skips enrichment for a fork-carrying record. |
| D1c fix | `src/main.ts` — `restoreSession` | One-line: pass the id already in hand. |

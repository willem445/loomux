# Design: group cost tracking

Status: implemented (issue #42).

## Problem

The group lifecycle page (GroupView) showed inaccurate cost numbers, and
`group_usage` returned `$0.00` while workers were actively burning tokens. Three
root causes, all stemming from the original best-effort statusline scrape
(issue #8 / PR #21):

1. **Wrong source.** Cost was a regex parse of each pane's visible statusline.
   On subscription plans (Claude Max) the Claude Code statusline shows
   `Cost: $0.00` regardless of real usage — so the source itself is wrong for
   those accounts, and panes without a parsable figure were silently dropped.
2. **No durability.** Killed or recycled panes fell out of the total entirely;
   the group forgot all historical spend the moment an agent exited.
3. **Dollars only.** Even when a figure parsed, it was a dollar amount with no
   token context — and dollars are meaningless on plans with no marginal cost.

## Principles

1. **Tokens are the honest metric; dollars are an estimate.** Token counts are
   read exactly from the CLI's own records. Dollar cost is derived from a small,
   dated price table and clearly labelled "estimated". Max-plan accounts pay no
   marginal dollar cost, so tokens are what the UI leads with.
2. **Read the real record, fall back to scraping only as a last resort.**
   Per-message token usage from the session transcript is the primary source;
   the statusline parse survives only as a labelled fallback.
3. **Accumulate durably.** An agent's usage is snapshotted when it exits, so a
   recycled pane still counts toward the group's lifetime total.
4. **Live vs lifetime split.** The panel shows current burn (live agents) and
   total spend (everything ever in the group, including killed agents).

## Source of truth per CLI (and its limits)

### Claude Code — transcript token records (primary)

Claude Code writes one JSONL line per message to
`~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl`. Each `assistant`
message carries an exact `usage` object (`input_tokens`, `output_tokens`,
`cache_creation_input_tokens`, `cache_read_input_tokens`) and the `model` that
produced it. `usage::parse_claude_transcript` sums these, deduplicating by
message `id` (a resumed/replayed transcript re-emits lines), skipping
non-assistant lines and non-billable `<synthetic>` models. The POLL does not
re-run that sum over the whole file each tick — see the cursor contract below.

**Limits.** The transcript records tokens, not dollars — so dollar cost is
always our own estimate (see the price table). Tokens are exact regardless of
plan. Locating the file is a scan of the project folders for `<session>.jsonl`
(the cwd→folder encoding is not re-derived). The projects root is
`~/.claude/projects` by default; the registry exposes a per-instance override
(`set_claude_projects_dir`) so tests point at a fixture tree without touching
global state (safe under parallel execution).

### Copilot CLI — no readable token record today (fallback only)

Copilot keeps only `session-state/<id>/workspace.yaml`, which records no token
counts we can read. Copilot sessions therefore have no transcript usage source
and fall through to the statusline parse. If a future Copilot build writes a
usage record, add a `copilot_session_usage` reader in `usage.rs`; it slots in
ahead of the fallback with no other change.

### Statusline parse — last resort

`parse_session_cost` still scrapes the dollar figure a CLI prints in its own
statusline. It runs only when no transcript usage was found, and its figure is
labelled "reported" (the CLI's own number) rather than "estimated". It is
unreliable — empty on Max plans, gone once the pane is killed — which is exactly
why it is no longer the primary source.

## Price table

`usage::price_for` maps a model id (by family substring) to per-1M-token rates:
input, output, cache-write (5-minute-ephemeral rate, 1.25× input — Claude Code's
default breakpoint), and cache-read (0.1× input). Rates are dated in-file
(**2026-07-04**, from Anthropic's published pricing). Unknown models return
`None`, and the session shows tokens only — no invented dollar figure. To
update: change the numbers and the date; add a family with a new `contains`
branch.

## Durable accumulation (`orchestration`)

`UsageSnapshot` rows persist to `<group>/usage.json`, keyed by CLI session id
(or `agent:<id>` when there is none). Keying by session id is deliberate: a
resumed session updates one row instead of double-counting, since the transcript
is cumulative.

- **On every `group_usage`**, each live agent's snapshot is refreshed from its
  current transcript (or statusline). The durable store then holds live plus
  historical (killed) snapshots.
- **On `mark_dead`** (the single choke point for kill/exit), the agent's final
  usage is captured before teardown — the transcript is still readable after the
  pane dies, which is what makes recycled panes keep counting.
- **`upsert_usage_snapshot` never downgrades.** A transcript only grows, so a
  read that comes back empty (transient failure, or a Copilot pane that never
  wrote a token record) must not zero a session's captured spend; the merge
  keeps the richer data and just refreshes identity.
- **Crash-safe persistence.** Writes go to `usage.json.tmp` and are atomically
  renamed over `usage.json`, so a crash mid-write never leaves a half-written
  file. On load, a parse failure (corruption, manual edit) preserves the file
  as `usage.json.bad` and audits it, rather than silently treating it as empty
  and overwriting all killed-agent history on the next upsert.

`group_usage` returns `{ live_cost_usd, lifetime_cost_usd, live_cost_basis,
lifetime_cost_basis, live_tokens, lifetime_tokens, agents:[…] }`. Lifetime sums
all snapshots; live sums only currently-live agents. Each total's `*_cost_basis`
is `estimated` (all token-derived), `reported` (all CLI statusline), `mixed`, or
`null` — so a total that blends estimated and reported dollars is never hidden
under one label. Each agent row carries its token breakdown, `source`
(`transcript`/`statusline`/`none`), `model`, `cost_usd`, and an `estimated`
flag.

## Reading the transcript incrementally — the cursor contract (#1239)

The usage poll is the app's hottest path: `orch_group_usage` is asked for by
the group view, the tab bar and `orch_autonomy` inside the same tick, and
`USAGE_POLL_MAX_AGE` (1 s) is the floor on how often the three of them share
one computation. That computation reads *every live agent's* transcript.

Reading it whole, every time, is the defect #1239 names. On a multi-day
session the file is tens of MiB; the poll opened it, ran `serde_json` over
every line, and rebuilt the message-id dedupe set from scratch — to advance
four totals by whatever the agent wrote in the last second. #1218/#1237
bounded that read's *memory* (it streams, and no longer materializes a
`String` whose doubling `Vec` grow could abort the process); they deliberately
did not touch the work. The 08-21 minidump's 1,701,161,634 page faults are
this loop.

### What a cursor holds

`usage::TranscriptCursors` keeps one `TranscriptCursor` per `(projects root,
session id)`:

- the **byte offset** consumed so far, which always sits immediately after a
  `\n`;
- the **fold** resumed from there — `TranscriptFold`, holding the four token
  totals, the accrued cost, the best-priced model, and the message-id dedupe
  set;
- the **stat** it last acted on (`len`, mtime, and creation time where the
  platform reports one);
- a 64-byte **anchor**: the tail of the region already folded.

The dedupe set is the part that makes resuming safe rather than merely fast. A
`--resume` re-emits assistant lines that were already counted, and the set is
what stops them counting twice; carrying it across ticks is what lets the
fold continue instead of restarting.

### The per-tick decision

One `stat` — the same call that validates the remembered path — then:

| Observation | Action |
| --- | --- |
| the cursor has been folding for `CURSOR_REVALIDATE_AFTER` | reset, whatever everything below says |
| `len` and mtime both unchanged | serve the cursor's totals; the file is not opened |
| grew, or mtime moved forward | re-read the anchor and, if it matches, fold the appended **complete** lines on — through one handle |
| shorter than the last stat | reset: discard the cursor, re-parse from byte zero |
| creation time differs | reset — a different file at the same path |
| mtime moved **backwards** | reset — restored over from a copy, a sync, a checkout |
| anchor no longer matches | reset — the **last 64 bytes** of the consumed region were rewritten |

The **creation-time** and **backwards-mtime** rows are defence-in-depth over
the anchor and the length, and which of them actually fires is
platform-dependent — measured, not assumed. Removing the backwards-mtime arm
reddens its test on Linux and Windows but not on macOS, where APFS drags the
birth time back with the mtime so the creation-time arm covers the case
instead; removing the creation-time arm reddens its test on Linux and macOS but
not on Windows, where NTFS **file tunneling** restores the original birth time
for a same-name recreate inside a ~15 s window, leaving that arm inert. Neither
arm is evidenced on all three platforms, and the realistic rotation — to
*different* content — is caught by the anchor or the length regardless.

Every reset costs exactly what the old code cost on every tick. That is the
shape of the whole design: **each guard fails toward slow, never toward
wrong** — with the top row doing more work in that sentence than it looks,
for the reason the next section gives.

The design rests on Claude Code appending to a session's `.jsonl` and never
rewriting earlier lines. Be precise about what happens if that stops holding,
because an earlier draft of this note was not: the guards notice a
**replacement**, a **truncation**, a **rotation**, an mtime restored
**backwards**, and any rewrite that touches the last 64 bytes of the consumed
region. They do **not** notice an in-place edit further back on a file that
keeps being appended to. The revalidation timer, not the guards, is what
covers that case.

### Why an anchor as well as `len` and mtime

`len`+mtime cannot see an in-place rewrite that lands on the same length, and
on a coarse-clock host it cannot see one inside a single timestamp tick
either. Re-reading the 64 bytes immediately before the offset and comparing
them to what was folded there can: any edit to that region, or any
replacement that shifts the content, changes them. It is 64 bytes on a path
whose entire point is a work bound, and a transcript line is hundreds of bytes
at minimum, so the anchor never spans more than the tail of one record.

The anchor is read through the **same file handle** the fold then reads from,
and that is a correctness requirement rather than a saved syscall. Verifying
through a handle of its own would leave a window in which the file is replaced
between the proof and the read — the cursor would verify one file and resume
into another, which is the one way this design could produce a *wrong* total
rather than a slow tick. Reading the anchor also leaves the handle at exactly
the offset, so the check costs one seek and 64 bytes.

### What the anchor does not prove, and the timer that covers it

The anchor proves that the **last `ANCHOR_BYTES` of the consumed region** are
still what was folded there. It does not prove the consumed region is intact,
and the difference is not academic:

> Let the consumed region be `[0, O)`. Edit any byte below `O − 64` in place —
> one `input_tokens` value in an earlier line — while the agent goes on
> appending normally. `len` grew, the mtime moved forward, the creation time
> is unchanged, and the anchor window is untouched. Every guard agrees, the
> edited bytes are never re-read, and no later append re-decides anything.

No stat arm and no anchor will ever catch that. Left alone it is not "one poll
window of stale totals" — it is wrong for the life of the cursor.

So the cursor is discarded on a timer: `CURSOR_REVALIDATE_AFTER` (5 minutes)
puts a ceiling on how long any incremental fold may run before the transcript
is re-parsed from byte zero regardless of what every other signal says. That
is what makes "fails toward slow, never toward wrong" a true statement rather
than a nearly-true one — the error becomes bounded by the interval instead of
permanent. Against a 1 s poll the timer still leaves the incremental path
doing roughly 1/300th of the old work, so the guarantee costs almost none of
the win, and it is deliberately shorter than `CURSOR_TTL` so a cursor that
survives eviction has revalidated at least once in between.

**That 1/300th is an average, not a distribution.** Cursors are built when an
agent is first polled, so a group's cursors tend to expire in the same tick,
and `compute_group_usage` walks its live agents serially. One tick in every
~300 therefore costs what a pre-#1239 tick cost — for every live agent at once
— rather than spreading one agent's re-parse per tick. The mean is what the
amortized figure says; the worst tick is the old worst tick. That is acceptable
because the old cost was paid on *every* tick and the poll is already coalesced
behind the usage memo, but it is the first thing to look at if a periodic hitch
on the group view is ever reported, and a per-cursor phase offset is the fix if
it is.

Both halves are pinned, deliberately: `an_edit_below_the_anchor_window_is_not_detected_by_any_guard`
asserts the cursor really does disagree with a full re-parse by exactly the
edit, and `the_revalidation_timer_bounds_that_blind_spot` asserts the timer
brings it back. A note that discloses a residual while the suite pins only the
happy half is the mismatch this repo keeps catching.

### Why not just poll less often

The cheaper fix for a 1 Hz whole-file re-parse is to stop doing it at 1 Hz:
raise `USAGE_POLL_MAX_AGE`, or throttle the transcript read specifically. That
cuts the same churn by the throttle factor, adds no cross-tick state, and has
no blind spot to disclose at all.

It was not chosen because it trades the thing the poll exists for. Freshness on
the group view is a real product property — a human watching an agent burn
tokens is watching *this* number — and throttling degrades it linearly with the
saving: a 60× cut in work is a 60× cut in freshness. The cursor decouples the
two, buying the same reduction while keeping a 1 s answer, at the cost of the
state and the residual described above. If that residual ever proves
troublesome in practice, throttling is the fallback that needs no new
invariants — and shortening `CURSOR_REVALIDATE_AFTER` is the dial in between.

### A partial trailing line is never consumed

A JSONL writer appends the record and its newline as separate bytes, so a 1 Hz
poll lands between them routinely. The offset therefore only ever advances
past a `\n`; a trailing line without one is read and discarded, and re-read on
the next tick. Folding a torn record would not be "one tick early", it would
be permanently wrong — the truncated line either fails to parse (and is
skipped forever, losing that message's tokens) or, worse, parses with
truncated numbers. Holding back costs at most one poll window of freshness on
the newest message.

Two consequences worth naming:

- **The file reader consumes only newline-terminated records.** That includes
  `claude_session_usage_in`, the whole-file read, which shares the same
  `fold_appended` — so the incremental answer and the full-re-parse answer are
  the same function fed a different starting offset. The pure `&str` parser
  (`parse_claude_transcript`) keeps `str::lines()` semantics; it is a string
  parser with a different contract, and it is what fixture tests use.
- **A line that is not valid UTF-8 is skipped, not fatal.** The reader this
  replaced used `.lines().map_while(Result::ok)`, which *stops* at the first
  such line — one bad byte silently truncated a session's usage to whatever
  preceded it. A cursor could not hold that behaviour anyway: stalling at a
  line forever would freeze the offset there.

### Bounds and locking

Cursors are evicted on access after `CURSOR_TTL` (10 min unused), so the map
is bounded by transcripts being *polled* — the live agents, plus recently-dead
ones for a few minutes. There is no lifecycle event to hang eviction on: a
cursor deliberately outlives its agent's pane, because `mark_dead` reads usage
after teardown.

**The per-entry cost is the genuinely new fact, and it is a residency change
rather than a size one.** Each cursor holds its fold's message-id dedupe set,
roughly `n_assistant_messages × ~60 B`: a 32 MiB multi-day transcript at ~3 KB
a line is a few thousand ids (~300 KB), a very long-lived one tens of
thousands (~3 MB). That set is not new — the old code built and freed an
identical one on *every tick*. What changed is that it is now **held** per
polled transcript for up to `CURSOR_TTL` instead of churned once a second, so
single-digit MB is resident across a busy group where before it was
single-digit MB allocated and freed every second. That churn is precisely what
the page-fault count in #1239 was measuring, so the trade is the point of the
change rather than a cost of it — but the resident figure is the one to look
at first if memory, not CPU, is ever the complaint.

`tests/usage_memory.rs` (#1218) pins peak live heap on the whole-file reader,
whose fold is dropped when it returns. The cursor's fold is not dropped, by
design, so that test does not bound it and no test does; the arithmetic above
is the bound.

The map holds `Arc<Mutex<..>>` per transcript, following the same map-lock →
release → leaf-lock rule as the usage memo: the outer lock is held only long
enough to clone one cell out, so one agent's full re-parse never blocks
another agent's tick.

The resolved transcript path lives in the cursor too. `claude_transcript_path`
scans every project folder under the root, and doing that once a second per
live agent is the same class of waste as the re-parse; it is re-validated by
the tick's single `stat` — the same `metadata` call that answers `len`, mtime
and creation time — and re-scanned when that comes back missing or not a file. The scan returns
whichever project folder matches first — directory order, already arbitrary
when it ran every tick — so pinning it makes that choice stable rather than
stable-by-luck.

## UI (GroupView)

The panel leads with tokens (`… tok`) and shows the dollar estimate with a `~`
and an `est`/`reported` marker, so a `$0.00` Max-plan figure is never mistaken
for "no usage". A lifetime line (survives kills) sits above a dimmer live line
(current burn). Per-agent rows show tokens plus the labelled cost, with a
tooltip giving the source, model, and full token breakdown.

## Testing

- `usage.rs` unit tests parse synthetic transcripts: token summing + per-model
  pricing, message-id dedup, skipping non-assistant/synthetic/malformed lines,
  unknown-model → token-only, empty transcript.
- Integration tests (`tests/orchestration.rs`): a killed agent stays in the
  lifetime total but drops out of live (with the no-downgrade merge);
  `mark_dead` captures usage from a fixture transcript (via the registry
  override) with no prior `group_usage` call; the durable write is atomic and
  leaves no temp file; a corrupt `usage.json` is preserved as `.bad` rather than
  wiped; and a total blending estimated and reported dollars is labelled
  `mixed`. No test ever spawns a real agent CLI.
- `tests/usage_cursor.rs` pins the cursor contract (#1239), including every row
  of the decision table above and BOTH halves of the residual — the guards
  missing an edit below the anchor window, and the timer bounding it. Appended lines fold
  onto the cursor and reach the full-re-parse answer (including a re-emitted
  message id, which must not count twice, and a model switch, which must
  re-decide the priced model); a tick reads only the appended bytes plus the
  anchor, off a file orders of magnitude larger; an unchanged transcript is
  not opened at all; a truncation and a **same-length** rewrite each reset the
  cursor; and a complete-but-unterminated trailing record is folded exactly
  once, when its newline arrives — not before. The work bound is asserted on
  `CursorWork::bytes_read`, which the reader increments at the single place
  bytes leave the disk, because the bound is invisible in the totals: the old
  whole-file re-parse produced identical numbers.
- `tests/usage_memory.rs` (#1218) still pins peak live heap against a ~16 MiB
  transcript. The polled path is now the cursor, but both go through the one
  streaming reader (`fold_appended`), so the property is shared by
  construction.

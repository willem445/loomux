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

One `stat`, then:

| Observation | Action |
| --- | --- |
| `len` and mtime both unchanged | serve the cursor's totals; the file is not opened |
| grew, or mtime moved forward | verify the anchor, seek to the offset, fold the appended **complete** lines on |
| shorter than the last stat | reset: discard the cursor, re-parse from byte zero |
| creation time differs | reset — a different file at the same path |
| mtime moved **backwards** | reset — restored over from a copy, a sync, a checkout |
| anchor no longer matches | reset — the consumed region was rewritten under us |

Every reset costs exactly what the old code cost on every tick. That is the
shape of the whole design: **each guard fails toward slow, never toward
wrong.** It rests on Claude Code appending to a session's `.jsonl` and never
rewriting earlier lines; if that ever stops holding, these guards are what
notice, and the fallback is the pre-#1239 full re-parse.

### Why an anchor as well as `len` and mtime

`len`+mtime cannot see an in-place rewrite that lands on the same length, and
on a coarse-clock host it cannot see one inside a single timestamp tick
either. Re-reading the 64 bytes immediately before the offset and comparing
them to what was folded there can: any edit to that region, or any
replacement that shifts the content, changes them. It is 64 bytes on a path
whose entire point is a work bound, and a transcript line is hundreds of bytes
at minimum, so the anchor never spans more than the tail of one record.

The residual blind spot, stated rather than hidden: a rewrite landing on the
same length **and** the same mtime **and** leaving those 64 bytes identical.
The file would have to be edited in place inside one filesystem timestamp
tick to produce it, and the consequence is one poll window of stale totals —
the next append moves the mtime and the anchor decides again.

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

The map holds `Arc<Mutex<..>>` per transcript, following the same map-lock →
release → leaf-lock rule as the usage memo: the outer lock is held only long
enough to clone one cell out, so one agent's full re-parse never blocks
another agent's tick.

The resolved transcript path lives in the cursor too. `claude_transcript_path`
scans every project folder under the root, and doing that once a second per
live agent is the same class of waste as the re-parse; it is re-validated with
one `is_file()` per tick and re-scanned when that fails. The scan returns
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
- `tests/usage_cursor.rs` pins the cursor contract (#1239): appended lines fold
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

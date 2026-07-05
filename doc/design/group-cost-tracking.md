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
non-assistant lines and non-billable `<synthetic>` models.

**Limits.** The transcript records tokens, not dollars — so dollar cost is
always our own estimate (see the price table). Tokens are exact regardless of
plan. Locating the file is a scan of the project folders for `<session>.jsonl`
(the cwd→folder encoding is not re-derived). `LOOMUX_CLAUDE_PROJECTS_DIR`
overrides the root for tests.

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

`group_usage` returns `{ live_cost_usd, lifetime_cost_usd, live_tokens,
lifetime_tokens, estimated, agents:[…] }`. Lifetime sums all snapshots; live
sums only currently-live agents. Each agent row carries its token breakdown,
`source` (`transcript`/`statusline`/`none`), `model`, `cost_usd`, and an
`estimated` flag.

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
  lifetime total but drops out of live (with the no-downgrade merge), and
  `mark_dead` captures usage from a fixture transcript with no prior
  `group_usage` call. No test ever spawns a real agent CLI.

# Design: agent usage-limit chips

Status: implemented (issue #80).

## Problem

The bottom toolbar shows live system resources (CPU / MEM / GPU / VRAM) but not
how close each agent CLI is to *its own* usage ceiling. When a Claude Code
session or weekly allowance is nearly spent, a run can stall mid-task with no
warning in loomux. Issue #80 asks to surface each CLI's limit consumption as a
compact chip next to the resource meters — honestly, with no fabricated numbers,
consistent with the estimated-vs-reported discipline of the #42 cost work.

## What is actually available (source investigation)

Limits, unlike token usage (#42), are **not** written to any local file we can
read. This was verified, not assumed:

### Claude Code — statusline only

- `~/.claude/stats-cache.json` records per-day/per-model token counts and
  `costUSD: 0` (subscription), but **no** session or weekly limit percentage.
- `~/.claude/settings.json` carries the statusline config, not any limit state.
- Session transcripts (`~/.claude/projects/**/<uuid>.jsonl`) carry exact
  `usage` token objects but no limit/allowance field (`appliedLimit` in tool
  results is an output-truncation line count, unrelated).

The only place a limit percentage surfaces is what the CLI renders in its own
**statusline** — Claude Code's built-in limit widget, or a third-party one such
as `ccstatusline` ("Session: N%", "Week: N%"). So loomux scrapes it best-effort
from the ANSI-stripped pane tail — the same last-resort channel
`parse_session_cost` uses for the dollar figure (#42).

### Copilot — no local allowance (shows nothing)

Copilot's local state (`~/.copilot/session-state/<id>/`) was inspected too:

- `events.jsonl` carries a shutdown event with `totalPremiumRequests` — a
  per-session count of premium requests *consumed*, and only flushed when the
  session ends.
- `vscode.requests.metadata.json` carries a per-request `creditsUsed` in an
  opaque micro-unit.

Both are **consumption**, not the account's premium-request **allowance** — the
denominator a "% of limit" needs. That number is server-side only. Showing a raw
count with no ceiling would be exactly the fabricated figure #42 is careful to
avoid, so loomux shows **nothing** for Copilot until a local allowance source
exists. (If a future build writes the allowance locally, add a reader in
`usage.rs` and a `copilot` branch to the event payload; the frontend chip
already renders per-CLI.)

## Parsing & aggregation (`usage.rs`, pure)

- `parse_claude_limits(text) -> Vec<LimitReading>` scans the ANSI-stripped tail
  bottom-up (the freshest render is the lowest line) and returns at most one
  reading per scope (`Session`, `Weekly`). Two anchors keep it from fabricating a
  figure out of ordinary terminal prose — the same false-match discipline the
  #40/#44 rounds applied to the attention scan:
  1. **Statusline region only.** Just the last few non-empty lines
     (`STATUSLINE_TAIL_LINES`) are scanned, not the whole 4 KB scrollback, so a
     paragraph mentioning a scope word higher up is never reached. (Not the
     literal last line: Claude Code's input-box border rows can sit just below
     the statusline.)
  2. **Label→percent adjacency.** A percentage counts only when its scope
     keyword *directly labels* it — `label: NN%`, separator characters only and
     at most `LIMIT_ADJACENCY_GAP` of them between. This rejects
     "the session is now 90% faster" (words between label and %) and, because
     percent-*before*-label is not accepted, "dropped 12% week over week" (a %
     that precedes the word). Statuslines render the label first; "12% week" is
     overwhelmingly prose.

  Keywords are conservative — only `session` and `week`/`weekly` — and a `NN%`
  token must be ≤ 100 to count. A bare `Context: 45%` (no scope keyword) is
  ignored; a combined `Session 34% · Week 12%` still parses both.
- `aggregate_claude_limits(&[Vec<LimitReading>]) -> ClaudeLimits` folds every
  live pane's readings to the **most-constrained** (highest consumed) value per
  scope. All panes share one signed-in account, so the max is the "closest to a
  cutoff" figure and tolerates a pane whose statusline lags *upward* — a stale,
  lower reading never wins.

  **Idle-pane staleness (trade, deliberate).** The opposite skew is real: right
  after a limit resets, a live-but-idle pane whose statusline hasn't re-rendered
  can pin a stale-*high* max for a scan or two. We accept it and label the figure
  honestly (a pane-statusline scrape, in the tooltip/note and the design here)
  rather than filter panes by recent agent output: statuslines self-refresh on
  their own timer even while the agent is idle, so "no recent output" ≠ "stale
  reading", and dropping such panes would discard valid current readings. If a
  cheap per-pane statusline-render timestamp ever exists, revisit.
- `ClaudeLimits::most_constrained()` picks the single number a chip shows
  (higher of session/weekly).

## Wiring (`orchestration`)

No new poll loop. The existing attention scan (`run_attention`, on its timer)
already gathers each agent pane's ANSI-stripped statusline tail for prompt
detection. `claude_usage_limits` reuses those same tails: for each live pane
whose per-role CLI is `claude`, it parses the tail and aggregates. The result is
emitted on a new `orch-usage-limits` event alongside `orch-attention`, so it
refreshes at the same cadence with no extra pty reads.

`usage_limits_payload` builds the event JSON and is a pure static fn so its
honesty labelling is unit-testable:

```json
{
  "claude": { "session_pct": 34, "weekly_pct": 12, "source": "statusline" },
  "copilot": null,
  "note": "…scraped from each live pane's statusline…reported by the CLI, not estimated. Copilot exposes no local allowance…"
}
```

`claude` is `null` when no live pane exposed a readout (default Claude config
without a limit widget) — the frontend then shows an `n/a` chip whose tooltip
explains why. `copilot` is always `null`.

## UI (statusbar)

A `CC` chip joins the resource meters in the footer, same label + fill-bar +
value shape. The pure `usagechip.usageChipView(UsageLimits)` decides its state:
the most-constrained percentage drives the bar (green→red as consumption rises,
like the other meters), and the tooltip carries the full session + weekly
breakdown plus provenance — "Source: live pane statusline, re-scraped each scan;
reported by the CLI, not estimated … an idle pane can briefly lag a limit reset
until it re-renders." With no data the chip greys to `n/a` and
the tooltip notes that Claude surfaces the figure only via a limit statusline
widget, and that Copilot has no local allowance. The event only fires while a
group is live, so the chip sits at `n/a` when no orchestration is running.

## Testing

- `usage.rs` unit tests cover the pure parse/aggregation: a session percentage;
  both scopes on one line; a realistic multi-segment (model | branch | limits |
  cost) statusline; the `weekly` label variant; **prose false-match negatives**
  (rev-18's "the login session is now 90% faster" and "dropped 12% week over
  week", plus two more of that class); the statusline-region bound (prose deep in
  scrollback is unreached, the bottom statusline wins); percentages without a
  scope keyword ignored; freshest-render-wins on a repeated scope;
  out-of-range/bare-number rejection; empty/unrelated text; most-constrained
  aggregation across panes; and the empty-aggregate and single-scope cases.
- `orchestration` unit tests (`usage_limits_payload_tests`) pin the event
  payload: empty limits → `claude` null and `copilot` null; present limits
  labelled `source: statusline` (never estimated); a single present scope still
  renders `claude`.
- Frontend tests (`test/usagechip.test.ts`) pin the chip view: `n/a` fallback
  with its explanatory tooltip, most-constrained selection (session vs weekly,
  either winning), a single present scope, and the "reported by the CLI, not
  estimated" honesty wording.

No test spawns a real agent CLI — parsing is exercised against fixture text.

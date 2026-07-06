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
  reading per scope (`Session`, `Weekly`). Each percentage is bound to the
  *nearest* scope keyword on its own line, so a combined `Session 34% · Week 12%`
  parses both and a bare `Context: 45%` (no scope keyword) is ignored rather than
  mistaken for a limit. Keywords are conservative — only `session` and
  `week`/`weekly` — and a `NN%` token must be ≤ 100 to count.
- `aggregate_claude_limits(&[Vec<LimitReading>]) -> ClaudeLimits` folds every
  live pane's readings to the **most-constrained** (highest consumed) value per
  scope. All panes share one signed-in account, so the max is the honest
  "closest to a cutoff" figure and tolerates a pane whose statusline has not
  refreshed yet — a stale, lower reading never wins.
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
breakdown plus provenance — "Source: live pane statusline, refreshed each scan;
reported by the CLI, not estimated." With no data the chip greys to `n/a` and
the tooltip notes that Claude surfaces the figure only via a limit statusline
widget, and that Copilot has no local allowance. The event only fires while a
group is live, so the chip sits at `n/a` when no orchestration is running.

## Testing

- `usage.rs` unit tests cover the pure parse/aggregation: a session percentage;
  both scopes on one line bound by nearest keyword; the `weekly` variant and a
  percent that precedes its label; percentages without a scope keyword ignored;
  freshest-render-wins on a repeated scope; out-of-range/bare-number rejection;
  empty/unrelated text; most-constrained aggregation across panes; and the
  empty-aggregate and single-scope cases.
- `orchestration` unit tests (`usage_limits_payload_tests`) pin the event
  payload: empty limits → `claude` null and `copilot` null; present limits
  labelled `source: statusline` (never estimated); a single present scope still
  renders `claude`.
- Frontend tests (`test/usagechip.test.ts`) pin the chip view: `n/a` fallback
  with its explanatory tooltip, most-constrained selection (session vs weekly,
  either winning), a single present scope, and the "reported by the CLI, not
  estimated" honesty wording.

No test spawns a real agent CLI — parsing is exercised against fixture text.

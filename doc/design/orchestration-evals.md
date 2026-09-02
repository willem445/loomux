# Orchestration evals — the per-PR scorecard

**Scope.** This note is the specification for `scripts/orch-scorecard.cjs`: what
each counter counts, written as the audit/transcript **shape** it reads, so that two
readers derive the same number from the same rows. It covers the narrow slice #2011
B1 needs — benchmarking the engine-owned review driver (#1778) by measuring, per pull
request, how much orchestrator attention and how many tokens the review loop cost,
and the orchestrator's **share** of those tokens.

It is deliberately not the whole of #2011. The axes table, the three eval classes and
the regression gate live in the plan on that issue; this note carries only what the
script implements, plus the two things a specification owes a reader who did not
write it: the attribution rules **with their gaps named**, and a section on what the
number cannot say.

**Retirement.** `scripts/orch-scorecard.cjs` is **deleted** in #2011 S3, once an
engine module (`crates/loomux-engine`, surfaced as the `group_metrics` MCP tool)
reproduces this output byte-for-byte on the fixture corpus. Until then the script is
the only reader and this note is its spec: a counter changes here first. When the
script goes, this note stays and re-points at the engine module — the definitions
below are the durable half.

---

## 1. Inputs

Four files, all of which the app already writes. Nothing here is a new row, a new
field, or a new gate.

| Input | What it gives | Where |
| --- | --- | --- |
| `audit.jsonl` (+ rotated `audit.N.jsonl`) | every row below | `<group>/` |
| `usage.json` | tokens and cost per CLI session, with `agent_id` | `<group>/` |
| `agents.json` | `id → role, block, session, task` | `<group>/` |
| the orchestrator's CLI transcript | per-turn token usage with a timestamp | `~/.claude/projects/<slug>/<session>.jsonl` |
| `--pr-meta` (optional) | `merged_at`, `build`, `issue` per PR | supplied by the caller from `gh` |

**Why the transcript is required for the orchestrator's tokens and `usage.json` is
not enough.** `usage.json` is a durable *snapshot* store keyed by CLI session:
`group-cost-tracking.md` states the reason — "a resumed session updates one row
instead of double-counting, since the transcript is cumulative". A row therefore
carries one lifetime total per session and there is no time series in it, so no
window can be cut from it. The orchestrator's session spans the whole benchmark
(both the hand-routed and the driven rounds are one lineage), so its `usage.json`
row is a single number covering every PR at once. The transcript is the only
per-turn record.

**What the script reads from the transcript, and nothing else:** `timestamp` and
`message.usage`. Each line is projected to those two fields at parse time and the
rest is dropped before anything is retained, so no transcript prose is ever held in
memory, printed, or written to a fixture. The tests run against a six-line synthetic
transcript written by hand.

---

## 2. Windows

Two windows per PR, because two different questions are being asked and they do not
have the same answer.

| Window | Start | End | Used for |
| --- | --- | --- | --- |
| **loop** | the first `rd-*` row with `detail.pr == N` | the last such row | `span_h`, loop notices |
| **pr** | the first audit row **naming** the PR (§3) | `merged_at`, else the last `rd-*` row, else the last naming row | wakes, orchestrator tokens |

The **driver counters and the review rounds sit in neither** — they are matched
structurally on `detail.pr` and counted wherever the row lands, which §4.3, §4.4 and
§5 all state and which `rows_counted_outside_pr_window` reports on. Only the four
counters named in the table above are window-gated.

Both take a **+10 minute tail** for membership tests (`--tail-min`, default 10) —
the rule #1778's S5 table used, and for the same reason: a merge or CI notice the
orchestrator reads after the driver has stopped writing still belongs to that loop.
`span_h` is the **untailed** span, so it is the honest duration of the thing measured.

A hand-routed PR has **no loop window**; its loop counters are zero and its
`span_h` is `null`. That asymmetry is the point — every other counter in this note
reads a row that predates the driver, which is what makes a before/after comparison
possible at all.

The `end_source` field says which arm produced the end, and it matters: without
`merged_at` the last-naming-row fallback is **days late**, because a merged PR keeps
being cited in the log. Measured on this group's own log, every one of the eleven
benchmark PRs was still being named 1.9 to 90 hours after it merged. Always pass
`--pr-meta` for a benchmark; the fallback exists so the script still answers on a
PR nobody has merge data for, and it declares itself when it fires.

---

## 3. "Names the PR"

A row names PR *N* when either:

1. `detail.pr === N` — structural, and what every `rd-*` and `review-verdict` row
   carries; or
2. the serialized `detail` matches `/#N(?![0-9])/` — the token join, with a
   trailing-digit guard so `#175` cannot match inside `#1751`.

Issues and pull requests share one number space on GitHub, so `#N` is unambiguous
once *N* is known to be a PR. The token join is heuristic **H1** (§6).

---

## 4. Counters

Every counter below is defined by the rows it reads. A counter with no matching row
reads zero, never absent — a table cell is never blank for a counter that simply did
not fire.

### 4.1 Orchestrator wakes

A **wake** is a `prompt` row whose `detail.to` is an agent whose `agents.json` role
is `orchestrator`. One paste is one wake. Its **kind** is decided by the leading
shape of `detail.text`, first match wins, in this order:

| Kind | Shape |
| --- | --- |
| `delegate-progress` | `^\[orrerix\] \S+ reports progress\b` |
| `delegate-done` | `^\[orrerix\] \S+ reports done\b` |
| `reviewer-report` | `^\[orrerix\] \S+ reports (approved\|request_changes)\b` |
| `delegate-blocked` | `^\[orrerix\] (\S+ reports blocked\b\|message from \S)` |
| `verdict-notice` | `^\[orrerix\] \S+ \([^)]+\) recorded verdict\b` |
| `system-notice` | anything else beginning `[orrerix]` |
| `other` | no `[orrerix]` prefix — in practice a human typing into the pane |

The order is the tie-break, and both ties are load-bearing: a reviewer's
`reports approved` is a **reviewer report**, not a generic delegate report, and a
verdict echo is a **verdict notice**, not a system notice. `system-notice` and `other` are defined
by exclusion on purpose — a new orrerix notice shape lands in `system-notice` without
silently joining a report class, and a new human phrasing lands in `other`.

A wake counts toward a PR when it names that PR (§3) **and** falls inside the PR
window.

### 4.2 Loop notices

Two numbers, and the difference between them is a correction to #1778's S5 table.

- **`loop_notices`** — an `[orrerix]`-prefixed wake naming the PR inside the **loop**
  window. This is orchestrator attention the loop actually cost.
- **`loop_notices_any_pane_s5`** — the same filter without the "delivered to an
  orchestrator pane" test. This is what S5's "orch notices" column counted, reproduced
  so that table stays checkable.

They differ by 1–2 per PR on all six driven PRs, and the difference is entirely the
driver's own resume prompts typed into a **worker** pane — which is precisely the
traffic the driver moved *off* the orchestrator. The S5 column therefore overstates
orchestrator involvement, and it overstates it in the direction that makes the driver
look worse. See §7.

### 4.3 Review rounds

`review-verdict` rows with `detail.pr == N`, bucketed by `detail.block` then
`detail.verdict` (lower-cased). `rounds` is the total. Matched **structurally and
counted wherever the row lands**, like the driver counters above — a verdict recorded
after the merge is still a round the PR cost. This counter is driver-blind: it reads
the same row for a hand-routed PR and a driven one.

### 4.4 Driver counters

`rd-*` rows with `detail.pr == N`, one bucket per action. These are matched
**structurally and counted wherever the row lands** — NOT gated on either window, so a
re-drive after the merge still counts; §5's `rows_counted_outside_pr_window` is what
reports the spill. (The loop window is derived FROM these rows, so in practice every
one of them is inside it; the `--pr-meta` merge time is what a late row can fall past.)

| Field | Row | Notes |
| --- | --- | --- |
| `drives` | `rd-started` | more than one means a re-drive after a cancel |
| `lane_spawns` | `rd-lane-spawned` | |
| `hand_backs` | `rd-handback` | |
| `refused` + `refused_by_reason` | `rd-refused` | keyed on `detail.reason` |
| `held` + `held_by_reason` | `rd-held` | keyed on `detail.reason` |
| `cancelled`, `consumed`, `satisfied`, `ci_green`, `resumed`, `pruned` | the matching `rd-*` | |

Any `rd-*` action not in that list is still counted, under its own action name, so a
new row in the driver's vocabulary (`review-driver.md` §5.4) cannot go missing —
it appears in the card rather than being dropped.

### 4.5 Orchestrator tokens

The sum of `input`, `cache_read`, `cache_creation` and `output` over every deduped
transcript **turn** whose `timestamp` falls inside the PR window.

**Dedup is not optional.** A Claude Code transcript writes an `assistant` line per
content block, so one API response appears two or more times carrying the same
`message.id` and the same usage object. The ratio on the orchestrator's own
transcript is close to 2:1 (30,842 assistant lines to 15,087 distinct message ids,
measured 2026-09-02) — summing the lines would report roughly double. That file is
live and grows, so re-derive the ratio rather than quoting this one. The rule: one turn per `message.id`, keeping the
occurrence with the **largest** `output_tokens`, because an earlier line was written
mid-stream and under-reports it. A line with no `message.id` is counted once and
declared in coverage as `usage_rows_without_id`.

Two figures are reported:

- **`tokens_window`** — the raw window sum. The orchestrator serves several PRs at
  once, so this is an **upper bound** for any one of them.
- **`tokens_attributed`** — the same sum scaled by this PR's share of the
  orchestrator wakes in the same window (`pr_wakes / window_wakes`). Heuristic **H5**.

Neither is "the tokens this PR cost". The raw figure over-counts by whatever else was
in flight; the apportioned figure assumes a wake costs the same whichever PR it is
about. Both are reported so a reader can see the spread, and `wake_share` carries
both operands so the apportionment is checkable rather than trusted.

### 4.6 Delegate tokens

A `usage.json` row is keyed by **CLI session**, not by agent. `group-cost-tracking.md`
says why ("keyed by CLI session id … a resumed session updates one row instead of
double-counting"), and the consequence has to be handled here: when a pane's session is
carried to a new agent id, that one row's `agent_id` names only the **last** occupant,
and every earlier agent on the session has no row of its own. Joining on `agent_id`
would report an agent that demonstrably spent tokens as having spent zero while
crediting its successor with the whole lineage — and on this group's store that is not
a corner case: **514 of 1356 rows sit on a session shared by more than one agent id,
carrying 31.2 G of 44.1 G tokens**.

So the row is looked up by the agent's **session** and split evenly across every agent
that occupied it (heuristic **H8**), then weighted by `1/k` where the agent is
attributed to *k* PRs (**H4**). The session split is self-correcting in the common
case — a pane reused by a worker's own successive agent ids has every occupant
attributed to the same PR, so the halves re-sum to the whole row — and gives a
fraction rather than all-or-nothing where only some occupants are attributed. An
`agent_id` lookup remains as a **fallback** for a row with no session key, and
`usage_key` per delegate says which index answered.

Each delegate reports `tokens` (the whole row its session carries) beside
`tokens_credited` (what this PR actually got after both weights), so the delegate
total is checkable by hand. `has_usage_row: false` means neither index had anything
for that agent: it contributes **0**, which makes the delegate side an
under-count and the orchestrator share correspondingly an over-estimate. The coverage
block is where a reader sees how often that happens.

An agent whose `agents.json` role is `orchestrator` is **excluded** — its spend is
already in §4.5 and counting it here would double it.

`usage.json` is cumulative per session and cannot be windowed (§1), so a delegate
reports its whole life against the PR it is attributed to. That is heuristic **H6**,
and it is a good approximation here only because delegates are spawned per task: a
worker or reviewer pane in this group serves one PR and is killed.

### 4.7 Orchestrator share

`orch / (orch + delegates)`, reported twice — once from `tokens_window`
(`orchestrator_pct_raw`) and once from `tokens_attributed`
(`orchestrator_pct_attributed`). Both are `null` when the denominator is zero, never
`0` — "no data" and "the orchestrator spent nothing" are different facts.

---

## 5. Attribution: agent → PR

Two tiers, and which tier carried an agent is reported per agent, because that
difference is exactly #2011 B2's scope.

**Structural.** The row itself carries both the agent and the PR:

- `rd-lane-spawned` and `rd-handback` — `detail.agent` + `detail.pr`;
- `review-verdict` — `actor` + `detail.pr`.

**Text.** An `agent-spawn` row **inside the PR window** whose serialized `detail`
(brief, name, branch) names `#N`. Heuristics **H2** and **H3**.

Structural wins: once an agent has a structural attribution, no text row can add a
PR to it. The window bound on the text tier is what stops a later brief citing an old
PR from claiming its tokens — without it, this very slice's own worker brief (which
names all eleven benchmark PRs) would be attributed to all eleven.

**Known gaps, all reported in the coverage block rather than swallowed:**

- `agents_unattributed_spawned_in_window` — an agent spawned inside some PR's window
  that no row ties to any PR. Its tokens are counted nowhere.
- `agents_split_across_prs` — an agent attributed to more than one PR, with the list.
  Each gets `1/k` (**H4**), which is a guess about how the agent's time divided.
- `rows_unclassified_in_window` — rows naming the PR inside its window that no
  counter consumed, grouped by action. This is the "did the scorecard see everything"
  check; it is expected to be non-empty (an `agent-spawn` is named but not counted).
- `has_usage_row: false` on a delegate — neither the session index nor the `agent_id`
  fallback had a row for it, so it contributes 0 and the delegate side is an
  under-count. On the eleven benchmark PRs it is **0 of 94** delegate entries: every
  one resolves through the session index. Under the `agent_id` join this replaced it
  was **28 of 94**, which is how that defect was found — so read a non-zero here as a
  reason to distrust the delegate totals, not as noise.
- `usage_sessions_shared_by_more_than_one_agent` — how many sessions the H8 split had
  to divide, and `usage_rows_unusable` — rows with neither a session key nor an
  `agent_id`, which are skipped.
- `rows_counted_outside_pr_window` — the reverse direction. A `review-verdict` or
  `rd-*` row is matched **structurally** on `detail.pr`, so it counts wherever it
  occurs: a late verdict or a re-drive after the merge is still a round the PR cost,
  and excluding it would silently under-count. That makes those two counters wider
  than the PR window, so the number of rows that fell outside it is reported. It is
  **0 on all eleven benchmark PRs**, so the widening is latent there rather than live.

---

## 6. The heuristics list

The script emits this list in every run, under `coverage.heuristics`, and it **is**
#2011 B2's scope — one row per place where a structural field would replace a guess.

| id | The guess | The structural fix |
| --- | --- | --- |
| H1 | A PR is joined to a row by the `#N` token wherever `detail.pr` is absent. | A `pr` field on `prompt` / `delivery-queued` rows. |
| H2 | An agent is joined to a PR by the `#N` token in its `agent-spawn` detail. | A `pr` field on `agent-spawn`. |
| H3 | A text-tier join counts only inside the PR window. | Same as H2 — the bound exists only because the token is ambiguous. |
| H4 | An agent attributed to *k* PRs contributes `1/k` of its tokens to each, and *k* counts only the PRs in **this run's selection** — run one PR alone and its shared agents get full weight, so always run the whole comparison set together. | A `block` and a `pr` on `UsageSnapshot`. |
| H5 | Orchestrator tokens are apportioned by wake share. | Per-turn PR attribution — nothing structural exists (§7). |
| H6 | `usage.json` is cumulative, so a delegate's whole life counts against its PR. | A windowed usage series, or accepting the approximation. |
| H7 | The PR window ends at `merged_at` passed in by the caller. | A loomux row for a human merge (#388). |
| H8 | A `usage.json` row is keyed by CLI **session**; a session carried to a new agent id names only its last occupant, so the row is split evenly across every agent that occupied it. | An `agent_id` (or `block` + `pr`) on **every** `UsageSnapshot`, not just the latest — the same missing field as H4. |

A run that adds a heuristic adds a row here in the same commit. The test suite
asserts every emitted heuristic has an id, a statement and a named fix, so an
undocumented guess fails rather than shipping quietly.

---

## 7. What this cannot say

The counters above measure **how much machinery a PR consumed**. None of them
measures whether the machinery was right.

- **It does not say whether a finding was real.** A review round is a
  `review-verdict` row. A `fail` that caught a genuine defect and a `fail` on a
  mis-stated body figure are the same row, and this scorecard counts them
  identically. Nothing here distinguishes them; the row that would
  (`finding-dispositioned`) does not exist and waits on findings being structured
  first (#995). Reading a drop in rounds as a quality improvement is unsupported by
  anything in this file.
- **It does not attribute an orchestrator turn to a PR.** §4.5 gives an upper bound
  and a wake-share apportionment, and both are declared heuristics. An orchestrator
  turn that read three PRs' notices and answered one of them is one turn; nothing in
  the transcript or the audit log says which PR it was about.
- **It does not say whether a round was the worker's fault or the brief's.** Two
  rounds on a PR whose brief was ambiguous and two on a PR whose worker was careless
  read the same.
- **It cannot compare across groups or repos.** Wake counts scale with how chatty a
  delegate's `report` discipline is, and token counts scale with the model and the
  context. A before/after is only meaningful within one group over one repo, dated
  to the workflow and template blobs at each end.
- **A window is not an isolation.** Concurrent PRs share the orchestrator's window;
  `wake_share` is reported so the overlap is visible, not so it is corrected.

---

## 8. Reproducing a historical measurement

The audit log is **live** and grows while it is being read, so a number measured
today is not reproducible tomorrow without a bound. `--cut <ms|iso>` drops every
audit row and every transcript turn after an instant, which is what makes an earlier
figure checkable: the plan's part-1 census names both its cut (`ts_ms`
`1788315192783`) and its row count (6337), and `--cut 1788315192783` reproduces that
row set exactly.

Group-wide totals (`group.files[]`) are reported per audit file rather than pooled,
because a generation boundary is where a rotation happened and pooling two
generations hides it.

---

## 9. Output shape

One JSON object. `--format table` renders the per-PR GFM table instead;
`--format both` prints each.

```
{
  generated_ms, inputs: { audit[], usage, agents, transcript[], pr_meta, tail_min, cut_ms },
  group:    { files: [ { path, rows, span_h, orchestrator_wakes, wakes_by_kind, ... } ] },
  prs:      [ {
    pr, build, issue, outcome,
    windows:      { pr: { start_ms, end_ms, end_source, tail_ms, span_h } | null,
                    loop: { start_ms, end_ms, tail_ms, span_h } | null },
    orchestrator: { wakes_total, wakes_by_kind, loop_notices, loop_notices_any_pane_s5,
                    wake_share: { pr_wakes, window_wakes, share },
                    tokens_window, tokens_attributed },
    review:       { rounds, by_block: { <block>: { <verdict>: n } } },
    driver:       { drives, lane_spawns, hand_backs, refused, held, ...,
                    refused_by_reason, held_by_reason },
    delegates:    { count, tokens, agents: [ { agent, block, role, tier, weight, pr_weight,
                    session_weight, shared_session_agents, tokens, tokens_credited,
                    usage_key, has_usage_row } ] },
    share:        { orchestrator_pct_raw, orchestrator_pct_attributed },
    rows_classified, rows_counted_outside_pr_window, rows_unclassified_in_window
  } ],
  coverage: { audit_rows_read, audit_parse_errors, rows_classified, transcripts[],
              agents_attributed, agents_unattributed_spawned_in_window,
              agents_split_across_prs, usage_rows_unusable, usage_sessions_indexed,
              usage_sessions_shared_by_more_than_one_agent, heuristics[] }
}
```

`rows_classified` is the positive control: it is the count of rows a counter actually
consumed, and a run reporting zero has not measured anything, however many rows it
read.

---

## 10. Testing

`test/orchscorecard.test.ts` over a synthetic corpus in
`test/fixtures/orchscorecard/`: one driven PR, one hand-routed PR with no merge time,
and one PR **named by nothing** as the negative control that makes every "the
mechanism ran" assertion fail-able. The corpus is built so that no two counters share
a value — a fixture whose axes are all one constant cannot tell a working counter
from a broken one.

The transcript fixture is six hand-written lines. Real transcript content is private
and never enters a fixture.

The corpus also carries one instance of each edge the text above promises to handle,
so none of them is a claim with no test behind it: a session shared by two agent ids
(the H8 split), an `rd-*` action this reader has no bucket for, a transcript line with
usage but **no** `message.id`, and a `usage.json` row with neither a session key nor an
`agent_id`.

The one instrument here is `npm test`. `tsconfig.json`'s `include` is `["src"]`, so
`tsc --noEmit` typechecks neither `scripts/` nor `test/` and has no opinion on any of
this — a mutation table for this file must not claim the compiler as a second reader.

# Design: progress timeline (issue #608)

Status: **in progress** — this note is being built up slice by slice against the
accepted plan on
[#608](https://github.com/willem445/loomux/issues/608#issuecomment-5151585803).
What is written here is what has landed:

- **Slice A:** the `gh_activity` backend command and its coverage contract.
- **Slice B:** the pure event model + layout math (`timelinemodel.ts`,
  `timelinelayout.ts`) and the `TimelineEvent` schema.
- **Slice C (pending):** the embeddable view, pane/shortcut/persistence wiring,
  user docs, and the `embedded-panels.md` cross-link.

## The feature, in one paragraph

A group-scoped, read-only view of orchestration progress on a time axis: when
issues were opened and closed, when PRs were opened and merged, plus the audit
log's own agent/delivery/report/gate events. Filterable by time window,
defaulting to the last 12 hours. It is a *reading* of data loomux already has —
it mutates nothing and adds no polling loop of its own.

## Data sources

| Source | Command | Slice |
|---|---|---|
| Orchestration audit log | existing `orch_audit` (unchanged) | B/C |
| Task board (current status only, not history) | existing `orch_tasks` (unchanged) | C |
| GitHub issue/PR lifecycle | **new** `gh_activity` | A |

The audit log needs no new backend at all: `orch_audit` already serves
`Vec<AuditEntry>` to the frontend for the audit view, and every event the
timeline plots (`agent-spawn`, `prompt`, `tool-call`, `task-upsert`,
`review-verdict`, `merge-gate-*`, …) is already in it. Extraction is
presentation-shaped and lives in DOM-free TS modules per this repo's
convention — see the plan's §2 for why a Rust-side event extractor was
rejected.

gh is the one genuinely missing source. The existing `gh_issue_list` /
`gh_pr_list` are open-state-only with pinned `--json` field sets that carry
`updatedAt` and nothing else — they cannot answer "what was opened, closed or
merged in this window", including for work a human did outside any group.

## The `gh_activity` command (Slice A)

A public contract, so it is written down here rather than only in the code:

```rust
#[tauri::command] gh_activity(repo: String) -> Result<GhActivity, String>

struct GhActivity {
    issues: Vec<GhIssueActivity>,   // number, title, state, created_at, closed_at, updated_at, url
    prs:    Vec<GhPrActivity>,      // ... + merged_at, head_ref
    limit:  usize,
    issues_truncated: bool,
    prs_truncated:    bool,
}
```

Typed frontend wrapper: `ghActivity(repo)` in `src/issues.ts` (hard constraint
5 — no view code touches Tauri IPC directly). ACL: registered in `lib.rs`'s
`generate_handler!`, listed in `command_manifest::APP_COMMANDS`, granted to
`main` via `permissions/sets/gh-read.toml` → `main-ui`; `tests/acl_manifest.rs`
is the guard that all three stay in step.

It follows `gh.rs`'s existing conventions exactly — arg-vector spawn, pinned
`--json` field set, lossy decode, `repo` resolved backend-side and used only as
a working directory (constraint 6's trust boundary), `CREATE_NO_WINDOW` on
Windows. Timestamps stay RFC-3339 **strings** over the wire and are parsed by
the frontend with `Date.parse`, so `gh.rs` still needs no date crate — and no
new dependency of any kind enters `src-tauri` (the getrandom hazard is not in
play because nothing is added to the graph).

### Three decisions worth the ink

**1. The sort order is pinned, because gh's default is the wrong one.**
`gh issue list` / `gh pr list` return items ordered by issue **number**
descending — newest *created* first, not newest *active* first. Verified
against gh 2.95.0 on this repo: a listing came back `633, 632, 628, 625, 622`
with non-monotonic `updatedAt`. Under that default, an issue opened months ago
and closed ten minutes ago drops off the page the moment 100 newer items exist
— so the default 12-hour window would silently lose exactly the events the view
is for. `--search "sort:updated-desc"` makes the page "the N most recently
active items", which is the property a time-window view needs. `--state all` is
the second non-default pin: the default `open` contains no close or merge event
at all.

**2. `merged_at`, not `state` or `closed_at`, decides "merged".** GitHub closes
a PR when it merges it, so a merged PR carries *both* timestamps. Keying a
merge event off `closed_at` would render every abandoned PR as a merge. The
pinned field set therefore includes `mergedAt`, and a test pins the
open / merged / closed-unmerged triple.

**3. The cap is reported, not silently applied.** Each list is bounded at 100
(`ACTIVITY_LIMIT`). Both the bound and whether it was reached come back in the
payload, so the view can render a coverage boundary; `limit` crosses the wire
instead of being duplicated as a frontend constant so the note and the query
cannot drift apart. `*_truncated` is deliberately conservative — a repo with
exactly 100 issues reports truncated, since one page cannot distinguish
"exactly full" from "full and more" — and over-reporting the boundary is the
safe direction.

`updated_at` is returned on every row for the same reason. Because the list is
sorted by activity, the oldest row's `updated_at` is a *precise* coverage
floor: nothing omitted was active more recently than that instant, so the view
can state "complete above T" rather than a vague "may be incomplete". This is
the frontend-side half of the honesty requirement, and Slice C owes the
rendering of it.

The rule behind all of that is `.loomux/lessons.md`'s "no silent caps": a chart
is uniquely good at looking complete, and a quiet period that is really a
missing page is indistinguishable from real quiet unless the view says so.

### Absent timestamps

`closed_at` / `merged_at` normalize to `None` not just for JSON `null` but for
an empty string and for Go's zero time (`0001-01-01T00:00:00Z`), which gh has
emitted for unset timestamps. Either would otherwise decode as a real instant
and plot a merge that never happened at the far left of the axis. Conversely a
*missing* `createdAt` decodes to `""` rather than failing the parse: one row
with a broken timestamp costs that row (the frontend parks it as undatable),
never the whole timeline.

## Failure behavior

One gh failure fails the whole `gh_activity` call rather than returning a
half-populated result. A timeline missing every PR looks like a quiet period;
an error is something the view can say out loud. The view's own gh layer is
additive on top of the audit data (per the plan, Slice C ships an audit-only
empty state if gh is unavailable), so a gh error degrades the view rather than
breaking it.

## The event model (Slice B)

Two DOM-free modules, unit-tested under `node --test`
(`test/timelinemodel.test.ts`, `test/timelinelayout.test.ts`). They hold every
answer that can be *wrong*; the SVG that consumes them is hand-validated, per
the repo's DOM-free-module convention.

Both are **self-contained — no imports at all, not even type imports.** tsc's
build forbids the explicit `.ts` extension an intra-src import needs for
`node --test` to resolve it directly (TS5097; see the note in `embedsplit.ts`),
so a bare specifier would work for one runner and not the other. The
consequence worth writing down: `timelinemodel.ts` **mirrors** `AuditEntry`
(auditsummary.ts) and `GhActivity` (issues.ts) as `*Like` interfaces rather
than importing them. Structural typing means the real values pass straight in,
and those mirrors are the one place a wire-shape change has to be re-checked.

### The schema

```ts
type TimelineEventKind =
  | "group" | "intake"                       // category: group
  | "agent-spawn" | "agent-exit"             // category: agents
  | "delivery" | "report" | "task-status"    // category: work
  | "verdict" | "merge" | "release"          // category: gates
  | "issue-opened" | "issue-closed"
  | "pr-opened" | "pr-merged" | "pr-closed"  // category: github
  | "ops";                                   // category: ops (default OFF)

interface TimelineEvent {
  ts_ms: number;
  kind: TimelineEventKind;
  label: string;                       // one line, never empty
  agent?: string; issue?: string; pr?: string;
  source: "audit" | "gh" | "audit+gh";
  detail: unknown;                     // the raw source record
}
```

**Kinds label a dot; categories are the lanes and the toggle chips.** The split
exists so the header stays readable without collapsing distinctions the labels
need.

Three refinements to the schema the plan sketched, all additive:

- **`delivery`, not `kickoff`.** The `prompt` audit detail is `{to, text}` and
  nothing else (mod.rs, `deliver_prompt_as`), so a first kickoff and a
  mid-stream delivery are indistinguishable in the data. The plan flagged this
  as "decide in B1 with the real data shape"; matching the kickoff's *text*
  would be a byte-shape guess against an open set, which this repo has been
  burned by before (`.loomux/lessons.md`, on fallible signals). The sender is
  in the data, so it goes in the label.
- **`intake` added.** `intake-signal` is real work arriving; `group` would
  mislabel it and `ops` would default-hide it. It shares the `group` lane and
  keeps its own label.
- **`pr-closed` added.** gh returns closed-unmerged PRs; without this kind such
  a PR opens on the chart and never resolves.

`source: "audit+gh"` marks an event both sources reported (see the dedupe
below). A `report` event leaves `issue`/`pr` unset on purpose: the MCP `ref`
argument (`"#123"`) does not say which it is, so it stays in the label and the
raw detail rather than being guessed into the wrong field.

### Nothing is silently dropped

The rule the whole model is shaped around. Every audit row ends in exactly one
of four places, and three of them are counted:

| Outcome | What it means |
|---|---|
| plotted | it has a kind of its own |
| `ops` lane | no kind of its own — a real event in a default-OFF lane, tallied per action in `unmapped` |
| `undatable` | no usable instant: the shims write a literal `ts_ms: 0` when `date` is unavailable (`loomux_audit`), and a 1970 dot would stretch the axis across 56 years of nothing |
| `malformed` | not an audit entry at all — one spliced line in `audit.jsonl` must not blank the view |

gh timestamps that will not parse (an empty `created_at`) are undatable too. A
`null` `closed_at` is *not*: that is an open issue, a state rather than a
missing timestamp.

`coverageNotes()` turns those counters, the 5000-entry `orch_audit` cap and
gh's own truncation flags into the sentences the view must render. They live in
the pure layer so the honesty text is unit-pinned rather than view prose nobody
tests. Two are deliberately weaker than they could be:

- **the cap note says "loaded at its cap", not "truncated".** `orch_audit`
  sends no truncation flag (`audit_log_windowed` keeps that for derivations),
  and a log holding exactly `AUDIT_VIEW_LIMIT` entries is indistinguishable
  from one that was cut.
- **the coverage-floor note says "audit coverage starts *T*", not "data is
  missing before *T*".** A window wider than the group's own lifetime is the
  ordinary case, and the frontend cannot tell a young group from one whose
  older generation was rotated away (`AUDIT_ROTATE_BYTES` keeps one). What it
  can say truthfully is that empty space at the left edge is "not recorded",
  not "nothing happened".

`AUDIT_VIEW_LIMIT` is mirrored as a frontend constant rather than plumbed —
the command does not send it, and the mirror carries the comment saying so.

### Merge dedupe

A merged PR can be reported twice: gh's `mergedAt` and the shim's own
`merge-gate-*` line. They collapse onto the gh row — gh has the authoritative
instant, and the gate's record survives in `detail.gate[]`. Three things the
dedupe deliberately does not do:

- **it never collapses a refused merge.** `merge-gate-blocked` at 11:00 and a
  granted merge at 11:30 are two real events in one PR's story; only a
  *permitting* gate event is the same event as the merge.
- **it never drops an audit-only merge.** That is a PR past gh's 100-row
  window, or a gate that allowed a merge gh never completed — exactly what
  this view exists to surface.
- **it never calls a gate event a merge.** The shim audits its *permission*
  and then exec's the real `gh`, which can still fail. An un-deduped gate event
  reads "merge of #640 allowed (autonomous)"; only a gh `mergedAt` says
  "merged".

Reference normalization matters here: the gh shim writes `"pr": "640"` (a
string, from `$num`), `review_verdict` writes `"pr": 640` (a number), and
agents write `"#640"`. All three normalize to the bare `"640"`, and anything
without digits is dropped rather than keyed to the wrong PR.

### Windowing

Presets are 1h / 6h / 12h (default) / 24h / 72h / all. `resolveWindow()` holds
three decisions:

- an **unknown preset id falls back to the default** — a value persisted by
  another build must not produce a blank chart;
- an event stamped **in the future extends the window** rather than falling off
  the right edge. A clock-skewed agent's report staying visible is the same
  principle as not plotting an undatable event at 1970;
- a window with **no span is widened** to `MIN_WINDOW_MS`, so the scale always
  has something to divide by (all events at one instant is a real case).

Both window edges are inclusive: a dot that vanishes because the window slid
by a millisecond is a worse bug than one counted twice at a boundary.

### Layout (`timelinelayout.ts`)

Knows nothing about `TimelineEvent`. It lays out anything with an instant and a
lane key, and the view supplies the key from `categoryOf()` — so the lane
grouping the view wants (by category now, sub-laned by agent later) is a caller
decision rather than a rewrite here.

- **Ticks are epoch-ms multiples.** A day tick is 00:00 UTC everywhere, which
  is what makes tick placement identical in every timezone and immune to DST.
  Labels are the view's job precisely because labels are where locale belongs.
  The ladder stops at 30 days and scales that step by whole multiples beyond
  it; calendar months are not a fixed number of ms and would break the
  property.
- **Clustering is per lane, greedy from the left**, against the cluster's first
  x — so a dense run becomes several bounded clusters rather than one that eats
  the lane, and a cluster's x is where it *started*, not a drifting mean. Each
  dot keeps `indices` into the array the caller passed, so expand-on-click
  needs no re-derivation.
- **Degenerate geometry is finite, never NaN.** A panel narrower than its own
  padding (a real state mid divider-drag) collapses to a zero-width plot
  instead of an inverted, mirrored axis.
- **Items outside the window are counted in `dropped`, never clamped onto the
  edge**, where they would read as real events at a time they did not happen.

### Where the PTY constraint lands

Nowhere in Slice B — these modules touch no terminal and no layout. The
constraint is Slice C's: the timeline is an embeddable view (#361), floating as
an overlay and, when docked, resizing through the same grid path every other
embedded panel uses. See `doc/design/embedded-panels.md`.

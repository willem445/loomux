# Design: progress timeline (issue #608)

Status: **in progress** — this note is being built up slice by slice against the
accepted plan on
[#608](https://github.com/willem445/loomux/issues/608#issuecomment-5151585803).
What is written here is what has landed:

- **Slice A (this note's current content):** the `gh_activity` backend command
  and its coverage contract.
- **Slice B (pending):** the pure event model + layout math
  (`timelinemodel.ts`, `timelinelayout.ts`) and the `TimelineEvent` schema.
- **Slice C (pending):** the embeddable view, pane/shortcut/persistence wiring,
  user docs, and the `embedded-panels.md` cross-link.

## The feature, in one paragraph

A group-scoped, read-only view of orchestration progress on a time axis: when
issues were opened and closed, when PRs were opened and merged, plus the audit
log's own agent/kickoff/report/gate events. Filterable by time window,
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

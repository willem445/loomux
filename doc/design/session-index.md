# Design: bounded, cached session discovery

Status: implemented (issue #493). Pairs with #479/#484 — same user-visible
complaint ("quite a bit of lag when I click restore on an orchestrator agent
session"), a different code path.

## Problem

Two facts, both measured on the reporting machine rather than inferred, from
`logs/breadcrumbs.log` on the 2026-07-30 restart:

```
20260730-115247 startup v1.0.0 unclean_prev=true …
20260730-115248 pty-open id=1 …                        <- restored panes opening
20260730-115305 startup list_sessions: 826 session file(s) scanned in 12.923059s
20260730-115305 startup list_sessions: 826 session file(s) scanned in 16.6985764s
20260730-115305 agent-spawn group=loomux-68435179 agent=orch-1 … resume=true
```

1. **The scan itself was O(every session ever recorded).** It opened every
   `~/.claude/projects/**/*.jsonl` and read up to 60 lines of each — 826 files,
   13–17 seconds — to produce a list that `truncate(300)` then cut to 300 rows.
   526 of those head-parses were thrown away every single time.

   The per-file cost is not small, and that is the crux: measured over a random
   60-file sample of the same real store (955 files, 830 MB total at the time of
   writing), the **first 60 lines** of a transcript sum to **174,599 characters**
   on average (median 169,909, min 30,912, max 477,167 — `Get-Content
   -TotalCount 60`, line lengths summed). So the scan read and JSON-parsed on the
   order of **140 MB** per run.

2. **It ran twice, concurrently.** Both scans finish in the same second, so the
   16.7s one started ~4s before the 12.9s one: that first one is the sidebar's
   boot prefetch (`sessions.refresh()`, unawaited since #342, kicked off as the
   restored panes opened at 11:52:48), and the later one is a
   *restore-this-group click*, whose resumability check called `listSessions()`
   directly. The `agent-spawn … resume=true` breadcrumb lands
   the moment the second scan finishes — i.e. the restore genuinely waited on
   it. The answer that click needed was already being computed and seconds from
   landing; asking for it again made both copies slower.

## Fix, in three parts

### 1. Metadata first, so parses are bounded by the row limit

`scan_sessions` collects **candidates** — `(path, source, mtime, len)` from
directory enumeration alone, no file opened — then sorts by mtime and truncates
to `LIST_LIMIT` (300, the same cap the list has always applied) **before**
anything is parsed.

The row set is unchanged: sorting on `DirEntry` metadata sorts on the identical
`modified_ms` the rows carried before, with the same comparator over the same
enumeration order, so the same "newest 300, newest first" comes out.

What changes is the scaling. A history that keeps growing now costs one `stat`
per session instead of a head-parse — answering #493's "does it scale with
history?" as *no longer monotonically*, since the parse budget is fixed at the
number of rows the UI can show.

### 2. A persisted index, so an unchanged file is parsed once ever

Each survivor's head-parse is cached in `<data root>/session-index.json`, keyed
by the session file's own path and validated by `(modified_ms, len)`. An
appended-to or rewritten transcript fails validation and is re-parsed; only a
byte-for-byte-unchanged file is served from the index.

**What is deliberately NOT cached** — the part that keeps this a cache and not a
second source of truth:

| Field | Why it's re-derived every scan |
| --- | --- |
| `resume_command` | Derived from loomux's launch-intent record (#456/#457), which changes independently of the transcript — a posture recorded after the index entry was written must still take effect. |
| `orch_group` (notice-only detections) | Derived from the session's cwd plus a `group.json` existence check. A group directory can be deleted while the transcript that named it stays byte-identical, so a cached value would keep claiming a group that no longer exists. |

The index stores only the transcript-derived facts (`id`, `title`, `cwd`, and the
raw `orch_role`/`orch_gid` detection) and rebuilds everything else.

The validating `(mtime, len)` pair comes from `fs::metadata`, not the cheaper
`DirEntry::metadata` the enumeration already has in hand: the latter does **not**
traverse symlinks, so a symlinked session file would be validated against the
LINK's mtime/len — stable while its target changes, the one path by which this
index could serve a stale row. `fs::metadata` follows the link, matching what the
pre-#493 scan's `mtime_ms` did. Measured cost of the difference: ~40ms across 826
files, against the head-parse it exists to avoid.

**Failure modes all degrade to "parse it again", never to a wrong answer:**
absent (first run), corrupt (quarantined by `uistate::load_or_quarantine`, the
same fail-safe `tabs.json` uses), or written by a different `version` (rejected
wholesale — which is what makes adding a field later a one-line change rather
than a migration). It is also **self-pruning**: it only ever holds the rows the
last scan returned, so it cannot grow past `LIST_LIMIT`, and a steady-state scan
that parsed nothing does not rewrite it at all.

The #440 hazard the issue explicitly warned about — "any caching/index must stay
correct across sessions loomux did not mint" — is handled structurally: the index
is keyed off what the directory enumeration finds, so a deleted session simply
isn't a candidate and can never be resurrected from a stale entry, and a session
loomux never saw is just a cache miss.

### 3. One owner for the scan (`src/sessionstore.ts`)

`SessionStore` holds the app's single session list. Two accessors, deliberately
different:

- `refresh()` — "I need a current read": the ↻ button, opening the sidebar, the
  #440 reconciler looking for transcripts that appeared since boot. Starts a
  scan, or **joins** the one in flight; never a second concurrent one.
- `ensureLoaded()` — "I need the list, whatever the newest read said": the
  group-restore resumability check. Serves the cached rows if a scan has already
  succeeded, joins one in flight otherwise, and only scans when neither can
  answer.

`ensureLoaded()` is what the restore click now calls. Freshness isn't what that
check needs: it asks whether session ids *captured at close* still have
transcripts, and a transcript the newest read already saw hasn't stopped
existing since. A rejected scan still rejects, so the caller can keep
distinguishing "no sessions" (assume nothing resumable) from "couldn't look"
(assume everything is) exactly as it could when it called `listSessions()`
itself.

The loss-safe *coalescing* of dropped refresh calls (a call arriving mid-scan
owing exactly one trailing re-run, rev-9 review) stays in `SessionBrowser`'s
`RefreshGate`, which also has to cover the `loadRoles()` half of a refresh and
the render. Two copies of that state machine would be worse than the seam.

## Measured effect

Same synthesized population on both sides — 826 claude sessions with ~172 KB
heads, the measured median shape of the real store — via
`cargo test --test sessionindex -- --ignored --nocapture measure`:

| Scan | Before (parse every file, no index) | After (as shipped) |
| --- | --- | --- |
| #1 (fresh index) | 8.40 s / 6.84 s — **826 parsed** | 5.88 s / 4.18 s — **300 parsed** |
| #2 (steady state) | 1.99 s / 5.70 s — **826 parsed** | 39 ms / 42 ms — **0 parsed** |
| #3 | 1.92 s / 5.86 s — 826 parsed | 13 ms / 53 ms — 0 parsed |
| #4 | 1.93 s / 2.61 s — 826 parsed | 12 ms / 54 ms — 0 parsed |

Two runs per side, because the machine was running concurrent agent builds
throughout and the wall clock shows it — the same before-scan measured 1.9 s once
and 5.9 s another time. The **parsed/reused counts are exact and
machine-independent**; the times are indicative. Steady state, worst
after-number against best before-number: **1.9 s → 54 ms**, and 826 head-parses
→ 0. Scan #1 is cold-OS-cache, #2–4 hot.

That spread is precisely why the tests assert on parsed/reused counts and only
this measurement harness reports time at all.

The residual, stated plainly: the **first** scan after this lands still parses up
to 300 heads (4–6s on that fixture). That work is off the blocking path (the boot
prefetch has been unawaited since #342, and the restore click now joins it
instead of adding a second scan), and every launch after it is ~12ms plus
whatever genuinely changed.

**Considered and rejected:** capping the head read by bytes (e.g. 64 KB instead
of 60 lines) would cut that first scan further, but `scan_claude_jsonl` is shared
with `find_claude_session_cwd` — the #412 resume-by-id cwd resolution — so a cap
would risk turning a session with a very large opening turn into an unresolvable
one. Not worth it for a one-time cost.

## What the tests pin

`src-tauri/tests/sessionindex.rs` asserts on the **shape of the work** —
`ScanStats`' `parsed`/`reused` counts — not on elapsed time. A wall-clock
assertion would be flaky on CI and, worse, would pass for the wrong reason on a
fast disk while a regression re-parsed everything.

| Property | Test |
| --- | --- |
| Parses bounded by the row limit, not history; same newest-300 rows, same order | `parse_cost_is_bounded_by_the_row_limit_not_by_history` |
| A session file is opened once, ever ("store opened once") | `a_second_scan_opens_no_session_file_at_all` |
| A steady-state scan doesn't even rewrite the index | `an_unchanged_scan_does_not_rewrite_the_index` |
| Changed transcript re-parsed; deleted one never resurrects (#440) | `a_rewritten_session_is_reparsed_and_a_deleted_one_never_resurrects` |
| The limit/index bound the LIST, never resume-by-id (#412/#440) | `the_row_limit_never_gates_resume_by_id` |
| Corrupt or foreign-version index → full parse, never a wrong row | `a_corrupt_or_foreign_index_degrades_to_a_full_parse` |
| Cached rows keep their orchestration identity (sidebar chips) | `cached_rows_keep_their_orchestration_identity` |
| Copilot ids come from `workspace.yaml`, incomplete dirs skipped | `copilot_rows_are_indexed_by_their_recorded_id_and_incomplete_dirs_are_skipped` |

`test/sessionstore.test.ts` pins the frontend half the same way — as a **scan
count**, not a timing: no caller can start a scan that a completed or in-flight
scan could have answered.

## Where the pieces live

| Concern | File |
| --- | --- |
| Candidate collection (metadata only, per CLI) | `src-tauri/src/sessions.rs` — `collect_claude_candidates`, `collect_copilot_candidates`, `candidate_meta` |
| Bound + cache + stats | `src-tauri/src/sessions.rs` — `scan_sessions`, `LIST_LIMIT`, `ScanStats` |
| Index load/save, version gate, test seam | `src-tauri/src/sessions.rs` — `load_session_index`, `save_session_index`, `set_session_index_path_for_test` |
| Re-derived (never cached) row fields | `src-tauri/src/sessions.rs` — `to_session_info` |
| Single owner of the scan, frontend | `src/sessionstore.ts` — `SessionStore` |
| Sidebar + roles + trailing-re-run coalescing | `src/sessions.ts` — `SessionBrowser` |
| The consumer that used to double-scan | `src/main.ts` — `resumeDormantGroup` |

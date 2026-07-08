# Design: durable writes + disk hygiene

Status: implemented (issues #133, #134).

## Problem

On 2026-07-07 a live orchestration ran the machine's `C:` drive to **0 bytes
free**. Two independent failures fell out of that one condition:

1. **The task board was destroyed.** An `upsert_task` persisted the board with
   `fs::write(tasks.json, …)`, which *truncates then writes*. The write failed
   partway with os error 112 ("not enough space on the disk"), so the previous
   good contents were already gone: `list_tasks` came back `[]` and every
   existing task id read as `unknown task`. All 13 live tasks and their note
   threads were lost. A failed write was **destructive** instead of a no-op.
2. **The disk filled in the first place** because every agent git-worktree that
   runs `cargo check`/`cargo test` grows its own `src-tauri/target` cache at
   5–7 GB each. A day of orchestrated work left ~10 worktrees ≈ 50 GB of
   duplicate build caches, with nothing bounding or reclaiming them.

This note covers the fix for both: make durable writes atomic (#133), and stop
worktrees from each paying a fresh multi-GB build cache, plus a backstop that
warns before the disk hits zero (#134).

## Part 1 — atomic durable writes (#133)

### The pattern

`atomic_write(path, bytes)` in `orchestration/mod.rs` replaces a file durably:

1. Write the new contents to a **same-directory** temp sibling
   (`.<name>.<pid>.<seq>.tmp`).
2. `sync_all()` the temp so its data blocks reach disk before it is linked into
   place — this is the exact guard against the disk-full failure mode, where a
   rename could otherwise expose a metadata-only file whose bytes never landed.
3. `fs::rename` the temp over the destination. On Windows this maps to
   `MoveFileExW` with `REPLACE_EXISTING`, which atomically replaces the target
   on the **same volume** (hence the same-directory temp — a cross-volume move
   is a non-atomic copy). A crash or failure at any point leaves the previous
   good file intact; at worst an orphaned `.tmp` sibling is left behind, never a
   truncated destination.

If the rename fails (the destination is momentarily locked — antivirus, an open
reader), it falls back to a direct write so the update isn't silently lost, and
keeps the temp on failure for manual recovery.

### getrandom constraint

The temp name must be unique per write, but the Windows-10 baseline can't load
the `ProcessPrng` that `tempfile`/`uuid`/`rand` pull in (see the Cargo.toml
note). So the name is deterministic: `pid` + a process-monotonic `AtomicU64`
counter. Uniqueness is what matters (two concurrent writers to one file must not
share a scratch path), not unpredictability — and an atomic counter delivers
that without a getrandom crate.

### Audit of every durable write in the group dir

| File | Writer | Lock while writing | Change |
| --- | --- | --- | --- |
| `tasks.json` | `write_tasks` | `tasks_lock` (all callers) | → `atomic_write` |
| `state.json` | `set_state` | **none** (see below) | → `atomic_write` |
| `agents.json` | `persist_agent_record` | `tasks_lock` | → `atomic_write` |
| `group.json` | `create_group` | creation-time, single writer | → `atomic_write` |
| `group.json` | `persist_max_agents` | — | already atomic; refactored onto the shared helper |
| `usage.json` | `upsert_usage_snapshot` | `tasks_lock` | already atomic; refactored onto the shared helper |
| `audit.jsonl` | `append_audit` | — | **append-only**; a failed append can't truncate prior lines, so left as-is (verified) |
| `configs/<id>.json`, role `*.md`, attachments | derived/one-shot | — | not mutable durable state — regenerated or uniquely named, so a failed write is retryable, not destructive; left as plain `fs::write` |

`state.json` is written by `set_state` **without a lock**. Two concurrent
`set_state` calls are still safe under `atomic_write`: each writes its own
uniquely-named temp and renames last-writer-wins, so the reader never sees a
torn file — the worst case is one update superseding another, never corruption.
(In practice a group has one orchestrator, so concurrent `set_state` is rare.)

`tabs.json` / `uistate.rs` (project tabs, PR #157) was **not** merged when this
landed; that store already writes atomically (temp + rename) and needs nothing.

## Part 2 — disk hygiene (#134)

### Shared worktree build cache

Agent worktrees are created under
`<repo-parent>/<repo-name>-worktrees/<name>` (see `git_worktree_add`) — same
drive as the main checkout. A pane whose cwd is a **linked git worktree** now
gets `CARGO_TARGET_DIR` pointed at `<main-repo-root>/.loomux-target`, so every
worktree shares one build cache instead of each growing its own 5–7 GB
`target/`. The near-dedup is the biggest disk win, and later workers get warm
builds.

The injection lives in `pty.rs::apply_pane_env` — the one place every pane's
child environment is assembled (both the #78 direct-CLI spawn and the shell
wrapper), so the #110 direct-spawn path is covered without any frontend change.
Worktree-ness is detected purely from the filesystem — a linked worktree's
`.git` is a *file* (`gitdir: …`) whose `commondir` resolves to the main repo's
`.git` — so **the main checkout keeps its own `target/`** (its `.git` is a
directory → `None`). An operator-set `CARGO_TARGET_DIR` is respected (not
overridden), and `LOOMUX_NO_SHARED_TARGET` is a one-env-var rollback.

`.loomux-target/` is gitignored in the main repo.

**Tradeoff (documented honestly):** concurrent `cargo` invocations against one
target dir **serialize on cargo's target-dir lock** — a second build blocks
until the first releases. This is acceptable here: workers mostly build at
distinct times, and `--locked` keeps inputs consistent so the shared cache stays
valid across worktrees. The alternative (per-worktree caches) is what filled the
disk. If serialization ever bites, the rollback env var restores per-worktree
`target/`.

### Low-disk backstop

`start_disk_monitor` samples free space on the workspace drive (the app-data
root, where the board/state live — the surface a disk-full write corrupts) every
`DISK_CHECK_INTERVAL` (60 s; slow on purpose — pressure builds over minutes, so
the `sysinfo` scan stays negligible). When free space crosses below
`LOW_DISK_BYTES` (5 GB — headroom for one more cold cargo build to be reclaimed
before writes fail at 0), it delivers **one** audited notice to each group's
orchestrator suggesting reclamation (end merged worktrees, `cargo clean` idle
ones, clear temp).

The latch discipline mirrors the watchdog/idle-tick notices:

- **One per episode** — a machine-wide latch, set on the crossing tick and
  cleared only once free space recovers past `LOW_DISK_CLEAR_BYTES`
  (`LOW_DISK_BYTES` + 2 GB). The hysteresis stops a disk hovering at the
  threshold from re-notifying every tick.
- **Paused groups are skipped** — their agents idle out on purpose and delivery
  is suppressed there anyway.

The latch/hysteresis (`low_disk_transition`) and the free-bytes read
(`free_disk_bytes`) are split so the transition logic is unit-testable without a
real disk; `disk_tick(free)` takes injected free-bytes for the same reason.

This is a **backstop**, not the fix — the shared cache is the structural cure.
Option 2 from the issue (auto-reclaim merged/idle worktrees) is mostly
orchestrator discipline and is left to the orchestrator/human for now.

## Tests

- `failed_task_write_leaves_board_intact` (Windows-gated): fault-injects a
  read-only `tasks.json` so the rename-over and the direct-write fallback both
  fail, and asserts the previous board is byte-for-byte intact and non-empty —
  the incident, reproduced without filling the disk. POSIX rename keys on
  directory write not file perms, so this injection is Windows-specific.
- `durable_writes_round_trip`: happy-path round-trip for `tasks.json` /
  `state.json` — atomicity didn't change semantics.
- `worktree_cwd_maps_to_shared_target_dir`: a fixture worktree tree maps to
  `<root>/.loomux-target`; a real checkout maps to `None`.
- `low_disk_transition_latches_once_with_hysteresis`,
  `low_disk_notice_reports_free_space`,
  `disk_tick_notifies_once_per_episode_and_skips_paused`: the latch, message,
  and per-episode/paused-skip behavior.

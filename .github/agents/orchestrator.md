---
name: orchestrator
description: >
  Repo-specific orchestrator queue discipline for loomux: issue-first, one
  in-flight PR per subsystem, strict branch+PR flow (main is protected).
role: orchestrator
mode: append
---
# loomux orchestrator — queue discipline

This addendum tightens *how* you schedule work; loomux's built-in mechanics
(spawning, the task board, report handling, guardrails) still apply.

## Merge policy
- **`main` is protected — merges to it are disallowed.** Every unit of work goes
  through a branch and a PR; you surface finished PRs at the human merge gate and
  never merge yourself.

## Queueing
- **Issue-first.** Before assigning implementation work, ensure a GitHub issue
  exists (label `agent-managed`); workers link it with `Closes #N`.
- **One in-flight PR per subsystem.** Treat these as separate lanes and avoid two
  concurrent workers editing the same lane: `orchestration/` (backend registry +
  MCP), `src/` frontend/UI, `git/` + `pty/` (platform), docs. Serialize within a
  lane; parallelize across lanes with worktrees.
- Assign the `worker` profile to implementation tasks and the `reviewer` profile
  to review tasks (they carry loomux's platform gotchas). Use a `planner` for
  anything whose approach isn't obvious yet.

## Hygiene
- Record each agent's `session` id on its task so follow-ups resume instead of
  cold-starting. Keep the human oriented with short status summaries.

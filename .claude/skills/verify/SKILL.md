---
name: verify
description: Verify a loomux change end-to-end — what can be exercised automatically, what must be left to the human, and the traps (GUI app, no live agent CLIs, Windows manifest).
---

# Verifying a change in loomux

Loomux is a GUI app (Tauri window) whose core behavior involves live PTYs and
real agent CLIs — the two things you must NOT drive yourself. Verification here
means: exercise everything that is automatable, then hand the human a precise
manual checklist for the rest.

## Always run (both halves — a change to one often breaks the other's assumptions)

Agent workers: whether these run locally or on `ci.yml` follows
`.claude/skills/ci-validate`'s scope/duration line, not an automatic "run it"
— quick/local iteration is fine capped at `-j 4` (`-- --test-threads=4` too,
for `cargo test`), but full-suite validation still goes to CI, which is the
sole authority for the CI gate regardless of what ran locally. The commands
below are unconstrained for the human running this skill by hand.

```sh
npm run build                 # tsc --noEmit typecheck + Vite bundle
npm test                      # frontend unit tests (Node 22 built-in runner)
cd src-tauri
cargo check --locked          # what CI gates on
cargo test --locked           # backend unit + integration tests
```

## Exercise the change itself, not just the suites

- **Pure frontend logic** (layout math, steering compose, spawn expiry,
  clipboard formatting…): the logic lives in a DOM-free module — write or
  extend a `test/*.test.ts` that drives the new behavior with realistic
  inputs, and also run a quick one-off `node --test` on that file.
- **Backend orchestration logic**: drive it through
  `src-tauri/tests/orchestration.rs` — it dispatches real MCP JSON through
  `dispatch()` against a real `OrchRegistry` in a temp dir. Faking an agent =
  creating a registry entry + calling tools with its `Caller`; never spawn a
  real CLI.
- **New backend commands**: confirm the `#[tauri::command]` is registered in
  `lib.rs` and has a typed wrapper in `src/pty.ts` — a missing registration
  compiles fine and fails only at runtime.

## Never do

- **Never spawn `claude` or `copilot`** (not even `--resume` or with a trivial
  prompt) — it burns the user's paid credits. The user does live validation.
- **Don't run `npm run tauri dev` unattended** — it opens a window and blocks
  forever. Only useful when the human is present to click through.
- **Don't add a unit test that links the full lib** — on Windows it will fail
  to load (missing comctl32-v6 manifest). Put it in `src-tauri/tests/`
  (see CLAUDE.md constraint 4).

## Hand off a manual checklist

End with a short, concrete list of what the human should verify live, e.g.:

- which pane/overlay to open and what to look for,
- which orchestration flow to run (spawn a worker, send a report, …),
- any Windows-specific behavior (ConPTY repaint, clipboard, Job Object kill).

Be specific ("split right twice, open git view, drag the graph/diff divider")
— "test the UI" is not a checklist. If the change touches scrollback, panes,
or PTY sizing, explicitly ask the human to watch for repaint artifacts.

---
name: worker
description: >
  loomux worker addendum: a Rust+Tauri / TypeScript desktop-app worker that
  knows this codebase's hard-won platform gotchas and how to verify a change.
# copilot-native keys (ignored by loomux, honored by Copilot's --agent):
tools: [read, edit, shell]
# loomux mapping + extensions:
role: worker
mode: append
allow: Bash(cargo:*), Bash(npm:*)
---
# loomux worker — house rules

You are implementing a change in **loomux**, a Tauri (Rust backend + TypeScript
frontend) terminal-multiplexer desktop app. Beyond the base worker contract:

## Verify before you report done
- Backend: `cd src-tauri && cargo test` (unit + the `orchestration` integration
  suite). `cargo check --tests` first if you only need a fast type check.
- Frontend: `npm run build` (this runs `tsc --noEmit` then `vite build`) and
  `npm test` (the `node --test` suite). All must be green.
- If you changed runtime behavior, exercise it — don't rely on tests alone.

## Platform gotchas that WILL bite you (learned the hard way)
- **Never resize the PTY to drive a UI feature.** Overlays, not splits — a
  ConPTY resize repaints scrollback and corrupts it. xterm padding belongs on
  `.xterm`, not the pty geometry.
- **Typed IPC wrappers only.** Tauri commands are called through the typed
  helpers in `src/*.ts`; don't hand-roll `invoke` with stringly-typed args.
- **No `getrandom`-based crates** in the backend — they fail at runtime on this
  Windows target (`ProcessPrng`, `0xc0000139`). If you need a temp dir in a
  test, `tempfile` is already vendored with the offending feature disabled.
- **Test executables need the comctl6 manifest**, which cargo only applies to
  integration-test targets — put backend tests under `src-tauri/tests/`, not as
  `#[cfg(test)]` unit tests that link the UI stack.

## Style
Match the surrounding code's density and idiom. Comments explain *why*, not
*what*. Reference code as `file:line`. Keep commits in logical units and
reference the issue (`#N`).

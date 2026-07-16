# Loomux — instructions for Claude Code

Tauri 2 desktop terminal multiplexer for AI agent management. Rust backend
(`src-tauri/`), vanilla-TypeScript frontend (`src/` — no UI framework), xterm.js
terminals, Vite. The README's *Architecture* section maps every module; deeper
designs live in `doc/design/`.

## Commands

| What | Command |
| --- | --- |
| Typecheck + bundle frontend | `npm run build` (runs `tsc --noEmit` first — this is the typecheck) |
| Frontend unit tests | `npm test` (Node 22 built-in runner, runs `test/**/*.test.ts` directly) |
| One frontend test file | `node --test test/layout.test.ts` |
| Backend check (what CI gates on) | `cargo check --locked` in `src-tauri/` |
| Backend tests | `cargo test --locked` in `src-tauri/` |
| One backend test | `cargo test --locked --test orchestration <name_filter>` in `src-tauri/` |
| Run the app | `npm run tauri dev` — opens a GUI window and never exits; don't run it unattended |

There is no lint/format gate (no eslint/prettier; rustfmt is not enforced in
CI) — match the surrounding style instead of reformatting.

### Agent workers: quick local iteration vs. full CI validation

The Commands table above is for humans. For agent workers, the line is scope
and duration, not a blanket ban: a hard-kill from every worker running
`cargo build` at once (#320) was answered first with an interim hard ban on
any local build/test; a per-class concurrency guard was then tried
(#318/#322) but shelved (its shim couldn't reliably intercept every
invocation path). What replaces both: quick local iteration — a single-file
test, an incremental `cargo check`, a quick build to sanity-check a change —
is fine, always capped at `-j 4`; full-suite or multi-platform validation,
anything you'd cite as proof in a PR, still goes to CI, which remains the
sole authority for the CI gate. See the `ci-validate` skill for the full
decision rule, the `-j 4` local-cap details, and the draft-PR-early CI flow.

## Hard constraints — check before coding

1. **Never resize the PTY for a UI feature.** Git view, task board, audit
   viewer, badges, compose strip — all are overlays or header/board chrome
   floating over the terminal. Resizing ConPTY triggers full repaints that
   pollute scrollback. Visual padding belongs on the `.xterm` element, not on
   the layout.
2. **No getrandom-based crates in `src-tauri`** (uuid v4, rand, tempfile with
   default features). They import `bcryptprimitives.dll!ProcessPrng`, which
   this project's Windows 10 baseline doesn't export — the binary then fails
   to load with 0xc0000139. Ids/tokens use std's OS-seeded `RandomState`. See
   the notes in `src-tauri/Cargo.toml` before adding any dependency.
3. **Never spawn real agent CLIs** (`claude`, `copilot`) to test or validate
   anything — it burns the user's paid credits. Tests fake the agent side
   (see `src-tauri/tests/`); the user does live agent validation themselves.
4. **Backend tests that link the lib must be integration tests**
   (`src-tauri/tests/*.rs`), not unit tests: Windows test executables need the
   comctl32-v6 manifest that `build.rs` embeds via `-tests`-scoped link args.
   Those args require at least one integration-test target to exist — never
   delete `tests/smoke.rs`.
5. **Frontend never touches Tauri IPC directly.** Every backend capability is
   a `#[tauri::command]` plus a typed wrapper in `src/pty.ts`; all other
   frontend modules go through those wrappers.
6. **Orchestration commands trust `group_id` as a path segment** — safe only
   because the webview is trusted. Never route agent-controllable input into
   group-scoped commands without a traversal/membership check.
7. **No agent ever merges a PR.** Open the PR and stop; the human reviews and
   merges.
8. **Loomux is a generic agentic-dev tool — never bake this repo's or this
   machine's quirks into product code.** No toolchain special-casing (nothing
   cargo-/npm-specific in `src-tauri`; express "what's expensive/guarded/built
   here" as repo config, the way the resource guard's `resources:` block does)
   and no operator-setup assumptions (paths, core counts, installed tools). A
   behavior that only makes sense for developing loomux itself belongs in
   `.loomux/` config or the dev docs, not the product. Precedent: the shared
   `CARGO_TARGET_DIR` cache was removed for violating this (#263).

## Code conventions

- Frontend logic that needs tests is extracted into DOM-free pure modules
  (`layout.ts`, `steer.ts`, `spawnexpiry.ts`, …) and tested in
  `test/*.test.ts` with `node:test` + `node:assert/strict`. DOM wiring is
  validated by hand — don't simulate a DOM in tests.
- Backend: unit tests inline under `#[cfg(test)]` only if they don't link the
  full lib; otherwise integration tests (constraint 4). Orchestration logic is
  covered in `src-tauri/tests/orchestration.rs`.
- `src-tauri/src/orchestration/mod.rs` is ~11k lines — read it selectively
  (grep for the function/struct), not top to bottom.
- Comments in this codebase explain *why* (design constraints, Windows quirks,
  issue numbers) — keep that density and style.
- Write tests that test intent, not implementation echoes.

## Refinements & scope increases from the user

Default: when the user asks for a refinement or feature addition on work already in
progress (an open PR, an active branch), **fold it into the active PR** rather than
deferring it to a follow-up issue. This is different from an agent inventing extra scope
mid-diff — that's still a review ground to bounce ("scope drift... split it"). Here the
user is the one increasing scope, deliberately, because they thought of the right shape
while watching the work land — that's a refinement, not drift. Only defer to a separate
issue when the user explicitly says to ("later", "follow-up issue", "separate PR"). Don't
narrow their ask back down to the original ticket on your own judgment.

## Git & GitHub workflow

- Commits: `type(scope): imperative subject (#issue)` — e.g.
  `fix(orchestration): expire timed-out spawn requests (#106)`. Common scopes:
  `orchestration`, `pty`, `gitview`, `launcher`, `tasks`, `clipboard`,
  `metrics`, `ui`, `build`, `release`.
- Branch from `main`; PR to `main`.
- GitHub issues are the work queue. Labels the orchestration workflow uses:
  `agent-managed` (an orchestrator owns it), `agent-ready` (groomed — go),
  `agent-investigation` (research only — post findings as an issue comment,
  no code), `agent-prototype` (build for demo/feedback).
- User-visible behavior changes must update the matching README section;
  substantial designs get a `doc/design/*.md` note.

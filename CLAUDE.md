# Loomux — instructions for Claude Code

Tauri 2 desktop terminal multiplexer for AI agent management. Rust backend
(`src-tauri/`), vanilla-TypeScript frontend (`src/` — no UI framework), xterm.js
terminals, Vite. `doc/design/architecture.md` maps every module; deeper designs
live in `doc/design/`.

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
CI) — match the surrounding style instead of reformatting. (Agents may still
run `rustfmt --check` as a *syntax* check, discarding its formatting
opinions — see the `ci-validate` skill.)

### Agent workers: NO local Rust builds — CI is the only build/test path

The Commands table above is for humans. For agent workers, **local
`cargo` builds and tests of ANY size are banned entirely** — no `cargo
build`, no `cargo check`, no single-test `-j 4` iteration, nothing that
invokes `rustc`. This is a human hard directive: a first compile costs
5-8 GB of `target/` per worktree, and a worker fleet building locally
exhausts the disk (#488).

How workers validate instead: push early, open a draft PR immediately,
and read CI (`ci-validate` skill's draft-PR-early flow). Iterate by
reasoning + pushing; CI is both the proof and the compiler.
Frontend-only commands that never invoke `rustc` (`npm run build`/`tsc`,
`npm test`/`node --test`) remain fine locally, as does `rustfmt --check
--edition 2021 <changed .rs>` — a parser, not a build, and the one pre-push
syntax check for Rust (#558; see the skill for the read-stderr recipe and why
`cargo check` is not covered). The one `cargo` exception: `cargo update
--workspace` for release lockfile bumps — dependency resolution only, never
compiles.

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
7. **No agent ever merges a PR to the default branch.** Open the PR and stop;
   the human reviews and merges. This is the rule for *every* agent —
   workers, reviewers and planners have no merge authority at all, anywhere,
   and must never merge, tag, or publish a release.
   **One narrow carve-out, for the orchestrator only:** an orchestrator may
   merge a sub-PR into a **non-default branch** it owns — typically an
   integration branch collecting a batch of sub-PRs for a single human
   review — and only once that sub-PR has a reviewer's approval, green CI,
   and every review finding fixed or explicitly deferred. Merging to the
   default branch is **never** covered: it always needs a per-PR grant from
   the human, as does creating a release or pushing a `v*` tag. The carve-out
   exists so a human reviews one combined PR instead of five, not to reduce
   how much a human reviews. When in doubt, open the PR and ask. (#469)
8. **Loomux is a generic agentic-dev tool — never bake this repo's or this
   machine's quirks into product code.** No toolchain special-casing (nothing
   cargo-/npm-specific in `src-tauri`; express "what's expensive/guarded/built
   here" as repo config, the way the resource guard's `resources:` block does)
   and no operator-setup assumptions (paths, core counts, installed tools). A
   behavior that only makes sense for developing loomux itself belongs in
   `.loomux/` config or the dev docs, not the product (precedent: #263).
9. **Never self-approve a security/install gate** (npm's `allow-scripts`
   review, a `gh` shim confirmation, anything else that exists to make a
   human or the orchestrator decide). If one fires, stop and
   `message_orchestrator`/`report("blocked", …)` instead of running the
   approve command yourself — even a narrowly-scoped approval is a security
   decision, and it isn't yours to make unprompted (precedent: #357). The
   repo pre-declares the one approval the build genuinely needs
   (`package.json`'s `allowScripts` field, committed); if `allow-scripts` or
   any other gate fires for something new, the answer is still to ask, not
   to decide.
   **If you're staring at an `allow-scripts` warning right now:** the
   `package.json` entry pins esbuild to an exact version
   (`"esbuild@0.25.12": true`) on purpose, not by oversight — it's the safer
   of npm's two forms (a name-only entry would silently cover every future
   version too). That means a routine `esbuild` version bump (pulled in via
   `vite` or a direct upgrade) makes the gate fire again for the new version
   — that is the pin working as designed, not a fix that broke. The right
   response is the same as for a brand-new package: stop and get a fresh
   human approval for the new version. Do not bump the pinned version
   yourself, do not switch it to a name-only entry to make the warning stop
   recurring, and do not add any other package's entry alongside it —
   widening this the "convenient" way is exactly the self-approval this
   constraint exists to prevent.

## Code conventions

- Frontend logic that needs tests is extracted into DOM-free pure modules
  (`layout.ts`, `steer.ts`, `spawnexpiry.ts`, …) and tested in
  `test/*.test.ts` with `node:test` + `node:assert/strict`. DOM wiring is
  validated by hand — don't simulate a DOM in tests.
- Backend: unit tests inline under `#[cfg(test)]` only if they don't link the
  full lib; otherwise integration tests (constraint 4). Orchestration logic is
  covered in `src-tauri/tests/orchestration.rs`.
- **An edit to a role template (`orchestrator|worker|reviewer|planner.md` under
  `src-tauri/src/orchestration/templates/`) re-blesses
  `src-tauri/tests/fixtures/pre222/` in the same commit** — the fixtures pin those
  four templates byte-for-byte modulo registered placeholders
  (`block.md`/`workflow.md` are deliberately not fixture-pinned; an edit whose
  only change is a `{{...}}` placeholder registered in LIVE needs no re-bless —
  the pin strips registered keys before comparing, precedent #859). Signature: `a_workflow_placeholder_must_sit_at_the_end_of_a_line_it_shares`
  and `the_toggle_off_leaves_every_instruction_file_byte_for_byte_what_it_was` go red
  alone, on a round where nothing else moved. Procedure and re-bless log:
  `src-tauri/tests/fixtures/pre222/README.md` (#867, #868, #874).
- `src-tauri/src/orchestration/mod.rs` is tens of thousands of lines — grep for
  the function/struct, don't read it top to bottom.
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
- **Delete a PR's branch once it merges.** `gh pr merge --delete-branch`
  handles it, but skips the remote delete when a local worktree still holds
  the branch — after cleaning the worktree, verify with
  `git ls-remote --heads origin <branch>` and `git push origin --delete
  <branch>` if it survived. Whoever performs the merge owns this step (#662).
- GitHub issues are the work queue. Labels the orchestration workflow uses:
  `agent-managed` (an orchestrator owns it), `agent-ready` (groomed — go),
  `agent-investigation` (research only — post findings as an issue comment,
  no code), `agent-prototype` (build for demo/feedback).
- User-visible behavior changes must update the matching user-docs page under
  `docs/` (the README is a pitch, not a manual — only touch it when the pitch
  itself changes); substantial designs get a `doc/design/*.md` note.
- **Historical context lives in design notes, ADRs, and issue/PR history —
  never in user docs, this repo's own agent instruction files
  (`.github/agents/`, `.claude/skills/`, `.loomux/workflow.yml`), or this
  file.** Incident stories, superseded rules, dates, and "how we got here"
  narratives pollute every future reader's context. Reader-facing text
  carries the current rule and its operational why, with at most a bare
  issue/PR ref as provenance; strip any such narrative you find when
  editing these surfaces — including `.loomux/lessons.md`, which carries
  the rule and fix only, with refs as provenance. Out of scope: code
  comments (the "comments explain *why*" convention) and the shipped
  agent-role templates (`src-tauri/src/orchestration/templates/`, governed
  by their design notes).

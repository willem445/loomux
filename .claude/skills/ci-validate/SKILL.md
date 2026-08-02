---
name: ci-validate
description: Why agent workers never build or test Rust locally (hard ban — CI is the only cargo path) and how to validate through the draft-PR-early CI flow; the one permitted local check on `.rs` files is `rustfmt --check` as a parser; frontend node-only commands stay local.
---

# Local iteration vs. CI proof

## The decision rule

> **Local `cargo` of ANY kind is banned for agents** — no `cargo build`,
> no `cargo check`, no single-test iteration, nothing that invokes
> `rustc` (human hard directive, #488). A fresh worktree's first compile
> costs 5-8 GB of `target/` per worker, and a fleet building locally
> exhausts the disk. Scope caps don't cap the first compile; only not
> compiling does.
>
> **Everything Rust goes to CI**: push early, open the draft PR
> immediately (below), read the results. Iterate by reasoning about the
> code and pushing; CI is both the compiler and the proof.
>
> **Frontend-only commands that never invoke `rustc` stay local-OK**:
> `npm run build`/`tsc` (the typecheck), `npm test`/`node --test`, a
> single frontend test file. These cost megabytes, not gigabytes.
>
> **One local check on `.rs` files is permitted, because it isn't a
> build**: `rustfmt --check` parses. See the next section — run it before
> every push that touches Rust.

CI is the sole authority for the CI gate, and now also the sole build
path. A worker citing a local run as validation is citing evidence it
should not have been able to produce.

**Why this is absolute** (#488 lineage): a full disk has destroyed a live
task board (#133), crashed loomux outright (#464), and killed the app
mid-session twice more on 2026-07-30. CI spends GitHub's disk, not this
machine's. If your worktree has a `target/` from before this rule,
`cargo clean` it now.

## The syntax check: `rustfmt --check` (#558)

The ban above left workers with no way to know whether the Rust they just
wrote *parses*. The cheapest possible defect then cost a full CI round: a
scripted rewrite that cut at the first `);` — which happened to sit inside a
string literal — left a dangling fragment and an unbalanced brace, and all
three build jobs failed to compile, not on assertions but on parsing (#558).

**Run this before every push that touches `.rs` files**, from `src-tauri/`:

```sh
rustfmt --check --edition 2021 <changed .rs files> >/dev/null
```

Three things about that command line, each of which will bite you if
dropped:

- **`--edition 2021` is mandatory.** rustfmt's CLI defaults to edition 2015,
  where `async fn` is a hard parse error — without the flag you get
  confident, wrong `error[E0670]`s on perfectly good code. The crate is
  edition 2021 (`src-tauri/Cargo.toml`).
- **`>/dev/null` is deliberate — discard stdout, and the redirect is not
  optional.** `--check` prints a *formatting* diff (`Diff in …`) for anything
  not rustfmt-shaped, and this repo is deliberately not rustfmt-formatted:
  hundreds of lines for a small file, but **12,483 for
  `src/orchestration/mod.rs`** and **14,997 for a whole-crate run** — measured.
  Forget the redirect on the file you are most likely to be editing and you
  dump ~12k lines of diff into your own context, which costs far more than the
  CI round this check was saving. They are noise here, not findings.
- **Read stderr; the exit code is ambiguous.** It's `0` clean, `1` for a
  formatting diff *and* for a parse error, `101` for a lexer error. The
  reliable signal is that **problems go to stderr and formatting diffs go to
  stdout**: a healthy run prints *nothing* to stderr no matter what the exit
  code says. So with stdout discarded, **any output at all on stderr means
  stop and read it** — don't grep for a particular token. It may be a parse
  error (`error:` / `error[E….]:`), a lexer error, or a module rustfmt
  couldn't resolve, which arrives as `Error writing files: failed to resolve
  mod …` and contains no `error:` token at all. Fix it before pushing.

When a change spans many files, parse the whole module tree instead — rustfmt
recurses into child modules, and a parse error in a child is reported by name:

```sh
rustfmt --check --edition 2021 src/lib.rs >/dev/null
```

That takes ~3-5s for this crate. Keep the `>/dev/null`: without it this is the
~15,000-line case above.

### This is a syntax check, NOT a formatting gate

This repo has no lint/format gate and rustfmt is deliberately not enforced in
CI. Nothing here changes that. rustfmt is being used as the parser it
contains, and its opinions about formatting are explicitly discarded (that's
what `>/dev/null` is for). So:

- Never run bare `rustfmt` (without `--check`) on a repo file — `--check`
  writes nothing; plain `rustfmt` rewrites in place.
- Never commit a reformatting, and never "fix" a `Diff in …` line.
- Match the surrounding style, exactly as before.

### Why this is permitted under the ban

**rustfmt is a parser, not a build.** It doesn't invoke cargo, doesn't invoke
`rustc` for codegen, resolves no dependencies, produces no artifacts, and
writes nothing to `target/` — verified on a worktree that had no `target/`:
after a whole-crate run it still had none, `git status` was clean, and the run
took ~3-5s. So it sits inside both bans at once: the #320 CPU ban (no meaningful
CPU, nothing to contend across worktrees) and the #488 disk ban (zero bytes
written).

**`cargo check` and `cargo build` remain banned, and the argument above does
not extend to them.** `cargo check` resolves the dependency graph and builds
metadata for every dependency into `target/` — that first full dependency
compile, 5-8 GB per worktree, *is* the thing #488 banned. "It doesn't produce
a binary" was never the distinction; "it doesn't write to `target/`" is.
There is no `--parse-only`-shaped cargo invocation that qualifies.

### What it does not catch

Parse errors only. Anything semantic sails straight through — type errors,
unresolved names, borrow-check failures, and (verified) a `format!` with the
wrong number of arguments. A clean rustfmt run is **not** validation and is
never cited as one; it only buys back the CI round that a syntax error would
have wasted. The definition of validated below is unchanged.

## The Cargo.lock exception

One local command has always been fine regardless of anything above: `cargo
update --workspace` in `src-tauri/`, when the `release` skill has just
bumped the version in `Cargo.toml`. CI's `cargo check --locked` only
*verifies* the lock is consistent — `--locked` makes it fail rather than
write anything back, so a stale lock can never self-heal from CI. Something
has to regenerate the lockfile before it can be committed and pushed.

`cargo update --workspace` is dependency resolution scoped to the
workspace's own members — it re-reads the manifests and rewrites the lock,
but never invokes `rustc`. Prefer it over `cargo check` for this step. Don't
also run `cargo check --locked` locally afterward to "prove it's
consistent" — that's what the bump PR's own CI run is for.

## The CI path — draft-PR-early flow

For anything beyond the frontend-only and `rustfmt --check` steps above:

1. **Commit and push early.** As soon as there's one coherent commit — it
   doesn't need to be the finished change — push the branch.
2. **Open a draft PR immediately**, before the change is done:
   ```sh
   gh pr create --draft --title "..." --body "..."
   ```
   This starts the ubuntu/windows/macos CI matrix on that first commit. Every
   subsequent push to the branch re-runs it, so the change gets validated
   incrementally instead of in one big local run at the end.
3. **Read results, don't guess:**
   ```sh
   gh pr checks <pr>
   gh run view <run-id> --log-failed   # when a check failed, to see why
   ```
4. **Never block the turn on the checks — register, end the turn, act on the
   notice.** With loomux's `notify_when` MCP tool available:
   ```
   notify_when(kind: "pr_checks", pr: <pr>)
   ```
   …then **end your turn.** loomux polls on your behalf and types a
   `[loomux] …` notice into your pane when the checks resolve.

   Waiting for that result in the same turn is a **deadlock**, not merely
   slow: the notice is delivered by typing into the pane, and a pane that is
   mid-turn cannot take a delivery, so the turn waits on a resolution queued
   behind itself. It has already cost 20+ minutes and a human to break
   (#590/#577). So: **no `sleep`, no `gh pr checks --watch`, no poll loop, no
   shell command that blocks until CI finishes.** A single instantaneous
   `gh pr checks <pr>` to see where things stand is fine — it is *waiting*
   that is banned, not looking.

   A PR that goes `CONFLICTING` never gets checks at all — GitHub creates no
   check-suite with no clean merge ref to run against — so no amount of
   waiting produces them; the watch resolves right away with a distinct "is
   CONFLICTING" notice instead of hanging toward expiry, and that means
   rebase, not "still waiting on CI". That notice is also the *only* way you
   find out, which is exactly what a blocked turn suppresses.

   Only where `notify_when` genuinely isn't available — no loomux pane, so
   nothing is being delivered to you and nothing can deadlock — poll
   `gh pr checks <pr>` yourself at **60 seconds or slower, never a tight
   loop.**
5. **Iterate by pushing fixes.** Between pushes, the local steps available to
   you are the frontend ones and `rustfmt --check` (above) — never a cargo
   build or test, and neither is the thing you cite as passing.
6. **Re-derive every citation after the push it describes.** A run is green for a
   **SHA**, not for a PR. Every push and every rebase invalidates the run ids,
   run links and "green on all three platforms" already written into the PR
   body — and that text survives the push untouched, so nothing marks it stale
   for you or for the reader. After any push or rebase, re-derive each one:
   ```sh
   gh run list --branch <branch> --json headSha,databaseId,conclusion,workflowName
   git rev-parse HEAD
   ```
   A run counts as this PR's evidence only when its `headSha` **is** the head
   you are reporting on. Three stale-green citations reached review in a single
   batch — one run three commits behind head (#571), and the same pre-rebase run
   cited twice across two reviews of #588 — each caught by a reviewer, none by
   the worker who wrote it (#596).
7. **Mark the PR ready once green:**
   ```sh
   gh pr ready <pr>
   ```

## Definition of validated

The PR's checks are green on all three platforms **for the head you are
reporting on**. That — not a local `cargo test` run, capped or not — is the
evidence a worker cites for "the suite passes" in a PR description or a `done`
report, and a run id carried over from before a rebase is evidence about a
commit that is no longer there (step 6).

## E2E (Playwright) is CI's job, same line

See `doc/design/e2e-testing.md` for the mechanism, isolation model, and CI
status. The `e2e-windows` job is a fourth platform in the same sense as the
ubuntu/windows/macos matrix above — it's CI's job to run the full suite, not
yours. It also runs `continue-on-error: true`, so it never blocks the merge
gate the way the three build/test jobs do; don't read a red `e2e-windows` the
same way as a red `build` job. GitHub-hosted `windows-latest` executes the job
at High integrity level, and WebView2 Runtime 150+ intentionally drops the
`WEBVIEW2_*` env-var channel for an elevated host process as by-design
local-privilege-escalation hardening (MicrosoftEdge/WebView2Feedback#5640,
closed as completed — not a bug Microsoft will fix). `ci.yml` works around it
with the HKLM policy Microsoft names as the supported alternative — confirmed
working (see the design doc's "CI status" section) — so a red `e2e-windows`
today most likely means a real spec/app problem, not the runner's execution
context. Still `continue-on-error` regardless: a new job earns required-check
status with a track record, not on day one.

Locally, PoC-level smoke only, and only against the isolated E2E profile —
never against a real install:

- A single spec file (`npx playwright test e2e/tests/<name>.spec.ts`) to
  sanity-check a change before pushing is fine, same local line as a single
  `node --test` file above (the cargo ban stands; specs don't invoke `rustc`).
- The exe under test must always be the `tauri.e2e.conf.json`-identifier
  build (`npx tauri build --debug --no-bundle --config
  src-tauri/tauri.e2e.conf.json`) launched through `e2e/fixtures.ts`'s
  `LOOMUX_DATA_DIR` + isolated-profile handling. Never point `LOOMUX_E2E_EXE`
  at an installed build or skip the identifier override — that's the fix for
  #394's shared-WebView2-process hazard, and skipping it locally reintroduces
  exactly the collision-with-a-running-instance risk it exists to prevent.
- The full `npx playwright test` suite as "it passes" evidence is CI's job,
  same as the backend/frontend suites above — cite the `e2e-windows` run, not
  a local one, **once that job is actually green** for a given push. While
  it's failing on the known High-IL/WebView2 issue above, cite a local
  full-suite run instead and say explicitly that `e2e-windows` is expected-red
  for the documented reason, not silently ignore it.

---
name: ci-validate
description: How agent workers decide between a capped local build/test and CI for a loomux change — local iteration under the resource guard vs. CI as the sole proof authority — plus the draft-PR-early flow for reading CI results.
---

# Local iteration vs. CI proof

Lineage: #318 was the incident (every worker running `cargo build` at once
exhausted the host) → #320 was the interim response (a hard ban on any local
build/test) → #322 built the actual fix, a per-class concurrency guard that
caps how many CPU-heavy commands of the same kind can run at once and makes
extra callers wait instead of stacking → #331 replaces the hard ban with the
discretion model below now that the guard exists to make local execution
safe again.

## Precondition — confirm the guard is actually active

The discretion model below only applies once **all three** hold:

1. **PR #322 is merged into `main`.**
2. **Your group/session was (re)started after that merge.** Shims are
   generated per spawn from the compiled guard spec — a pane opened before
   the merge landed has no guard wired into its `PATH`, guard or no guard in
   the codebase.
3. **The human has set a nonzero `guardrails.resource_guard_limits` slot
   count** for the class you're about to run (`rust-build` covers `cargo
   build`/`check`/`test`; `node-build` covers `npm run build`/`npm
   test`/bare `node`, per this repo's own dogfooded
   `.loomux/workflow.yml`). This is a per-machine override the human sets in
   Guardrails, not something visible from the repo alone — ask if you're not
   sure it's set.

If any of the three doesn't hold, you have no concurrency cap protecting the
host — treat local builds/tests as still banned and go straight to "The CI
path" below until it does.

## The decision rule

> Would running it locally be faster or easier — a single-file test, an
> incremental `cargo check`, a quick `tsc` pass? **Do it locally**, capped
> (below) — the guard queues concurrent CPU-heavy callers of the same class
> rather than letting them stack.
>
> Do you need full-matrix proof — the merge-gate green, red-before-green
> evidence for a PR? **That's CI's job, always.** Local green is iteration
> speed. It is never proof.

CI remains the sole authority for the CI gate. A worker citing a local run
as "the suite passes" in a PR description or a `done` report is citing the
wrong evidence — cite the PR's CI run instead (see "Definition of validated"
below).

## Running locally, capped

Once the precondition holds, cap every local build/test invocation so it
can't starve the guard's own slot accounting or the host:

- **Always `-j 4` for agent local builds** (human directive): `cargo
  build -j 4`, `cargo check -j 4`, `cargo test -j 4`. Or set it once per
  session with `export CARGO_BUILD_JOBS=4` instead of repeating the flag.
  `-j` caps the **compile phase's** parallelism.
- **CPU-heavy test suites additionally take `-- --test-threads=4`** — a
  separate cap on the **test run's own** thread count, e.g. `cargo test -j 4
  -- --test-threads=4`.
- **npm/tsc need no flag** — they're single-process (`npm run build`, `npm
  test`, `node --test test/layout.test.ts`).

## The Cargo.lock exception (still applies regardless of the guard)

One local command was always allowed even under the old hard ban and stays
exactly as it was: `cargo update --workspace` in `src-tauri/`, when the
`release` skill has just bumped the version in `Cargo.toml`. CI's `cargo
check --locked` only *verifies* the lock is consistent — `--locked` makes it
fail rather than write anything back, so a stale lock can never self-heal
from CI. Something has to regenerate the lockfile before it can be committed
and pushed.

`cargo update --workspace` is dependency resolution scoped to the
workspace's own members — it re-reads the manifests and rewrites the lock,
but never invokes `rustc`. Prefer it over `cargo check` for this step. Don't
also run `cargo check --locked` locally afterward to "prove it's
consistent" — that's what the bump PR's own CI run is for.

## The CI path — draft-PR-early flow

For anything that needs full-matrix proof (or if the precondition above
doesn't hold yet):

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
4. **Watch without polling.** If a loomux `notify_when` MCP tool is
   available, register it and go idle instead of checking in a loop:
   ```
   notify_when(kind: "pr_checks", pr: <pr>)
   ```
   loomux polls on your behalf and types a `[loomux] …` notice into your pane
   when the checks resolve. If `notify_when` isn't available in this
   environment, poll `gh pr checks <pr>` yourself at a slow cadence —
   **60 seconds or slower, never a tight loop.**
5. **Iterate by pushing fixes.** Local iteration (capped, per above) is fine
   between pushes — it just isn't the thing you cite as passing.
6. **Mark the PR ready once green:**
   ```sh
   gh pr ready <pr>
   ```

## Definition of validated

The PR's checks are green on all three platforms. That — not a local `cargo
test` run, capped or not — is the evidence a worker cites for "the suite
passes" in a PR description or a `done` report.

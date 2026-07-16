---
name: ci-validate
description: How agent workers decide between a capped local build/test and CI for a loomux change — quick local iteration (always -j 4) vs. full/longer-running validation on CI — plus the draft-PR-early flow for reading CI results.
---

# Local iteration vs. CI proof

Lineage: #320 was the interim response to a hard-kill (every worker running
`cargo build` at once exhausted the host) — a hard ban on any local
build/test. A per-class concurrency guard was attempted (#318/#322) but
shelved (2026-07-16): its shim couldn't reliably intercept every invocation
path (PowerShell/cmd bypassed it), so the coverage wasn't worth the
complexity right now. What's below is the model that replaces both: no guard
involved, no precondition to check — just a plain cap on local jobs plus a
line drawn on scope/duration, not on any mechanism's state.

## The decision rule

> Quick/local iteration — a single-file test, an incremental `cargo check`,
> a quick build to sanity-check a change? **Do it locally**, capped at `-j
> 4` (below). There's nothing to gate this on; it's just a sane default so
> one agent's build doesn't eat all the CPU on a machine several agents
> share.
>
> Full/longer-running validation — the whole suite, multi-platform proof,
> anything you'd cite as "the suite passes" in a PR description or a `done`
> report? **That's CI's job.** Push, open the PR, and wait for it — don't
> run it locally on your own clock instead of waiting for CI.

CI remains the sole authority for the CI gate. A worker citing a local run
as full validation is citing the wrong evidence — cite the PR's CI run
instead (see "Definition of validated" below).

## Running locally, capped

Cap every local build/test invocation — unconditionally, not gated on
anything:

- **Always `-j 4` for agent local builds** (human directive): `cargo
  build -j 4`, `cargo check -j 4`, `cargo test -j 4`. Or set it once per
  session with `export CARGO_BUILD_JOBS=4` instead of repeating the flag.
  `-j` caps the **compile phase's** parallelism.
- **CPU-heavy test suites additionally take `-- --test-threads=4`** — a
  separate cap on the **test run's own** thread count, e.g. `cargo test -j 4
  -- --test-threads=4`.
- **npm/tsc need no flag.** `npm run build`/`tsc` is single-process. `npm
  test`/`node --test` actually parallelizes across test *files*, but it's
  lightweight enough on this codebase's suite size that no additional cap is
  needed.

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

For anything that's full/longer-running validation rather than quick local
iteration:

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
   **60 seconds or slower, never a tight loop.** A PR that goes `CONFLICTING`
   never gets checks at all — GitHub creates no check-suite with no clean
   merge ref to run against — so the watch resolves right away with a
   distinct "is CONFLICTING" notice instead of hanging toward expiry; that
   means rebase, not "still waiting on CI".
5. **Iterate by pushing fixes.** Quick local iteration (capped, per above) is
   fine between pushes — it just isn't the thing you cite as passing.
6. **Mark the PR ready once green:**
   ```sh
   gh pr ready <pr>
   ```

## Definition of validated

The PR's checks are green on all three platforms. That — not a local `cargo
test` run, capped or not — is the evidence a worker cites for "the suite
passes" in a PR description or a `done` report.

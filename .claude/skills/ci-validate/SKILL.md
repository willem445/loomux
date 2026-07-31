---
name: ci-validate
description: Why agent workers never build or test Rust locally (hard ban — CI is the only cargo path) and how to validate through the draft-PR-early CI flow; frontend node-only commands stay local.
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

> **Local `cargo` of ANY kind is banned for agents** — no `cargo build`,
> no `cargo check`, no single-test iteration, nothing that invokes
> `rustc`. Human hard directive, 2026-07-30, after the THIRD
> disk-exhaustion incident: the previous scope-based carve-out ("a single
> test you are actively iterating on is fine at `-j 4`") still cost each
> fresh worktree its first full dependency compile — 5-8 GB of `target/`
> per worker — and a six-worker fleet filled the drive and crashed loomux
> with every worker individually obeying the rule. Scope caps don't cap
> the first compile; only not compiling does.
>
> **Everything Rust goes to CI**: push early, open the draft PR
> immediately (below), read the results. Iterate by reasoning about the
> code and pushing; CI is both the compiler and the proof.
>
> **Frontend-only commands that never invoke `rustc` stay local-OK**:
> `npm run build`/`tsc` (the typecheck), `npm test`/`node --test`, a
> single frontend test file. These cost megabytes, not gigabytes.

CI is the sole authority for the CI gate, and now also the sole build
path. A worker citing a local run as validation is citing evidence it
should not have been able to produce.

**Why this is absolute** (#488 lineage): a full disk has destroyed a live
task board (#133), crashed loomux outright (#464), and killed the app
mid-session twice more on 2026-07-30. CI spends GitHub's disk, not this
machine's. If your worktree has a `target/` from before this rule,
`cargo clean` it now.

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
  sanity-check a change before pushing is fine, same "quick local iteration"
  line as a single `cargo test`/`node --test` file above.
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

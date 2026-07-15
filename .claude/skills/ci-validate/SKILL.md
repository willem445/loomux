---
name: ci-validate
description: How agent workers validate a loomux change via GitHub Actions instead of local cargo/npm builds — push early, open a draft PR, read CI results, iterate. Use this in place of running cargo check/test or npm run build/test on the host.
---

# Validating a change via CI, not local builds

`.github/workflows/ci.yml` runs on every PR: `cargo check --locked`, `cargo test
--locked`, `npm run build` (typecheck + bundle), and `npm test` — each across
ubuntu/windows/macos. That's the same coverage the `verify` skill's "Always
run" block describes for a human running it locally. Agent workers get it for
free from GitHub's runners and must use that instead of running it on the
host (#320): a hard-kill was caused by every worker in a group running `cargo
build` at once and exhausting the host.

**Rule of thumb: if it spawns a compiler or test runner, it goes to CI.**

## The trap

`npm run build` runs `tsc --noEmit` as its first step — that's a build, not a
free typecheck. Don't run `tsc --noEmit` locally either, standalone or via
`npm run build`. The only way to typecheck is to let CI do it.

## What stays OK locally

File edits, `git` operations (status/diff/add/commit/push/log), and reading a
single file to check its contents. None of these spawn a compiler or test
runner.

## Workflow

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
   **60 seconds or slower, never a tight loop** — the same host-overload
   failure mode this skill exists to avoid applies to polling too, just
   spread over time instead of concentrated at once.
5. **Iterate by pushing fixes**, not by re-running anything locally. Edit,
   commit, push; CI re-runs automatically.
6. **Mark the PR ready once green:**
   ```sh
   gh pr ready <pr>
   ```

## Definition of validated

The PR's checks are green on all three platforms. That — not a local `cargo
test --locked` — is the evidence a worker cites for "the suite passes" in a
PR description or a `done` report.

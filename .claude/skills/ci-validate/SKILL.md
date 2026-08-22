---
name: ci-validate
description: Why agent workers never build or test Rust locally (hard ban — CI is the only cargo path) and how to validate through the draft-PR-early CI flow; the one permitted local check on `.rs` files is `rustfmt --check` as a parser; frontend node-only commands stay local, after the `npm ci` a freshly-cut worktree needs first.
---

# Local iteration vs. CI proof

## The decision rule

> **Local `cargo` of ANY kind is banned for agents** — no `cargo build`,
> no `cargo check`, no single-test iteration, nothing that invokes
> `rustc` (human hard directive, #488; a cargo-intercepting shim is not
> the answer either — #318/#322). A fresh worktree's first compile costs
> **5-8 GB** of `target/` per worker, and a fleet building locally
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

### Run `npm ci` first — a fresh worktree has no `node_modules`

`node_modules/` is gitignored, so a worktree orrerix just cut for you has
**none**, and the frontend commands above do not work until you install.
The trap is that `npm test` *looks* like it mostly works anyway: the
DOM-free pure modules are tested with `node:test`/`node:assert` and Node's
own TypeScript stripping, which need no packages at all, so you get a
1246/1249-style pass with only the few files importing a real package red
(`ERR_MODULE_NOT_FOUND: Cannot find package '@xterm/headless'` — a package
plainly listed in `package.json`). `npm run build` fails less subtly
(`'tsc' is not recognized`). Neither is a broken test or a red `main`;
both mean *you never installed*. Install once per worktree, then re-run —
and never report a missing-package error as a suite failure.

**Why this is absolute:** a full disk takes the whole machine down with it
— the live task board, the app, and every worker in the fleet at once
(#488, #133, #464). CI spends GitHub's disk, not this machine's. If your
worktree has a `target/`, `cargo clean` it now.

## The syntax check: `rustfmt --check` (#558)

Without a local build there is nothing to tell a worker whether the Rust it
just wrote even *parses*, and a syntax error costs a full CI round on all
three build jobs before anything is learned. Bulk or script-generated edits
are the usual source — a rewrite that cuts at the first `);` finds the one
sitting inside a string literal (#558).

**Run this before every push that touches `.rs` files**, from the repo root
(paths are what matter to rustfmt, not the working directory, and `.rs` files
now live in more than one crate):

```sh
rustfmt --check --edition 2021 <changed .rs files> >/dev/null
```

Three things about that command line, each of which will bite you if
dropped:

- **`--edition 2021` is mandatory.** rustfmt's CLI defaults to edition 2015,
  where `async fn` is a hard parse error — without the flag you get
  confident, wrong `error[E0670]`s on perfectly good code. Every crate in the
  workspace is edition 2021 (`src-tauri/Cargo.toml`,
  `crates/loomux-engine/Cargo.toml`, `crates/loomux-server/Cargo.toml`).
- **`>/dev/null` is deliberate — discard stdout, and the redirect is not
  optional.** `--check` prints a *formatting* diff (`Diff in …`) for anything
  not rustfmt-shaped, and this repo is deliberately not rustfmt-formatted:
  hundreds of lines for a small file, but **15,513 for
  `src/orchestration/mod.rs`** and **18,094 for a whole-crate run** — measured.
  Forget the redirect on the file you are most likely to be editing and you
  dump ~15k lines of diff into your own context, which costs far more than the
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

That takes ~5s for this crate. Keep the `>/dev/null`: without it this is the
~18,000-line case above.

### This is a syntax check, NOT a formatting gate

This repo has no lint/format gate and rustfmt is deliberately not enforced in
CI. Nothing here changes that. rustfmt is being used as the parser it
contains, and its opinions about formatting are explicitly discarded (that's
what `>/dev/null` is for). So:

- Never run bare `rustfmt` (without `--check`) on a repo file — `--check`
  writes nothing; plain `rustfmt` rewrites in place.
- Never commit a reformatting, and never "fix" a `Diff in …` line.
- Match the surrounding style.

### rustfmt parses the *Rust* — not the shell or jq inside it

`src-tauri/src/orchestration/mod.rs` holds **three** generated shell scripts, each
in a `const TPL: &str = r#"…"#`: `gh_shim_sh` (850 lines, the merge gate),
`git_shim_sh` (101, the release/tag-push gate) and `loomux_shim_sh` (20, the
self-launch refusal). `workflow.rs`'s `BASE_*_JQ` consts hold jq programs the gh
shim interpolates. To rustfmt all of these are one string literal: it reports
nothing, and **no local check parses them at all**. A dropped `;;` or an
unbalanced quote therefore reaches CI intact, where it does not fail as one
test — the script stops parsing and every test of that shim dies at once, so a
mutation round taken on that push is unattributable and has to be discarded
(#1181).

Extract the literal and run the parser the language has, before pushing:

```sh
node .scratch/xtpl.cjs src-tauri/src/orchestration/mod.rs 'pub fn gh_shim_sh' \
  > .scratch/gh-shim.sh && sh -n .scratch/gh-shim.sh
```

**Anchor on the function name, never on `const TPL`.** All three shims spell the
literal identically and the extractor takes the *first* match, so a `const TPL`
marker returns the **gh** shim whichever one you were editing — you get `sh -n`
exit 0 on a script you never touched. That is the false green this whole section
exists to prevent, and on `git_shim_sh` it is a security gate. Use `pub fn
gh_shim_sh` / `pub fn git_shim_sh` / `pub fn loomux_shim_sh`; all three extract
clean (850 / 101 / 20 lines, `sh -n` exit 0 each).

(`.cjs`, not `.js` — the root `package.json` is `"type": "module"`. The extractor
is four lines: read the file, slice between the `r#"` after the marker and the
next `"#`.) The `__PLACEHOLDER__` tokens are ordinary words and parse fine, so
the un-substituted template is checkable as-is. `sh -n` is a parse, not a run —
nothing in the script executes.

**Read the exit code, not the line number.** Dropping the ` ;;` from one inline
`case` arm in the extracted gh shim was reported **12 lines downstream** of the
edit when measured — a parser tells you where it gave up, not where you broke
it, the same way Git Bash reports a quoting error far from the real line.
Nonzero exit plus `syntax error near unexpected token` is the signal; the line
is a starting hint. (A three-line synthetic reproducer will point straight at
the bug and mislead you about this — measure on the real artifact.)

**The jq consts have no local check** — there is no `jq` on this machine, and
gh's is `gojq`, reachable only through a network call. Their parser of record is
the CI fixture test (`the_base_green_reductions_reduce_real_payloads_to_the_right_word`),
which pipes committed payloads through real `jq`. It opens with a `have_jq()`
guard that prints `SKIP …` and returns, so it is green on a runner without `jq`
having parsed nothing — grep the job log for that `SKIP` line before treating it
as evidence. Add a fixture there rather than reasoning about what a reduction
returns.

### Why this is permitted under the ban

**rustfmt is a parser, not a build.** It doesn't invoke cargo, doesn't invoke
`rustc` for codegen, resolves no dependencies, produces no artifacts, and
writes nothing to `target/` — verified on a worktree that had no `target/`:
after a whole-crate run it still had none, `git status` was clean, and the run
took ~5s. So it sits inside both bans at once: the #320 CPU ban (no meaningful
CPU, nothing to contend across worktrees) and the #488 disk ban (zero bytes
written).

**`cargo check` and `cargo build` remain banned, and the argument above does
not extend to them.** `cargo check` resolves the dependency graph and builds
metadata for every dependency into `target/` — that first full dependency
compile, **5-8 GB** per worktree, *is* the thing #488 banned. The distinction
is not "it doesn't produce a binary"; it is "it doesn't write to `target/`".
There is no `--parse-only`-shaped cargo invocation that qualifies.

### What it does not catch

Parse errors only. Anything semantic sails straight through — type errors,
unresolved names, borrow-check failures, and (verified) a `format!` with the
wrong number of arguments. A clean rustfmt run is **not** validation and is
never cited as one; it only buys back the CI round that a syntax error would
have wasted. The definition of validated below is unchanged.

## The Cargo.lock exception

One local command is permitted regardless of everything above: `cargo
update --workspace` at the repo root (the Cargo workspace root), when the
`release` skill has just bumped the version in `src-tauri/Cargo.toml`. CI's
`cargo check --locked` only
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
   **`gh pr checks` exits non-zero for "not finished yet", not only for
   "failed": `8` means *checks pending*, `1` means a check actually failed.**
   A pane surfaces either as a red "Exit code N" tool error, so read the code
   and the per-check rows — treating an `8` as a failure sends you debugging
   green CI. The `E2E (Playwright, experimental)` row is the usual reason an
   otherwise-finished PR still reports `8`.

   Check the **command's** help, not the general page: `gh pr checks --help`
   ends with *"Additional exit codes: 8: Checks pending"*, while `gh help
   exit-codes` lists only 0/1/2/4 and never mentions `8` — it just warns that
   *"a particular command may have more exit codes, so it is a good practice
   to check documentation for the command."* Reading only the general page is
   how `8` comes to look like an anomaly rather than a documented state.
4. **Never block the turn on the checks — register, end the turn, act on the
   notice.** With orrerix's `notify_when` MCP tool available:
   ```
   notify_when(kind: "pr_checks", pr: <pr>)
   ```
   …then **end your turn.** orrerix polls on your behalf and types a
   `[orrerix] …` notice into your pane when the checks resolve.

   Waiting for that result in the same turn is a **deadlock**, not merely
   slow: the notice is delivered by typing into the pane, and a pane that is
   mid-turn cannot take a delivery, so the turn waits on a resolution queued
   behind itself, and only a human can break it (#590/#577). So: **no
   `sleep`, no `gh pr checks --watch`, no poll loop, no
   shell command that blocks until CI finishes.** A single instantaneous
   `gh pr checks <pr>` to see where things stand is fine — it is *waiting*
   that is banned, not looking.

   A PR that goes `CONFLICTING` never gets checks at all — GitHub creates no
   check-suite with no clean merge ref to run against — so no amount of
   waiting produces them; the watch resolves right away with a distinct "is
   CONFLICTING" notice instead of hanging toward expiry, and that means
   rebase, not "still waiting on CI". That notice is also the *only* way you
   find out, which is exactly what a blocked turn suppresses.

   Only where `notify_when` genuinely isn't available — no orrerix pane, so
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
   you are reporting on. A citation that survives a rebase untouched is the
   defect a reviewer catches and its author never does (#571, #588, #596).
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

A run id also goes stale on a **rerun** — same id, same `headSha`, new verdict —
which the `headSha` check structurally cannot catch, because nothing about the
commit changed. Re-read `run_attempt` and `conclusion` (`gh api
repos/<owner>/<repo>/actions/runs/<id>`) at the body gate, and never write a
present-tense claim about state *outside* your diff — "`main` is red", "merges
are frozen" — into a body a squash turns into the permanent commit message: past
tense with the attempt named, or drop it. Signature: a body citing a failure a
rerun has since cleared (#1196).

## Red-before-green evidence goes through CI too

Every PR owes its new tests seen *failing* without the change. With local
`cargo` banned there is no base-branch test run to produce that red, and
`git stash` is separately forbidden (one stash stack across every worktree,
#299) — so the red half is produced on a **throwaway scratch branch with its
own draft PR**, and CI's log is the failure line you quote.

1. **Commit your real work first** (#493) — the scratch edits are destructive
   and a `git checkout --` to undo them takes everything uncommitted in the
   file with it.
2. Cut `scratch/<issue>-red-<n>` from your branch head, set **one** behaviour
   aside — leave everything else wired — and push. One branch per behaviour,
   numbered, so a wave can go out together (see below).
3. Open it as a draft titled `[scratch] … — do not merge`, body saying which
   single behaviour is neutered and that every failure line will be quoted in
   the real PR.
4. Quote the run link and the failure lines in the real PR body; **close the
   scratch PR and delete its branch** once cited.

**Prefer one branch per round, pushed as a wave.** Reusing one scratch branch
(below) still works — it just serialises rounds that are independent, at a full
CI cycle each: #1196 cut five branches instead, queued within 14 s of each other
and all conclusive 14 min later, against ~64 min of serialised run time. A branch
per round also keeps each red citable at its own SHA, so when the two rules below
retire one round's evidence — a watched red is dated to the commit it was watched
on, and transfers only if you SHOW it does — only that round is re-cut, and the
others stay citable where they are. Bound it by the job list, not by a
remembered product: `ci.yml` today runs **four** jobs per run — `build` on
`ubuntu-22.04`, `windows-latest` and `macos-latest`, plus `e2e-windows` — so a
five-round wave is **twenty** concurrent jobs. Re-read that list rather than
this sentence if `ci.yml` gains or loses a job, and don't launch a wave against
the green run you are waiting on (#1196).

**One behaviour per round.** Two at once, or a neuter that stops it compiling,
and the failures stop being attributable to the behaviour they evidence — a
compile error proves nothing. Several rounds on one scratch branch is normal.

**Your mutation must not leave the token a source-shape pin greps for.** Several
guards here assert on the *text* of a generated artifact (`sh.contains("diff-too-large")`,
the scans in `tests/groupid.rs` / `tests/pathseg.rs` / `tests/perf_dispatch.rs`).
Comment the behaviour out and name it in the comment — `// removed the
diff-too-large refusal` — and the token is still in the file, so the pin stays
**green while the behaviour is gone** and you bank a coverage claim nothing
evidenced. Delete the lines, or comment them with wording that contains none of
the strings the pins match, and re-run the identical mutation if you got a green
you did not expect. Signature: a mutation reddens the behavioural test and its
source-shape sibling stays green (#1181).

**A watched red is dated to the commit it was watched on.** The mutation table
in a PR body is a set of hand-derived claims, and a later review round that
rewrites the mutated lines retires the red measured on them as silently as a
rebase does — the test name still exists, so nothing goes red to tell you. On
each round, re-check every earlier row against the shipped tree and mark the ones
whose site no longer exists as **superseded** rather than restating them. Better,
stop the table needing that: promote the superseded expression into a permanent
in-suite witness — a literal copy of the *old* reduction, deliberately not
derived from the live constant, asserted against a committed fixture (`BEFORE_ROUND_1`
in `src-tauri/tests/mergequeue.rs`). The red then lives in the suite instead of on
a deleted scratch branch (#1181).

**And where the site still EXISTS, the red transfers only if you SHOW it does.**
That is the commoner half of the paragraph above: rebases land mid-review here, so
the head a PR merges from is rarely the one a scratch round was cut on, and a
mutated site that is still there reads as still-valid without being it. What
carries the red is byte-identity of the regions the round mutated and of the tests
it reddened, at both commits. `git range-diff <old-base>..<old-head>
<new-base>..<new-head>` marking every prior commit `=` is the shortcut for a pure
rebase — but only once `git merge-base --is-ancestor <old-base> <new-base>` has
exited 0. All-`=` says the patches match, never that the bases are related: two
merely-diverged bases report a clean all-`=` too (measured), and then the tree
around your mutation is not the tree you measured on. Name the commit each round
descends from, never "the current head" (#1182 — rounds F/G/H, four rebases).

**A red only counts against a banked green.** Keep the unmutated tree's passing
run for the same tests: a test that has never passed reddens for its own bug,
not for your mutation, and the red then evidences nothing about the property.
And a mutation that **hangs** the suite is a timeout, not a red — the job dies
without naming an assertion, so there is no failure line to quote. Race a
watchdog inside the test so the failure arrives as an assertion instead (#744).

### The frontend half runs its base red locally — build the isolated tree right

Everything above is the Rust path. Node commands are not banned, so a frontend PR
produces its own red locally — but checking out the base is not the way: a new
`src/*.ts` module does not exist there, so every test in the file dies on a
module-load error, which masks the behaviour instead of evidencing it. Build an
isolated tree from the BASE's `src/` and copy in **only** the new module, so the
imports resolve and everything the assertions are *about* is still the base's.

**Then check what those tests read off disk.** Guards here are source-scanning by
convention (CLAUDE.md), so `test/*.test.ts` files `readFileSync` other `src/`
files as text — a file missing from the isolated tree reddens the tests that read
it with `ENOENT`, and that red is a harness artefact, not evidence. Quoting one is
the easiest mistake in this flow. Measured on #1189's tree: the complete isolated
tree gives `tests 16 / pass 14 / fail 2`; the same tree with `src/pane.ts` removed
gives `fail 3`, the extra being `pane.ts arms its fit timer from this policy`
(`test/resizeburst.test.ts:482`). Read every failure's *reason*, and re-run after
copying in whatever failed on `ENOENT` (#1189).

### The trap: `cargo test` stops at the first failing test *binary*

Neutering a lib function reddens the **lib** suite, and cargo then never runs
the integration binary at all — so every integration-level assertion you meant
to evidence produces no output, and the round is wasted. Split by target:

- To redden a **unit test in `src-tauri`'s own lib** (e.g.
  `src/orchestration/digest.rs`), neuter the pure function. That lib suite is
  the first binary, so it is reached. Check where the file lives before
  picking it: #888 is moving `orchestration/` modules into the engine crate
  one batch at a time, and a file that moved is governed by the next bullet,
  not this one.
- To redden a **unit test in `crates/loomux-engine/src/`** (e.g.
  `workflow.rs`) **or in `crates/loomux-server/src/`**, the same move works but
  the ordering is against you. The observed order of a `cargo test --locked
  --workspace` run is: `loomux_lib` unit tests → `loomux` bin → **every**
  `src-tauri/tests/*` integration binary → `loomux_engine` unit tests →
  `loomux_server` lib unit tests → `loomux_server` bin unit tests → doc-tests.
  So the two `crates/` members run **last**, with `loomux-server` last of all,
  and a plant that reddens anything earlier — `src-tauri` lib, any integration
  binary, or the engine — stops the run before they execute, which means the
  red says nothing about them. Pick a plant the rest of the suite does not
  catch, and read the earlier targets passing in the same run as part of the
  evidence. (Order read off a real green run's log, not inferred: run
  31888299216 on PR #1023. Re-read it rather than trusting this line if cargo's
  scheduling ever looks different.)
- To redden an **integration test in `src-tauri/tests/`**, neuter the
  **wiring** instead — the call site, or the gate's consumption of the value —
  and leave the lib function intact, so the lib suite stays green and the
  integration binary is actually reached.

The same stop-at-first-failure also bounds a round that **did** hit its target:
if the property is pinned in two binaries, the later one never ran, so it is
covered by inspection, not by a watched red — say which half moved rather than
claiming both (#1181; the repo rule it instantiates is CLAUDE.md's "a red
evidences only the assertion it REACHED and MOVED").

Precedent: #869 (scratch PRs #870, #872).

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
with the HKLM policy Microsoft names as the supported alternative, and that
workaround is confirmed working (see the design doc's "CI status" section) —
so a red `e2e-windows` most likely means a real spec/app problem, not the
runner's execution context. Still `continue-on-error` regardless: a job earns
required-check status with a track record, not on day one.

Locally, PoC-level smoke only, and only against the isolated E2E profile —
never against a real install. **This local path is for humans**: producing
the exe under test requires `npx tauri build`, a full `rustc` compile, which
the cargo ban covers — agents validate E2E through the CI job only.

- A single spec file (`npx playwright test e2e/tests/<name>.spec.ts`) to
  sanity-check a change before pushing (against an exe a human already
  built) is the local line here.
- The exe under test must always be the `tauri.e2e.conf.json`-identifier
  build (`npx tauri build --debug --no-bundle --config
  src-tauri/tauri.e2e.conf.json`) launched through `e2e/fixtures.ts`'s
  `ORRERIX_DATA_DIR` + isolated-profile handling. Never point `LOOMUX_E2E_EXE`
  at an installed build or skip the identifier override — that's the fix for
  #394's shared-WebView2-process hazard, and skipping it locally reintroduces
  exactly the collision-with-a-running-instance risk it exists to prevent.
- The full `npx playwright test` suite as "it passes" evidence is CI's job,
  same as the backend/frontend suites above — cite the `e2e-windows` run for
  the head you are reporting on, never a local one. A red `e2e-windows` is a
  real failure to investigate and say something about, not background noise;
  it simply doesn't block the merge gate (`continue-on-error`). A substitute
  local full-suite run is a human's to produce — the exe build is `rustc`.

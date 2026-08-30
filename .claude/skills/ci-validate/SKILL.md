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
in a `const TPL: &str = r#"…"#`: `gh_shim_sh` (1007 lines, the merge gate),
`git_shim_sh` (106, the release/tag-push gate) and `loomux_shim_sh` (25, the
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
clean (1007 / 106 / 25 lines, `sh -n` exit 0 each).

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
   This step covers the **run** citations only. Commit SHAs quoted in prose go
   stale on the same rebase and stay locally resolvable, so nothing here catches
   them — see *Commit SHAs go stale differently from run ids* below (#1327).
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

### Commit SHAs go stale differently from run ids — check them against the PR ref

Step 6 re-derives run ids because `headSha` makes them checkable. The **commit
SHAs** a body cites in prose — "all eight addressed in `601594c`", "checking out
`bed9fe0` reproduces this exactly" — come through a rebase *unchanged and still
resolvable*, which is why the same worker re-derives one and not the other.

Two checks that look like they would catch it, and do not:

- **`git cat-file -e <sha>` passes for every orphan.** The pre-rebase objects are
  still in your own object store (and in a reviewer's, if they fetched the earlier
  heads), so every stale SHA in the body resolves on the two machines that look at
  it, and on nobody else's.
- **Ancestry against `main` fails for the good SHAs too.** The PR squashes, so no
  branch commit is an ancestor of `main` afterwards — a pass/fail split this
  cannot produce.

The frame that separates them is the PR's own ref, which is what a reader still
has after the squash:

```sh
# the LEADING + is load-bearing: the PR ref moves non-fast-forward on every force-push,
# which is exactly the case this check exists for. Without it a re-run is rejected, the
# local ref stays at the PRE-rebase head, and the orphans ARE ancestors of that — so the
# check reports pass on the one defect it was written to catch.
git fetch origin +refs/pull/<n>/head:refs/tmp/pr<n>
# every SHA-shaped token in the POSTED body (gh pr view <n> --json body)
git merge-base --is-ancestor <sha> refs/tmp/pr<n>; echo "  ancestor=$? $(git log -1 --format=%s <sha>)"
```

Ancestry alone is not enough: a SHA can be reachable and still name the wrong
commit, so assert the **subject** matches the role the body assigns it. Recover a
rewritten mapping from `git reflog` (or the pre-rebase head you still have),
never by position — the rebase may have reordered or squashed.

Do this at the body gate, with the quote check below. Editing the body afterwards
makes `body-unchanged` refuse a recorded verdict and reopens the gate: that is the
mechanism working, and the fix is to ping the reviewer to re-record, not to leave the
SHAs wrong. Signature: run ids re-derived after a rebase, commit SHAs beside them not
— including the one the body tells a reader to check out (#1327).

### A SHA on ANOTHER open PR's branch: cite the subject, check their ref

The check above frames a SHA against *your* PR's ref. A body or comment that cites a
**sibling PR's** commit — "#1470 already carries `829ea8a4`, which prunes this set" — has
no such frame: that branch force-pushes on its own schedule, between your rounds, with
nothing on your side changing. Both traps above still apply and neither check fires,
because the orphan is in someone else's history:

```sh
# the SIBLING's number, not yours; leading + for the same non-fast-forward reason
git fetch origin +refs/pull/<sibling>/head:refs/tmp/pr<sibling>
git merge-base --is-ancestor <sha> refs/tmp/pr<sibling>   # non-zero = orphaned
git log refs/tmp/pr<sibling> --oneline --grep '<subject>'  # what to cite instead
```

Cite the **commit subject**, which survives their rebase. Two extras this direction adds:
the citation can rot with your PR already merged, so re-check it at the body gate of every
round rather than only after your own rebase; and a SHA that ships in a **code comment**
outlives the body entirely — no gate re-reads it, so it must be a subject from the start.
Signature: a cross-PR SHA that still `git cat-file -e`s locally and is an ancestor of
nothing the sibling still publishes (#1487 N5).

### The re-stamp that fixes those SHAs corrupts the ones naming a BASE

Sweeping the body and re-stamping every SHA to the new head is the obvious cure for the
**Commit SHAs go stale differently from run ids** section above, and it is a defect generator:
a base citation (`cut from <sha>`, `whose parent is`) must NOT move, and no sweep can tell it
from a head citation. `cat-file` and that section's ancestry check both PASS on the corrupted
line, because it now names this PR's own head; only the subject check can catch it, and only
if you ask what role that sentence assigns the SHA.

```sh
# temporal: a scratch round cannot be cut from a commit that did not exist when it ran
git log -1 --format=%cI <cited-base>       # vs the run's createdAt (gh run view <id> --json createdAt)
# derive the base rather than naming it, so a reader checks it in one command
git fetch origin +refs/pull/<scratch-pr>/head:refs/tmp/pr<scratch-pr>   # not local otherwise
git rev-parse refs/tmp/pr<scratch-pr>^     # "#1432's head e30e92ae, whose parent is ed299ebb"
```

Best of the three is neither check: **delete the base claim from any line that does not need
one** — a line naming no base cannot go stale. Then census rather than sample: enumerate every
SHA-shaped token in the POSTED body and check each against what its own sentence claims it is.
Signature: two adjacent "cut from" sentences naming different SHAs for one round, the second
twelve lines from a structurally identical finding a reviewer had already read past (#1429 B2
round 4, B3 round 5 — three instances in two rounds, plus one the worker self-caught).

### A range's baseline is a *different* check from a SHA's ancestry

The check above asks *is this SHA still on the branch*. A body that cites a **range** —
`git diff <base>..HEAD --stat` as the proof that nothing else bled into the diff — is
asking a different question, and every check above answers it wrongly. After a second
rebase the commit you rebased onto the *first* time is still resolvable, still an
ancestor of the PR ref, and still carries the subject the body names it by. It is simply
no longer the **merge base**, so the diffstat silently absorbs everything `main` landed
in between — a number that is freshly measured, obeys `Every number in a PR body`, and
is wrong.

```sh
git fetch origin +refs/pull/<n>/head:refs/tmp/pr<n>
# the second fetch is load-bearing: an explicit refspec does NOT move origin/main, and a
# long-lived worktree's copy dates from session start. Merge-basing against a stale one
# returns the PREVIOUS rebase's target — a valid, correctly-subjected ancestor, and the
# exact wrong baseline this section exists to catch.
git fetch origin main
BASE=$(git merge-base refs/tmp/pr<n> origin/main)   # the ONLY baseline an isolation claim may cite
git diff --stat "$BASE"..refs/tmp/pr<n> | tail -1
```

Re-run it after **every** rebase, not just the last one, and re-derive the per-file
deltas beside it so they still sum to the total. Signature: an isolation diffstat whose
baseline passes ancestry *and* subject and whose file count is two high — 63 files/314+
against the previous rebase's target, 61/258+ against the merge base (#1324).

### Rebase NEUTRALITY is a third question, and the head-to-head diff answers a different one

The section above fixes the baseline for ONE measurement. *Did this rebase change my patch?* is
not that question, and `git diff <old-head> <new-head>` does not ask it: it reports everything
the **new base** absorbed, so a rebase that replayed your work untouched still prints that base's
own commits. Measured on #1755 (`fb48d73a` rebased to `fe3cf20e` onto `baaa2d9e`), where the
rebase left the PR's own content byte-identical in both files — `SKILL.md` included, which the PR
itself edits by 8 lines. Those two SHAs are the pre- and post-rebase heads of the since-deleted
branch `ci/1685-scratch-matrix`, so they resolve only where a stale ref survives; the merged
squash is `a3ba6e48` and `refs/pull/1755/head` is `7a913855`:

```sh
git diff --stat fb48d73a fe3cf20e
#  .claude/skills/ci-validate/SKILL.md | 14 ++++++++++++++
#  CLAUDE.md                           |  7 +++++++
#  2 files changed, 21 insertions(+)    <- ALL of it the new base's own work
```

Ask it of each head against its OWN merge base. `git range-diff` is the one-line form:

```sh
OLD_BASE=$(git merge-base origin/main <old-head>)
NEW_BASE=$(git merge-base origin/main <new-head>)
git range-diff "$OLD_BASE..<old-head>" "$NEW_BASE..<new-head>"
#  1:  fb48d73a = 1:  fe3cf20e     <- '=' means the patch replayed unchanged
```

Do **not** settle it by byte-comparing the two `git diff "$BASE"..<head>` patches. They differ on
the `index <blob>..<blob>` line and on hunk-header offsets for any file whose base moved lines
above your hunk — on #1755 SKILL.md went `@@ -845` -> `@@ -859`, while `ci.yml`, which the base
did not touch, kept a byte-identical patch — so a naive `diff` of the two false-FAILs in exactly
the case it was reached for. Strip those two line kinds, or use `range-diff`.

An `=` from `range-diff` **outranks** a raw head-to-head diff that disagrees; they are answers to
different questions, not two opinions on one. Signature: a blocking finding citing insertions the
rebase absorbed from the new base — in a file your patch also touches, or one it does not —
recorded beside a `range-diff` the same review already ran and got `=` on (#1755: +14 in
`SKILL.md`, a file the PR's own patch edits by 8 lines; raised in round 2, corrected in round 3).

### `gh pr diff --patch` is the wrong instrument for a diffstat

Measure a PR's diffstat from the **plain** `gh pr diff <n>` (its net diff) or from the
merge-base `git diff --stat` above. `--patch` emits the **commit series** — `git
format-patch` form, one patch per commit — so `git apply --stat` counts a file once per
commit that touches it and adds back every line an intermediate commit wrote and a later
one removed. `gh pr diff <n> --stat` is not a flag at all (`unknown flag: --stat`, gh
2.95.0), and Step-0-style "stop on any error" rules turn that into a refusal.

```sh
gh pr diff <n> > diff.txt && git apply --stat diff.txt | tail -1        # net — correct
gh pr diff <n> --patch | grep -c '^diff --git'                          # entries: per commit
gh pr diff <n> --patch | grep '^diff --git' | sed 's|.* b/||' | sort -u | wc -l   # real files
```

Signature: a diffstat several times the PR's real size, stated confidently because the
command ran clean. Measured on #1395's head `cebb64c2` (9 commits, 10 files):
`--patch | git apply --stat` reports **27 files / 1400+ / 112-** where the plain diff
reports **10 / 1320+ / 32-** — 27 entries over 10 unique paths. The control is a branch
whose commits touch **disjoint** files: merged #1352 (2 commits, 3 files) gives
`3 files changed, 5 insertions(+), 7 deletions(-)` from **both** forms, which is why the
mistake survives a small-PR test. Inflation tracks per-file overlap, not commit count
(#1395 B1).

### Body quotes are checked against head, never eyeballed

A body that **quotes** a passage out of a file in its own diff has made a claim a
machine can settle, unlike a paraphrase: pull each quoted passage out of the
*posted* body and assert it appears in the file the body names it from, at head.
Whitespace-normalise both sides before matching — the body rewraps, the file
carries its own hand-wrap and CRLF — and read the file with
`git show <head>:<path>`, blob-to-blob, never against a worktree copy.

Three things keep it from being decorative:

- **Scope it to the section describing the shipped text** (*What changed*).
  Evidence sections legitimately quote strings that are absent from head by
  design — a mutation that was reverted, a phrase the PR removed, a check's own
  label — so a whole-body match reddens on exactly the disclosure you want
  people to write. State where the scope ends wherever you report the run.
- **Print the passage count, and mutation-test the check** before citing it, per
  *a coverage claim is a claim*: a parser that matched zero passages exits green
  and proves nothing, and a check nobody has seen redden is not evidence. Splice
  the superseded phrasing back into a copy of the body and confirm it reddens on
  that quote and only that quote.
- **Harvest blockquotes, and report what you did not harvest.** The blockquote is
  the form to scope to. An *inline* quotation — a phrase in parentheses, a clause
  folded into a sentence of your own — is out of the harvest on purpose: inlining
  licenses a re-casing or an elision that no matcher can tell from drift, so
  `An orrery` quoted mid-sentence as `an orrery` is a true claim that reddens.
  Check those by hand and name them, because `1 passage checked` where the
  section held three is a scope claim, not a pass.

Signature that you needed this: the body's *What changed* quotes the exact
phrasing a later commit on the same branch removed, and the squash then
republishes it permanently (#1271).

### A claim-purge sweep is wrap-insensitive, or it is blind — and its receipt is NON-ZERO

CLAUDE.md's *correcting a false claim is a multi-surface edit* sends you to grep the
ENTITY a claim names. `grep` is line-oriented and prose and rustdoc here are
hand-wrapped, so a multi-word pattern cannot match an instance the wrap split — and
every control the zero-receipt rule mandates still passes, because the sweep DID match
the unwrapped copies. The receipt is non-zero and reads as healthy. The
whitespace-normalisation the quote harvest above already mandates is what the sweep
needs too; run two passes and reconcile their totals. (The section BELOW is the other
blind non-zero receipt: there the pattern reads the DIFF and misses an insert with no
context line above it. Same symptom, different cause — this one is the LINE form,
that one the DIFF form.)

**Discovery is a SINGLE TOKEN.** A phrase can be split by a wrap, so only one token
cannot — shortening a phrase to a shorter phrase does not reach the property. Take the
rarest token of the ENTITY the claim names, and widen from there.

```sh
# 1. DISCOVERY - one token, no space. This pass bounds everything below it.
LC_ALL=C.UTF-8 grep -rn --include='*.rs' --include='*.md' 'try-lock' <roots>

# 2. CONFIRMATION - ONE grep, recursive over the same ROOTS, never over a file
#    list derived from pass 1 (such a list cannot surface a file pass 1 missed).
#    -z makes each FILE one record, so the pattern may cross a line break; spell
#    every space of the phrase as [\s/!]* to absorb the break and whatever
#    comment marker the next line starts with. -P needs a UTF-8 locale (the
#    zero-receipt rule above), and the exit status is grep's own: 0 found, 1 not.
LC_ALL=C.UTF-8 grep -rPzoH --include='*.rs' --include='*.md' \
  'no timeout,[\s/!]*no[\s/!]*try-lock' <roots>
```

Matches print NUL-separated; `| tr '\0' '\n'` renders them, and discards the exit
status while doing it — the same trade as the `| wc -l` the zero-receipt rule warns
about. Quote the sweep without the pipeline.

Where the two disagree, the difference is what a phrase sweep could not see. Worked
instance, re-runnable off `git archive 24a428a6 crates src-tauri doc`: the plain phrase
`grep -r "no timeout, no try-lock"` returns **2** over
`crates/loomux-engine/src/published.rs`, `src-tauri/tests/perf_dispatch.rs` and
`src-tauri/src/orchestration/views.rs` (`views.rs:9`, `perf_dispatch.rs:1512`) and misses
`published.rs:7` and `perf_dispatch.rs:81`, where the phrase reads `no timeout, no` /
`try-lock` across a comment line break. Pass 2 returns **4** over those three and **5**
over the whole 187-file tree, the fifth being `doc/design/polled-views.md` — which only
a recursive pass reaches. It is one process: **0.15s** over those 187 files, exit **0**;
the same sweep for a term the tree does not carry exits **1**.

**The residual, because the two totals agreeing is not proof of completeness.** Both
are bounded by the TOKEN you chose — pass 1 greps it, pass 2's phrase contains it —
so a claim whose token was itself reworded is invisible to both while the totals still
agree: the same healthy receipt this section exists to kill, one level up. That is why
pass 1 takes the ENTITY's token and not the phrasing you rewrote; where no single token
is stable, the sweep cannot certify completeness and the claim needs reading, not
grepping.

Signature that you needed this: a sweep receipt you already published as complete, and a
re-sweep that finds more — #1346 went 1 → 2, #1408 3 → 4, #1667 2 → 4
(#1158, #1191, #1283, #1344).

### A diff-shaped sweep is controlled DIFFERENTIALLY, not by a match-somewhere control

A sweep whose pattern reads the **diff** — `+` lines, `-` lines, a `+x` sitting under
a *context* `y` — carries a blindness that a same-shape positive control cannot see.
It matches only where the neighbourhood is unchanged, so an insert into a run of lines
the same PR added has no context line above it and never matches. The control that
decides is the **defective blob itself**: run the sweep over `<base>..<pre-fix head>`
as well as `<base>..<fixed head>` and require the **hit set to change**. Identical hit
sets mean the sweep never saw the defect and cannot certify it was the only instance —
however many hits it printed, and however cleanly a positive control matched.

Worked instance, re-runnable at `b6c1beae`: CLAUDE.md's #1229 doc-splice signature
(`+///` directly under a context `///`) over #1426's `mod.rs`. `4e5a537e..cd164c72`
(the tree that CONTAINED the splice) and `4e5a537e..a8ed6604` (the fixed tree) return
the **same two hits** under `git diff` (the squashed range diff; `git log -p` over the
same ranges yields four on each side — the identity is form-invariant, but a receipt
that does not name its form reads as a contradiction to whoever reproduces it in the
other one), neither of them the defect — the whole `has_live_manager` doc
block is `+` lines, so no context `///` sits above the bad insert. The signature is
recognisable in the FILE and not in the diff, so reading the blob finds it and the
sweep never fires.

**The capable instrument for that class is a name-keyed doc census**, blob-vs-blob
(never against a worktree copy — CRLF, and the `<ref>:<path>` dot-directory trap):

- read each blob with `git show <ref>:<file>`;
- for every function definition line, walk *up* past any attribute lines and record
  whether the line above starts with a doc comment — keyed by the function's NAME;
- diff the two maps: which names GAINED a doc block, which LOST one, and the fn count
  at both ends as the population control.

At #1426's own transition (`cd164c72` to `a8ed6604`): the fn population is UNCHANGED
across the two blobs — that invariance is the control, not the number itself, which is
relative to your fn-matching pattern (two independent censuses of this transition
counted 986 and 1005 and decided identically). Exactly
one name gains a block (`live_delegate_count`), zero lose one — the splice is solely
fixed, which is what the body's sweep claimed and could not show.

**The census has the mirror blind spot, and it is worth stating.** Run `4e5a537e` to
`cd164c72` — base to the DEFECTIVE head — and it reports **zero** losses, because the
stolen doc block was itself authored by that PR: nothing was lost relative to base.
So neither instrument catches this prospectively. What does is reading every doc block
the diff ADDS against the `fn` it now sits on: a rustdoc summary line is the FIRST line
of the block, so if it describes some other function, the block is spliced. That is a
handful of blocks per PR, and it is a read, not a sweep.

Signature that you needed this: a sweep receipt whose hit count is non-zero and whose
hits do not include the finding the sweep was written for (#1426 B3, round 3).

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
others stay citable where they are. That holds only while nothing the survivors
*reach* has moved either: once a review fix lands mid-wave, re-cut the wave
rather than defend the survivors ("that criterion does not close", below).
Bound the wave by the job list, not by a remembered product: `ci.yml` today
runs **four** jobs per run — `build` on `ubuntu-22.04`, `windows-latest` and
`macos-latest`, plus `e2e-windows` — so a
five-round wave is **twenty** concurrent jobs. Re-read that list rather than
this sentence if `ci.yml` gains or loses a job, and don't launch a wave against
the green run you are waiting on (#1196).

**Bank the base green at the wave's own SHA — and re-derive `origin/main`
immediately before you push.** "A red only counts against a banked green" (below)
is a citation rule for one round; for a wave it is a *launch precondition*,
because whatever breaks the tree breaks every round identically and a compile
error evidences nothing. **Your green is dated to the `main` it merged against,
not only to your SHA**: `ci.yml` runs on `push` for `main` alone, so every
scratch round is proved on `refs/pull/N/merge` — your head merged with `main`'s
tip *at run-creation time* — while the `headSha` it reports is your branch head
by itself. A sibling that merged in between is in every round's tree and in none
of your evidence, and neither side is broken on its own. Re-run the green on the
merge if `main` moved. Signature: a whole wave returns failing on one message and
none of them is an assertion (#1236 — seven rounds dead on a
`#[global_allocator]` collision present in neither the wave base nor `main`, only
in the merge of the two, from a sibling that landed four minutes earlier).

**One behaviour per round.** Two at once, or a neuter that stops it compiling,
and the failures stop being attributable to the behaviour they evidence — a
compile error proves nothing. Several rounds on one scratch branch is normal.

**A neuter that removes the mechanism outright cannot evidence a HARDENING of the
assertion.** Both the old wording and the new one redden under it, so the row
evidences that the mechanism runs at all — never that your stronger specimen or
wider loop catches something the previous version missed. The hardening's
counterfactual is a *different* neuter: one that leaves the mechanism in place and
breaks only the case you hardened for. Cut that round, or say in the body which
counterfactual the hardening closes and that no round here neuters it. Signature:
the same table carries a sibling row proving the mechanism was removed wholesale
(`unwrap_err()` on an `Ok` — output == input) beside a claim that the pre-hardening
assertion "would have gone green against this same neuter" (#1229 round 2 B1).

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
it reddened, at both commits — **necessary, and not sufficient**: read "that
criterion does not close" below before you bank it.
`git range-diff <old-base>..<old-head>
<new-base>..<new-head>` marking every prior commit `=` is the shortcut for a pure
rebase — but only once `git merge-base --is-ancestor <old-base> <new-base>` has
exited 0. All-`=` says the patches match, never that the bases are related: two
merely-diverged bases report a clean all-`=` too (measured), and then the tree
around your mutation is not the tree you measured on. Name the commit each round
descends from, never "the current head" (#1182 — rounds F/G/H, four rebases).

**That criterion does not close — and the cheap fix is to stop needing it.**
Byte-identity covers the region you mutated and the test that reddened; it never
looks at what that test *calls*. A transitive callee rewritten after the
measurement — a review fix two frames down — retires the red exactly as silently,
and again nothing goes red to say so. Extending the check to "identity plus a
judgement call about the callees" only banks more hand-verified inertness claims,
which is the shape this loop keeps catching stale. A wave is ONE CI cycle
whatever its width, so re-cut every carried round from the head and delete the
transfer argument instead — the head need not move for that, only the body.
Signature: a round's mutated site is byte-identical at both commits and a
function it calls is not (#1236 — three of eight rounds reached
`record_crash_first_phase` / `newest_crash_log_since`, both rewritten by that
PR's own review fixes).

**And that premise is optimistic, which makes the re-cut cheaper rather than dearer.**
"Byte-identity covers the region you mutated and the test that reddened" is itself an
AND, and every wave-shaped instrument — an anchor sweep, `git apply --check` over the
patches, a hunk-overlap table — answers the first half alone while reading as a complete
proof, because it is one, of half the criterion. Disjoint hunks do not close the second:
two hunks hundreds of lines apart sit inside one `#[test]`. So re-cut. If a round is
carried anyway, hash the reddening test's whole body blob-vs-blob at both heads —
`git show` each side, extract by its `fn` header and brace-match — against an unedited
sibling as the discriminating control. Signature: a "not re-cut, verified
mechanically" argument whose instruments are all patch- or hunk-shaped, with no
per-test hash (#1722 — `l7a_…` was 15 075 bytes at the wave head and 16 242 at
`dc8802ff`, a 1 167-byte rewrite behind clean hunk checks; the wave head `0030b137`
resolves only through `git fetch origin refs/pull/1722/head`).

**A re-cut wave needs its own OPEN PRs — a pushed scratch branch builds nothing.**
`ci.yml` is `on: push: branches: [main]` plus `pull_request`, so a branch is built only
while a PR is open on it: over the last 400 runs NOT ONE `push` run is on a branch other
than `main` or a `v*` tag. Measure that zero, not the pass/fail split beside it — the
window slides with every run in the repo, so a total quoted here is stale on arrival.
Step 4 above closes the scratch PR
and deletes its branch once cited, which is what makes the second wave the dangerous one
— re-pushing those branches and reusing their PR numbers builds nothing at all, and
`gh pr checks` keeps answering for the round you already quoted. Assert `state == OPEN`
before reusing a scratch PR and open fresh ones otherwise; step 6's `headSha` cross-check
is what catches the miss. Signature: no run newer than the previous round for a branch you
just pushed (#1361 round 2 — 17 branches pushed and silently unbuilt against closed PRs
whose head refs were already deleted).

**A red only counts against a banked green.** Keep the unmutated tree's passing
run for the same tests: a test that has never passed reddens for its own bug,
not for your mutation, and the red then evidences nothing about the property.
And a mutation that **hangs** the suite is a timeout, not a red — the job dies
without naming an assertion, so there is no failure line to quote. Race a
watchdog inside the test so the failure arrives as an assertion instead (#744).

**Reconcile every round's test count against the banked green's.** Read passed +
failed off the round's own log and check the total matches the green run's total
for that binary. A row that does not reconcile means its red is not attributable
to the behaviour the round was cut for **until you can say why** — a mutation
with side effects, a flake, a test added or removed between the two runs, or the
fail-fast truncation that stops a red run reaching later binaries. The extra reds
are the ones you would otherwise quote. Signature: a round reddens three tests
where one was expected (#1236 — eight rounds, each reconciling to 376).

**A round that reddens NOTHING is a finding; a round that reddens EVERYTHING is not
evidence.** Publish the zero-red row rather than dropping it — a dropped row leaves the
reader believing every arm has its own counterfactual — then diagnose it, because the
remedies diverge — and a case can PRESENT as one and RESOLVE as another, which is why the
row is diagnosed rather than handed to the first fit. The property is defended TWICE, so
removing one arm changes nothing: cut the follow-up round that removes *both* (#1361 rounds
4→5, #1299 M10→M10b). Or the mutation landed textually somewhere it can never fire: re-place
it, and assert non-inertness structurally rather than trusting the edit (#1426 round 2, open
when this was written). Or the code is unwitnessable and the answer is to DELETE it rather
than widen the round — #889 read as defence-in-depth and resolved this way, because the
honest wider row "would have left an unwitnessable branch in the code", the same discharge
as #664/#686's dead production code behind a comment claiming otherwise. At the other end, one mutation
reddening many tests attributes to none of them — narrow it to a single red where you can
(#1300, 9→1), and where you cannot, say so and name the per-property witness each broad
red already has (#1361 round 11, #1358). Signature: a wave table with an empty "test
reddened" cell, or a row whose count is the mutation's blast radius rather than the
property's — the reconcile rule above catches the arithmetic, not the attribution.

**A decision the wave cannot DRIVE never becomes a row — and the table still reconciles.**
Every rule above diagnoses a round you *cut*; none of them catches the round you could not cut.
A decision welded into an I/O seam — inside a function taking `AppHandle`, or a method on the
drainer — has no surface a test here can call, so no round is cut for it, no zero-red row
appears, and the counts reconcile exactly. Enumerate the decisions the diff makes BEFORE
cutting the wave and check each has a callable surface; extract the ones that do not into a
pure function (the `worker-deep.md` §3 convention — a precondition of this evidence, not only
a readability one) and give each its own round. Signature: a complete, reconciling N-round
table in which no row names a decision the diff plainly makes (#1501 — a coverage regression
shipped under TWELVE green rounds, `record_contributions_for` then extracted pure, round 13
and `j16` following).

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

**The order is alphabetical by PACKAGE name**, so a rename or a new workspace
member moves it — never infer it, and never carry a remembered one. Observed
order of a `cargo test --locked --workspace` run:

    loomux_engine unit → loomux_server lib unit → loomux_server bin unit
      → loomux_lib unit → orrerix bin → every src-tauri/tests/* integration
      binary → doc-tests

Re-derive it rather than trusting this line: take the newest green `CI` run on
your branch and grep the ubuntu leg's *Test backend* step for `Running`. No run
id is pinned here on purpose — a run cannot exist until after the commit that
would cite it, so any id written on this surface is one commit stale the moment
it lands, and a stale id is what makes the whole line look untrustworthy.

- To redden a **unit test in `crates/loomux-engine/src/`** (e.g.
  `workflow.rs`) **or in `crates/loomux-server/src/`**, neuter the pure
  function. Both `crates/` members run **first**, so the plant is reached
  before anything in `src-tauri` can stop the run. The cost is at the other
  end: everything in `src-tauri` runs after them, so a plant here stops the run
  before the `src-tauri` lib and every integration binary — those are exactly
  the targets you would otherwise quote as "the rest of the suite passed in the
  same run", and they did not execute. Read which targets actually ran.
- To redden a **unit test in `src-tauri`'s own lib** (e.g.
  `src/orchestration/digest.rs`), neuter the pure function. `loomux_lib` runs
  after both `crates/` members and before every integration binary, so a plant
  there is reached, and the two `crates/` members passing in the same run are
  readable evidence. Check where the file lives before picking it: #888 is
  moving `orchestration/` modules into the engine crate one batch at a time,
  and a file that moved is governed by the bullet above, not this one.
- To redden an **integration test in `src-tauri/tests/`**, neuter the
  **wiring** instead — the call site, or the gate's consumption of the value —
  and leave the lib function intact, so the lib suite stays green and the
  integration binary is actually reached.
- **When the mutation ITSELF is the plant, the collateral red can be UNAVOIDABLE
  and none of the three above has an answer.** Disarming a primitive reddens that
  primitive's own unit tests, in `crates/`, before cargo can reach
  `src-tauri/tests/`; there is no plant site that avoids it. Silence the collateral
  reds instead: `#[ignore = "[scratch] silenced so cargo reaches tests/<file>.rs"]`
  — the reason string prints in the log, so the round discloses its own staging
  — or `--no-fail-fast`, so every binary reports (#1361, #1426). Reshaping the
  mutation to thread between the pins works when it is available and cannot be
  relied on; where nothing is, the claim is carried by inspection and says so
  (#1464, #1572).
  **Then read the TARGET binary out of the run before citing it** — the section
  above says to read which targets ran, and this is how: its own
  `Running tests/<file>.rs` line AND that binary's own `test result:` totals.
  Signature: the failing job's totals are an EARLIER binary's while the claim is
  about a later one — one `Running` line in the log where the round needed two, and
  a design note citing a sweep the run never reached (#1667).

The same stop-at-first-failure also bounds a round that **did** hit its target:
if the property is pinned in two binaries, the later one never ran, so it is
covered by inspection, not by a watched red — say which half moved rather than
claiming both (#1181; the repo rule it instantiates is CLAUDE.md's "a red
evidences only the assertion it REACHED and MOVED").

Precedent: #869 (scratch PRs #870, #872).

**At most two scratch rounds per test (#1685).** A counterfactual that has not
reddened after two rounds is not re-cut: disclose it in the PR body as a
boundary — `the sweep asserts X; a misclassified writer has not been
demonstrated to trip it (rounds N, M green)` — and the reviewer judges the test
on its assertion. A `[scratch`-titled PR runs one platform and no E2E, so a
round costs one build job plus a three-second planner, not four; a third
round is an unbounded loop wearing evidence's clothes.

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

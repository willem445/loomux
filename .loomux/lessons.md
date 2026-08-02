# Repo-recorded lessons

Hard-won knowledge about this repo that should survive past the orchestration group that
learned it. See `doc/design/lessons.md` for what this file is, how it's injected, and its
trust guardrails. Newest entries go at the bottom (an append log); nothing here parses these
headings — they're for readability only.

## No getrandom-based crates in src-tauri

`uuid` v4, `rand`, `tempfile` with default features, or anything else that pulls in the
`getrandom` crate must stay out of `src-tauri`'s dependency tree. They import
`bcryptprimitives.dll!ProcessPrng`, which this project's Windows 10 baseline doesn't export —
the binary then fails to load with `0xc0000139`. Agent ids/tokens use std's OS-seeded
`RandomState` instead. Before adding any new dependency, check the notes in
`src-tauri/Cargo.toml` and audit its transitive deps for a `getrandom` edge
(`cargo tree -e normal --target all -i getrandom@<version>`).

## Never resize the PTY for a UI feature

Git view, task board, audit viewer, badges, compose strip — all of these are overlays or
header/board chrome floating over the terminal, never a resize of it. Resizing ConPTY
triggers full repaints that pollute scrollback. Visual padding belongs on the `.xterm`
element, not on the layout.

## Never `git stash` in a loomux worktree

The stash stack lives in the shared `.git` and is one stack across *every* worktree of a
repo, not per-worktree. Two workers running concurrently in separate worktrees collided on
`git stash`: one popped/dropped entries assuming the stack was its own and nearly destroyed
the other's WIP before noticing mid-operation and recovering it (#299, a live near-miss —
pure luck it was caught). Commit WIP to your own branch instead (a small commit you
amend/reset/squash later). If you must stash, `git stash push -m "<your agent id>: ..."` and
only ever `pop` an entry carrying your own marker.

## A claim is a deliverable

A comment, design note, audit label, or PR body stating something the code
doesn't do is a defect, not a slip — it stops the next reader from checking.
This pattern has recurred repeatedly on this repo: #461 catalogues seven
instances from one session (e.g. an audit action labeling a failed delivery
a success), and the batch that filed this entry produced another — PR #489
offering a persona `allow:` opt-in as a regression's fix when it's actually
blocked at *two* structural layers, the workflow parser and `persona_inject`'s
#222 capability closure (#490) — an impossible mitigation reads as an answer
and stops the search. This entry itself first shipped with a miscount and a
one-layer version of that same claim, caught in review, not before. Delete a
claim you can't point at code for, don't soften it; quote raw fetched text
for CLI/API facts, never a paraphrase (#453).

## Commit real work BEFORE capturing red evidence

Red-before-green on this repo usually means running the NEW tests against
the OLD behavior — which means temporarily patching a file back and then
restoring it. `git checkout -- <file>` restores by discarding ALL
uncommitted work in that file, not just the experiment: it silently ate a
real, minutes-old correctness fix during #493's evidence capture (re-done
later as b4c7d96; same destructive-git class as the `git stash` entry
above). The cheap rule: commit the real work first — a small commit you
amend or squash later — then patch for the red run, then restore. Never
point a destructive git command at a file holding anything you haven't
committed.

## Any suppression driven by a fallible signal must be BOUNDED

A guard that holds an action back "while X is true" needs an answer to *what
if X is wrong and never clears*. On this repo the answer keeps being "a human
notices and does it by hand": #496, #513 (27 min — per-attempt bound,
unbounded retry loop), #518 (a false "user typing" pinning a delivery's
human-input block for the late monitor's full 4h life). Bounds landed one at
a time as each fired — #500 for the idle tick, #518 for the delivery hold.

Three things learned the hard way:

- **Fixing the signal does not excuse bounding the consumer.** #499 gated the
  stamp at its source and #518 happened anyway: that gate is a byte-shape
  match over an OPEN set of terminal auto-reply shapes, and the next shape is
  always unmodelled. Fix the signal *and* bound what reads it.
- **Releasing on evidence beats releasing on elapsed time.** A pure timeout
  must be tuned against "what if the human really is mid-sentence" — which is
  why #500's clamp needed a per-group knob. #518's releases only when a
  second, independent reading says there is nothing to clobber
  (`input_pending` false), so it needs no knob and cannot erode the rule it
  sits under.
- **Enumerate in writing.** "Extend this to every consumer" means grep the
  subsystem and publish the list, the already-fine ones included. #518's
  enumeration refuted the issue's own premise about where the gap was.

## `rustfmt --check` is the one local Rust check agents may run

Local cargo is banned outright for agents (#320 CPU, #488 disk) — but that left no way to
know whether newly written Rust even *parses*, so the cheapest defect cost a full CI round: a
scripted rewrite cut at a `);` inside a string literal, and all three build jobs failed on
parsing, not assertions (#558). `rustfmt` is a parser, not a build — no cargo, no dependency
resolution, no `target/` bytes, ~3-5s for this whole crate — so it sits inside both bans. From
`src-tauri/`: `rustfmt --check --edition 2021 <changed .rs files> >/dev/null`. The flag is
mandatory (rustfmt's CLI defaults to edition 2015, where `async fn` is a false parse error);
stdout is discarded because `--check` also prints *formatting* diffs and this repo is
deliberately not rustfmt-formatted — 12,483 lines of them for `orchestration/mod.rs` alone, so
dropping the redirect floods your own context; the exit code is ambiguous (1 means either), so
**any output on stderr is the signal — read it, don't grep it for `error:`** (an unresolved
`mod` arrives as `Error writing files: …`, with no `error:` token). It is a syntax check and
nothing more — never run bare `rustfmt`, never
commit a reformatting, and never cite a clean run as validation: it catches no type error, no
unresolved name, not even a `format!` with the wrong argument count. `cargo check` stays
banned and this argument does not reach it — it writes dependency metadata to `target/`, which
is exactly the cost #488 banned.

## Never block a turn waiting on CI — the resolution is queued behind the turn

A `[loomux] …` notice is delivered by *typing into a pane*, and a pane that is mid-turn cannot
take a delivery. So a turn that blocks waiting on CI is waiting for something whose resolution
is queued behind the turn itself: it cannot resolve, by construction, however long you leave it.
A worker on #577 did both halves at once — it registered a `notify_when` CI watch (right) and
*also* blocked its turn on a shell-level wait for checks on the new head (fatal). Two merges had
landed underneath it, so the PR was `CONFLICTING` and GitHub was never going to create the
check-suites that wait was blocked on, while the watch's own CONFLICTING notice — which #337
built for exactly this case — sat undeliverable behind the blocked turn. 20+ minutes, broken by
the host watchdog plus a human reading the pane by hand (#590).

The rule, which `orchestrator.md` had carried for weeks and the delegate templates never got:
**register the watch, end the turn, act on the notice.** Reading a state once is fine; *waiting*
is the defect. It generalizes past CI — anything whose answer arrives as a pane delivery
(another agent, a human, a long remote job) is a condition you must not hold a pane open for. It
sits beside *any suppression driven by a fallible signal must be BOUNDED* above and is stricter
than it: a bound would only have shortened this hold, because the wait's own resolution channel
stayed blocked for as long as the wait ran. Don't wait at all.

## A PR body's claims are about a SHA and a scope — the body doesn't know when either moved

Two failures from the same family, both of them a true sentence that quietly stopped being true
while the text stayed put.

**Green is a fact about a SHA, not about a PR.** Any push or rebase invalidates every run id,
run link and "green on all three platforms" already written in the body, and the body survives
that push untouched — so the claim rots silently and reads exactly as it did when it was true.
Three instances in one batch, two different workers: #571 cited a run three commits behind head,
and #588 cited a pre-rebase run at review 1 and then the *same* pre-rebase run again after the
rebase at review 2. Reviewers caught all three; no worker caught its own. The rule is
re-derivation, not care: after any push or rebase, list the runs for the new head (`gh run list
--branch <branch> --json headSha,databaseId,conclusion`), assert `headSha` equals `git rev-parse
HEAD`, and update the body before reporting (#596, now in `worker.md`'s DoD and the
`ci-validate` skill).

**`Closes #N` is a fact about scope, and a squash merge honors it regardless.** GitHub reads the
keyword out of the squashed commit message and closes the issue no matter how partial the change
was — a "layer 1 only" or "Mitigates" sentence elsewhere in the body does not qualify it. #569
and #590 were both auto-closed that way this session with real scope still open, and both had to
be spotted and reopened by hand (the same trap `squash-merge-autoclose` names from the merge
side). Partial scope links as `Part of #N` / `Mitigates #N`; `Closes` is for the PR that finishes
the issue outright. Worth the same post-merge check either way: after a squash, confirm the
issues you only *mitigated* are still open.

**Choosing the right keyword is not enough — the scan is textual and context-blind.** GitHub
matches `close`/`fix`/`resolve` in any inflection immediately followed by `#N`, anywhere in the
PR body and in every commit message a squash aggregates into its own message: inside a
blockquote, inside a caveat, inside a sentence asking a human to do it manually. #569 was
auto-closed a *second* time, an hour after the first, by PR #615 — which linked `Part of #569`
deliberately, argued the choice in a blockquote, and ended it "Please close #569 by hand if you
agree", the one construct in the whole body GitHub's scan matches (confirmed after the fact:
`closingIssuesReferences` on #615 lists #569, and the aggregated squash message contains no
keyword adjacent to an issue number). The habit that survives this: before opening or updating
a `Part of` PR, grep the body *and* `git log` for keyword-next-to-`#N` and reword it ("#569
stays open", "for the human to close out"); and whoever merges scrubs the aggregated message
first and re-reads the partly-addressed issues after. Both halves are now in the templates
(`worker.md` DoD item 7 for the authoring side, `orchestrator.md` for the merge side).

## A model that re-implements the algorithm proves the algorithm, not the code

`queue.rs`'s `drainer_lifecycle` is an exhaustive interleaving search with a `guard_checks_generation`
knob, and a shipped mutation test (`unconditional_guard_removal_reproduces_the_round_2_double_drain`)
flips that knob and asserts the checker goes red. That reads like coverage of `DrainerGuard::drop`,
and #497's triage recorded it as exactly that. It is not: the model contains its own copy of the
logic and no test reads `mod.rs`, so mutating the real guard to remove unconditionally leaves the
whole suite green. Confirmed rather than argued — a scratch commit put raw ungenerationed removals
at both the existing site and a new one, and CI passed on all three platforms (PR #606, `0ed4dfe`,
run 30690784043).

Two things to carry:

**Distinguish "the model has an event for this" from "a test fails when this code changes."** A
property/mutation test over a model bounds the *design*. Only a test that executes the real function
bounds the *code*. Both are worth having; conflating them puts a coverage claim in a design note
that no mechanism keeps true (#552's exact subject, and #562's argument for types over tables).

**Check what a path's tests can even reach before crediting it.** `DrainerGuard` is built only by
`run_queue_drainer`, which needs an `AppHandle`, and the suite says so in a comment — so that
guard's `Drop` has never executed in a test, and no amount of careful reading of the test names
would have said so. `grep` for the constructor, then ask what constructs *that*. When the answer is
"nothing a headless test can build", the honest fix is usually to move the logic somewhere a test
can reach (here: into a newtype with its own unit tests), not to write a more careful comment.

## `/tmp` is one namespace shared by the whole fleet — scratch files go in your own worktree

A worker wrote its PR body to `/tmp/body.md` and ran `gh pr edit --body-file /tmp/body.md`;
another worker, seconds apart, had done exactly the same. PR #621 was published carrying #612's
body (#625). It was detected and fully restored, and the other PR was undamaged — but only
because a worker re-read its own PR afterwards. There is no lock, no error and no warning on
this path: the second writer wins and both agents are told everything succeeded.

The mechanism is structural, not a slip. Worktrees isolate the *repo*; they isolate nothing
else on the machine, so every agent in every group shares one `/tmp` — and the filenames an
agent reaches for (`body.md`, `notes.txt`, `review.md`) are exactly the ones every other agent
reaches for too. Same class as the shared `.git` stash stack (#299) and the shared
`CARGO_TARGET_DIR` that was retired for it (#263): the collision is invisible precisely because
the shared resource looks private from inside one agent.

The rule, now in `worker.md` and `reviewer.md`: temp and scratch files live under the agent's
own worktree — `./.scratch/`, gitignored — never a bare `/tmp` name. A path only you can own
costs nothing and removes the failure mode entirely.

## A green suite's coverage claim is a claim like any other — the mutation round is what corrects it

Three PRs in one batch had a mutation round FALSIFY the author's own written claim about what
guards a property — not merely supply missing red evidence. #664: a code comment credited
sibling-target threading in land_batch with a refusal that the recorded-target comparison
actually produces; the mutation (M11) removed the threading, the predicted red stayed green,
and the DEAD PRODUCTION CODE was deleted with a real test added in its place. #673: a test
named for the credit rule passed with the guard removed — a
vacuous test in the exact spot where a wrong answer tells a pane a lost report is handled;
designing the mutation caught it before any run. #682: the body credited one negative control
with catching a blanket relabel; the run showed a PAIR performs the catch, and one member is a
pre-existing test whose connection to the property is invisible from its name.

The pattern: attribution written from intention ("this test guards X", "this code enforces
Y") is wrong often enough that it should not survive unexecuted — and the false claim can
live in a PR body OR a code comment; both die the same way. When anything claims a specific
test or mechanism polices a specific property, run the one mutation that removes it and watch
WHICH tests redden
— the diff between predicted and actual failure lists is where the review value lives. A
mutation whose result matches prediction is evidence; one that doesn't is a correction, and
disclosing it beats a quiet re-run every time.

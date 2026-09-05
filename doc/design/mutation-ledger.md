# Design: the mutation / red-before-green ledger, generated (#2507)

`scripts/mutation-ledger.cjs` builds the red-before-green ledger a PR body carries — one
row per scratch round, with the run id, the head SHA that run measured, the tests that
reddened and the suite's passed/failed split — by reading the runs' own logs, and re-checks
a posted one against those logs.

## 1. Why it is generated rather than typed

The evidence discipline in `CLAUDE.md` and the wave recipe in
`.claude/skills/ci-validate/SKILL.md` both end in the same artefact: a table of figures,
each dated to a run and a head. Producing it by hand has one failure mode and it is
structural rather than careless — **every fix push stales a figure, and nothing mechanical
points at which one**. No test goes red, no grep finds a stale hit, the prose still reads
perfectly, and `git range-diff` reports `=`. The defect surfaces only when a reviewer
re-derives a number by hand, which costs a review round with the code unchanged.

That cost was measured: receipt churn was the second-largest orchestrator cost of the beta8
round (#2321), and #2239 and #2308 each spent three or more review rounds on it. On the
ten-PR corpus classified for #2168, 12 of 23 blocking rounds were a number or a statement in
the body rather than a defect in the code, and 7 of those 12 were collateral of the previous
round's own fix.

Every figure in that table is already printed in a log GitHub keeps. So the ledger is read
out of the log, and the only thing a worker types is the one thing a log cannot know: which
behaviour each round set aside.

## 2. The rule the script holds itself to

**Every figure is READ, never derived.** The suite total is one `test result:` line's own
`passed + failed`, not a sum, not last round's number adjusted by arithmetic, not a count of
the names the failure block happened to print. That last one matters: the names are kept as
a *cross-check* on the count, and when the two disagree the script says so rather than
silently preferring either — a disagreement means the block was truncated, which is a fact
about the evidence and not a rounding question.

**A parse that yields no totals is an ERROR, never an empty ledger.** This is the whole
positive control, and it is the reason `selectSuite` throws. A truncated log, a workflow
that printed no totals, a target renamed out from under the selector — all three produce "no
suite", and a ledger row with empty figures reads exactly like a round that reddened
nothing. One of those two is a finding to publish; the other is an instrument that did not
run. They must not render the same.

## 3. What it reads, and how a figure is attributed

A CI run's log carries **many** `test result:` lines — one per test binary, times one per
platform leg. #2239's ledger figures (593/1, 588/6, 590/4, 592/2) are the `loomux_engine`
lib target on the ubuntu leg alone, out of a run whose targets total in the thousands.
Attribution is therefore three-part and all three are load-bearing:

- **the job**, from the log's own `<job>\t<step>\t<timestamp>` prefix — two legs both carry a
  `loomux_engine` suite, and picking one silently dates a figure to the wrong platform;
- **the target**, from a `Running … (target/debug/deps/<stem>-<hash>)` banner, with the build
  hash stripped. Not the banner *above* the result — the oldest banner not yet resolved,
  which is §3.1 and is the difference between 280 and a plausible 25;
- **`what` the target is** — `unittests src/lib.rs` against `unittests src/main.rs`. A
  crate's lib and bin unittests build to the same `deps/<crate>-<hash>` stem and differ only
  here: `loomux_server` is 25 tests as the lib and 0 as the bin in the same job of the same
  run, and a selector keyed on the stem alone returns whichever came first with nothing to
  say the other existed.

The frontend suite is read from `node --test`'s TAP trailer: `# pass` excludes skips while
`# tests` includes them, so neither is derivable from the other and both are carried; `total`
stays `passed + failed` so it means the same thing as the cargo one. Failing names come from
the `not ok N - <name>` lines, and the count still comes from `# fail`.

`ranTargets` is reported beside each row because `cargo test` stops at the first failing test
*binary*. A round that reddens the engine lib means every `src-tauri` integration binary
never executed — which is exactly the set a reader would otherwise assume "passed in the same
run".

### 3.1 A `Running` banner is a QUEUE entry, not a cursor

Cargo prints the `Running` banner on **stderr** while the test binary prints its own output
on **stdout**, and a CI log is the two streams interleaved by arrival. So two banners land
back to back while the first binary’s trailing lines are still in flight. Measured on run
33937535584’s ubuntu leg: `Running … loomux_server-…/main.rs` and `Running …
loomux_lib-…/lib.rs` are adjacent, and the `test result: ok. 25 passed` after them belongs
to `loomux_server`’s LIB, two banners back.

A reader that treats the banner as "the target of the lines that follow it" therefore
reports `loomux_lib` at 25 instead of 280 and drops a target entirely — 51 targets read as
49 — and nothing says so, because every individual figure it prints is a figure the log
contains. That is the worst shape available: not an error, not a blank, a plausible wrong
number.

Banners are queued instead, and each `test result:` pops the oldest unresolved one. Cargo
runs test binaries sequentially, so banner order **is** result order — interleaving reorders
lines across the two streams, never within one. Failure blocks accumulate the same way: they
are stdout, ahead of their own binary’s result, so the ones seen since the last result belong
to the suite that result closes even when another binary’s banner arrived in between. Each
binary’s own `running N tests` is queued alongside and carried per suite, which makes the
attribution checkable rather than merely argued: it must equal `passed + failed + ignored`,
and a round where it does not is reported as a note rather than printed as a figure. A
result with an empty queue is named `(unattributed)`, never folded into a neighbour.

### 3.2 Two spellings of ESC, and why the failure is silence

GitHub's stored log colours the `Running` and `error` lines, so nothing matches a pattern
anchored on the bare word until the escapes are stripped. The stored form is the real 0x1B
byte. A log fetched through the sandboxed shell agents run under here arrives with every
0x1B rewritten to the two-character caret notation `^[` — run 33928049423's log, fetched
into this worktree, contains zero 0x1B bytes.

A stripper that knows only the byte therefore recognises **no target at all** and returns no
figures, rather than wrong ones. That is the shape §2's error exists for, and it is also why
`test/fixtures/mutationledger/` carries one fixture in each spelling: the pair pins that both
parse to the same figures, and a separate negative control pins that a byte-only stripper
finds nothing in the caret form. The caret arm is anchored to the whole CSI shape, so a bare
`^[` in test output is not eaten.

## 4. `--check`, and where the seam with `pr-body-check` is

`scripts/pr-body-check.cjs` re-measures a posted body against the PR's own **head**: blob
sizes, diffstat, SHA resolution and ancestry, whether a cited run exists and what `headSha`
it reports. It never opens a log. So it can tell you a run is real and that the SHA beside it
resolves — and it cannot tell you whether "588 passed; 6 failed" is what that run said.

`mutation-ledger --check` opens the log and never looks at head. It re-reads, per ledger row:
the head SHA the run reports against the SHA the row claims; the reddened set as a *set*,
compared on each name's last `::` segment because a body spells a test as declared while the
harness prints it module-qualified; and the number of names the row lists against the run's
own `failed` figure. It then re-reads the `**Round N — k tests** (P/F)` bullets against the
same runs, because a bullet is a second statement of a figure the row already carries and the
two drift apart independently — #2239's own body records a round whose paragraph said *five*
where its row, its count line and the run's log all said six.

### 4.1 The three reading rules, and why each is written against the GENERATOR

Both halves of this script must agree, and the only thing that proves they do is running
one into the other. Each rule below was got wrong in a way no fixture for either half
alone could show (#2512 round 2).

**A claimed name is whatever is backticked in the reddened cell, with no shape filter.** An
identifier-shaped filter is a fact about *cargo* names. A `node --test` name is prose with
spaces, so under such a filter every claimed name in a frontend row was discarded, the
claim set came back empty, and the checker reported "the row names no reddened test"
against a row naming them all correctly. That is the generator failing its own checker on a
body nobody had touched, with no edit available that would clear it. The cell exists to
list names; what is in it IS the claim, and a token matching nothing in the log is reported
as a disagreement rather than filtered out of the question.

**The split is read out of the FIRST parenthetical after the bold, and it may carry a
trailing clause.** `renderLedger` writes `(2/2, run 33928069681 @ `f9afc965`)`; a pattern
demanding the closing paren straight after `P/F` read no split at all, and emitted nothing
— not an OK, not a CHECK, not a MISMATCH. A checked ledger and an unchecked one
rendered identically, which is §2 turned on the script itself. The window before the paren
cannot cross a paren, so a later parenthetical is never mistaken for the split; a first one
that carries no `P/F` (the green-round form `(run 33917588137 @ `3774789b`)`) means the
bullet claims no split, which is a different thing from claiming a wrong one.

**A `|`-run inside a ``` fence is not a table.** A body legitimately QUOTES a ledger table,
and this note and this feature’s own PR both do. Read fence-blind, an illustrative table
quoted above the real one is what `--check` re-reads, the real ledger is never opened, and
the summary still reports a row count: a clean report about the wrong table.

The two scripts are complements, not alternatives. Run both before `report(done)`.

Severities match `pr-body-check`'s so a worker reads one vocabulary: **MISMATCH** (a figure
disagrees with the log; must be zero), **CHECK** (narrowed to a judgment the script cannot
make), **OK**. Both scripts exit 0 always — a report, never a gate. A gate on prose would
have to be right about intent; this is only ever right about a number.

## 5. What it cannot see

Stated here rather than discovered later, because each of these is a case where the honest
output is a CHECK or an error rather than a confident row.

- **A log that truncated.** GitHub drops the middle of a very large step's log. The script
  cannot recover the missing lines; what it can do is notice that the failing names it found
  do not reconcile with the summary's `failed`, and say so. Where the truncation takes the
  `test result:` line itself, there are no totals and §2's error fires.
- **An expired log.** GitHub keeps run logs for a bounded window; past it, `gh run view
  --log` returns nothing and the row is a CHECK ("no log available"), never an OK. A ledger
  older than the retention window is not re-checkable, which is an argument for cutting the
  citable wave once, at the settled head, rather than early.
- **A workflow that prints no totals.** A step that swallows its test output, or a runner
  whose reporter is not TAP, yields no `# pass` line. `node --test` on Node 24 defaults to
  the `spec` reporter, whose totals read `ℹ pass N`; CI runs Node 22 and gets TAP. The
  script reads TAP only, and a `spec`-reported run is the §2 error rather than a half-read
  row — deliberately, because reading totals from a shape whose failing-name lines the script
  does not parse would produce a row with figures and no names.
- **Which line of a panic block a human meant to quote.** The script takes the line
  immediately after `panicked at …:`, which is the assertion. A body that quoted a different
  line of the same block is not wrong, so `--check` does not compare the failure-line column
  at all; it is presentation, and the columns that carry claims are checked instead.
- **A multi-word bullet count.** The bullet reader takes one token — a digit or a single
  word — before `tests`/`each`. "a couple of tests" is not read, and produces no finding of
  any severity. It is bounded by the table rows, which are re-read whatever the prose says;
  widening it would be a parser for English, which is the thing that cannot be right about
  intent. The residual is pinned in the suite rather than only described here.
- **A runner that executes test binaries in parallel.** §3.1’s queue rests on cargo running
  them sequentially, so that banner order is result order. Nothing in this repo does
  otherwise, and the per-suite `running N tests` cross-check is what would notice if
  something did — the counts stop reconciling, and the round is reported as a note rather
  than printed as a figure. It is a check, not a fix: interleaved parallel output would need
  a different reader.
- **Whether the round was cut from the right base.** "Cut from" is a git fact about a commit,
  not a fact in the log. The script carries whatever the rows file declares and prints it;
  `ci-validate`'s `git rev-parse <round-commit>:<file>` check against head is still a
  separate, manual step, and it is the one with no natural trigger.
- **Whether the mutation was a good one.** A round that reddens nothing may be a weak test, a
  bad mutation, a property defended twice, or dead code. The script publishes the zero-red
  row and labels it a finding; diagnosing it is `ci-validate`'s job and a human's.

## 6. Input, output, and where the state lives

Input is a rows file — the base (banked green) run, and one row per round naming its run
id and the behaviour it set aside. **A row names a run id, not the `PR number@sha` form
#2507 also offered.** That is a deliberate narrowing rather than an omission: the head SHA
is then derived from the run’s own metadata instead of typed, which is the whole point of
the tool — a hand-supplied SHA is one more figure that goes stale. A worker who has a PR
rather than a run id gets the run from `gh run list --branch <b> --json headSha,databaseId`
(#2512 rev-std non-blocking 1). Output is the markdown table plus the bullets, on stdout, for
a worker to paste into the body's agent layer. Nothing is written to the repo.

Fetched logs are cached under `.scratch/mutation-ledger-logs/` (gitignored, worktree-local per
the #625 rule) keyed by run id, so a 31-row ledger re-generated across four review rounds
downloads each log once instead of 124 times.

**Only a COMPLETED run is cached.** "A finished run’s log is immutable" is true, and an
in-flight run’s log is not — and the in-flight one is exactly what a worker hits, because
the natural first use is checking a draft body against its own CI run while that run is
still going. Cached, that partial output is banked permanently and every later `--check` on
the worktree reads half-finished text, which can report a plausible `0 MISMATCH` off a suite
that never finished. So the run’s status is asked for first; an in-flight run is read, used,
warned about on stderr, and not written. The probe fails safe: a status it cannot get is
treated as not-complete (#2512 rev-std non-blocking 3).

**Logs reach the pure core through a reader, not a map.** One CI log for this repo measures
7,385,209 bytes, so an object holding all 31 of #2239’s rounds keeps ~229 MB of text resident
for the whole call — more as UTF-16 — on top of `execFileSync`’s 512 MB `maxBuffer` per fetch.
Each log is read exactly once per row, so nothing needs holding: the CLI passes a function of
the run id that pulls one log from the cache and lets it go. The object form is still
accepted, and is what the offline suite uses (#2512 rev-final premortem 2).

The pure core takes log **text**, never a run id, so `test/mutationledger.test.ts` drives the
whole generator and the whole checker over fixtures and never runs `gh`.

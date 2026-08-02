# Design: Bors-style bisecting merge queue (#581)

Status: **design only — no queue code exists yet.** This note is slice B of #581 and gates
every later slice; the plan (#581 comments, planner `plan-151`, parts 1–7) makes the note a
hard prerequisite for slice C. Nothing here is implemented. Line numbers are as of this
writing against `main` at `a6223b2`; symbols are the durable reference, lines are a
convenience.

Slices: **A** `Task.pr_base` (parallel with this note) · **B** this note · **C** `mergeq.rs`
pure core + `merge_queue:` parsing · **D** the driver · **E** MCP tools + docs · **F**
read-only UI.

## Two decisions recorded here, both deliberately reversible

Both were flagged by the plan as the calls a human may want back, and both were adjudicated
on #581 under the batch-2 autonomy grant. They are recorded as **decisions**, not open
questions — each with the alternative that lost and the seam that makes reversing it cheap.

1. **Batch construction is a merge-commit scratch ref, not squash replay** (§8). The losing
   alternative and the one-function reversal seam are written down there.
2. **The backend pushes refs and opens draft PRs — new authority, accepted** (§3). Bounded,
   namespace-scoped, loud on failure, default off.

## 1. Problem & thesis

CLAUDE.md constraint 7's carve-out (#469) lets an orchestrator merge approved sub-PRs into a
**non-default integration branch it owns**, so a human reviews one combined PR instead of
five. Today that path is sequential and unguarded *at the batch level*:

- Sub-PRs merge one at a time, by hand, as each one's own gate clears.
- The gate is **per-PR**. The `gh` shim (`orchestration/mod.rs::gh_shim_sh:361`, gate body
  ~705–960) mechanically refuses `gh pr merge` until every reviewer the workflow gate names
  has recorded a `pass` against the PR's *current head*. That is real enforcement, and it
  says nothing whatsoever about the **combination**.
- N individually-green sub-PRs can therefore produce a **red integration branch** — a
  semantic conflict, a test that only fails when two changes coexist, a lockfile that
  resolves differently once both are present.
- When that happens there is **no attribution**. The orchestrator sees red and bisects by
  hand, or guesses.

This is the one place loomux's thesis — *gates enforced from outside the agent process* —
stops short. The per-PR gate refuses mechanically; the batch has no gate at all.

**Thesis:** extend outside-the-agent enforcement from the PR to the combination. Before a set
of approved sub-PRs reaches the integration branch, the host merges them speculatively onto a
scratch ref, lets the repo's own CI judge that exact object, and lands **that same object** on
green. On red it bisects and attributes mechanically, so no human and no agent has to guess
which change broke the combination.

The queue is a **gate**, not an accelerator. It exists because a green sub-PR is evidence
about a PR and not about a branch — the same distinction the lessons file records for CI
citations ("green is a fact about a SHA, not about a PR"), applied one level up.

**Not in scope**, each for a reason:

- **Anything touching the default branch.** Constraint 7; §7 is the structural proof.
- **Loomux running builds or tests.** Constraint 8; §5 — the queue *observes* verification,
  it never defines or invokes it.
- **Auto-briefing the culprit's worker from the backend.** §9 — attribution is mechanical,
  routing is orchestrator judgment.
- **Any change to how sub-PRs are reviewed.** The workflow gate is reused, not replaced (§6).
- **The per-PR human grant path** (`mod.rs:943–960`) stays byte-for-byte untouched.

## 2. Prior art

- **Bors** (rust-lang) — the original speculative-merge queue. Batch *k* approved PRs onto a
  scratch ref, run CI **once** for the batch, fast-forward on green, bisect on red. Its
  central invariant is the one this design keeps: *the commit that was tested is the commit
  that lands.* Everything else here is plumbing around that sentence.
- **Gastown's Refinery** — a per-project merge-queue processor that batches merge requests
  from completed workers, runs verification gates, and merges Bors-style. It is the component
  that lets Gastown run 20–30 agents without a human serialising every merge. Surfaced in the
  July 2026 landscape review that produced #581; Gastown and `gh-aw` were the only two
  systems found with host-enforced gates comparable to loomux's, and the bisecting queue was
  the main primitive loomux lacked.
- **GitHub's native merge queue — rejected**, for three independent reasons, any one of which
  is disqualifying:
  1. **It is branch-protection shaped.** Enabling it requires a protection rule on a
     long-lived branch plus repo-admin (often org-level) setup. Integration branches here are
     **ephemeral and orchestrator-owned** — minted for one batch of work and deleted after
     the human squashes them into `main`. Provisioning a protection rule per ephemeral branch
     is not a workflow anyone would run.
  2. **It is default-branch oriented.** The native queue merges into the protected branch.
     That is precisely the branch constraint 7 forbids this feature from touching, and
     pointing it at an integration branch instead means re-creating the protection scaffolding
     described above for each one.
  3. **It moves merge authority out of the host.** The whole point of the shim, the workflow
     gate, and the audit log is that enforcement lives where an agent cannot reach it *and*
     where loomux can compose it with its own gate (§6) and record it (§11). A GitHub-side
     queue would land merges loomux neither authorised nor audited.

  What the native queue does well and this design copies anyway: speculative batching, the
  "tested is landed" guarantee, and dequeuing the culprit rather than the whole batch.

## 3. Ownership & authority

**The queue runs in the loomux backend.** Not in an orchestrator's context, not as a
procedure an agent follows.

Why the backend:

- **Host-enforced.** An agent enforcing its own gate is exactly what #151 taught against, and
  it is the reason the merge gate is a `PATH` shim rather than a paragraph in a template.
- **Deterministic.** Batch composition, bisect steps, and refusals are pure functions with
  tests, not a model's judgment on a given day.
- **Restart-durable.** A batch outlives the app (§4). An orchestrator-run queue dies with a
  compact.
- **Free.** It burns no agent tokens; a batch that takes 40 minutes of CI costs zero context.

**New authority, named honestly.** This is the first time the backend writes to the outside
world on its own initiative, and that deserves to be stated rather than buried:

- **The backend pushes git refs.** Precedent: `src-tauri/src/git.rs::git_push:454` already
  pushes with the user's ambient credentials. What is *new* is that the queue chooses the
  refspec rather than a human pressing a button. Bounds: the only two refspecs the queue ever
  constructs are `<sha>:refs/heads/loomux/mq/<batch-id>` (the scratch) and
  `<scratch-sha>:refs/heads/<validated-target>` (the landing); the landing push is
  **fast-forward only** — never `--force`, never `+` — so a target that moved under the batch
  makes the push *fail* rather than overwrite (§10).
- **The backend creates and closes draft PRs via `gh`.** Precedent for the mechanism:
  `src-tauri/src/gh.rs::gh_output:69` — `Command::new("gh")` with an **arg vector**, never a
  shell string, `current_dir` validated, `NO_COLOR`/`GH_PAGER`/`GH_PROMPT_DISABLED` set,
  `CREATE_NO_WINDOW` on Windows. What is new: every existing `gh` call in that module is a
  **read** (`gh_pr_list`, `gh_pr_view`, `gh_activity`). This is loomux's first backend write
  to GitHub, and it is limited to draft PRs whose head is in the `loomux/mq/*` namespace,
  plus one comment on a culprit PR (§9).

Bounds on the new authority, all of them testable:

- **Default off** (§12). An absent `merge_queue:` block means behavior is byte-for-byte
  unchanged.
- **Namespace-scoped.** The queue creates, and deletes, only refs under `loomux/mq/*`, by
  exact name built from the batch id — never a pattern sweep (§10).
- **No unbounded retry.** Every external call gets one attempt per state transition; failure
  moves the entry to a terminal state, audits, and surfaces. The lessons file's rule — *any
  suppression driven by a fallible signal must be bounded* — is why there is no "keep trying"
  arm anywhere in this design.
- **Loud.** Every push, PR creation, landing, refusal, and cleanup failure is an audit event
  (§11) and, where it changes what the orchestrator should do, a decision-grade notice.

**Rejected alternative: an orchestrator-run queue procedure** — the queue as a template
section the orchestrator follows. Rejected on three counts: it costs tokens proportional to
batch size, it is compact-fragile (a queue that forgets its in-flight batch is worse than no
queue), and it puts gate enforcement back inside the agent process, which is the #151 lesson.

## 4. State & lifecycle

**`merge_queue.json`**, one per group, in the group dir beside `state.json`, `tasks.json`,
`queue.json`, and `audit.jsonl`. Versioned (`version: 1`), atomically written, unknown fields
tolerated on read so an older build can load a newer file without destroying it.

**Entry states:**

    queued ──> batching ──> ci-wait ──> landing ──> landed
                  │            │           │
                  │            └──> bisecting ──> kicked-back
                  │                                    │
                  └──> kicked-back (conflict)          └──> (survivors) queued
    (any non-terminal) ──> cancelled

- `queued` — enqueued and eligible in principle; not yet in a batch.
- `batching` — selected into the batch currently being constructed.
- `ci-wait` — the scratch is pushed, the draft PR is open, checks are being observed.
- `landing` — checks are green and the gate is being re-verified at the moment of submit (§6).
- `bisecting` — the batch went red and this entry is inside the search.
- `landed` / `kicked-back` / `cancelled` — terminal.

**"Paused" is not a state — it is an eligibility predicate, computed live.** The plan
(part 2, answer 3) requires that an entry whose PR got rebased stops being batchable, because
a rebase moves the head and every verdict binds to a commit, so the passes die. Modeling that
as a state would mean *persisting* a fact that is only true relative to the PR's head right
now, and a persisted staleness flag is a claim that rots the moment the world moves. Instead
the entry stays `queued` and carries a `blocked_reason: Option<String>` refreshed at every
batch build; a `queued` entry with a blocking reason is skipped, shown to the human with that
reason, and becomes eligible again the instant a re-review covers the new head. Seven states,
none of them lying.

**One in-flight batch per target.** Bors discipline: dispatch as soon as the queue is
non-empty and nothing is in flight — no timed accumulation window, entries pile up naturally
while a batch runs. Two reasons beyond simplicity: the fast-forward invariant (§8) is trivial
to reason about when only one batch can be racing the target head, and a single in-flight
batch makes the crash-reconcile question ("what was happening when we died?") have exactly
one answer.

**Restart reconcile — the #467/#468 pattern, copied deliberately.** The delivery queue learned
this the hard way; `mod.rs::recover_persisted_queue:31537` is the shape to mirror:

- **Two phases.** Phase 1 runs under a once-only guard held across the whole phase (the
  `HashSet::insert` doubles as the check): read the file, parse, **push the batch-id sequence
  past the maximum recovered id** so a fresh batch cannot reuse one, classify entries into
  resumable and stranded, audit both. Nothing inside phase 1 may deliver or enqueue — the
  mutex is not reentrant. Phase 2, after the guard drops, sends the notices phase 1 collected.
- **Reconcile against reality, not just the file.** For an entry in `ci-wait` or `landing`,
  the file says what loomux *intended*; the truth is whether the scratch ref still exists,
  whether the draft PR is still open, and where the target head is now. Resume only when the
  world matches the record; otherwise fail the entry **loudly** — audit, notice, terminal
  state. Never silently drop and never silently retry.
- **The snapshot is not deleted on read.** It is rewritten alongside live entries, so recovery
  is re-runnable across N restarts rather than being a one-shot that a crash mid-recovery can
  lose.

## 5. Verification contract

**The queue observes verification. It never defines or runs it.** "The repo's verification" is
whatever the repo's own CI config says, full stop — the same source the shim's `ci-green`
clause already trusts (`mod.rs:877–884`). No toolchain string exists anywhere in queue code.
A repo that builds with `make` needs zero loomux changes. This is CLAUDE.md constraint 8, and
it is the constraint that got the shared `CARGO_TARGET_DIR` removed (#263).

**The observation path: a draft PR from the scratch ref into the target, then poll its
checks.** Push `loomux/mq/<group>-<batch-id>`, open a **draft** PR into the integration
branch, and read its checks. Three reasons this shape and not another:

1. It reliably triggers PR-triggered CI in any repo whose CI runs on PRs — the exact
   assumption the shim's `ci-green` clause already makes.
2. It gives `gh pr checks` a handle, so the classification code that already exists gets
   reused rather than reinvented.
3. It gives the human a URL to watch, which is most of what "observability" means here.

**Reuse the existing classification — do not write a third.** Terminal-state logic exists
twice already: `orchestration/notify.rs::pr_checks_result:310` (with `check_is_pending` at
~299) and `orchestration/intake.rs::parse_pr_list:183`. Both already encode the property that
matters most — **an empty check list is not success**, it is pending. `notify.rs` is the one
to build on, since it also handles the `"no checks reported"` stderr case.

One adaptation is needed and should be written as an adapter over that helper rather than a
fork of it: `pr_checks_result` returns `PollResult::Met` for a **failing** run as well as a
passing one, because for a *watch* "the checks resolved" is the event. The queue needs the
distinction, so it maps the same helper's output into a queue-shaped verdict:

    enum BatchVerification { Pending, Green, Red { failing: Vec<String> }, Unavailable }

Consuming the shared helper and narrowing at the edge keeps one definition of "pending" in the
codebase; re-deriving pass/fail from raw JSON would make three.

**The no-checks case is bounded, because an unbounded wait is the defect this repo keeps
re-learning.** A repo with no CI at all classifies as `Pending` forever. The lessons file's
rule is that *any suppression driven by a fallible signal must be bounded*, and its corollary
is that *releasing on evidence beats releasing on elapsed time*. So: the primary release is
the checks going terminal, and `checks_timeout_minutes` (default 60, clamped like the notify
TTLs) is the **backstop** — on expiry the batch surfaces to the orchestrator as
**unverifiable**, explicitly, rather than sitting pending in silence. Unverifiable is not
green: nothing lands.

**Rejected: watching `workflow_run` events / push-triggered runs directly.** Run-id discovery
after a push is racy, and push-triggered CI is not guaranteed by a repo that only runs CI on
PRs — which is the same repo shape the shim already assumes. A draft PR makes the trigger
explicit instead of hoping for one.

**Cost.** A green *k*-batch is **+1** CI run in total, since each PR already ran its own
checks. A red batch adds roughly ceil(log2 *k*) bisect runs — at the default `max_batch: 3`,
at most 2 extra.

## 6. Gate composition — one gate definition, two enforcement points

**The landing push is a merge the shim never sees.** The shim gates the `gh` binary *inside an
agent pane* via `PATH`; the backend is not an agent pane, and the landing verb is a `git push`
rather than `gh pr merge` in any case. If nothing else were done, the queue would be a hole
straight through the merge gate — an agent could enqueue a PR whose reviewers have not passed
and have the host land it.

So **the backend re-enforces the same gate itself**, by calling the same parsers the shim's
decision mirrors — never by reimplementing the decision:

- `workflow.rs::parse_gate_file:1876` — the gate definition (`require`, `reviewers`, `also`).
- `workflow.rs::parse_verdict_file:1524` — one recorded verdict: line 1 the verdict, line 2
  the **head sha it binds to**, line 3 timestamp, line 4 agent id, line 5 body digest.
- `workflow.rs::Verdict::parse:1298` and `is_blocking:1311` — `pass | fail | escalate`,
  lowercase-strict; anything else is `None`, and **one `fail` beats any number of passes**.
- `workflow.rs::evaluate_merge_gate:1775` — the decision itself.

Read against `verdicts/pr-<N>/<block>` plus the PR's live head. Three properties carried over
verbatim, because dropping any one of them would make the queue a weaker gate than the shim
it sits beside:

- **A stale pass is not a pass.** A verdict whose line-2 sha is not the PR's current head is
  stale (the shim's own `stale` arm, ~848). A rebase therefore silently disarms an entry —
  which is exactly the `blocked_reason` path in §4.
- **The body-digest asymmetry (#565) is preserved.** The shim digest-checks *passes* only
  (`mod.rs:886–922`, `workflow.rs::canonical_body:1432` / `body_digest:1447`). The queue does
  the same, so a PR body rewritten after its approvals cannot smuggle an unreviewed
  description into a batch.
- **`also: [ci-green]` still means the sub-PR's own checks.** The batch's checks are an
  additional signal, never a substitute for the per-PR one.

**Two enforcement points: batch build, and again at landing.** This is the #532 rule —
*re-verify every write gate at the moment of submit* — and it is not redundancy. Between
building a batch and landing it there is a full CI cycle, tens of minutes in which a reviewer
can record a `fail`, a PR can be rebased, or a body can change. Verifying only at build would
land a decision that was true half an hour ago.

One gate definition, two enforcement points, zero drift. **A third implementation of the gate
decision is a defect, not an optimization** — if the queue's needs ever diverge from the
parsers, the parsers move.

## 7. Constraint-7 structural proof

Constraint 7: the queue must be **structurally incapable** of targeting the default branch.
Not "does not", *cannot*. Five layers, each independently sufficient, **none of them keyed on
agent-writable data**.

**A note on which lookup is authoritative.** The default branch used for these refusals is
resolved the way the shim resolves it — `gh repo view <repo> --json defaultBranchRef`
(`mod.rs:759`; note #294: the repo goes **positional**, not through `-R`). It is deliberately
*not* `git.rs::default_base_ref:631`, despite that being the obvious reuse: that helper
answers "what does local git think the default base is", derived from local refs after a
best-effort fetch, and local refs are not an authority a security refusal should rest on.
Reuse is a virtue right up to the point where it changes what a check means.

1. **Enqueue.** `queue_merge` resolves the PR's `baseRefName` and the repo default **live**,
   via the real `gh` — the same two lookups the shim makes at `mod.rs:750` and `759`. Base
   equal to default is refused. **A failed lookup also refuses**, mirroring the shim's
   `unverifiable-base` posture (`mod.rs:761–763`): unknown is never treated as safe.
2. **Batch build.** The target is re-resolved and re-checked against the default. Mismatch
   aborts the batch.
3. **Landing — the write.** Inside the *same function* that constructs the push, re-resolve
   default and target **at the moment of submit**, and only then build the refspec
   `<scratch-sha>:refs/heads/<target>`. This is what defeats the adversarial case: the default
   branch **renamed to the queue target's name** between build and landing. No code path
   anywhere constructs a push refspec from the default-branch name, and the landing function
   takes the validated target as its only branch input — there is no argument it could be
   handed that would make it write elsewhere.
4. **The only landing verb is a fast-forward push to the validated target.** The queue never
   calls `gh pr merge`, so it cannot reach the shim's default-branch arms at all, and the
   per-PR human grant path (`mod.rs:943–960`) is untouched byte-for-byte. Being ff-only also
   means a target that moved makes the push fail rather than overwrite.
5. **Tests pin the refusals**, in `src-tauri/tests/orchestration.rs`: enqueue against the
   default refused; batch build against a target that became the default aborted; the
   adversarial rename refused at step 3; a lookup failure refused rather than defaulted; and
   the landing function's refspec asserted to be exactly `<tested-sha>:refs/heads/<target>`.

**`Task.pr_base` (slice A) is a hint, never a gate input.** It is agent-writable board data.
It exists to make the Approve-button relabel accurate (`docs/orchestration.md`) and to let the
queue *display* a base without a `gh` round-trip. Every decision above re-resolves live. The
field's doc comment says so, so it never gets promoted into a gate input by a future reader
who sees a convenient string sitting there. This is the constraint-6 lesson applied to a field
instead of a command argument.

## 8. Batch construction & the Bors invariant — DECIDED

**Decision: the scratch ref is the current integration head plus one merge commit per queued
PR head, in queue order.** Adjudicated on #581; recorded here with its loser and its seam.

    scratch = target_head
    for entry in batch:            # queue order, deterministic
        scratch = merge(scratch, entry.pr_head)   # conflict -> kick back, no CI spent

Land with `git push origin <scratch-sha>:refs/heads/<target>`, fast-forward only.

**Why: the SHA that was tested is the SHA that lands.** That is the Bors invariant and it is
the entire point of the feature. CI ran on `<scratch-sha>`; the object that becomes the new
integration head *is* `<scratch-sha>`, byte for byte, not a rebuild that resembles it. A
second benefit falls out free: each sub-PR's head becomes reachable from its base, so GitHub
marks those PRs **merged** on its own, with no extra API calls and no manual closing.

**Rejected alternative: per-PR squash replay** — squash each PR onto the target in turn after
a green batch. It is genuinely attractive: it matches this repo's stated linear-history
preference, and it is what a human does by hand today. It loses on the invariant. Replay
**rebuilds** every SHA, so what lands is not what CI tested — the batch's green becomes
evidence about commits that no longer exist, which is the precise failure the queue was built
to prevent. It also needs manual PR closing, since no replayed commit contains the PR's head.

Linear history is preserved where it actually matters: **the final human squash of the
integration branch into `main` keeps `main` linear**, exactly as it does today. The
merge-commit noise lives only on an ephemeral integration branch that is deleted after that
squash.

**The reversal seam.** The choice is isolated in **one function** in `mergeq.rs`:

    fn build_scratch(target_head: &str, entries: &[Entry], git: &impl GitRunner)
        -> Result<ScratchRef, BatchBuildError>

Nothing outside it knows how the scratch was built; every downstream stage takes a SHA.
Reversing to squash replay is a rewrite of that function and its tests — **plus** the landing
verb, since replay cannot land by ff-push of a single tested SHA. That second half is stated
here so a future reverser does not discover it halfway through: the seam makes reversal
contained, not free.

**Conflicts cost no CI.** A merge that conflicts during construction kicks that entry back
immediately, before anything is pushed; the batch rebuilds without it.

### The squash-merge auto-close gotcha, restated

Already-documented territory, restated because the queue changes where merges happen and a
reader here will be reasoning about merge commits:

- GitHub's closing-keyword scan (`close`/`fix`/`resolve` immediately followed by `#N`, any
  inflection, anywhere in the body or the aggregated commit message) fires on merge **into the
  default branch**. Batch merge commits reach only the **integration** branch, so a stray
  `Closes #N` in a sub-PR body **cannot** fire at batch landing.
- It fires later, at the **final human squash of integration → `main`**, whose aggregated
  message carries every sub-PR's text — and it will close whatever it names, however partial
  that PR's scope actually was. The precedent is on the record: #569 and #590 were both
  auto-closed with real scope still open, and #569 a second time by a PR that linked
  `Part of #569` deliberately and still tripped the scan on a sentence *asking a human* to do
  it by hand.
- **The queue does not scrub bodies.** Rewriting agent- or human-authored PR text to defuse a
  keyword is loomux editing the record, which is worse than the problem. The owner of the
  final squash message stays the human who performs it.
- **The queue's own text is loomux-authored and must never contain the pattern.** The batch
  draft PR body lists its sub-PRs as bare `#N` references with no keyword in front of them,
  and the culprit comment (§9) does the same. That is a rule the batch-body builder's tests
  pin, not a habit.

## 9. Bisect & attribution

**Mechanical attribution host-side; routing judgment orchestrator-side.**

The search, on a red batch:

- **k = 1** — that PR is the culprit. No further CI.
- **k > 1** — split the batch in half, build a scratch from the first half, run it, and
  recurse into the half that reproduces red. ceil(log2 k) runs; at `max_batch: 3`, at most 2.
- Bisect depth is bounded by `max_batch`, so the search always terminates.

On a culprit:

- **A comment on the culprit PR** — the durable record, where a human or the owning worker
  will actually look: failing check name, run link, batch id, and the sibling set. All
  gh-sourced text passes through `notify.rs::sanitize_gh_text:494`, the same sanitizer every
  crossing-text boundary in this codebase uses.
- **One decision-grade notice to the orchestrator.** One fact that changes what it does next,
  plus the PR link — not a narration.
- **Survivors are auto-requeued**, at the front, preserving their original order. They were
  never implicated; making them wait behind newly-enqueued work would punish them for a
  neighbour's failure.

**It does not auto-brief the owning worker.** Worker liveness, resume-versus-fresh-spawn, and
folding this feedback with whatever else is pending are judgment calls, and the board mapping
`pr → assignee/session` that equips them belongs to the orchestrator. The backend produces the
*fact*; the orchestrator makes the *call*.

**Honest limit: bisect finds _a_ culprit, not necessarily _the_ culprit.** A genuine pairwise
interaction — A fine alone, B fine alone, red together — attributes to whichever entry the
search isolates, which depends on the split. The comment says so in as many words and names
the batch id and the sibling set, so the reader can see the interaction rather than being told
a half-truth confidently. Overclaiming here would be exactly the "a claim is a deliverable"
failure the lessons file records.

**A batch that is still red at k = 1 while that PR's own checks are green** is surfaced as an
infrastructure/flake case — a notice saying so — not looped on. Same bound as everywhere else.

## 10. Failure modes & bounds

Every row below is a designed path, an audit event (§11), and a test.

| Failure | Behavior |
|---|---|
| Push auth failure | Batch aborts, entries return to `queued`, cleanup runs, orchestrator notice, audit. **No retry loop.** |
| `gh` not installed | `gh_output`'s `"gh-not-found"` sentinel (`gh.rs:69`) propagates; the queue reports itself **unavailable** rather than silently doing nothing. |
| Merge conflict during construction | That entry kicks back immediately; batch rebuilds without it. **No CI spent** (§8). |
| CI never attaches / repo has no checks | Bounded by `checks_timeout_minutes`; surfaces as **unverifiable**. Nothing lands (§5). |
| Target head moved under an in-flight batch | The ff-only landing push **fails**. Batch aborts, entries requeue, next build starts from the new head. Never `--force`. |
| Gate re-check fails at landing | Landing refused, that entry kicks back, survivors requeue (§6). |
| Crash mid-batch | Reconcile from `merge_queue.json`: verify the scratch ref exists, the draft PR is open, and the target head matches. Resume only if the world matches; otherwise fail entries **loudly**. Never drop silently (§4). |
| Entry cancelled while `batching`/`ci-wait` | The in-flight batch is abandoned and rebuilt without it; cleanup runs. |
| Queue grows without bound | Entries are capped (`64`); enqueue past the cap is refused with a stated reason, keeping `merge_queue.json` bounded. |

**Cleanup runs on every exit path** — green, red, conflict, timeout, cancel, crash-reconcile,
and abort. Close the draft PR, delete the scratch ref. Two rules on the deletion:

- **Namespace only.** Only refs under `loomux/mq/*` are ever deleted, by **exact name** built
  from the batch id — never a pattern sweep, never a glob, never "delete branches matching".
  A cleanup routine that can be talked into a wildcard is a data-loss bug waiting for its
  input.
- **Cleanup failure never blocks landing and never fails a batch.** It audits
  (`mq-cleanup-failed`) and leaves a ref behind, which the next reconcile can see. A leaked
  scratch ref is cheap; a batch held hostage by a failed `git push --delete` is not.

## 11. Public contracts

Each item below is a **public contract** in the CLAUDE.md sense — a command signature, a wire
shape, a file format, or a persisted schema — and this note is their design note.

### 11.1 MCP tools (three, orchestrator-role-gated)

Built via the `add-orch-tool` skill so every layer moves together. Role gating uses the
`review_verdict` **double-gate** precedent: a cosmetic listing filter *and* a real dispatch
check, because a tool omitted from a listing is still callable.

    queue_merge(pr: number, target?: string)
      -> { queued: true, position: n } | { refused: "<reason>" }
      refusals: base-is-default | base-unverifiable | gate-not-met | already-queued
              | queue-full | queue-disabled

    merge_queue_status()
      -> { enabled, target, entries: [{ pr, state, blocked_reason?, since_ms }],
           batch?: { id, prs, state, draft_pr, checks } }

    cancel_queued_merge(pr: number)
      -> { cancelled: true } | { refused: "<reason>" }

### 11.2 The `merge_queue:` block in `.loomux/workflow.yml`

A sibling of the existing `gates:` block, parsed in `workflow.rs` alongside it:

    merge_queue:
      enabled: true                 # default false
      max_batch: 3                  # default 3
      checks_timeout_minutes: 60    # default 60, clamped like the notify TTLs

**An absent block means the feature is off and behavior is byte-for-byte unchanged** — the
same posture `gates:` takes. Parse errors are **loud**, following the existing
`workflow-invalid` audit path; a malformed block never degrades to defaults, because a queue
running on silently-substituted policy is a queue nobody can reason about.

*A correction to the plan worth recording:* the plan and CLAUDE.md constraint 8 both cite "the
way the resource guard's `resources:` block does" as the precedent. **That block does not exist
in `workflow.rs` today** — constraint 8 names it as the intended pattern, not as shipped code.
So `merge_queue:` follows the *principle* (loomux carries the mechanism; the repo declares what
is guarded, expensive, or built here) and the *shape* of the block that does exist, `gates:`.
Stating this here so the next reader does not go looking for a parser to copy.

### 11.3 `merge_queue.json`

    {
      "version": 1,
      "target": "feat/integration-batch-2",
      "entries": [
        { "pr": 612, "head": "<sha>", "state": "queued",
          "blocked_reason": null, "enqueued_ms": 0, "batch": null }
      ],
      "batch": { "id": "mq-7f3a", "prs": [612, 613], "scratch_sha": "<sha>",
                 "draft_pr": 640, "state": "ci-wait", "started_ms": 0 }
    }

Unknown fields are tolerated on read (forward compatibility), and the file is never deleted by
recovery — rewritten alongside live entries, the #467/#468 posture.

### 11.4 The scratch-ref namespace

    refs/heads/loomux/mq/<group>-<batch-id>

**Reserved.** Nothing else in loomux writes under `loomux/`, and the queue writes nowhere else.
Batch ids come from the std `RandomState` idiom — **no getrandom-based crates** (constraint 2;
see the notes in `src-tauri/Cargo.toml`).

### 11.5 Audit events

Emitted through the registry's `audit(group, actor, action, detail)` (`mod.rs:17572`), kebab-
case, matching the existing convention (`queue-recovered`, `merge-gate-allowed`, …):

`mq-enqueued` · `mq-enqueue-refused` · `mq-batch-built` · `mq-batch-pushed` ·
`mq-checks-green` · `mq-checks-red` · `mq-checks-unverifiable` · `mq-landed` ·
`mq-land-refused` · `mq-bisect-step` · `mq-culprit` · `mq-kicked-back` · `mq-cancelled` ·
`mq-cleanup-failed` · `mq-recovered` · `mq-stranded`

Every state transition and every external write appears here. An audit action must name what
actually happened — labeling a failed landing as a success is the exact defect class #461
catalogues.

### 11.6 Frontend surface (slice F)

One read-only Tauri command `orch_merge_queue` plus a typed wrapper in `src/pty.ts`
(constraint 5 — the frontend never touches IPC directly), feeding a DOM-free
`src/mergequeue.ts` model. Presentation is **overlay/chrome only — never a PTY resize**
(constraint 1).

## 12. Rollout

- **Product default: off.** No `merge_queue:` block, no behavior change anywhere. This is not
  a soft default — it is the reversal mechanism (see the last bullet below).
- **Enabled per repo**, in that repo's `.loomux/workflow.yml`, exactly like `gates:`.
- **Ordering** (plan part 7): A ∥ B → C (gated on this note's sign-off) → D (also needs A;
  both touch `mod.rs`) → E; F after C, parallel with D/E. Slices A, D and E serialize on
  `mod.rs`/`mcp.rs`; the queue engine is a new file (`orchestration/mergeq.rs` — `queue.rs` is
  taken by the delivery queue) precisely so `mod.rs` touches stay wiring-only.
- **Dogfooding this repo is a proposal, not a consequence of this note.** A `merge_queue:`
  block for loomux itself is drafted in slice E for the human to accept or decline. Turning
  the queue on for the repo that develops the queue is a decision with its own risk, and it is
  not one a design note gets to make on the human's behalf.
- **`templates/orchestrator.md` gains a queue-mode addendum** (slice E): the non-default-branch
  line at ~667 gains a when-queue-enabled clause, and the merge-frontier section (~744–770)
  gets the observation that the queue *reduces* the O(n²) fan-rebase pressure it warns about —
  siblings need no proactive rebase after each batch, because the speculative merge **is** the
  mergeability probe and only a real conflict kicks back.
- **Reversal:** delete the yml block and the feature is off; with `enabled: false` every queue
  code path is unreachable, and a test pins that. The two flagged decisions have their own,
  narrower reversal seams (§3, §8).

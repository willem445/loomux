# Golden templates — what a default group reads, pinned

These are byte copies of `src/orchestration/templates/{orchestrator,worker,reviewer,planner}.md`,
**seeded** from the commit *before* the advanced-orchestrator toggle (`4b93282`, the #222
integration branch with the block model and the workflow pane on it, and nothing else) —
which is where the directory's name comes from.

They are not frozen forever: they are the *last human-blessed* copy. When the role
templates deliberately change, the fixture is re-blessed (see below) and the diff on this
directory is the record of what every default group was told to do differently. Re-blessings
so far:

- **#222, findings-disposition policy** — `orchestrator.md` (disposition step in the review
  loop; open-question merge hold; "the codebase's advocate" posture) and `reviewer.md`
  (findings labelled blocking/non-blocking; an approval with findings open must say so).
  `worker.md` and `planner.md` still stand exactly as they did pre-#222.

- **#236, engineering standards** — all four files, the first time `worker.md` and
  `planner.md` have moved since the seed. `orchestrator.md`: an **Engineering standards**
  section (concrete grounds to reject a plan or bounce a PR — coupling, a duplicated
  mechanism, an unargued dependency, a contract change with no design note), gated at plan
  intake *and* the completion check; red-before-green evidence demanded and an unevidenced
  `done` refused; post-merge ownership of the default branch (red main stops the queue, one
  fix-forward attempt, then revert); **re-sync the fleet** — every open branch rebases when
  the default branch moves, stale as well as conflicted; a learning loop; and permission to
  *file* an issue (never to *start* one). `worker.md`: red-before-green in the DoD, plus the
  stated-constraints bar (dependencies, public contracts, reuse). `reviewer.md`: three new
  lanes — security/trust boundaries, dependency hygiene, algorithmic cost — and the duty to
  *verify* the author's evidence rather than read it. `planner.md`: a plan must address
  boundaries, reuse-before-invention, dependencies, public-contract changes and the
  alternatives it considered.

- **#239, the verdict bind** — `reviewer.md` and `orchestrator.md` only (`worker.md` and
  `planner.md` did not move). Carried forward from #238, which found that the rule every
  default group had been reading — *"a blocking finding means `--request-changes`, not
  `--approve`"* — **cannot be obeyed in this repository**: GitHub refuses both flags on a PR
  opened by your own account, which is every PR here, because a whole group authenticates as
  one GitHub user. A reviewer that could not `--request-changes` had been given no legal way
  to say "no", and the only other action the template named was `--approve`. So what a default
  group's reviewer is now told: the binding record is **the verdict you state** in the review
  body and repeat in `report(...)` — the channel the orchestrator actually merges on — with
  `--comment` named as the fallback when GitHub refuses the flag, and a refusal that may never
  decay into an approval or a softened verdict. The orchestrator is told the same in its
  disposition step, and its merge gate now says **where the verdict lives** (not GitHub's
  review state, which stays `COMMENTED` on a same-account PR — an orchestrator that looked
  there would find no approval to gate on, or would read `COMMENTED` as one).

- **#264, loop until green** — `worker.md` only. A new **Loop until green** section
  between the git workflow and the definition of done: open the PR as a draft early
  and loop by pushing fixes until `gh pr checks` is green on every platform, then
  `gh pr ready` — never mark a PR ready, or report `done`, while CI is red. If a
  worker genuinely cannot reach green after a real attempt, it reports `blocked` and
  says so on the issue instead of marking the PR ready. Pairs with the
  orchestrator's existing **CI gate** (unchanged here) — this is the worker-side half
  that keeps that gate a formality instead of a fix loop it inherits. Re-blessed
  twice more in the same PR:

  - **rev-10's delta review**: the section originally told workers to loop with
    local `cargo`/`npm` commands before opening the PR, which #321's interim,
    group-wide ban on local builds (`ci-validate`, #320) made unfollowable — the
    loop was reframed onto the draft PR's own CI, per that skill, with the
    local-command instructions dropped entirely.
  - **rev-30's delta review**: #321 was itself repurposed mid-flight from that
    interim hard ban toward a per-class concurrency guard (#318/#322) that would
    have gated local runs on the guard being confirmed active. The absolute
    "agent workers don't run `cargo`/`npm` on the host" parentheticals softened
    into a deferral to the `ci-validate` skill for when that applied.
  - **rev-3's delta review**: the guard (#322) was shelved before merging — its
    shim only caught Bash-tool invocations, not PowerShell/cmd, so the coverage
    wasn't worth the complexity. #321's current head drops the guard precondition
    entirely: quick local iteration is *unconditionally* fine, capped at `-j 4`;
    only full/longer-running validation defers to CI. The parentheticals were
    reworded again to match — "capped at `-j 4`", no guard, no precondition — and
    this fixture's own "confirmed active" language is gone along with it. The
    draft-PR/loop-until-green/blocked-report shape underneath is still unchanged.
- **#266, fine-grained plan steps** — `worker.md` and `planner.md` only. `planner.md` gains a
  **Steps** bullet in the plan format: decompose the approach into small, individually
  verifiable steps, each naming its own verification (a test going red then green, an
  observable output, a specific file or state), sized so a worker can complete and verify one
  before starting the next. `worker.md` gains an **Execute the plan step by step** section:
  work the brief's steps one at a time, verify each against its own stated check before moving
  on, and treat a step whose verification won't pass after a real attempt as something to
  report rather than quietly skip past.

- **#337, CONFLICTING never gets checks** — `orchestrator.md` and `worker.md` only. A
  `notify_when(kind: "pr_checks")` watch now resolves the moment its PR goes
  `CONFLICTING`, with its own distinct notice, instead of polling `gh pr checks` toward
  expiry against a PR GitHub will never create a check-suite for. `orchestrator.md`'s
  **The CI gate** section and `worker.md`'s "waiting on your own PR's CI" bullet both
  gain a one-line pointer to that behavior.

- **#338, explicit worktree requirement** — `orchestrator.md` only. `spawn_agent`'s tool
  description and the **Planning & scheduling** section both drop "a plain branch in the repo
  (`worktree: false`) is fine" — a worker spawn now always cuts a dedicated worktree and
  cannot turn it off; there is no more shared-repo option to describe. **Re-sync the fleet**'s
  "clean and trivial: do it yourself" bullet gains the mechanical-work convention: do the
  checkout in the PR's own worker worktree if it still exists, otherwise in a staging worktree
  of your own (`<repo>-worktrees/orch-staging`, reused across mechanical work) — never in the
  main clone, which is the human's environment.

`the_toggle_off_leaves_every_instruction_file_byte_for_byte_what_it_was` renders
**these** with the six pre-#222 template variables and asserts that a group launched
with the advanced orchestrator **off** gets exactly that text. They are the
*independent* side of that comparison, and that is their whole point: the first
version of the test built its expected value out of the live template, so both sides
moved together and unconditional prose added to a template sailed through the very
pin advertised to stop it (rev-11 F1).

## If this test fails

It is telling you that **the text every agent in every default group reads has
changed**. That is not automatically wrong — but it is never incidental, so it needs
a human, not a re-run.

- If you *meant* to edit the role templates, re-bless the fixture: copy the changed
  template over the file here, in its own commit, and say in the message what
  changed for the agents. The diff on this directory is then the review surface for
  "what did we just tell every worker to do differently?".
- If you did **not** mean to change what a default group reads — you were adding
  workflow-conditional prose — then the prose is in the wrong place. It belongs in
  `templates/workflow.md` or `templates/block.md`, behind `{{WORKFLOW}}` /
  `{{BLOCK_NOTE}}`, which resolve to the empty string for the built-in roster.

Line endings are normalized before comparison (there is no `.gitattributes`, so these
are CRLF on Windows and LF elsewhere) — the assertion is about the words, not about
the checkout.

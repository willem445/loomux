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
- **#328, `request_compact` as the primary compact mechanism** — `orchestrator.md` only.
  The pre-existing "Compact at lulls" invariant used to tell the orchestrator to type
  `/compact` itself and then manually treat the next turn like a session start. It now
  calls `request_compact()` as the last action of a turn instead (loomux pastes `/compact`
  once the pane is actually idle, never mid-turn), names the pre-compact offload checklist
  as a precondition (`request_compact` warns, never blocks, if it looks skipped), and drops
  the manual re-sync instruction now that loomux's own mandatory post-compact re-injection
  does that automatically. It also tells the orchestrator what a `[loomux] context at NN% …`
  escalation notice means.

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

- **#359, extend the worktree requirement to reviewers** — `orchestrator.md` and `reviewer.md`.
  Live incident: two reviewers (rev-36, rev-38) collided in the shared main clone — one checked
  branches out and restored `main` while the other was mid-review on a different branch, knocking
  it off its checkout. `spawn_agent`'s worktree default/reject guard (#338) now covers reviewers
  exactly like workers: `worktree` defaults on and `worktree: false` is rejected for either.
  `orchestrator.md`'s `spawn_agent` bullet states the guarantee for both roles and the incident
  it closes. `reviewer.md`'s **Review protocol** step 1 explains the worktree is scratch space cut
  from the default branch, not a checkout of the PR under review (that branch may already be
  checked out in the worker's own worktree) — and gives the `gh pr checkout <n> --detach`
  convention for inspecting the PR's actual code locally, since a bare `gh pr checkout <n>` grabs
  the branch by name and collides with whichever other worktree already holds it.

- **#339 refinement, reopening state honesty** — `orchestrator.md` only. A new bullet in **The
  task board** section: reopening a `pr`/`human-testing` item (routing reviewer findings back to
  a worker) must flip `status` back to `in-progress` the same moment, not just eventually — the
  board's Approve button is gated on status alone, so leaving a reopened item's status untouched
  leaves Approve showing on work that is no longer ready. Pairs with the board itself now doing
  this automatically for the human's own **✎ Changes** action.

- **#329 expansion, the directive ledger** — all four files. A new `note_directive(text,
  replace?)` tool bullet, and a **Directive ledger** section (`orchestrator.md` folds it into
  **Durability rules** instead, alongside the existing compact material): record a human
  directive, scope decision, or piece of feedback via `note_directive` BEFORE acting on it —
  a diary kept at receipt time, because the CLI's own emergency auto-compact gives no warning
  turn to offload one first. Curate the ledger (`replace: true`) once a compact re-grounds an
  agent in its own tail. `orchestrator.md`'s existing "Compact at lulls" text also gains one
  sentence: loomux now recognizes the CLI's own emergency auto-compact when it happens (not
  just the three loomux-initiated/human-typed paths #328 covered) and re-grounds the pane the
  same way — but only durable state already offloaded comes back, which is what the ledger is
  for.

- **#398, terse decision-grade reports** — all four files. Every `report(...)` tool-doc bullet now
  teaches the structured shape (`outcome`/`ref`/`detail_url`/`note`, the note hard-capped ~500
  chars by the tool itself) instead of the free-text `status`/`summary` pair — every role's report
  is a **notification, not the record**: the full detail (PR body/comment, issue comment, review
  body) is posted to GitHub FIRST, and the report just points at it. The legacy shape still works
  (soft-deprecated: accepted, but no longer taught). `worker.md`'s **Review findings** section and
  `orchestrator.md`'s worker-reports-a-PR step 2 both flip the request-changes loop the other way:
  the orchestrator routes one line ("read the findings and revisit"), never the findings
  themselves — the worker reads them off the PR directly.

- **#332, event-driven intake wake** — `orchestrator.md` only, landed in the same PR as #398
  above (both attack the orchestrator's context from opposite ends: #398 shrinks inbound report
  bloat, #332 eliminates empty idle-tick turns). The **Autonomous mode (idle-tick)** section gains
  a paragraph naming the host-side gate: a zero-token poll checks for new intake-label/PR-check-
  state signals before an idle tick fires, a tick with nothing new (and no other wake reason — a
  pending CI watch, a watchdog stall) is skipped quietly and audited rather than spending a turn,
  a bounded fallback still wakes the orchestrator unconditionally on a slow cadence regardless,
  and a tick that DOES fire because of the gate names what changed so the orchestrator doesn't
  re-poll it. `worker.md`/`reviewer.md`/`planner.md` are untouched by this one.

- **Benchtest findings on #398/#332 (rev-31's live testbed run), two more re-blessings in the same
  PR:**
  - **Terse reports still triggered reflexive `gh` re-reads.** The testbed's audit + transcript
    forensics showed the orchestrator re-reading a PR's diff/body/comments/mergeable-state
    repeatedly across consecutive same-verdict reports (25 `gh` calls across one PR's review
    lifecycle, several of them exact repeats). `orchestrator.md` gains an **"act on the report,
    don't re-derive it"** rule right where `report(...)` is first introduced (read the artifact
    only when the next action needs its CONTENT — CI/mergeable state for a merge, nothing for a
    routing hand-off) and step 2's worker-reports-a-PR hand-back is tightened to an explicit
    one-line template with a named, bounded exception (an ADDITIVE delta only — context the
    reviewer lacked — never a restatement). `worker.md`/`reviewer.md`'s `report(...)` bullets gain
    mandatory per-outcome examples for what earns `note` space; `reviewer.md`'s is reserved for
    orchestrator-decision-relevant facts (needs-human-decision, cross-PR conflict, accepted
    residual+tradeoff, a blocker's one-sentence mechanism) and explicitly never a findings summary.
  - **Compact-nudge min-context floor.** The same testbed run showed 3-4 real compactions, all at
    ~20-31% context — the lull timer's quiet-window gate firing at the right moment but the wrong
    context level. `orchestrator.md`'s existing **Compact at lulls** paragraph (#328/#329) gains a
    sentence naming the new floor and telling the orchestrator not to call `request_compact` out
    of lull habit below roughly 50% — the tool itself stays unconditionally available at any
    context level (agent judgment always wins); only loomux's own unprompted heuristic nudge is
    gated. `worker.md`/`reviewer.md`/`planner.md` are untouched by this one too (compact-nudge is
    orchestrator-only by default).

- **Smart-default re-blessing (rev-65's review of the min-context floor above)** —
  `orchestrator.md` only. The floor as first shipped was a plain `u32` defaulting to `0`
  (off) — a re-benchtest at default config would have reproduced the exact over-compaction
  the floor exists to fix, since nothing turns it on without a manual setter call.
  `compact_nudge_min_context_percent` is now tri-state (`Option<u32>`: unset → the 50%
  smart default applies automatically the moment the quiet-window (`compact_nudge_minutes`)
  is on; explicit `0` → floor disabled; explicit `N` → `N`), resolved fresh on every gate
  check rather than baked in at group creation, so turning the quiet-window on later still
  gets the default with no re-launch. `orchestrator.md`'s **Compact at lulls** paragraph is
  reworded to say the floor is "automatic the moment the quiet-window is on, nothing to
  configure" instead of describing a value the operator would otherwise have had to set.

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

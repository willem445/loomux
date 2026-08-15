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

- **#423, an unloaded workflow config is not this group's roster** — `orchestrator.md` only. A
  live incident: an orchestrator found an untracked, leftover custom-workflow config on disk
  (from an earlier custom-roster session on the same repo checkout) and adopted its declared
  blocks and process steps as though they were this group's actual config — dispatching work
  the built-in template's own funnel discipline would have refused. A new paragraph, right
  after the `{{WORKFLOW}}` substitution point (so it reaches EVERY default group, not only
  ones with a declared workflow): a custom workflow config is this group's roster only when
  the kickoff itself named it (the `{{WORKFLOW}}` paragraph above, non-empty); a config merely
  found on disk some other way is not, and must not be adopted — mention the discrepancy to
  the human once and continue with the roster actually in effect. (Deliberately avoids the
  literal token `workflow.yml`, which `the_default_rendering_never_names_the_gate_machinery`
  refuses in a default rendering for an unrelated, still-valid reason — a default group has no
  gate/verdict mechanism to be sent after; this paragraph is about the OPPOSITE case, so it
  says "a custom workflow config" instead.)

- **#398, terse decision-grade reports** — all four files. Every `report(...)` tool-doc bullet now
  teaches the structured shape (`outcome`/`ref`/`detail_url`/`note`, the note hard-capped ~500
  chars by the tool itself) instead of the free-text `status`/`summary` pair — every role's report
  is a **notification, not the record**: the full detail (PR body/comment, issue comment, review
  body) is posted to GitHub FIRST, and the report just points at it. The legacy shape still works
  (soft-deprecated: accepted, but no longer taught). `worker.md`'s **Review findings** section and
  `orchestrator.md`'s worker-reports-a-PR step 2 both flip the request-changes loop the other way:
  the orchestrator routes one line ("read the findings and revisit"), never the findings
  themselves — the worker reads them off the PR directly.

- **#398 benchtest follow-up: terse reports still triggered reflexive `gh` re-reads** (live
  testbed run, loomux-testbed-cc077f09). The testbed's audit + transcript forensics showed the
  orchestrator re-reading a PR's diff/body/comments/mergeable-state repeatedly across
  consecutive same-verdict reports (25 `gh` calls across one PR's review lifecycle, several of
  them exact repeats). `orchestrator.md` gains an **"act on the report, don't re-derive it"**
  rule right where `report(...)` is first introduced (read the artifact only when the next
  action needs its CONTENT — CI/mergeable state for a merge, nothing for a routing hand-off)
  and step 2's worker-reports-a-PR hand-back is tightened to an explicit one-line template with
  a named, bounded exception (an ADDITIVE delta only — context the reviewer lacked — never a
  restatement). `worker.md`/`reviewer.md`'s `report(...)` bullets gain mandatory per-outcome
  examples for what earns `note` space; `reviewer.md`'s is reserved for orchestrator-decision-
  relevant facts (needs-human-decision, cross-PR conflict, accepted residual+tradeoff, a
  blocker's one-sentence mechanism) and explicitly never a findings summary. `planner.md` is
  untouched by this one.

- **#332, event-driven intake wake** — `orchestrator.md` only. The **Autonomous mode (idle-tick)**
  section gains a paragraph naming the host-side gate: a zero-token poll checks for new
  intake-label/PR-check-state signals before an idle tick fires, a tick with nothing new (and no
  other wake reason — a pending CI watch, a watchdog stall) is skipped quietly and audited rather
  than spending a turn, a bounded fallback still wakes the orchestrator unconditionally on a slow
  cadence regardless, and a tick that DOES fire because of the gate names what changed so the
  orchestrator doesn't re-poll it. `worker.md`/`reviewer.md`/`planner.md` are untouched by this
  one. **rev-33 N2 fix (#429):** this paragraph named the watched label `agent-investigate`,
  missing the `-ion` the real GitHub label (and the poller's own `INTAKE_LABELS` const) actually
  uses — corrected to `agent-investigation`.

- **Compact-nudge min-context floor (benchtest finding on a live testbed run of this feature)** —
  `orchestrator.md` only. The same run that exercised the idle-tick gate above also showed 3-4
  real compactions, all at ~20-31% context — the lull timer's quiet-window gate firing at the
  right moment but the wrong context level. `orchestrator.md`'s existing **Compact at lulls**
  paragraph (#328/#329) gains a sentence naming the new floor and telling the orchestrator not to
  call `request_compact` out of lull habit below roughly 50% — the tool itself stays
  unconditionally available at any context level (agent judgment always wins); only loomux's own
  unprompted heuristic nudge is gated. `worker.md`/`reviewer.md`/`planner.md` are untouched by
  this one too (compact-nudge is orchestrator-only by default).

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

- **#445, delivery queue** — `orchestrator.md` only. A hold-cap expiry in `deliver_prompt`
  (the pane's box had human input in it, or an interactive question was on screen) used to
  DESTROY the prompt and tell the orchestrator "held: ... — re-send when clear" — misleading,
  since nothing was left to re-send to. It now safely QUEUES the prompt and flushes
  automatically, in order, the instant the pane is deliverable again (no timeout — the release
  condition is a human answering, which can take hours). The **Silent-agent recovery** section's
  held-delivery paragraph is rewritten: never re-send on a `queued` notice (it would just
  duplicate the entry already waiting), read the flush header before acting on what follows, and
  only a `DROPPED` notice (queue full, or the agent's pane closed) means the payload is actually
  gone and needs re-deriving.

- **#465, dontAsk narrows a planner's ad hoc shell reach** — `planner.md` only. Closing
  the fail-open direction (a NEW Claude Code editing tool silently working because
  nothing named it in a deny list) moved a planner from `auto` permission mode to
  `dontAsk` (pre-approved tools only — see `claude_effective_permission_mode`'s doc and
  `doc/design/orchestration.md`'s `#465` section). One side effect: `auto` mode's
  background safety-classifier used to let a planner run an ad hoc shell command (e.g.
  `cargo check`) with no prior approval; `dontAsk` denies anything not in
  `--allowedTools` or Claude's own built-in read-only Bash set outright, with no
  classifier fallback. Step 2 of the **Planning protocol** now says so: only
  `git`/`gh` and the built-in read-only commands are reachable by default, and a
  planner that needs something else says so in the plan rather than assuming it ran.
  **Review round 1 (#489) correction:** the first version of this step claimed a
  build/typecheck command "is denied outright unless it was separately pre-approved
  for your block" — checked against the code and false. Two #222 capability-closure
  guards (`workflow.rs:891`'s parser refusal, `mod.rs`'s `persona_inject` emptying
  `extra_allow` for every read-only block regardless of source) make the denial
  absolute: there is no per-repo opt-in, by any mechanism, today. Corrected to say so
  plainly; the opt-in question itself is filed separately as #490 rather than answered
  here or built into this fix.

- **#462, the reviewer's editing tools are denied at the CLI** — `reviewer.md` only. Reviewer
  containment used to be instruction-backed *only*: `Role::is_read_only()` matched the planner
  alone, so a reviewer's CLI got no deny flags and nothing structurally stopped it editing
  files. It now launches under `Containment::NoEdits` — `--disallowedTools Edit Write
  NotebookEdit` on Claude, `--deny-tool "write"` on Copilot. A closing paragraph tells the
  reviewer that, so the first denial reads as policy rather than as a broken environment, and
  names what is deliberately untouched (the shell for tests and `gh pr checkout --detach`, `gh`
  for posting the review) plus the one legitimate write — a review body too long for `--body` —
  and the shell route to it. The *contract* is unchanged: a reviewer was already told it does
  not fix and does not push; this only says which half of that a flag now enforces.


- **#507, bulk board approvals** — `orchestrator.md` only. The human can now tick several board
  rows and approve them in one action, so the merge gate can deliver **one** notice naming
  several PRs where it only ever delivered one per PR. The **merge gate** section gains a bullet
  under the one-time-grant one: the consolidated shape it will now receive (`GRANTED one-time
  merges of PRs #a, #b, #c …`, per-task notes on their own trailing lines, items approved with no
  resolvable PR number called out separately), and — the half that matters — how to read it: N
  ordinary per-PR grants delivered once, not one broad permission. Merging a listed PR opens no
  other, a PR not on the list is not granted, and one grant expiring or being consumed leaves
  the rest untouched. Without that paragraph a single sentence listing three PRs is exactly the
  thing an orchestrator could over-read into merging something the human never authorized — the
  failure the gate exists to prevent. The backend is unchanged in authority: each PR still gets
  its own single-use, ~30-min grant file. `worker.md`/`reviewer.md`/`planner.md` are untouched.

- **#468/#467, the delivery queue survives a restart** — `orchestrator.md` only. The queue #445
  shipped was in-memory, so a loomux restart destroyed whatever was queued behind a blocked pane;
  it is now written to disk on every mutation. What a default group's orchestrator is now told:
  add `queue_orphans()` to the session-start re-sync, and treat a non-empty result as a **to-do
  list, not a log** — each row is something somebody sent that nobody received, so re-send it to
  a pane that exists now or say you are dropping it as stale, never silently. Deliveries that
  loomux could re-bind on its own (the orchestrator's own pane, or an agent resumed onto the same
  session id) are re-queued automatically in their original order and are deliberately NOT in
  that list. The **Silent-agent recovery** held-delivery paragraph gains the three post-restart
  notices and what each means, with the rule that two of the three describe deliveries already on
  their way — so "never re-send without checking `queue_orphans()` first". One case is named as
  genuinely unrecoverable: text already typed into a pane and waiting only for Enter when loomux
  restarted, where the pane is gone and no bytes remain to re-send. `worker.md`/`reviewer.md`/
  `planner.md` are untouched — `queue_orphans` is orchestrator-only, and a delegate's side of
  the queue contract (never re-send on a `queued` notice) did not change.

- **#590, delegates never block a turn on CI** — `worker.md` and `reviewer.md` only
  (`orchestrator.md` and `planner.md` did not move). Live deadlock on #577: a worker
  registered a `notify_when` CI watch **and also** blocked its own turn on a shell-level wait
  for the same checks. The PR had gone `CONFLICTING` under two merges, so GitHub was never
  going to create the check-suites that wait was blocked on — and the watch's own CONFLICTING
  notice (#337, built for exactly this) is delivered by *typing into the pane*, which a pane
  mid-turn cannot accept. The turn was waiting on a resolution queued behind itself; 20+
  minutes, broken by the host watchdog plus a human reading the pane by hand. `orchestrator.md`
  had carried this rule for weeks (**Monitoring open PRs**: "never sit in a wait loop, never
  `sleep`") and the delegate templates never got it, which is the entire gap. Both now carry a
  **Never block a turn on CI** section stating the deadlock mechanism in one sentence, the
  register / **end the turn** / act-on-the-notice shape, that reading a state once is fine and
  only *waiting* is banned, and that `CONFLICTING` is the case waiting can never discover
  because no check-suite is ever created for it. `worker.md`'s git-workflow CI bullet now
  points at that section instead of restating half of it, and its **Loop until green** no
  longer reads as an in-turn poll loop; `reviewer.md`'s `notify_when` tool bullet gains the
  same pointer, and its version of the rule is the first time a default group's reviewer is
  told which watch kind to register or that a `CONFLICTING` PR never produces checks at all.
  `orchestrator.md` is deliberately untouched — confirmed to carry its own version, and
  duplicating it would have been the change this fixture exists to make visible. Layer 2 of
  #590 (host-side detection of an undeliverable notice) is parked for design and is not here.

- **#596, a worker's claims are about a SHA and a scope** — `worker.md` only (`reviewer.md`,
  `orchestrator.md` and `planner.md` did not move). Two stale-claim families from one batch,
  both a true sentence that quietly stopped being true while the text stayed put. **Green is a
  fact about a SHA**: any push or rebase invalidates every run id and "green on all three
  platforms" already in the PR body, and the body survives untouched, so the claim rots
  silently — #571 cited a run three commits behind head, and #588 cited a pre-rebase run at
  review 1 and then the *same* pre-rebase run again after the rebase at review 2. Three
  instances, two workers, every one caught by a reviewer and none by its author, which is why
  the new **DoD item 4** is a procedure (`gh run list --json headSha,…`, assert against `git
  rev-parse HEAD`, update the body before reporting) rather than an exhortation to be careful.
  It sits deliberately next to red-before-green so it rides the same checklist. **`Closes #N`
  is a fact about scope**: a squash merge honors the keyword out of the squashed commit message
  however partial the change was, so **DoD item 7** now reserves `Closes` for the PR that
  finishes an issue and sends partial scope to `Part of #N` / `Mitigates #N`, naming the squash
  mechanism so the choice doesn't read as style — #569 and #590 were both auto-closed this way
  in one session and reopened by hand. The git-workflow PR bullet stops offering `Closes #N` as
  the only way to link an issue. The other three roles are untouched on purpose: the PR body is
  written by the *worker*, and the reviewer/orchestrator side of head-staleness is already
  carried elsewhere — `orchestrator.md` states that a new head re-stales every recorded verdict,
  and `list_verdicts` pins verdicts by SHA. Duplicating a worker's authoring rule into three
  more templates would have been the change this fixture exists to make visible.

- **#455, a delivery id and what makes a repeat a duplicate** — all four files, one new
  **Duplicate deliveries** section each. Live incident: a worker received the same kickoff
  brief twice and recognised it only by noticing it had asked the same question before. The
  audit rules loomux out as the duplicator — one `prompt` action, `attempts: 1`, one Enter —
  so the duplication happens after the bytes leave loomux (the CLI re-processing one queued
  paste), and nothing stamped a delivery so a receiver could say *"I have already seen this
  one"*. Every kickoff header now carries `Delivery id: <group>/<agent>/k1`, and the rule each
  template states is: **a brief whose delivery id you have already acted on is a duplicate —
  acknowledge it in one line and do nothing else.**

  The half that needed the care is the exception, because #517/#585's kickoff recovery
  **deliberately re-sends a brief that never arrived**, byte-for-byte and therefore with the
  same delivery id. So the rule is keyed on *acted on*, never on *seen*: a re-delivery of a
  kickoff the agent never got to act on is acted on once, normally, and the templates say so
  in as many words. The per-role wording differs only in what "do nothing else" forbids (a
  second PR, a second review, a second plan, a re-dispatch); `orchestrator.md` additionally
  gets one line on how to READ a delegate that reports a duplicate — it did the work once, not
  zero times.

- **#610, a planner may fetch docs again** — `planner.md` only, one sentence in the
  planning protocol's step 2. The old text told a planner its allowlist "pre-approves only
  `git`/`gh` shell commands plus built-in read-only ones", which stopped being true when
  #610 added `WebFetch`/`WebSearch` to a read-only pane's `permissions.allow`. That
  addition is a capability decision, not a bug fix (see `doc/design/orchestration.md`'s
  #610 subsection for the argument and the residual), and the reason it is worth the
  re-bless is that a capability nobody is told about is one nobody uses: this repo's own
  `agent-cli-reference` skill *requires* reading a vendor's official reference before
  designing anything CLI-dependent, and a planner working from an instruction sheet that
  says it cannot fetch will design from recall instead. The `cargo check`-is-denied half is
  unchanged and still stated — executing code stays out of reach.

- **#578, queue notices about the orchestrator's own pane** — `orchestrator.md` only, one
  paragraph in the delivery-queue guidance. loomux can never type a queue notice about the
  orchestrator's OWN pane into that pane (it would queue behind the very block it reports),
  so those notices now ride back as an extra content block on the orchestrator's next tool
  result. The re-bless is warranted because the channel is *new to the reader*: an unexplained
  `[loomux] …` block appearing on the end of an unrelated tool result is exactly the kind of
  thing an agent either ignores or over-reacts to. The paragraph says what it is, routes each
  line back to the `queued`-vs-`DROPPED` rules already stated above it, and names the two
  properties that change how it should be handled — it needs no acknowledgement, and it
  **drains once**, so it will not be repeated on the next call.

- **#622, a planner's build command is un-allowed, not denied** — `planner.md` only. The
  correction to the half #610 left standing. The template said a build/typecheck command is
  *"denied outright, permanently, with no per-repo way to widen it"*, which is false by the
  same merge-across-scopes rule that makes the escape hatch beside it work: loomux emits no
  general `Bash` denial (`CLAUDE_EDIT_DENY_TOOLS` is `Edit`/`Write`/`NotebookEdit`,
  `CLAUDE_READONLY_DENY_GIT` is `git commit`/`git push`), so a repo-level
  `permissions.allow` merges in and grants it. The error was conservative — it understated
  what a user controls rather than overstating containment — which is why it shipped and why
  it is worth fixing now rather than never: a planner told a capability is impossible will
  not look for it, and a user who wanted their planner to typecheck was told not to try. What
  the template says now separates the two directions: what loomux *denies* can never be
  allowed back, what it merely never allowed the repository's own `.claude/settings.json` may
  have granted, and assume it didn't unless you see it there. `docs/orchestration.md` carries
  the same correction for the human-facing side; the other three templates never made the
  claim.

- **#625, `/tmp` is one namespace and a squash reads your prose** — `worker.md`,
  `reviewer.md` and `orchestrator.md` (`planner.md` writes no files and merges nothing).
  Two incidents, both a shared resource that looks private from inside one agent.

  **Scratch files.** A worker wrote its PR body to `/tmp/body.md` and `gh pr edit
  --body-file`'d it; another worker had picked the same path seconds earlier, and PR #621 was
  published carrying #612's body. Restored, nothing lost — but only because a worker re-read
  its own PR, since the path has no lock, no error and no warning: the second writer wins and
  both agents are told it worked. Worktrees isolate the repo and nothing else on the machine,
  so `worker.md` (a new git-workflow bullet) and `reviewer.md` (beside its `--body-file`
  carve-out, the one place it is told to write a file at all) now say scratch goes under the
  agent's own worktree — `./.scratch/`, gitignored — never a bare `/tmp` name. Same
  shared-namespace class as the `git stash` bullet already next to it (#299) and the retired
  `CARGO_TARGET_DIR` (#263).

  **Closing keywords.** #569 was auto-closed by a squash for the *second* time in one
  session. The first (#586) was the ordinary trap `worker.md` already covered — `Closes` on
  partial scope. The second was PR #615, which linked `Part of #569` deliberately and
  explained the choice at length, and whose explanation ended "Please close #569 by hand if
  you agree": GitHub's scan is textual and context-blind, matching `close`/`fix`/`resolve`
  next to `#N` anywhere in the body or in any commit message a squash aggregates, including
  inside the sentence arguing against closing. Choosing the right keyword is therefore not
  enough, and that is the new half. `worker.md`'s DoD item 7 gains the authoring side (grep
  your own body and `git log` for the pattern before posting); `orchestrator.md` gains the
  merge side as its own subsection before the post-merge routine (scrub the aggregated
  message before merging; re-read the partly-addressed issues after, and reopen what closed).
  Split that way on purpose: the body is written by the worker and the squash is performed by
  whoever merges, so neither template could carry both halves without telling an agent to
  police a step it never takes.

- **#579, `queue_orphans` grew a second list** — `orchestrator.md` only, three places: the
  tool's one-line entry in the tool list, one sentence in the delivery-notice section, and a
  new **Durability rules** bullet. A delivery refused at the front door (the target pane was
  already at the per-pane cap of 8) never gets a queue id, so neither id-keyed orphan
  derivation could ever see it — the sender got its synchronous error and nothing the
  orchestrator calls could enumerate the loss. It now surfaces as `refused`, beside `orphans`.
  The re-bless is worth it for the two ways the new list does NOT behave like the old one, both
  of which an orchestrator will get wrong if nobody tells it: it is **not restart-shaped** (a
  full pane refuses arrivals during perfectly ordinary operation, so a non-empty `refused` on a
  session with no restart in it is not a bug), and its rows were **already reported to their
  senders** in-band, so the ones actually needing action are those whose sender has since died
  and those loomux sent to itself. The bullet also states what `text: null` means here versus in
  the orphan list, and that reading the list re-admits nothing. Review NB1 added one more
  sentence to that bullet: check `refused_window_truncated` before reading `refused_count: 0` as
  "nothing was refused", because the audit window the count comes from is itself capped at 5000
  entries — a reader who takes a partial count for a complete one stops looking exactly when
  there is something to find.

- **#582, the task board carries ordering** — `orchestrator.md` and `planner.md`
  (`worker.md`/`reviewer.md` did not move: neither writes the board). A task had no
  relationships at all, so the ordering between items lived only in the orchestrator's
  context window and its `set_state` prose — which is why "what's unblocked right now"
  was re-derived from prose after every compact, `blocked` was a status with no object,
  and `assignee` was a plain field anyone could overwrite. The board now carries `deps`
  (blocking) and `related` (annotation), a derived `ready` on every `list_tasks` row, and
  an atomic `claim`.
  The re-bless is warranted because a field nobody is told to set is dead weight: the
  whole point of persisting structure is that the orchestrator writes it down instead of
  remembering it. `orchestrator.md`'s **task board** section gains four rules — encode a
  plan's ordering as `deps` rather than `set_state` prose; read "what's startable" off
  `ready: true` (`queued` ∧ every dep `done`) instead of re-deriving the queue; assign
  with `claim: true` and treat a refusal as the board saying the task is taken or blocked,
  never as something to route around with a plain `assignee` write; and keep `blocked` for
  blockers *outside* the board, since intra-board ordering is now machine-readable. Plus
  one line that deleting a task strips its id from everyone else's links, so nothing
  dangles. The `list_tasks`/`upsert_task` tool bullet gains the three new row fields.
  `planner.md`'s **Suggested worker split** bullet asks for the serialize/parallel
  structure to be stated explicitly per slice — that sentence is what the orchestrator
  turns into deps, and a split whose ordering is left implicit becomes prose again.
  The board UI half (chips, a ready affordance, dep editing) is a separate slice and
  changes nothing an agent reads.

- **#544, a capability class is never acquired by omission** — `orchestrator.md` only
  (`worker.md`/`reviewer.md`/`planner.md` do not spawn anything and did not move). Live
  incident: three reviewer-shaped briefs (`name: "rev: #536 …"`, tasks saying "review this PR"
  and "record your verdict with `review_verdict`") were spawned with `kind` omitted and came
  back as **workers** — read-write panes with edit tools and `git commit`/`push` — because
  `kind` defaulted to `worker`, the *most*-privileged class. Nothing objected: every
  containment guardrail this repo has (#448/#462/#465) protects a pane that was correctly
  *classified*, and none of them fire when the classification itself was acquired by
  forgetting an argument. `spawn_agent` now **refuses** a fresh spawn that names neither
  `kind` nor `block`, with a message saying what to pass; `resume_session` keeps its #254
  inheritance untouched (omitting both there inherits the resumed session's own block, which
  is stricter than any default). The re-bless is warranted because the template stated the old
  contract in as many words — "*`kind`: `worker` | `reviewer` | `planner`, default `worker`*" —
  so an orchestrator reading it would keep omitting the argument and read the new refusal as a
  loomux bug. The bullet now states the requirement, names the incident in one sentence so the
  rule doesn't read as ceremony, and flags the resume exception; **Planning & scheduling**'s
  worktree bullet gains the `kind: "worker"` its example had been eliding.

- **#581 slice A, the board carries a PR's base branch** — `orchestrator.md` only (no
  other role writes the board). `Task` gained `pr_base`, and a field nobody is told to
  set is dead weight, so the **task board** section gains one rule: record `pr_base` in
  the same `upsert_task` call that records `pr`, using the branch name gh reports
  (`gh pr view <n> --json baseRefName`). What it buys the human is a board that can tell
  a merge into the default branch from a sub-PR into an integration branch, and relabel
  Approve accordingly instead of warning about a default-branch merge gate on a PR that
  isn't headed there. The rule says in the same breath that it is DISPLAY metadata and
  nothing gates on it — the board is agent-writable, so a stale or wrong value misleads
  a human rather than opening a merge, and every merge decision re-resolves the real
  base ref live. The `list_tasks` tool bullet gains the new row field.

- **#381, a first-turn MCP primer** — all four files. A fresh session used to wade through
  hundreds of lines of policy before learning what to DO on turn one (`orchestrator.md`'s own
  checklist sat behind an 11-rule INVARIANTS digest, near the bottom of a 1000+-line document).
  Each template now opens with a short **Your first turn** section, above everything else,
  naming the exact first-turn call sequence for that role's actual tools — `get_state`/
  `list_tasks`/`list_agents`/`gh issue list`/`list_notifications`/`queue_orphans` for the
  orchestrator (six calls, matching **Durability rules**' own session-start list in
  substance — PR #706 review B1 caught a first cut that dropped `queue_orphans` while
  claiming to be that same sequence, and review round 2 caught the fix-up's own gloss
  describing only the restart-shaped `orphans` list when the call also returns `refused`,
  which can be non-empty on an ordinary session with no restart); the delivery-id check,
  `note_directive` and
  `report("progress", ...)` for a worker; `gh pr view` and the verdict-bearing `report(...)`
  for a reviewer; `gh issue view` and the plan-then-report contract for a planner. Each
  template's closing line ("everything below is the detail") was also reworded (review N4)
  to stop reading as licence to skim past mandatory sections it doesn't summarize (a
  worker's **Git workflow**/**Definition of done**, a reviewer's **Never block a turn on
  CI**). INVARIANTS and every other section are otherwise untouched — this only moves what
  a fresh session hits first.

- **#850, a review report is one line** — `reviewer.md` only. Step 5 already said the
  findings stay on the PR and are never re-typed into the report; what it did not say is
  what that makes the report — so a reviewer could satisfy every sentence in it and still
  send a 400-word restatement of its own review. The step now names the shape (verdict,
  reference, pointer, findings count) and the cost that makes it a rule rather than a
  preference: everything typed into the orchestrator's pane becomes context that agent
  re-pays for on every turn afterwards, so a retold review is the most expensive
  redundancy in the group. The same rule reaches a *gated* reviewer through its block
  note and a `mode: replace` one through `mechanics_core(Reviewer)` — both of which also
  carry the recorded-summary target (~100 words) that only exists where `review_verdict`
  does, which is why it is not in this file.

- **#865, `list_tasks` caps `done` rows** — `orchestrator.md` only. `list_tasks()`'s
  wire shape changed from a bare compact-row array to `{ tasks: [...], omitted_done:
  N }`: `done` rows are now capped at the newest 20 by `updated_ms` by default, with
  `omitted_done` naming how many were left off (0 when none were) and `include_all:
  true` returning the whole board. The `list_tasks()` tool bullet states the new
  envelope, the cap, and `include_all` — otherwise the post-compact re-sync sends an
  orchestrator back to a description of an array this call no longer returns, right
  when a long-lived board first starts eliding rows.

- **#866, `group_usage` MCP tool gains a summary default** — `orchestrator.md` only. The
  `group_usage()` bullet becomes `group_usage(detail?)` and now describes the summary
  the tool actually returns by default (`agent_count`, `top_agents`, and a `rest` rollup
  with a live/historical split) plus the `detail: true` escape hatch to the full
  per-agent table, instead of the old "total + per-agent" description that no longer
  matches a default call.

- **#778, the consent boundary points both ways** — `orchestrator.md` only (`worker.md`/
  `reviewer.md`/`planner.md` neither start work nor read the funnel, and did not move). Full
  autonomy inverts the start default for a group: every open issue becomes eligible except the
  ones the human held. Nothing host-side blocks a start — the funnel has always been
  contract-enforced, and the *enforced* boundary stays ship-side in the `gh` shim — so under this
  mode the template text **is** the consent boundary, which is why the re-bless is the review
  surface for the whole feature. **INVARIANT 8** is rewritten to state both directions: opt-in is
  still the default (including plain autonomous mode), and full autonomy applies only when the
  kickoff config or a `[loomux] FULL AUTONOMY ENABLED` notice says so, with `agent-hold`, a struck
  triage row, and an untriaged pre-existing issue as the three exceptions to eligibility — closing
  on the sentence the rest of the feature hangs off: it widens what may be **started**, never what
  may be **shipped**. A new **Full autonomy** subsection under **Autonomous mode (idle-tick)**
  carries the operational half: post one ranked triage plan over all open issues and wait for an
  explicit go before touching the pre-existing backlog; read the `issue #N eligible under
  full-autonomy` wake lines (and treat a `PARTIAL` summary as a backlog the poller did not fully
  see, so a plan built on it says so); select in a stated priority order (board order, milestone/
  priority label, `agent-ready`, then a value judgment against the goal); announce a one-line
  rationale per pickup in the pane and as the board task's first note; park an out-of-goal issue
  at the bottom of the board rather than starting or holding it; stop when the queue empties; and
  start nothing new once a disable, an autonomous-off or the budget money-stop ends the mode. A
  fourth **Label signals** row states `agent-hold` itself: absolute, never removed by an agent,
  addable only to an issue the agent filed.

- **#795, `PARTIAL` names which fetch was short** — `orchestrator.md` only, one bullet. The
  intake poll bounds two listings, not one, so a `PARTIAL` caveat can now come from either the
  open-issue fetch or the open-PR fetch. The clause added by #778 named only the issue case, and
  read as an assertion about the backlog whichever fetch was actually truncated. It now splits:
  the issue half is unchanged, and a short **open-PR** fetch is stated as what it is — the check
  sweep saw only the newest open PRs, so a PR outside that window finishing CI produces no wake,
  and the orchestrator must check it rather than read the silence as "still running".

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
  template over the file here **with its workflow-era placeholder key removed**, in its
  own commit, and say in the message what changed for the agents. The diff on this
  directory is then the review surface for "what did we just tell every worker to do
  differently?".

  **A plain `cp` of the live template is wrong**, and it fails in a way that hides its
  own cause. These files are the live template *minus* the key(s) `LIVE` lists for it
  in `tests/workflow.rs` — read that array, not this sentence, which is a copy of it and
  has been stale before: `{{WORKFLOW}}`, `{{POST_MERGE_WORKFLOW_HOOK}}` and `{{MERGE_QUEUE}}`
  for `orchestrator.md`, `{{BLOCK_NOTE}}{{ADVISOR_CONSULT_NOTE}}` for `worker.md`,
  `{{BLOCK_NOTE}}` for `reviewer.md` and `planner.md` — because
  `a_workflow_placeholder_must_sit_at_the_end_of_a_line_it_shares` asserts exactly
  "live, stripped of its keys, equals the golden". The *legacy* vars (`{{GROUP_ID}}`,
  `{{REPO}}`) are not keys and must stay. Leave a key in and that test fails with a
  full-file `left`/`right` dump rather than a one-line cause — the "byte copies"
  phrasing at the top of this file is older than those keys, and reading it literally
  is what produced the red that added this paragraph (#590).
- If you did **not** mean to change what a default group reads — you were adding
  workflow-conditional prose — then the prose is in the wrong place. It belongs in
  `templates/workflow.md` or `templates/block.md`, behind `{{WORKFLOW}}` /
  `{{BLOCK_NOTE}}`, which resolve to the empty string for the built-in roster.

Line endings are normalized before comparison (there is no `.gitattributes`, so these
are CRLF on Windows and LF elsewhere) — the assertion is about the words, not about
the checkout.

## Verifying a re-bless by hand — and the CRLF trap

Reviewing a re-bless means checking two things, and #594's review named them: the **patch**
on each golden is identical to the patch on its live template (nothing extra rode along),
**and** the whole **file** equals its live template with that file's keys stripped (nothing
pre-existing silently drifted). Level 1 alone passes a golden that was correct in the diff
and stale everywhere else; level 2 alone passes one where an unrelated edit was smuggled in
alongside a legitimate one.

**Level 2 is where the reviews keep going wrong, and it is not a disagreement about the
content.** Two consecutive reviews (#594, #603) reported a level-2 mismatch that was not
real: the golden's committed bytes are LF, the live template in a Windows working tree is
CRLF, and a `sed`/`diff` comparison of the two therefore differs on **every line** while the
words are identical (#498). What follows is a mismatch report that names no specific line,
which reads exactly like "the whole file drifted".

So compare **bytes with the keys removed, in binary, without a line-based tool** — the same
substitution `render_with_legacy_vars` does, not a text filter:

```sh
python -c "
keys={'orchestrator':['{{WORKFLOW}}','{{POST_MERGE_WORKFLOW_HOOK}}','{{MERGE_QUEUE}}'],
      'worker':['{{BLOCK_NOTE}}','{{ADVISOR_CONSULT_NOTE}}'],
      'reviewer':['{{BLOCK_NOTE}}'],'planner':['{{BLOCK_NOTE}}']}
for f,ks in keys.items():
    live=open('src-tauri/src/orchestration/templates/%s.md'%f,'rb').read()
    for k in ks: live=live.replace(k.encode(),b'')
    gold=open('src-tauri/tests/fixtures/pre222/%s.md'%f,'rb').read()
    print(f, 'OK' if live==gold else 'MISMATCH')
"
```

Both files come out of the same working tree, so both carry that tree's line endings and the
comparison is honest either way. If it still says MISMATCH, that is a real one.

//! Substance pins for the **role prompts** — the text every loomux group's agents actually read
//! (`templates/{orchestrator,worker,reviewer,planner}.md`, rendered into the group's state dir).
//!
//! Prose is production code here: every sentence is executed literally by an agent, and a rule
//! that quietly disappears in a future edit fails silently and invisibly — the agent simply stops
//! doing it, and nobody finds out until a PR merges with its findings dropped or a red `main`
//! sits there all afternoon. These tests are the seatbelt: each load-bearing rule is pinned to
//! the text that carries it, so deleting it is a failing test that **names the rule it deleted**.
//!
//! Three things make a prose pin actually able to fail, and all three are the scar tissue of a
//! review that mutated the templates rather than reading the tests (#236):
//!
//! 1. **Whitespace-collapsed matching** ([`flat`]). These are hard-wrapped markdown files: a pin
//!    that fires when a paragraph is re-wrapped reports "you changed the rule" when no rule moved,
//!    and *that* red is what teaches people to bless a diff without reading it.
//! 2. **Region scoping** ([`section`]). Most load-bearing rules appear twice by design — once in
//!    the INVARIANTS digest (the rule) and once in the body (its procedure). A document-wide match
//!    is satisfied by either, so deleting the body's procedure leaves the pin green, rescued by
//!    the digest: the rule survives as a slogan with no instructions attached.
//! 3. **Uniqueness** ([`pinned`]). An anchor that occurs more than once inside the region it is
//!    asserted in cannot fail when the rule it names is deleted — some other occurrence rescues
//!    it. So that is a failing test *here*, rather than a defect found later by mutating prose.
//!
//! Every anchor below was mutation-verified: delete the markdown unit (list item or paragraph)
//! that carries the rule, and the owning test goes red.

use loomux_lib::orchestration::{Guardrails, OrchRegistry};
use std::fs;

/// Guardrails for the group this suite is about: the **default** one — no workflow file, the
/// advanced orchestrator off. That is deliberate and it is the whole scope of this file.
///
/// #222 replaced the flat per-role model fields these rails used to set with a `blocks` roster,
/// and it added the toggle: with `advanced_orchestrator: false`, `{{WORKFLOW}}`/`{{BLOCK_NOTE}}`
/// render empty and the agent reads the templates as every group that never opted in reads them.
/// So this suite pins what the *default* is told, and `workflow.rs` pins what a *gated* group and
/// a `mode: replace` persona are told (`mechanics_core`). A rule in only one of them is a rule
/// one kind of group is not being told — see `doc/design/orchestration.md`.
fn rails() -> Guardrails {
    Guardrails {
        max_agents: 2,
        agent_cli: "claude".into(),
        auto_ops: false,
        advanced_orchestrator: false,
        idle_kill_minutes: 0,
        max_spawns_per_hour: 0,
        watchdog_stall_minutes: 0,
        ..Guardrails::default()
    }
}

fn test_registry() -> (OrchRegistry, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let reg = OrchRegistry::new(dir.path().to_path_buf());
    reg.set_port(45999);
    (reg, dir)
}

/// The instruction file an agent of this role actually reads, as loomux renders it.
///
/// Pinning the *rendered* file rather than the template source is deliberate: it is the artifact
/// the agent opens, and it proves the rule survived rendering as well as editing.
fn instructions(file: &str) -> String {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let text = fs::read_to_string(reg.state_root().join(&g.id).join(file))
        .unwrap_or_else(|e| panic!("{file} must be written to the group dir: {e}"));
    assert!(!text.contains("{{"), "{file} has an unsubstituted template variable:\n{text}");
    text
}

/// Lowercased, with every run of whitespace collapsed to one space. See the module docs (1).
fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The slice of a `flat`ted document between two markers — the SECTION a rule must live in.
/// See the module docs (2).
fn section<'a>(flat_doc: &'a str, start: &str, end: &str) -> &'a str {
    let from = flat_doc
        .find(start)
        .unwrap_or_else(|| panic!("the document has lost its `{start}` section entirely:\n{flat_doc}"));
    let rest = &flat_doc[from..];
    let to = rest[start.len()..].find(end).map(|i| i + start.len()).unwrap_or(rest.len());
    &rest[..to]
}

/// Assert that `region` carries the rule `why`, and that `anchor` names it **uniquely**.
///
/// Presence is the obvious half; uniqueness is the half that makes the pin able to fail at all.
/// See the module docs (3). A pin you cannot make fail is worse than no pin: it is a claim of
/// coverage.
fn pinned(region_label: &str, region: &str, anchor: &str, why: &str) {
    let n = region.matches(anchor).count();
    assert!(
        n > 0,
        "{region_label} has lost the rule it owes: {why}\n\nanchor `{anchor}` is gone. If you are \
         changing this deliberately, change the pin in the same commit and say so in the PR.\n\n\
         Region as rendered:\n{region}"
    );
    assert_eq!(
        n, 1,
        "the anchor `{anchor}` occurs {n}× in {region_label}, so it CANNOT FAIL when the rule it \
         names is deleted — another occurrence rescues it, and the pin silently stops pinning \
         ({why}). Anchor the rule's own clause instead of a phrase it shares with its \
         neighbours.\n\nRegion as rendered:\n{region}"
    );
}

// ---------------------------------------------------------------------------------------------
// The INVARIANTS digest — what has to survive a compaction
// ---------------------------------------------------------------------------------------------

#[test]
fn the_invariants_digest_leads_the_document_and_carries_what_compaction_would_cost() {
    // The orchestrator prompt anticipates its own compaction ("your context may have compacted";
    // "compact at lulls") — and a summary keeps a document's SHAPE and loses its RULES. So the
    // rules whose loss is dangerous are stated once, at the top, where an orchestrator re-reading
    // its instruction file after a compaction hits them first. The digest is only worth anything
    // if it (a) precedes the bulk of the document and (b) names the rules that would actually
    // hurt: a merge without a gate, a merge past an open question, a dropped finding, an
    // unevidenced test, a red default branch, an unlabelled issue started.
    let orch = instructions("orchestrator.md");
    let o = flat(&orch);

    let digest = o.find("## invariants").expect("orchestrator.md must open with an INVARIANTS digest");
    let tools = o.find("## your loomux mcp tools").expect("the tools section still exists");
    assert!(
        digest < tools,
        "the digest must lead the document — a rule stated 400 lines in is a rule a summary has \
         already dropped: {orch}"
    );

    let head = &o[digest..tools];
    pinned("the INVARIANTS digest", head, "re-read this block at every session start",
        "the digest must say what it is FOR — surviving compaction — and tell the orchestrator to \
         re-read it after one, because the whole premise is that its memory of these rules is the \
         thing a summary throws away");

    for (rule, why) in [
        ("never merge to the default branch unless a gate opened for you",
         "the merge gate — the one rule an agent must never forget it is under"),
        ("holds that pr's merge, in every mode",
         "a question you asked the human holds the merge in EVERY mode — auto-merge, one-time \
          grant, supervised dangerous mode"),
        ("telling is not asking",
         "…and its first distinction: without it the policy deadlocks on its own required deferral \
          notice, because an orchestrator that ANNOUNCED something believes it is awaiting an answer"),
        ("your call",
         "…and its second: 'answered' means DECIDED, including the human handing the decision back"),
        ("the pr stays open",
         "…and its third: a question never answered leaves the PR open, which is a correct outcome \
          and never a reason to merge anyway"),
        ("an approval is not a disposition",
         "an approval with findings left open is not 'done'"),
        ("a reason, a filed issue",
         "…and the three costs of deferring one — a reason, a filed issue AND a line to the human. \
          Drop them and 'deferred' silently becomes free, which is the failure this policy exists \
          to stop"),
        ("you own the architecture, not only the acceptance criteria",
         "the engineering bar beyond the acceptance criteria"),
        ("no test is believed until it has been seen to fail",
         "red-before-green: an unevidenced test is a decoration"),
        ("red main stops everything",
         "a merge it performed owns the default branch's next CI run"),
        ("when the default branch moves, every open branch is stale",
         "a moved base makes every open branch STALE, which is not the same as conflicted"),
        ("the label funnel is the consent boundary",
         "file freely; never groom or start an unlabelled issue"),
        ("look, don't build",
         "…and the label says WHICH work: `agent-investigate` is not a licence to write code"),
        ("every loop is bounded",
         "every loop terminates — CI attempts, review rounds, rebases, architectural bounces"),
        ("full uuid",
         "a session id resumes only in FULL — a truncated one does not resolve"),
        ("your context is not the memory",
         "externalize every decision — the board and GitHub outlive the session"),
    ] {
        pinned("the INVARIANTS digest", head, rule, why);
    }

    // The body must not RE-ARGUE what the digest owns: the digest states each rule, exactly one
    // body section carries its procedure, and the rest cross-reference by number. INVARIANT 3's
    // own sentence is the canary — 0 means the disposition procedure was dropped (the digest's
    // one line cannot carry the policy on its own), 2+ means an edit put the repetition back.
    let body = &o[tools..];
    let canary = "a finding that contradicts the change's";
    assert_eq!(
        body.matches(canary).count(),
        1,
        "INVARIANT 3's rule must appear EXACTLY once in the body: {body}"
    );
}

// ---------------------------------------------------------------------------------------------
// The findings-disposition policy — the most load-bearing prose in the suite
// ---------------------------------------------------------------------------------------------

#[test]
fn the_orchestrators_findings_policy_survives_in_substance() {
    // The policy this pins exists because of a live incident: a worker shipped a zero-guard both
    // reviewers approved and both filed the same non-blocking finding on (`divide(5, '0')` still
    // returned Infinity, which is exactly what the change's own rationale said it prevented). The
    // orchestrator raised it to the human as an open question — and merged the moment the second
    // approval landed, before the answer came. Everything was procedurally green; the feature
    // shipped weaker than the issue asked for and two reviews' worth of feedback went in the bin.
    //
    // Nothing there was a bug in a gate. The failure was POLICY, so the fix is prose — and prose
    // with no pin under it is prose that the next compression deletes. One assert per rule, each
    // inside the section that owes it, so a deletion names what it deleted.
    let orch = instructions("orchestrator.md");
    let o = flat(&orch);
    let disposition = section(&o, "3. **disposition every finding**", "### the merge gate");
    let gate = section(&o, "### the merge gate", "### after a merge you performed");

    for (region, name, rule, why) in [
        (disposition, "the disposition step", "default: fix it in this pr",
         "the DEFAULT disposition — route the finding back to the worker and re-review; a \
          non-blocking finding is minutes of work, and it is the signal that compounds"),
        (disposition, "the disposition step", "a finding that contradicts the change's",
         "the blocking-REGARDLESS call: a finding contradicting the change's own stated rationale \
          means the change does not do what it claims"),
        (disposition, "the disposition step", "whatever severity the reviewer gave it",
         "…and that the call is the ORCHESTRATOR's — the reviewer rates the diff, it owns the \
          requirement"),
        // The bind is on the VERDICT, not on the `gh` flag (rev-23 F1). GitHub refuses BOTH
        // `--request-changes` and `--approve` on a PR opened by your own account — which is the
        // normal case here, since every agent in a group authenticates as one GitHub user and that
        // user authors the PRs. Every review this repository has ever received is `COMMENTED`. So a
        // rule anchored on the flag binds nothing, while the channel the orchestrator actually
        // gates on — "the reviewer approved", learned from its `report(...)` — stays unconstrained:
        // a reviewer could label a finding blocking, report `approved`, and satisfy every sentence.
        // That is #235's original incident, resurrected by the rule written to prevent it.
        (disposition, "the disposition step", "\"changes requested\" verdict, not an approval",
         "…and that the label BINDS to the VERDICT: an approval carrying a reviewer-labelled \
          blocking finding is a contradiction to send back, not to merge on — and the verdict, not \
          GitHub's review state, is what the orchestrator reads"),
        (disposition, "the disposition step", "why the fix doesn't belong in",
         "deferral cost 1 — a REASON naming why the fix doesn't belong in THIS PR ('scope' is a \
          category word; 'it'd only take ten minutes' is a reason to FIX it)"),
        (disposition, "the disposition step", "carrying the finding verbatim",
         "deferral cost 2 — a filed FOLLOW-UP ISSUE carrying the finding, not a paraphrase"),
        (disposition, "the disposition step", "one line to the human",
         "deferral cost 3 — the LINE TO THE HUMAN, which is the only thing that gives a deferred \
          finding a future"),
        (disposition, "the disposition step", "filing it is not doing it",
         "…and that the filed issue PARKS the finding in the label funnel rather than discharging it"),
        (disposition, "the disposition step", "round of findings on the same pr",
         "the loop's BOUND — three rounds and the PR settles, or a reviewer with one new nit per \
          round runs it forever"),
        // rev-23 F1's other half: the merge gate opens on "the reviewer approved", and the
        // orchestrator learns that from the reviewer's report + review body — NOT from GitHub's
        // review state, which stays COMMENTED whenever the reviewer and the author are the same
        // account. Say where the verdict lives, or the gate reads a channel nobody constrained.
        (gate, "the merge gate", "not github's review state",
         "the gate must say WHERE the verdict lives: the reviewer's `report(...)` and the top of \
          its review body. GitHub's review state stays COMMENTED on a same-account PR, so an \
          orchestrator that looked there would find no approval to gate on — and one that treats \
          COMMENTED as approval has no gate at all"),
        (gate, "the merge gate", "open-question hold",
         "the HOLD: a question you asked the human holds that PR's merge in every mode"),
        (gate, "the merge gate", "telling is not asking",
         "…without which the policy deadlocks on its OWN required deferral notice: a deferral you \
          announced is not a question you await"),
        (gate, "the merge gate", "your call",
         "…'answered' means DECIDED, including the human handing the decision back"),
        (gate, "the merge gate", "the pr stays open",
         "…and a question never answered leaves the PR open: a correct outcome, and never a reason \
          to merge anyway"),
    ] {
        pinned(name, region, rule, why);
    }
}

// ---------------------------------------------------------------------------------------------
// Engineering standards — grounds to send work back beyond the acceptance criteria
// ---------------------------------------------------------------------------------------------

#[test]
fn the_orchestrator_can_send_work_back_on_design_grounds_not_only_acceptance_criteria() {
    // The completion check used to ask exactly one question — "does the PR satisfy the acceptance
    // criteria?" — and a codebase can answer yes to that on fifty consecutive PRs and still rot:
    // coupling, a second copy of a mechanism it already had, a dependency nobody argued for, a
    // contract changed with no design note. The prompt gave the orchestrator the MANDATE ("the
    // codebase's advocate") and no grounds to exercise it on. The grounds are stated ONCE and
    // referenced from the two places the decision is actually made: plan intake (cheap — no code
    // exists yet) and the completion check (still cheaper than a revert).
    let orch = instructions("orchestrator.md");
    let planner = instructions("planner.md");
    let o = flat(&orch);

    assert!(o.contains("## engineering standards"), "the grounds need one authoritative site: {orch}");
    let standards = section(&o, "## engineering standards", "## delegation protocol");
    for (ground, why) in [
        ("cross-module coupling", "cross-module coupling / a dependency pointing the wrong way"),
        ("duplicating an existing mechanism", "a second mechanism where the repo already had one"),
        ("an unjustified new dependency",
         "a dependency nobody argued for — permanent, and the whole repo carries it"),
        ("public-contract change with no design note", "a contract change that ships undocumented"),
    ] {
        pinned("Engineering standards", standards, ground, why);
    }
    pinned("orchestrator.md", &o, "intake the plan before you delegate",
        "the standards must gate the PLAN — before any code exists is the cheap moment");
    pinned("orchestrator.md", &o, "does it clear the bar in engineering standards?",
        "…and the completion check, where the PR is still cheaper to bounce than to revert");
    // Bounded like every other loop: six grounds, several of them judgment calls, sitting at a
    // step the reviewer has already passed. Without a bound, "fix the coupling → now the scope
    // drifted → now the design note is missing" is a loop only the orchestrator can see.
    pinned("Engineering standards", standards, "architectural bounce per pr or plan",
        "the bounce must be bounded: ONE bounce, naming every ground it has");
    pinned("Engineering standards", standards, "no longer a bounce",
        "…and a second disagreement is a question for the human, which holds the merge like any \
         other question");

    // The planner owes the matching content — a plan that never named its boundaries cannot be
    // gated on them.
    let p = flat(&planner);
    let design = section(&p, "- **design: boundaries, dependencies, alternatives**", "- **test strategy**");
    for (duty, why) in [
        ("which module owns the new code", "which module owns the code and which seams it crosses"),
        ("reuse before invention",
         "the mechanism the repo already has — the alternative that should most often win"),
        ("name every new one and argue it", "every new dependency, argued"),
        ("public-contract changes", "a contract change, with its design note planned as part of the work"),
        ("alternatives considered",
         "the options that lost, and why — a plan with one option in it is a plan that didn't look"),
    ] {
        pinned("planner.md's design section", design, duty, why);
    }
}

// ---------------------------------------------------------------------------------------------
// Red before green — and the exemption that keeps it from eating its own tail
// ---------------------------------------------------------------------------------------------

#[test]
fn red_before_green_is_demanded_evidenced_verified_and_bounded_by_its_exemption() {
    // "Tests that would fail if the feature were broken" was in the worker's DoD from the start —
    // as an ASSERTION NOBODY EVER CHECKED. That is the most common quality failure in autonomous
    // coding and it is invisible from the diff: a suite that is green whether or not the feature
    // exists. Closing it needs three surfaces to move together — the worker PRODUCES the evidence,
    // the orchestrator REFUSES `done` without it, the reviewer VERIFIES it rather than reading it
    // (a quoted failure line is text, and text is not a red test).
    let worker = instructions("worker.md");
    let orch = instructions("orchestrator.md");
    let reviewer = instructions("reviewer.md");
    let (w, o, r) = (flat(&worker), flat(&orch), flat(&reviewer));

    let dod = section(&w, "## definition of done", "## review findings");
    pinned("worker.md's DoD", dod, "against the code *without* your change",
        "the worker must run the new tests against the code WITHOUT the change — that is the whole \
         of red-before-green ('base branch' alone is a phrase it shares with the git workflow)");
    pinned("worker.md's DoD", dod, "the failure line it printed",
        "…and produce the evidence itself (command + failure line), not a claim that the tests are good");

    // The exemption, and why the rule needs one: stated unconditionally, red-before-green bounces
    // every PR that legitimately adds no test — including the two this very suite prescribes (the
    // learning loop's DOCS PR and a red main's REVERT), asking for evidence that cannot exist, on
    // red main, in the unattended mode the rule was written for. The four classes are enumerated
    // once, in worker.md, and the price is one line: which class, why, suite green.
    pinned("worker.md's DoD", dod, "no new testable behavior",
        "the exempt CLASS — a change whose intent carries no new testable behavior");
    for class in ["docs- or comment-only", "a revert", "a pure rename/move", "a re-blessed golden"] {
        pinned("worker.md's DoD", dod, class,
            "the four exempt classes are enumerated on purpose — a boundary an agent has to guess \
             at is one it will guess wrong, and 'my change is basically a refactor' is how an \
             untested feature ships");
    }
    pinned("worker.md's DoD", dod, "naming which of",
        "…and the exemption COSTS one line NAMING WHICH class it is: that line is its entire \
         safety, because it turns 'there was nothing to test' into a reviewable claim instead of \
         an assertion nobody can check");

    pinned("the worker brief", &o, "**red-before-green evidence**",
        "the brief must ask for the evidence up front — a bar the worker first hears about at the \
         completion check is a round-trip nobody needed");
    let check = section(&o, "4. do your own **high-level** completion check", "5. confirm the pr's ci");
    pinned("the completion check", check, "is **not done**",
        "the orchestrator must REFUSE a `done` whose PR shows no test failing on the base branch — \
         a duty nobody enforces is a duty nobody performs");
    pinned("the completion check", check, "the exemption, and its price",
        "…and must know the exemption exists, or it bounces a docs PR forever");

    pinned("reviewer.md", &r, "check the red-before-green",
        "the reviewer checks the evidence in the test-quality lane");
    pinned("reviewer.md", &r, "missing evidence is a finding",
        "…absent evidence is itself a finding, or the worker's duty has no consequence");
    pinned("reviewer.md", &r, "neutralize the change",
        "…and PRESENT evidence is a claim to reproduce, not proof: the reviewer breaks the behavior \
         and watches the test go red itself");
    pinned("reviewer.md", &r, "no new testable behavior",
        "…and checks the exemption's CLAIM rather than its label — a 'pure rename' that changes a \
         default is a behavior change wearing an exemption");
}

// ---------------------------------------------------------------------------------------------
// Post-merge: the default branch is yours until it is green, and the fleet gets re-synced
// ---------------------------------------------------------------------------------------------

#[test]
fn a_merge_the_orchestrator_performed_owns_the_default_branchs_next_ci_run() {
    // Auto-merge, a one-time grant and supervised dangerous mode all let the orchestrator LAND
    // code — and then the prompt went quiet. A PR green on its own branch can still break the
    // default branch (a semantic conflict with whatever landed under it; a job that only runs
    // post-merge), and a red default branch blocks every worker in the group. Nothing told it to
    // look, so nothing would have looked.
    let orch = instructions("orchestrator.md");
    let o = flat(&orch);
    let aftermath = section(&o, "### after a merge you performed", "### re-sync the fleet");
    let at = "the red-main procedure";

    pinned(at, aftermath, "post-merge run",
        "a merge the orchestrator performed must be followed to the default branch's CI");
    pinned(at, aftermath, "stop merging",
        "red main halts the merge queue — the next merge lands on a broken branch");
    // The freeze has to carve out its own remedy, or it forbids the one merge that makes main
    // green: main can only BECOME green through that merge, so a literal orchestrator halts, hands
    // the revert to the human, and waits — under auto-merge, where nobody is at the keyboard.
    pinned(at, aftermath, "no further **feature** merges",
        "the freeze is on FEATURE merges, or it forbids the merge that unbreaks the branch");
    pinned(at, aftermath, "the merge that *makes* main green",
        "…and must say WHY the fix/revert PR is the exception: it is the exit from the red state");
    pinned(at, aftermath, "fix forward once",
        "fixing forward is bounded to ONE attempt — the CI gate's 3-attempt bound does not apply \
         here, because the damage is already merged");
    pinned(at, aftermath, "git revert -m 1",
        "the remedy is a REVERT PR, concretely — without the command the rule degrades into 'keep \
         trying to fix it', which is the unbounded loop this exists to stop");
    pinned(at, aftermath, "restoring main costs a revert",
        "…and the revert is the DEFAULT, not the fallback: restoring main costs a revert, debugging \
         it in place costs everybody's afternoon");
}

#[test]
fn every_open_branch_is_re_synced_after_the_default_branch_moves() {
    // The sweep watched CI and comments — both of which stay green while a PR silently goes
    // CONFLICTING because something else merged underneath it. But conflict is the wrong trigger:
    // STALE is not the same as CONFLICTED. A branch that still merges cleanly was reviewed, tested
    // and CI'd against code that no longer exists, so its green checks describe the past, and
    // waiting for `CONFLICTING` to appear is waiting for the cheapest moment to rebase to pass.
    let orch = instructions("orchestrator.md");
    let o = flat(&orch);

    let sweep = section(&o, "## monitoring open prs", "## the learning loop");
    pinned("the open-PR sweep", sweep, "--json mergeable",
        "the sweep must ask whether the PR still merges — green checks say nothing about it");
    pinned("the open-PR sweep", sweep, "conflicting", "…and know the state it is looking for");

    let resync = section(&o, "### re-sync the fleet", "## the ci gate");
    let at = "the re-sync rule";
    pinned(at, resync, "stale is not the same as conflicted",
        "the whole rule lives in that distinction — a conflict-only trigger waits for the most \
         expensive moment to rebase");
    pinned(at, resync, "branch it will merge into",
        "a sub-PR rebases onto ITS base (an integration branch), not reflexively onto main — \
         backwards, and a merged feature's commits get dragged through someone else's PR");
    pinned(at, resync, "owning worker",
        "a real conflict belongs to the worker that wrote the code (resumed), not to the orchestrator");
    pinned(at, resync, "one attempt",
        "…bounded exactly like the CI gate's fix loop — a rebase loop is an expensive way to not ship");
    pinned(at, resync, "invalidates the review",
        "…and the rebase IS a push: the review you were holding is now a review of code that no \
         longer exists, so it has to be re-requested — which is the price of freshness, and the \
         reason to pay it early");
    // The license to scope it is part of the rule, not a footnote: applied literally after every
    // merge on a stack, this costs O(n²) REVIEWS, not just rebases.
    pinned(at, resync, "re-sync the merge frontier, not the whole tree",
        "the re-sync must scope itself to the branch that actually MOVED — a PR two levels deep is \
         not stale until its own base moves, and re-syncing it early pays twice");
    pinned(at, resync, "costs o(n²) reviews",
        "…and must name the cost it is avoiding: re-syncing an n-deep stack per merge is quadratic \
         in REVIEWS, because every rebase invalidates the review on the PR it touches");
    pinned(at, resync, "a fan is not a stack",
        "…and the FAN case, which is the common shape: with many siblings on one base every sibling \
         is on the frontier, so 'rebase the frontier immediately' after each merge is the O(n²) the \
         license exists to avoid — rebase the one you are about to merge, and batch the rest");
    pinned(at, resync, "held on an unanswered question alone",
        "…and a PR held on an unanswered question is not going anywhere: rebasing it invalidates a \
         review nobody can act on");
}

// ---------------------------------------------------------------------------------------------
// Notifications (#243): the PR sweep is now the fallback for a lost notice, not the primary path
// ---------------------------------------------------------------------------------------------

#[test]
fn a_lost_notification_degrades_to_the_old_poll_on_sweep_fallback_not_a_silent_hang() {
    // #243 pulled `gh pr checks` out of the orchestrator's own PR sweep and replaced it with a
    // background notification the orchestrator registers and then ignores until it fires. That
    // delivery is best-effort (#112 — a fired notice can land unsubmitted and still be recorded as
    // delivered), so what stands between a LOST notice and an orchestrator that silently never
    // hears its CI finished is exactly one paragraph: the sweep survives, explicitly, as the
    // fallback. By this suite's own philosophy (a rule that quietly disappears in a future edit
    // fails silently and invisibly), a safety net that is the ONLY thing between "best-effort" and
    // "silent hang" cannot be left to survive on vibes through the next prose edit.
    let orch = instructions("orchestrator.md");
    let o = flat(&orch);
    let sweep = section(&o, "## monitoring open prs", "## the learning loop");

    pinned("the open-PR sweep", sweep, "not permission to stop tracking the pr",
        "a registered notification must NOT read as license to stop tracking the PR on the board/ \
         sweep — the notification is a convenience layered on top of ownership, not a replacement \
         for it");
    pinned("the open-PR sweep", sweep, "degrades to today's poll-on-sweep behavior",
        "…and the sweep must be named EXPLICITLY as the fallback: since delivery is best-effort \
         (#112), deleting this sentence turns a lost notice into a silent hang with nothing left to \
         catch it");
}

// ---------------------------------------------------------------------------------------------
// Autonomy without consent creep: file freely, never start; and distil what recurs
// ---------------------------------------------------------------------------------------------

#[test]
fn the_orchestrator_may_file_an_issue_it_may_never_start_and_it_distils_what_recurs() {
    // Two halves of one boundary: what the orchestrator may do UNPROMPTED. Filing is free — an
    // observation that never became an issue is one nobody will ever act on. Starting is the
    // human's consent, and the label funnel is where it is given. A learning loop that files a
    // convention issue is inside that boundary; one that grooms and starts it is not.
    let orch = instructions("orchestrator.md");
    let o = flat(&orch);

    let funnel = section(&o, "## label signals", "## planning & scheduling");
    let at = "the label funnel";
    pinned(at, funnel, "you may file; you may not start",
        "the permission and its boundary, stated in one breath — the whole point is that they are \
         inseparable");
    pinned(at, funnel, "gh issue create", "…concretely enough to act on");
    pinned(at, funnel, "filing it is not doing it",
        "a filed issue is PARKED in the funnel, exactly like a deferred finding — say so, or 'I \
         filed it' becomes a way to close a problem without solving it");
    pinned(at, funnel, "groom an issue the human hasn't",
        "the funnel forbids GROOMING an unlabelled issue — rewriting someone else's issue with \
         acceptance criteria is the step right before starting it, and 'you may not start it' does \
         not cover it");

    let loop_ = section(&o, "## the learning loop", "## durability rules");
    let at = "the learning loop";
    pinned(at, loop_, "not an incident",
        "it triggers on a recurring PATTERN (a finding class, a repeated CI burn, a convention \
         re-flagged), never on a single incident — the whole guard against make-work");
    pinned(at, loop_, "do not dispatch a worker on it because it is \"only docs\"",
        "…and it must NOT dispatch its own artefact: an unlabelled issue the orchestrator noticed \
         itself is not more startable than a finding a reviewer raised, which has to park in the \
         funnel too");
    pinned(at, loop_, "suggested label",
        "…it files the lesson with a suggested label and stops; the human's label starts it");
}

// ---------------------------------------------------------------------------------------------
// The reviewer: lanes, and findings that say what they are
// ---------------------------------------------------------------------------------------------

#[test]
fn the_reviewer_has_the_lanes_and_classifies_every_finding() {
    // The reviewer's priorities were correctness, tests, requirement fit, docs and style — and
    // nothing on trust boundaries, dependencies or algorithmic cost, in a repo where a bad
    // dependency BRICKS THE BINARY (the getrandom/ProcessPrng rule) and a trust boundary holds
    // only because the webview is trusted (`group_id` as a path segment). A lane nobody was told
    // to cover is a lane nobody reviews, and the absence is invisible: the review comes back clean.
    let reviewer = instructions("reviewer.md");
    let r = flat(&reviewer);

    for (lane, why) in [
        ("trust boundar",
         "the SECURITY lane — which inputs are attacker- or agent-controllable, and where they land"),
        ("new dependency",
         "the DEPENDENCY lane — a dep is permanent, the whole repo carries it, and it can violate a \
          repo's platform rules fatally"),
        ("algorithmic cost",
         "the COST lane — what the change costs at the sizes it will really see"),
    ] {
        pinned("reviewer.md", &r, lane, why);
    }

    // And every finding is classified, because the orchestrator has to disposition each one before
    // the PR merges and cannot do that from unlabelled prose. The label then BINDS — but it binds
    // to the VERDICT, not to a `gh` flag (rev-23 F1): GitHub refuses `--request-changes` AND
    // `--approve` on a PR opened by your own account, which is the normal case when a whole group
    // authenticates as one GitHub user, so every review this repo has ever received is `COMMENTED`.
    // A reviewer that could not `--request-changes` had been given no legal way to say "no", and
    // the only other action the template named was `--approve` — the #235 incident, rebuilt by the
    // rule meant to prevent it. So: the verdict in the review body and the `report(...)` is the
    // binding record, `--comment` is the named fallback, and a refused `--request-changes` may
    // never decay into an approval or a softer verdict.
    pinned("reviewer.md", &r, "label every finding",
        "findings are labelled blocking / non-blocking — the orchestrator dispositions each one and \
         cannot do it from unlabelled prose");
    pinned("reviewer.md", &r, "stated rationale",
        "…and a finding that contradicts the change's OWN stated rationale is not a nit, however \
         small the fix: it says the change does not do what it claims");
    pinned("reviewer.md", &r, "your verdict is \"changes requested\", not \"approve\"",
        "…and the label binds to the VERDICT: a blocking finding means a changes-requested verdict, \
         so an approval with findings open is only ever an approval with NON-blocking findings open");
    pinned("reviewer.md", &r, "the binding record is the verdict you state",
        "…and the VERDICT — stated in the review body and repeated in `report(...)` — is the binding \
         surface, because it is the channel the orchestrator actually merges on; the `gh` flag is \
         only the mechanism, and it is one GitHub refuses on a same-account PR");
    pinned("reviewer.md", &r, "post with `--comment`",
        "…so the fallback must be NAMED (`--comment`, verdict at the top of the body), or a reviewer \
         that hits the refusal improvises — and the only other action the template names is \
         `--approve`");
    pinned("reviewer.md", &r, "never a reason to `--approve`",
        "…and a refused `--request-changes` may never decay into an approval or a softened verdict: \
         the mechanism was unavailable, the finding was not");
    // `findings still open` occurs twice — step 3 states the rule, step 5 states the reporting
    // duty — so `pinned` rejects it: either would rescue the other. Anchor step 5's own clause.
    pinned("reviewer.md", &r, "disposition pending",
        "…and an approval that leaves findings behind must SAY so, in the review body and the \
         report: the orchestrator merges on what you told it, and a review that reads like a clean \
         bill of health is how feedback dies at the merge");
}

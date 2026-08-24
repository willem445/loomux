//! Substance pins for the **manager's prose** — `templates/manager.md`, the
//! `mechanics_core(Role::Manager)` arm it must stay in lockstep with, and the
//! `{{MANAGER_NOTE}}` fragment that is the orchestrator's whole half of the
//! feature (#1161 M4).
//!
//! Its own file, and not `tests/prompts.rs`, for a reason that is a property of
//! the feature rather than a filing preference: `prompts.rs` pins what a
//! **default** group reads (its `rails()` turns the advanced orchestrator off,
//! and its module doc calls that "the whole scope of this file"), and a manager
//! exists only where a workflow file declares one —
//! `write_instruction_files`'s class-fallback loop deliberately never writes
//! `manager.md` for a default group. Every test here therefore needs a
//! workflow-declared roster, which is `tests/workflow.rs`'s shape; it is split
//! out from there for the same reason `manager_lifecycle.rs` was (#1161 M3),
//! to keep three concurrent slices off one 9000-line file.
//!
//! Integration tests, not unit tests, per CLAUDE.md constraint 4: a test
//! executable linking the full lib needs the comctl32-v6 manifest `build.rs`
//! embeds through `-tests`-scoped link args.
//!
//! The three properties that make a prose pin able to fail at all are
//! `prompts.rs`'s, and [`flat`], [`section`] and [`pinned`] below are the same
//! three helpers for the same three reasons: whitespace-collapsed matching (a
//! re-wrapped paragraph must not read as a deleted rule), region scoping (a
//! rule stated in two places is satisfied by either, so the pin must name the
//! region that OWES it), and uniqueness (an anchor occurring twice inside its
//! region cannot fail when the rule it names is deleted).
//!
//! Every anchor below was mutation-verified: delete the markdown unit that
//! carries the rule and the owning test goes red, naming the rule it lost.

use loomux_lib::orchestration::workflow::{self, Block};
use loomux_lib::orchestration::{mechanics_core, GroupId, Guardrails, OrchRegistry, Role};
use std::fs;
use std::path::PathBuf;

// ───────────────────────────────── harness ─────────────────────────────────

/// A registry with every test-only directory override applied — the same
/// rationale as `workflow.rs`'s `relaunch_registry`, duplicated because these
/// are separate integration-test binaries: an `OrchRegistry::new` built without
/// them falls through to the REAL `~/.claude/agents` on the next spawn (#464).
fn test_registry() -> (OrchRegistry, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let reg = OrchRegistry::new(dir.path().to_path_buf());
    reg.set_port(45999);
    reg.set_claude_agents_dir_override(dir.path().join("claude-agents"));
    reg.set_copilot_agents_dir_override(dir.path().join("copilot-agents"));
    reg.set_compact_hook_dir_override(dir.path().join("compacthook"));
    reg.set_copilot_hooks_dir_override(dir.path().join("copilot-hooks"));
    (reg, dir)
}

/// Guardrails for a group that RUNS the repo's workflow file — the manager is
/// declarable only behind the advanced-orchestrator toggle (#222/#1161
/// clarification 1), so every test here needs it on.
fn rails() -> Guardrails {
    Guardrails {
        max_agents: 6,
        agent_cli: "claude".into(),
        auto_ops: false,
        advanced_orchestrator: true,
        ..Guardrails::default()
    }
}

/// A throwaway repo carrying a workflow file. One level below its own temp
/// root, for `workflow.rs`'s `Repo` reason (a worktree cut as a SIBLING of the
/// repo must still fall inside the tree `Drop` reclaims).
struct Repo {
    _root: tempfile::TempDir,
    repo: PathBuf,
}

impl Repo {
    fn with_workflow(yaml: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        let dir = repo.join(".orrerix");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("workflow.yml"), yaml).unwrap();
        Repo { _root: root, repo }
    }
    fn path(&self) -> String {
        self.repo.to_string_lossy().replace('\\', "/")
    }
}

fn instructions(reg: &OrchRegistry, group: &GroupId, file: &str) -> String {
    fs::read_to_string(reg.state_root().join(group.as_str()).join(file))
        .unwrap_or_else(|e| panic!("{file} must exist in the group dir: {e}"))
}

/// The rendered instruction file, line endings normalized. There is no
/// `.gitattributes` covering these, so the template is CRLF on Windows and LF
/// elsewhere; these assertions are about the words, not about the checkout.
fn instructions_lf(reg: &OrchRegistry, group: &GroupId, file: &str) -> String {
    let text = instructions(reg, group, file).replace("\r\n", "\n");
    assert!(!text.contains("{{"), "{file} has an unsubstituted template variable:\n{text}");
    text
}

/// Lowercased, with every run of whitespace collapsed to one space, so a pin
/// fires on a deleted rule and not on a re-wrapped paragraph.
fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The slice of a `flat`ted document between two markers — the SECTION a rule
/// must live in. Without this a whole-document `contains` is satisfied by any
/// other statement of the same rule, and deleting the one that carries the
/// procedure leaves the pin green.
fn section<'a>(flat_doc: &'a str, start: &str, end: &str) -> &'a str {
    let from = flat_doc
        .find(start)
        .unwrap_or_else(|| panic!("the document has lost its `{start}` section entirely:\n{flat_doc}"));
    let rest = &flat_doc[from..];
    let to = rest[start.len()..].find(end).map(|i| i + start.len()).unwrap_or(rest.len());
    &rest[..to]
}

/// Assert `region` carries the rule `why`, and that `anchor` names it
/// **uniquely** — presence is the obvious half, uniqueness is the half that
/// makes the pin able to fail.
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

/// The smallest roster the manager's prose is real in: one manager under its
/// reserved id (so `manager.md` is the file it writes) and one worker so the
/// roster is an ordinary one rather than a manager on its own.
const WITH_MANAGER: &str = "version: 1\nblocks:\n\
     \x20 - id: manager\n    kind: manager\n\
     \x20 - id: worker\n    kind: worker\n";

/// Every tool `doc/design/manager.md`'s enumeration WITHHOLDS from
/// `Role::Manager`, spelled as the manager's prose would have to spell it to
/// send anyone after one.
///
/// The rule is `the_default_rendering_never_names_the_gate_machinery`'s
/// (`tests/workflow.rs`), one class over: prose naming a mechanism the reader
/// does not have is an invitation to go looking, and the manager is the class
/// with the widest gap between what it reads about and what it holds — it is
/// told about the fleet in order to talk about it, and told about the board in
/// order to answer "how is it going". Conditional framing does not save it.
///
/// Derived from the WITHHELD half of that table and nothing else, deliberately:
/// `list_verdicts`, `list_tasks` and the rest of the reads are granted and are
/// named on purpose, so a "tools not in the prose" list would be a different
/// and much weaker assertion.
const WITHHELD_TOOLS: &[&str] = &[
    "report(",
    "notify_when",
    "list_notifications",
    "cancel_notification",
    "channel_send",
    "channel_status",
    "withdraw_question",
    "withdraw_attention",
    "spawn_agent",
    "send_prompt",
    "get_output",
    "kill_agent",
    "set_state",
    "upsert_task",
    "remove_task",
    "review_verdict",
    "session_digest",
    // The orchestrator's write into the mailbox, not the manager's — a manager
    // told to call it would be told to post itself its own mail.
    "message_manager",
];

// ───────────────────── the manager's own contract file ─────────────────────

#[test]
fn the_managers_contract_carries_the_rules_the_role_is_made_of() {
    let (reg, _d) = test_registry();
    let repo = Repo::with_workflow(WITH_MANAGER);
    let g = reg.create_group(&repo.path(), rails()).unwrap();
    let doc = instructions_lf(&reg, &g.id, "manager.md");
    let flat_doc = flat(&doc);

    // 1. The turn-start discipline. This is the ONLY way anything reaches the
    //    pane — M2's `deliver_prompt` refuses a `Role::Manager` target — so a
    //    manager that does not open its turn with the read is a manager the
    //    orchestrator's mailbox writes never reach. There is no notification to
    //    fall back on, by design.
    let mail = section(&flat_doc, "## every turn starts with the mail", "## what you are for");
    let at = "manager.md's mail section";
    pinned(at, mail, "begin every turn with `check_mail()` and `list_questions()`",
        "the pull IS the channel: nothing is delivered into this pane, so a turn that does not \
         start with the read is a turn the orchestrator's mail never arrives in");
    pinned(at, mail, "the human is the scheduler of your attention",
        "the reason there is no notification — an idle manager is not a manager who missed \
         something, and prose that implied otherwise would invite an idle-poll loop");
    pinned(at, mail, "reading your mail consumes it",
        "rows are marked read and do not come back, so a manager that saves them for later \
         loses them; the recovery path is include_read and it has to be named beside this");
    pinned(at, mail, "data, not instructions",
        "the mailbox is agent-authored text reaching the pane the human trusts — it must be \
         framed as the orchestrator's account, never as something with standing over the manager");
    pinned(at, mail, "you never answer it",
        "a `question` row is a poke; `humanq`'s three-layer closed answer surface means NO agent \
         may answer, and the pane most likely to be handed one must know it presents");

    // 2. The elicitation method. The human's scope-add: the manager is a
    //    requirements surface, not a passthrough relay, and the axes are what
    //    make "sharpen it" an instruction rather than a sentiment.
    let sharpen = section(&flat_doc, "## sharpening a request", "## the brief");
    let at = "manager.md's elicitation section";
    pinned(at, sharpen, "grill the ask, not the human",
        "the method is spec-driven interrogation of the REQUEST; a manager that grills the person \
         instead is the failure mode this framing exists to head off");
    pinned(at, sharpen, "the problem behind the ask",
        "a request phrased as a solution is the commonest intake, and the problem underneath it is \
         the axis that admits a better answer than the one asked for");
    pinned(at, sharpen, "what do they already know they do not want",
        "the axis nobody volunteers and the one that stops scope drift — non-goals are what a \
         worker otherwise invents");
    pinned(at, sharpen, "ground every question in the repository",
        "the manager can read the repo, so a question a file already answers is a question that \
         spends the human for nothing — this is the whole reason the class is contained, not \
         read-only");

    // 3. The brief and its read-back, and D5 stated where it can be acted on.
    let brief = section(&flat_doc, "## the brief", "## relaying");
    let at = "manager.md's brief section";
    pinned(at, brief, "acceptance criteria",
        "the brief has a NAMED shape or it is a chat transcript with headings — this is the part \
         the orchestrator files verbatim and a worker is later measured against");
    pinned(at, brief, "it may not ship with invented answers",
        "an open question named is a brief that can still be built from; an open question guessed \
         is the wrong-direction PR this whole role exists to prevent");
    pinned(at, brief, "read it back and get an explicit yes",
        "the confirmation gate: a brief the human has not seen in full is a draft, and relaying a \
         draft is the manager speaking for them");
    pinned(at, brief, "silence is not a yes",
        "the three ways a read-back is faked — no reply, a yes to a summary rather than the text, \
         and a yes that predates an edit — are what the gate has to enumerate to hold");
    pinned(at, brief, "licenses filing the issue",
        "decision D5: the in-conversation yes moves the brief onto GitHub and nothing else");
    pinned(at, brief, "the start-work label",
        "D5's other half — the label is the human's own hand on their own record, and the manager \
         neither moves it nor asks for it to be moved");

    // 4. Relay fidelity and the authority line. `message_orchestrator` is the
    //    manager's only outbound channel, so these two rules are the entire
    //    difference between a delegate of the human's attention and a delegate
    //    of their authority.
    let relay = section(&flat_doc, "## relaying", "## questions, and what needs the human");
    let at = "manager.md's relay section";
    pinned(at, relay, "quote the human verbatim",
        "the orchestrator has no other way to tell a direction from the manager's reading of one; \
         a paraphrase is a directive the human never gave");
    pinned(at, relay, "relay only what they confirmed",
        "an inferred preference relayed as a decision is the manager deciding — the failure the \
         read-back gate exists to make impossible");
    pinned(at, relay, "never the human's authority",
        "a relay must never read as a grant: starting and merging work are gated by the human's \
         own hand on GitHub, and the interceptor refuses a relayed one exactly as it refuses the \
         orchestrator's own");

    // 5. The first-turn primer names the calls this role actually makes — the
    //    #381 property, and here it is the mail read that must lead, since a
    //    first turn that skips it starts the conversation blind.
    let primer = section(&flat_doc, "## your first turn", "## every turn starts with the mail");
    pinned("manager.md's first-turn primer", primer, "`check_mail()`, then `list_questions()`",
        "the turn-start discipline has to be in the primer as well as in its own section, or a \
         fresh session's FIRST turn is the one turn that skips it");
}

#[test]
fn the_managers_contract_never_names_a_tool_it_does_not_have() {
    // The `the_default_rendering_never_names_the_gate_machinery` rule, one
    // class over. A manager reads about the fleet in order to talk about it,
    // which makes it the class most likely to be handed the vocabulary of
    // tools it cannot call — and a manager that tries one burns a turn on a
    // refusal in front of the human, then reads the same line again next turn.
    let (reg, _d) = test_registry();
    let repo = Repo::with_workflow(WITH_MANAGER);
    let g = reg.create_group(&repo.path(), rails()).unwrap();
    let doc = instructions_lf(&reg, &g.id, "manager.md");

    // Positive control FIRST: this file must still be teaching tools by name at
    // all, or every absence below passes by the subject having vanished.
    for granted in ["check_mail(", "list_questions(", "message_orchestrator(", "ask_human(", "note_directive("] {
        assert!(
            doc.contains(granted),
            "control: manager.md must still name the tools the manager DOES have, or the \
             absences asserted below are vacuous — `{granted}` is gone:\n{doc}"
        );
    }

    for tool in WITHHELD_TOOLS {
        assert!(
            !doc.contains(tool),
            "manager.md names `{tool}`, which `Role::Manager`'s enumerated surface WITHHOLDS \
             (mcp.rs's manager arm refuses it, and doc/design/manager.md argues why). Prose \
             naming a tool the reader cannot call sends it after a refusal in front of the \
             human. Say what the manager does in words, or name a tool it holds."
        );
    }
}

#[test]
fn a_replace_mode_manager_still_gets_the_rules_the_template_carries() {
    // The lockstep half, and the reason `mechanics_core` is reachable from a
    // test at all: a `mode: replace` persona's block never reads its class
    // template, so that arm is the WHOLE orrerix contract such a pane gets. A
    // rule living only in `manager.md` is a rule that manager was never told.
    //
    // Reached by calling `mechanics_core` directly rather than by rendering a
    // replace-persona block the way `a_replace_mode_liaison_persona_...` does,
    // because decision D1 makes that render unreachable: `persona_allowed`
    // answers false for a manager, so `parse_workflow` refuses `prompt:` /
    // `profile:` / `allow:` on one and nothing can construct a replace-mode
    // manager through the parser. The arm is still a real contract — D1 is a
    // policy decision a later human opt-in could relax, and a hand-edited
    // `group.json` reaches it today — so it is pinned as one.
    let core = flat(&mechanics_core(Role::Manager, None));
    let at = "mechanics_core(Manager)";

    pinned(at, &core, "not one of its delegates",
        "the class it is NOT: a manager that read itself as a delegate would look for the report \
         and branch discipline every other class has");
    pinned(at, &core, "begin every turn with `check_mail()` and `list_questions()`",
        "the pull IS the channel — the same rule manager.md's mail section owes, and a replace \
         persona reads only this copy of it");
    pinned(at, &core, "sharpen the ask, then read it back",
        "the elicitation method and its confirmation gate, compressed to the one clause that \
         makes them non-optional");
    pinned(at, &core, "quoted verbatim",
        "relay fidelity: the orchestrator cannot tell a direction from a reading of one, and this \
         is the only place a replace-persona manager is told so");
    pinned(at, &core, "never the human's authority",
        "the authority line — a persona that forgot to say it must not thereby let the pane \
         believe it has some");
    pinned(at, &core, "the start-work label",
        "decision D5 rides in the core for the reason red-before-green does: the persona is the \
         swappable half, and a manager that thought a relayed yes started work would start it");
    pinned(at, &core, "you never write the repository",
        "the containment posture in words — the CLI-level edit deny is the structure, this is the \
         instruction that stops it trying");

    // The negative control this arm needs most: the DELEGATE spine
    // (`report`/branch/one-task-per-session) is `common`, and every clause of
    // it is false here. `mechanics_core` builds the manager's text separately
    // for exactly that reason, and a future edit that folds this arm back into
    // `common` to save a duplication is the regression.
    for spine in ["report(status, summary)", "create your branch", "One task per session"] {
        assert!(
            !mechanics_core(Role::Manager, None).contains(spine),
            "mechanics_core(Manager) carries the DELEGATE spine clause `{spine}` — a manager has \
             no report (its session never completes), no branch (it never writes) and no one task \
             (its session IS the human's conversation). This arm is deliberately not `common`."
        );
    }
    // …and the control for that control: the clauses must still be real, or
    // the three absences above pass on a spine that no longer exists.
    let worker = mechanics_core(Role::Worker, None);
    for spine in ["report(status, summary)", "create your branch", "One task per session"] {
        assert!(
            worker.contains(spine),
            "control: `{spine}` must still be in the delegate spine, or the manager's absence of \
             it is asserted against nothing:\n{worker}"
        );
    }
}

// ──────────────── the orchestrator's half: {{MANAGER_NOTE}} ────────────────

#[test]
fn manager_prose_stays_silent_unless_a_roster_declares_one() {
    // The #222 silence discipline, and the proof that this slice touches no
    // goldened template: a group with no manager must not read one word about
    // one. `templates/orchestrator.md` is not edited at all — the whole
    // orchestrator-side change is a fragment behind `{{MANAGER_NOTE}}`, which
    // renders to the empty string here.
    let (reg, _d) = test_registry();
    let repo = Repo::with_workflow(
        "version: 1\nblocks:\n\
         \x20 - id: worker\n    kind: worker\n\
         \x20 - id: rev\n    kind: reviewer\n",
    );
    let g = reg.create_group(&repo.path(), rails()).unwrap();

    let orch = instructions_lf(&reg, &g.id, "orchestrator.md");
    // Control: this group DID get the workflow section, so the absence below is
    // about the manager fragment and not about a render that produced nothing.
    assert!(
        orch.contains("## This repo declares a workflow"),
        "control: a custom roster must still get its workflow section: {orch}"
    );
    // Distinctive tokens rather than the bare word "manager": the fragment's
    // own heading and the two tool names it teaches can appear nowhere else,
    // whereas an ordinary English "manager" in some future paragraph of
    // unrelated prose would make this pin fire on a rule that did not move —
    // the false red the module docs call the one that teaches people to bless
    // a diff without reading it.
    for token in ["You have a manager", "message_manager", "check_mail", "mailbox"] {
        assert!(
            !orch.contains(token),
            "orchestrator.md names `{token}` for a roster that declares no manager — a group \
             without one must read nothing about one (#222's silence rule). Manager vocabulary \
             belongs behind {{{{MANAGER_NOTE}}}} in templates/workflow.md."
        );
    }
    // And no manager instructions file was written for it either.
    assert!(
        !reg.state_root().join(g.id.as_str()).join("manager.md").exists(),
        "a roster with no manager must have no manager.md"
    );
}

#[test]
fn a_declared_manager_gives_the_orchestrator_its_note_with_the_block_id() {
    let (reg, _d) = test_registry();
    // A CUSTOM id, deliberately: every rule in the fragment is addressed to a
    // specific pane, so the id has to be interpolated rather than a generic
    // "your manager". A fragment that named the class instead would read the
    // same for the reserved id and be wrong for every other roster.
    let repo = Repo::with_workflow(
        "version: 1\nblocks:\n\
         \x20 - id: mgr-desk\n    kind: manager\n\
         \x20 - id: worker\n    kind: worker\n",
    );
    let g = reg.create_group(&repo.path(), rails()).unwrap();
    let orch = instructions_lf(&reg, &g.id, "orchestrator.md");

    assert_eq!(orch.matches("**You have a manager.**").count(), 1, "exactly once: {orch}");
    assert!(orch.contains("`mgr-desk` is the pane the HUMAN talks to"), "{orch}");
    // Placement: the placeholder is line-final in `templates/workflow.md`, so
    // the fragment brings its own blank line or the `**…**` lands mid-sentence.
    assert!(
        orch.contains("workaround in your head.\n\n**You have a manager.**"),
        "the fragment must bring its own blank line, not land mid-paragraph: {orch}"
    );
    // The manager is NOT in the delegate list — `is_spawnable_block` filters it
    // — which is what keeps this fragment from contradicting the same
    // document two screens up.
    assert!(!orch.contains("**`mgr-desk`** —"), "a manager is not a delegate row: {orch}");

    let flat_doc = flat(&orch);
    let note = section(&flat_doc, "**you have a manager.**", "a custom workflow config is your group's roster");
    let at = "the manager note";

    pinned(at, note, "you do not open it, and you cannot",
        "`spawn_agent` refuses a manager by kind AND by block, so an orchestrator told to start \
         it would burn a turn on the refusal every turn it re-read the line");
    pinned(at, note, "`message_manager(text, kind)` is your only way to reach it",
        "every delivery into the pane is refused, so an orchestrator that reached for send_prompt \
         would get an error instead of a message — name the tool that works");
    pinned(at, note, "it is therefore not a way to get anyone's attention now",
        "a durable mailbox read on the manager's next turn is not a notification; an orchestrator \
         that treated it as one would post something urgent and consider the human told");
    pinned(at, note, "send milestones, not a running commentary",
        "the mailbox has a 32-unread cap that REFUSES the writer rather than dropping the human's \
         status — a commentary stream is what reaches that cap");
    pinned(at, note, "never forward operational traffic to it",
        "delegate reports and notices are what turn a mailbox into something nobody reads; this \
         is the loop the fragment exists to forbid");
    pinned(at, note, "quoting the brief **verbatim**",
        "the issue is the durable artifact and the manager's containment keeps it from filing \
         one — a brief the orchestrator paraphrases into an issue is the human's ask lost at the \
         one hand-off that was supposed to preserve it");
    pinned(at, note, "filing is all that yes licenses",
        "decision D5 on the receiving side: the orchestrator is the one that COULD act on a \
         relayed yes, so it is the one that has to be told the yes does not reach the label");
    pinned(at, note, "is a human directive",
        "what the orchestrator RECEIVED must reach its ledger as the human's word, or a relayed \
         direction decays into a suggestion from another agent");
    pinned(at, note, "never the human's authority",
        "the relay-is-not-a-grant rule, stated to the side that would be the one to act on a \
         grant it never got");
    pinned(at, note, "invariant 2 is untouched by the indirection",
        "the never-block question protocol survives a presenter change because the hold was never \
         in the presenter — an orchestrator told otherwise would invent hold machinery");
    pinned(at, note, "settle your own overtaken row",
        "the manager has no withdraw_question, so a question answered in conversation stays in \
         the human's inbox unless the orchestrator closes it");
    pinned(at, note, "status is its job, not a briefing you owe it",
        "self-served status is the latency argument the whole class exists for; pushing status at \
         it spends the orchestrator turn the design removed");
    pinned(at, note, "never `kill_agent` `mgr-desk`",
        "inside this group the orchestrator is the one thing that can end the human's own pane, \
         and an idle manager is its normal state rather than a delegate to reclaim");
    pinned(at, note, "nothing here depends on it being alive",
        "degradation: direct access to this pane is the escape hatch, and it is what makes the \
         whole feature safe to add");
}

// ─────────────────────────── BRIEF-CARRY (w-875 N9) ───────────────────────────

#[test]
fn a_custom_id_manager_block_is_never_handed_the_delegate_spine() {
    // `block.md`'s closing paragraph recaps "the mechanics this file does not
    // change" — and it is the paragraph that tells the reader to believe this
    // file over its own instructions, so a wrong recap there does not read as a
    // mismatch to resolve. It reads as the correction.
    //
    // It reaches a manager whose id is not the reserved `manager`: `block_note`
    // early-returns for a builtin id with no persona, so `- id: mgr-desk` is
    // the case, and it was being handed `report(status, summary)`, the branch →
    // PR flow, and the human gating "every merge" — three clauses that are each
    // false for a pane with no report, no branch and no work of its own.
    let (reg, _d) = test_registry();
    let repo = Repo::with_workflow(
        "version: 1\nblocks:\n\
         \x20 - id: mgr-desk\n    kind: manager\n\
         \x20 - id: hand\n    kind: worker\n    prompt: Build things.\n",
    );
    let g = reg.create_group(&repo.path(), rails()).unwrap();
    let doc = instructions_lf(&reg, &g.id, "mgr-desk.md");

    // Control: the block note is actually there. Without this the absences
    // below pass on a document that never got the fragment at all.
    assert!(
        doc.contains("## Your block") && doc.contains("**`mgr-desk`** block"),
        "control: a custom-id block must still get its block note: {doc}"
    );
    for spine in ["report(status, summary)", "branch → PR flow", "the human gates every merge"] {
        assert!(
            !doc.contains(spine),
            "mgr-desk.md's block note recaps `{spine}` as a mechanic this manager has. A manager \
             has no report, no branch and no merge of its own — and this is the paragraph that \
             tells it to believe this file over its own instructions:\n{doc}"
        );
    }
    // The recap is not deleted, it is REPLACED: the sentence still lands as a
    // recap of what the workflow file could not change, in the manager's terms.
    assert!(
        doc.contains("the read-back that has to come before any relay"),
        "the recap must state the manager's OWN mechanics, not merely omit the delegate's: {doc}"
    );

    // The other half of the selection — a custom WORKER block in the same
    // group still reads the delegate spine, so the rule is keyed on the class
    // and is not `block.md` having quietly lost its closing paragraph.
    let worker = instructions_lf(&reg, &g.id, "hand.md");
    for spine in ["report(status, summary)", "branch → PR flow", "the human gates every merge"] {
        assert!(
            worker.contains(spine),
            "control: a custom worker block must still get the delegate recap — `{spine}` is \
             gone, so the manager's absence of it is asserted against nothing:\n{worker}"
        );
    }
}

// ───────────────────────────── lockstep guards ─────────────────────────────

#[test]
fn the_manager_template_is_the_only_place_its_block_placeholder_may_sit() {
    // `manager.md` carries exactly one workflow-conditional key, and its
    // line-final placement is what lets a manager-less render leave the file
    // untouched to the byte. `tests/workflow.rs`'s
    // `a_workflow_placeholder_must_sit_at_the_end_of_a_line_it_shares` asserts
    // this across all five templates against the goldens; this is the
    // manager-scoped restatement that fails by naming the manager, so an M4
    // edit that moved the key onto its own line is told what it broke rather
    // than being handed a re-bless diff.
    let tpl = loomux_lib::orchestration::MANAGER_TPL.replace("\r\n", "\n");
    assert_eq!(tpl.matches("{{BLOCK_NOTE}}").count(), 1, "exactly one, or the strip in the golden pairing is ambiguous");
    let line = tpl
        .lines()
        .find(|l| l.contains("{{BLOCK_NOTE}}"))
        .expect("the key is on some line");
    assert!(
        line.trim_end().ends_with("{{BLOCK_NOTE}}"),
        "`{{{{BLOCK_NOTE}}}}` must sit at the END of a line it shares — a key on its own line \
         leaves a blank line behind when it renders empty, which is a byte change to what a \
         manager-less render produces. Line: {line:?}"
    );
    assert!(
        !tpl.contains("{{LOCKS}}") && !tpl.contains("{{ADVISOR_CONSULT_NOTE}}"),
        "the manager holds no locks and consults no advisor — a key here that `LIVE` does not \
         list is one the golden pairing will not strip, and the diff it produces names nothing"
    );
}

#[test]
fn a_manager_block_is_never_spawnable_and_never_a_reviewer() {
    // The two membership predicates the fragment's prose asserts as facts about
    // code. Pinned here beside the prose that relies on them: "you do not open
    // it, and you cannot" and "it reviews nothing" are claims, and a predicate
    // that stopped answering this way would leave them false with nothing red.
    let wf = workflow::parse_workflow(WITH_MANAGER).expect("the manager roster parses");
    let mgr: &Block = wf.block("manager").expect("the manager block");
    assert!(!workflow::is_spawnable_block(mgr), "a manager is opened for the human, not by the orchestrator");
    assert!(!workflow::is_reviewing_block(mgr), "a manager reviews nothing and can satisfy no gate");
    // Controls: the same predicates say yes to the block that IS both.
    let worker = wf.block("worker").expect("the worker block");
    assert!(workflow::is_spawnable_block(worker), "control: a worker is spawnable");
    let rev = workflow::parse_workflow("version: 1\nblocks:\n  - id: rev\n    kind: reviewer\n")
        .expect("a lone reviewer parses");
    assert!(
        workflow::is_reviewing_block(rev.block("rev").unwrap()),
        "control: a plain reviewer reviews, or the manager's exclusion is asserted against nothing"
    );
}

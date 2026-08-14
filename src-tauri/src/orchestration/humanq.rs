//! The human-question registry (#946 slice Q1) — the durable record behind
//! `ask_human` / `list_questions` / `withdraw_question`.
//!
//! # Why the engine owns this, and not a liaison agent
//!
//! A blocking orchestrator question halts the whole fleet: while a CLI's own
//! interactive dialog is on screen that pane cannot take a delivery, so every
//! worker report queues behind it. One human-absent question is an overnight
//! outage. The fix is to make asking *asynchronous* — the orchestrator
//! registers a question, gets an id back immediately, and keeps orchestrating.
//!
//! The obvious shape is a dedicated liaison agent that holds the pending
//! question and relays it. That re-creates the incident one level up: an agent
//! pane is an LLM session, and it compacts, dies and gets idle-killed. So the
//! pending question is **engine state** — a per-group file, `questions.json`,
//! beside `tasks.json` — and any presenter (the webview inbox, a liaison pane,
//! a chat bridge) is a client of that record, never the record itself.
//!
//! # The trust boundary — the part to read before changing anything here
//!
//! **Every agent may ASK. No agent may ever ANSWER.**
//!
//! An answer settles a question the *human* was asked, and un-blocks the work
//! that was waiting on it. If an agent could produce one, the mechanism would
//! be a machine asking itself for permission — the same self-served-gate shape
//! CLAUDE.md constraint 9 refuses for install/security prompts, and it would
//! make the whole feature theatre.
//!
//! That is enforced structurally rather than by convention, in three layers:
//!
//! 1. **No answer tool exists.** `mcp.rs`'s `call_tool` is a closed match on
//!    tool names; none of its arms reaches [`super::OrchRegistry::answer_question`].
//!    An agent cannot call what has no name. `no_agent_token_can_answer_a_question`
//!    dispatches every tool the surface offers and asserts the question is
//!    still pending; `the_mcp_surface_has_no_path_to_the_answer_entry_point`
//!    scans `mcp.rs`'s source so a future slice cannot wire one in quietly.
//! 2. **The source is a property of the entry point, never an argument.**
//!    [`AnswerSource`] is a closed enum whose variants are trusted surfaces,
//!    and `orch_question_answer` hard-codes [`AnswerSource::Webview`] rather
//!    than accepting a `source` string. There is no spelling of "answer as
//!    someone else", so there is nothing to validate and nothing to forge.
//! 3. **Provenance is durable.** Every settle and every refusal is audited
//!    (`question-answer`, `question-reject`) carrying the source tag, so
//!    "who answered this, and what was turned away" is reconstructable from
//!    the log rather than from memory.
//!
//! If you are adding a second answer surface (the #947 chat bridge is the
//! planned one), add an [`AnswerSource`] variant and a trusted entry point
//! that supplies it. Do **not** add a `source` parameter, and do not add an
//! MCP tool.
//!
//! # Naming
//!
//! `Question*` is already taken in `mod.rs` by the pane detector that decides
//! whether a CLI is showing an interactive prompt (`QuestionMatch`,
//! `QuestionWitnessed`, …) — unrelated machinery. This module keeps its own
//! vocabulary behind the `humanq::` path (`humanq::Question`,
//! `humanq::Status`) so neither grep nor reader has to disambiguate.

use serde::{Deserialize, Serialize};

use super::notify::sanitize_gh_text;

/// The per-group file, beside `tasks.json` / `state.json` in the group dir.
pub const QUESTIONS_FILE: &str = "questions.json";

/// Longest question body. Generous, because a self-contained question is the
/// point — but bounded, because this text is delivered into a pane and (from
/// #947) into a chat message.
pub const QUESTION_TEXT_MAX: usize = 2000;

/// Longest single suggested answer.
pub const OPTION_TEXT_MAX: usize = 200;

/// Most suggested answers one question may carry. A question needing more than
/// this is a question that has not been decided down to a choice yet.
pub const OPTIONS_MAX: usize = 8;

/// Longest answer body accepted from a trusted surface.
pub const ANSWER_TEXT_MAX: usize = 2000;

/// Most questions that may be pending in one group at once.
///
/// Not a rate limit — a backstop on the file. Reaching it means the
/// orchestrator is asking faster than a human could ever answer, which is a
/// symptom worth refusing loudly rather than absorbing silently.
pub const PENDING_MAX: usize = 32;

/// Settled (answered/withdrawn) rows kept in the file. Older ones are dropped
/// on the next write; the audit log keeps all of them regardless, so this is a
/// cap on the hot read, not on the history.
pub const SETTLED_RETAINED: usize = 20;

/// Settled rows `list_questions` returns alongside the pending ones. The
/// omitted count travels with the response, so a filtered list is never
/// mistaken for the whole one.
pub const LIST_SETTLED_CAP: usize = 10;

/// Total cap on the composed `[loomux] answer to q-N …` notice.
///
/// Wider than `notify::NOTICE_TOTAL_CAP` (400), deliberately: those notices
/// are status lines whose payload is a check name, while this one carries a
/// human's actual decision — truncating it to a status line's budget would
/// throw away the sentence the orchestrator is waiting on.
pub const ANSWER_NOTICE_CAP: usize = 2400;

/// Where a question is in its life. Terminal states are both settled: an
/// answered question got its decision, a withdrawn one was overtaken by
/// events. Neither can be re-settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pending,
    Answered,
    Withdrawn,
}

impl Status {
    pub fn is_settled(self) -> bool {
        !matches!(self, Status::Pending)
    }
    pub fn label(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Answered => "answered",
            Status::Withdrawn => "withdrawn",
        }
    }
}

/// How loudly this question wants the human's attention.
///
/// Carried but not yet acted on: slice Q2 keys the latched attention item and
/// the opt-in desktop toast off it. It is in the schema from the first slice
/// on purpose — the persisted shape is a public contract, and adding a field
/// to it later means migrating files that are already holding questions a
/// human has not answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Urgency {
    Normal,
    High,
}

impl Default for Urgency {
    fn default() -> Self {
        Urgency::Normal
    }
}

impl Urgency {
    /// Parse a tool argument. Unrecognized is an ERROR, never a defaulted
    /// `normal`: an orchestrator that wrote `"urgent"` meant to raise the
    /// priority, and silently filing it as routine is the failure this
    /// mechanism exists to prevent.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "normal" => Ok(Urgency::Normal),
            "high" => Ok(Urgency::High),
            other => Err(format!("unknown urgency {other:?} — use \"normal\" or \"high\"")),
        }
    }
}

/// One question put to the human, and its answer once it has one.
///
/// Every field past the required core carries `#[serde(default)]` so a file
/// written by an older build still loads, following `Task`'s convention.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Question {
    /// `q-1`, `q-2`, … — minted per group from the file's own high-water mark,
    /// exactly as task ids are. Deliberately legible rather than opaque: this
    /// id is quoted in a board note, in the answer notice, and (from #947) in
    /// a chat message, and it is never a capability — holding it grants
    /// nothing, since the only surfaces that can act on it are trusted ones.
    pub id: String,
    /// The agent that asked. Orchestrator-only today; recorded rather than
    /// assumed so the audit answers "who asked" without inference.
    pub asker: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// The board task this question is holding up, if any — what lets the
    /// orchestrator un-block exactly one task when the answer lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default)]
    pub urgency: Urgency,
    pub status: Status,
    #[serde(default)]
    pub created_ms: u64,
    /// The human's decision, verbatim as the trusted surface supplied it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Which trusted surface settled it — [`AnswerSource::tag`], or
    /// `withdrawn:<agent>` when the asker took it back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_ms: Option<u64>,
}

/// **Which trusted surface an answer came from — a closed set, and never a
/// caller-supplied string.**
///
/// See this module's trust-boundary section. Each variant corresponds to an
/// entry point loomux itself controls; there is no variant for an agent, and
/// adding one would defeat the feature rather than extend it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnswerSource {
    /// The human typing in the app's own webview, via the
    /// `orch_question_answer` Tauri command. Trusted for the same reason the
    /// merge-grant commands are: the webview is loomux's own UI, and the
    /// gesture is a human's.
    Webview,
}

impl AnswerSource {
    /// The stable string recorded on the question and in the audit log.
    ///
    /// Returns `String` rather than `&'static str` because the planned #947
    /// variant carries an identity (`telegram:<chat_id>`), and a signature
    /// that has to widen later is a signature every call site re-touches.
    pub fn tag(&self) -> String {
        match self {
            AnswerSource::Webview => "webview".to_string(),
        }
    }
}

/// What `ask_human` was asked to register, after argument parsing and before
/// validation. Split from the registry method so the validation below is a
/// pure function of its input.
#[derive(Clone, Debug, Default)]
pub struct AskRequest {
    pub text: String,
    pub options: Vec<String>,
    pub task: Option<String>,
    pub urgency: Urgency,
}

/// Normalize and bounds-check an ask, or say exactly what is wrong with it.
///
/// Rejects rather than truncates. A question silently cut at 2000 characters
/// is a question whose actual ask may have been the part that was dropped, and
/// the asker has no way to see that happened.
pub fn validate_ask(req: AskRequest) -> Result<AskRequest, String> {
    let text = req.text.trim().to_string();
    if text.is_empty() {
        return Err("text required: a question with no body is nothing for a human to answer".into());
    }
    if text.chars().count() > QUESTION_TEXT_MAX {
        return Err(format!(
            "question text is {} characters, max {QUESTION_TEXT_MAX} — ask the decision, not the \
             whole context; the detail belongs on the issue or PR you cite",
            text.chars().count()
        ));
    }
    if req.options.len() > OPTIONS_MAX {
        return Err(format!(
            "{} options, max {OPTIONS_MAX} — a question with more choices than that has not been \
             narrowed to a decision yet",
            req.options.len()
        ));
    }
    let mut options = Vec::with_capacity(req.options.len());
    for opt in req.options {
        let opt = opt.trim().to_string();
        if opt.is_empty() {
            return Err("an empty option is not a choice — drop it or give it text".into());
        }
        if opt.chars().count() > OPTION_TEXT_MAX {
            return Err(format!("an option is {} characters, max {OPTION_TEXT_MAX}", opt.chars().count()));
        }
        options.push(opt);
    }
    let task = req.task.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    Ok(AskRequest { text, options, task, urgency: req.urgency })
}

/// Bounds-check an answer from a trusted surface.
///
/// Trusted is not the same as unbounded: the webview's answer box is a text
/// area, and a paste of a whole file would otherwise become a pane delivery.
pub fn validate_answer(answer: &str) -> Result<String, String> {
    let answer = answer.trim().to_string();
    if answer.is_empty() {
        return Err("an answer needs text".into());
    }
    if answer.chars().count() > ANSWER_TEXT_MAX {
        return Err(format!(
            "answer is {} characters, max {ANSWER_TEXT_MAX}",
            answer.chars().count()
        ));
    }
    Ok(answer)
}

/// The next id for this group: `q-{highest + 1}`, read off the file rather
/// than a counter, exactly as `upsert_task` mints `t-N`. Ids are never reused
/// because settled rows are retained (and, once dropped, only ever above the
/// high-water mark that produced them).
pub fn next_id(existing: &[Question]) -> String {
    let max: u32 = existing
        .iter()
        .filter_map(|q| q.id.strip_prefix("q-").and_then(|n| n.parse().ok()))
        .max()
        .unwrap_or(0);
    format!("q-{}", max + 1)
}

/// The notice delivered into the orchestrator's pane when a question is
/// settled by an answer.
///
/// **`answer` is untrusted text entering a `[loomux]` line.** It was typed by
/// a human rather than generated by an agent, but the pane cannot tell those
/// apart and a newline in it would forge a second line that reads as its own
/// legitimate notice — so it goes through `sanitize_gh_text` like every other
/// field of every other notice. The id and the source tag are loomux-built and
/// need no sanitizing; they are emitted BEFORE the answer so that the cap
/// trims the answer's tail rather than swallowing the attribution.
pub fn answer_notice(id: &str, source_tag: &str, answer: &str) -> String {
    let body = sanitize_gh_text(answer, ANSWER_TEXT_MAX);
    let text = format!("[loomux] answer to {id} (via {source_tag}): {body}");
    text.chars().filter(|c| !c.is_control()).take(ANSWER_NOTICE_CAP).collect()
}

/// Drop the oldest settled rows past `keep`, preserving every pending one.
///
/// Pending rows are never pruned at any count: a question the human has not
/// answered is the one thing this file exists to not lose. `PENDING_MAX`
/// is what bounds those, by refusing new asks rather than by deleting old
/// ones.
pub fn prune(questions: &mut Vec<Question>, keep: usize) {
    let settled = questions.iter().filter(|q| q.status.is_settled()).count();
    if settled <= keep {
        return;
    }
    let mut to_drop = settled - keep;
    // Oldest first: the vector is append-ordered, so a forward scan drops the
    // longest-settled rows.
    questions.retain(|q| {
        if to_drop > 0 && q.status.is_settled() {
            to_drop -= 1;
            false
        } else {
            true
        }
    });
}

/// What `list_questions` returns: every pending question (oldest first, which
/// is the order they should be answered in), then the newest settled rows up
/// to `cap`, plus how many settled rows were left off.
///
/// Pending rows are never omitted and never counted in the omitted total — a
/// caller reading this to decide what is still outstanding must see all of it.
pub fn project_list(questions: &[Question], cap: usize) -> (Vec<Question>, usize) {
    let mut pending: Vec<Question> =
        questions.iter().filter(|q| !q.status.is_settled()).cloned().collect();
    let settled: Vec<Question> =
        questions.iter().filter(|q| q.status.is_settled()).cloned().collect();
    let omitted = settled.len().saturating_sub(cap);
    pending.extend(settled.into_iter().skip(omitted));
    (pending, omitted)
}

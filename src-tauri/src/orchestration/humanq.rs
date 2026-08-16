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
//! pane is an LLM session, and it compacts, wedges and dies. So the
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
//!    An agent cannot call what has no name.
//!    `no_agent_token_can_answer_a_question_through_the_mcp_surface` dispatches
//!    every tool the surface offers and asserts the question still carries no
//!    answer afterwards — *no answer*, deliberately, rather than *still
//!    pending*: [`withdraw_question`](super::OrchRegistry::withdraw_question) is
//!    on that surface and legitimately settles a question as `withdrawn`, and
//!    the whole point is that taking your own question back is not the same
//!    power as deciding it. `the_mcp_surface_has_no_path_to_the_answer_entry_point`
//!    scans `mcp.rs`'s source so a future slice cannot wire one in quietly.
//! 2. **The source is a property of the entry point, never an argument.**
//!    [`AnswerSource`] is a closed enum whose variants are trusted surfaces,
//!    and `orch_question_answer` hard-codes [`AnswerSource::Webview`] rather
//!    than accepting a `source` string. There is no spelling of "answer as
//!    someone else", so there is nothing to validate and nothing to forge.
//!    **The closed SET is itself pinned**, not just the type's whereabouts:
//!    `the_mcp_surface_has_no_path_to_the_answer_entry_point` reads the variant
//!    list off this file's own declaration, so an `AnswerSource::Agent` added
//!    here — a legitimate home for the type, and therefore invisible to a
//!    "where may it be named" check — reddens a test. Every variant is a party
//!    empowered to settle a question the human was asked; that list is the
//!    boundary.
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

/// Longest single suggested answer — the option's LABEL, which is also the
/// text an answering surface quotes back verbatim as the human's answer.
pub const OPTION_TEXT_MAX: usize = 200;

/// Longest per-option description (#1091). Wider than the label because this is
/// where the trade-off goes — "what you give up by picking this" — while the
/// label stays short enough to sit on a button.
pub const OPTION_DESC_MAX: usize = 500;

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

/// One named alternative on a question — a bare string, or a label with the
/// reasoning behind it (#1091).
///
/// **Untagged, and a description-less option is normalized back to
/// [`OptionSpec::Plain`] before it is stored.** Both halves matter:
/// untagged means a `questions.json` written by the Q1 build — where every
/// option was a bare string — parses unchanged, so nothing migrates; and
/// normalizing on the way in means a richer build writing an option that
/// carries no description writes a bare string too, rather than quietly
/// changing the shape of every file it touches. The object form appears on
/// disk exactly when a description was actually given.
///
/// The reverse is deliberately not promised: an OLD build reading a NEW file
/// that does carry an object option fails its parse *loudly* (the read posture
/// this module's `questions()` doc argues for) rather than dropping the row.
/// Downgrade safety was never on offer here — losing a pending question the
/// human has not answered is the one failure the registry exists to prevent,
/// and a loud refusal is how it is prevented.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OptionSpec {
    /// `"ship it here"` — the Q1 shape, and still the stored shape whenever
    /// there is no description to carry.
    Plain(String),
    /// `{"label": "ship it here", "description": "one review, bigger diff"}`.
    Detailed {
        label: String,
        /// Absent in the wire form is the same as empty, and an empty one is
        /// normalized to `Plain` by [`validate_ask`] — so a stored `Detailed`
        /// always carries text here.
        #[serde(default)]
        description: String,
    },
}

impl OptionSpec {
    /// The choice itself — what a surface puts on the button, and what an
    /// answer quotes back verbatim.
    pub fn label(&self) -> &str {
        match self {
            OptionSpec::Plain(label) => label,
            OptionSpec::Detailed { label, .. } => label,
        }
    }

    /// The reasoning under the label, when there is any. `None` and `Some("")`
    /// would mean the same thing to a renderer, so the empty case never
    /// reaches one: it is normalized away at validation.
    pub fn description(&self) -> Option<&str> {
        match self {
            OptionSpec::Plain(_) => None,
            OptionSpec::Detailed { description, .. } => {
                Some(description.as_str()).filter(|d| !d.is_empty())
            }
        }
    }
}

/// How many of a question's options the human may choose (#1091).
///
/// Only meaningful alongside `options` — [`validate_ask`] refuses it on a
/// question that has none, because there is then nothing for it to describe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Select {
    /// Pick one. The default: a question is a decision.
    Single,
    /// Pick any number — the ask really is "which of these", not "which one".
    Multi,
}

impl Default for Select {
    fn default() -> Self {
        Select::Single
    }
}

impl Select {
    /// Parse a tool argument, with [`Urgency::parse`]'s posture: unrecognized
    /// is an ERROR, never a defaulted `single`. An orchestrator that wrote
    /// `"multiple"` meant the human to be able to pick several, and silently
    /// filing that as a one-of-N choice loses part of the answer it wanted.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "single" => Ok(Select::Single),
            "multi" => Ok(Select::Multi),
            other => Err(format!("unknown select {other:?} — use \"single\" or \"multi\"")),
        }
    }
}

/// Whether an answering surface offers a free-text box beside the options.
///
/// The default is `true`, and it is a default rather than a setting most asks
/// touch: the affordance being mirrored (a CLI's own question dialog) always
/// offers an "other" escape, and a human who can only pick from an agent's
/// list is a human whose actual answer has nowhere to go. Denying it is an
/// explicit opt-OUT, and only meaningful when options exist.
fn free_text_default() -> bool {
    true
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
    pub options: Vec<OptionSpec>,
    /// How many options the human may pick. `single` unless the ask said
    /// otherwise; meaningless without `options`, which is why an ask that
    /// gives one without the other is refused rather than stored.
    ///
    /// Serialized unconditionally, like `urgency`/`status` and unlike
    /// `options`/`task` — the `skip_serializing_if` in this struct marks the
    /// fields that can be genuinely ABSENT, while this one always has a value
    /// that decides how the row is answered. A pending question is a record a
    /// human may read straight out of the file, so it says what it means
    /// rather than making the reader know the defaults (rev-802 N5).
    #[serde(default)]
    pub select: Select,
    /// Whether the answering surface offers a free-text box as well. Defaults
    /// to `true` — including for a Q1-era row that has no such field, which is
    /// the right reading of it: the only answer surface those rows were ever
    /// written for was free text.
    #[serde(default = "free_text_default")]
    pub allow_free_text: bool,
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
///
/// `select` and `allow_free_text` are `Option` here and plain values on
/// [`Question`], and that asymmetry is load-bearing rather than untidy:
/// "single" and "free text allowed" are what an ask that said nothing gets,
/// but they are also what an ask can say explicitly, and [`validate_ask`]
/// refuses either of them on a question with no options. Collapsing the
/// request to the stored type would erase the difference between *said* and
/// *defaulted*, and the refusal — the thing that tells an orchestrator its
/// `select: "multi"` did nothing — would have to be duplicated at the parse
/// site, where it could then drift.
#[derive(Clone, Debug)]
pub struct AskRequest {
    pub text: String,
    pub options: Vec<OptionSpec>,
    pub select: Option<Select>,
    pub allow_free_text: Option<bool>,
    pub task: Option<String>,
    pub urgency: Urgency,
}

impl Default for AskRequest {
    /// Hand-written rather than derived because `bool`'s derived default is
    /// `false`, and `allow_free_text: false` is the one value this field must
    /// never acquire by accident — it takes the human's escape hatch away.
    /// `None` is what "the ask did not say" has to mean here.
    fn default() -> Self {
        AskRequest {
            text: String::new(),
            options: Vec::new(),
            select: None,
            allow_free_text: None,
            task: None,
            urgency: Urgency::default(),
        }
    }
}

impl AskRequest {
    /// What to store for `select`: what the ask said, or `single`.
    pub fn select_or_default(&self) -> Select {
        self.select.unwrap_or_default()
    }

    /// What to store for `allow_free_text`: what the ask said, or `true`.
    /// See [`free_text_default`] for why the default is the permissive one.
    pub fn free_text_allowed(&self) -> bool {
        self.allow_free_text.unwrap_or_else(free_text_default)
    }
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
        let label = opt.label().trim().to_string();
        if label.is_empty() {
            return Err("an empty option is not a choice — drop it or give it text".into());
        }
        if label.chars().count() > OPTION_TEXT_MAX {
            return Err(format!(
                "an option is {} characters, max {OPTION_TEXT_MAX}",
                label.chars().count()
            ));
        }
        let description = opt.description().unwrap_or_default().trim().to_string();
        if description.chars().count() > OPTION_DESC_MAX {
            return Err(format!(
                "an option description is {} characters, max {OPTION_DESC_MAX} — the description \
                 is the trade-off in a line, not the case for it; cite the issue or PR for that",
                description.chars().count()
            ));
        }
        // A description-less option is stored as the bare string it was in Q1,
        // whichever form it arrived in. See `OptionSpec`.
        options.push(if description.is_empty() {
            OptionSpec::Plain(label)
        } else {
            OptionSpec::Detailed { label, description }
        });
    }
    // `select` and `allow_free_text` describe a list of options. Given without
    // one, they are not harmless no-ops to absorb: each says the orchestrator
    // believed it was shaping a choice the human would be offered, and storing
    // them silently would leave that belief uncorrected. Refuse, and name the
    // missing half.
    if options.is_empty() {
        if req.select.is_some() {
            return Err(
                "select needs options — it says how many of them the human may pick, and this \
                 question offers none"
                    .into(),
            );
        }
        if req.allow_free_text == Some(false) {
            return Err(
                "allow_free_text: false needs options — with no options and no free text there \
                 is nothing left for the human to answer with"
                    .into(),
            );
        }
    }
    let task = req.task.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    Ok(AskRequest {
        text,
        options,
        select: req.select,
        allow_free_text: req.allow_free_text,
        task,
        urgency: req.urgency,
    })
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

/// Drop the longest-ASKED settled rows past `keep`, preserving every pending
/// one.
///
/// **Ask order, not settle order** — the distinction is real, not pedantry. The
/// vector is append-ordered by when a question was *asked*, and nothing
/// re-orders it when one is settled, so a question asked early and answered
/// late sits ahead of one asked late and answered immediately. A forward scan
/// therefore evicts by age-since-asking, which is the useful order anyway (the
/// oldest exchange is the least likely to still be worth reading) and is not
/// the same thing as evicting the longest-settled row.
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
    // Forward scan over an ask-ordered vector, so this evicts the rows asked
    // longest ago. Not the longest-SETTLED ones — see the doc above.
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

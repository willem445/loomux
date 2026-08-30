//! Shared fixtures for the backend integration tests.
//!
//! Included with `mod common;` — this is `tests/common/mod.rs`, so cargo does
//! NOT build it as a test target of its own (only `tests/*.rs` are), and
//! CLAUDE.md constraint 4 is untouched: everything here compiles INTO the
//! including integration test, which is still where anything linking the lib
//! has to live.
//!
//! # Why this file exists (#1702)
//!
//! The defect #1702 fixes was invisible to every test in this repo for four
//! betas, and the reason is a FIXTURE SHAPE rather than a missing assertion.
//! `attention_tick` deadlocks only when an agent is simultaneously (a) running,
//! (b) bound to a pty, (c) present in the `by_pty` reverse index and (d) quiet
//! past the attention window. `attention_setup` in `orchestration.rs` — the
//! helper every attention test uses — spawns agents with NO pty, so `pty_id` is
//! `None`, the per-agent mask is never reached at all, and the whole class is
//! unreachable from the suite. The soak lane has the same gap from the other
//! end: it wedges a lock deliberately and probes, so it measures what happens
//! to a VICTIM of a hold and never builds a holder out of ordinary state.
//!
//! So the generator below fabricates the shape the field has and the suite did
//! not: several agents, each pty-bound AND `by_pty`-mapped AND session-bound,
//! each carrying a session's worth of delivered prompts, each with a large
//! rendered tail. It is a generator rather than one test's inline setup because
//! #1702's siblings and the soak lane want the same subject.
#![allow(dead_code)]

use loomux_lib::orchestration::{Delivery, GroupId, OrchRegistry, Role};
use std::collections::HashMap;
use std::sync::Arc;

/// A dialog tail: what a CLI parked on a permission menu actually renders.
/// `prompt_wait_detected` reads this as a question.
pub const DIALOG_ENDING: &str = "Do you want to proceed?\n\u{276f} 1. Yes\n  2. No";

/// One fabricated long-lived orchestration session.
pub struct LongLivedSession {
    /// Every agent the generator spawned, in creation order.
    pub agent_ids: Vec<String>,
    /// `agent id -> pty id`, for the panes it bound.
    pub pty_of: HashMap<String, u32>,
    /// `agent id -> CLI session id`. Handed back because
    /// [`loomux_lib::orchestration::OrchRegistry::delivered_mask_lines`] takes
    /// the session from its CALLER (#1702) rather than resolving a pty through
    /// `by_pty` + `agents` itself — so a test that wants the record has to hold
    /// what an agent snapshot would have held.
    pub session_of: HashMap<String, String>,
    /// The LAST prompt line recorded against each agent's session — the one a
    /// resumed pane would be rendering, and the one still guaranteed to be in
    /// the record after the drop-oldest cap has evicted the rest.
    pub last_delivered: HashMap<String, String>,
    /// How many prompt deliveries were recorded against EACH agent's session.
    /// The whole point of the fixture: the record's size does not follow this.
    pub deliveries_per_agent: usize,
    /// `output_total` per agent, for `attention_tick`'s `outputs` argument.
    pub outputs: HashMap<String, u64>,
    /// An empty last-human-keystroke map — nobody has typed into these panes.
    pub no_input: HashMap<String, u64>,
}

/// Fabricate `agents` panes on `group`, each bound to a pty and a CLI session
/// and each carrying `deliveries_per_agent` recorded prompt deliveries.
///
/// Takes an EXISTING registry rather than building one, so the caller keeps
/// whatever agent-directory overrides its own harness applies (#464: a spawn
/// that writes into the developer's real `~/.claude/agents` is the leak those
/// overrides exist to stop, and a second construction site here is a second
/// place to forget them).
///
/// `group`'s guardrails must admit `agents` spawns; `spawn_agent` refuses past
/// `max_agents` and this returns fewer agents than asked for if it does, which
/// a caller should assert on rather than discover downstream.
pub fn fabricate_long_lived_session(
    reg: &Arc<OrchRegistry>,
    group: &GroupId,
    agents: usize,
    deliveries_per_agent: usize,
) -> LongLivedSession {
    let mut out = LongLivedSession {
        agent_ids: Vec::new(),
        pty_of: HashMap::new(),
        session_of: HashMap::new(),
        last_delivered: HashMap::new(),
        deliveries_per_agent,
        outputs: HashMap::new(),
        no_input: HashMap::new(),
    };
    for i in 0..agents {
        let name = format!("w{i}");
        let Ok(a) = reg.spawn_agent(group, Role::Worker, &name, "do work", false, None) else {
            break;
        };
        // A pty id that cannot collide with a real one in the same binary, and
        // that is stable per index so a failure names a pane.
        let pty = 9000 + i as u32;
        // Both halves, and both are load-bearing for #1702's trigger:
        // `set_session_for_test` is what makes `delivered_prompt_record` reach
        // past its first `?`, and the `bind_pane_for_test` below writes the
        // `by_pty` entry that makes `session_for_pty` reach its second lock at
        // all.
        let session = format!("00000000-0000-4000-8000-{i:012}");
        reg.set_session_for_test(&a.id, &session);
        // The SHIPPED bind (#1702 P4), not `set_pty_for_test`. The reason is
        // that the seam is a SECOND write site for the same fields, free to
        // drift out of step with the real one — a fixture built on a
        // re-implementation proves the algorithm rather than the code
        // (`.orrerix/lessons.md`) — and that it writes neither the
        // `agent-bind` audit row nor the breadcrumb a real session's log
        // carries.
        //
        // Not because the seam would leave `status` wrong: `spawn_agent`
        // already marked this agent `Running` on its no-app-handle branch, so
        // headlessly that write is redundant. See `bind_pane`'s own doc — the
        // first version of this comment claimed otherwise and scratch round j3
        // falsified it.
        reg.bind_pane_for_test(group, &a.id, pty);

        // A session's worth of deliveries. Each line is DISTINCT: the record
        // de-duplicates, so recording one line a thousand times would fabricate
        // a record of one entry and prove nothing about churn.
        let mut last = String::new();
        for d in 0..deliveries_per_agent {
            // QUESTION-SHAPED on purpose. A pane whose tail ends in a recorded
            // line that is not prompt-shaped tells you nothing: it fails to
            // flag whether or not the mask claimed it, so a fixture built that
            // way cannot distinguish a masking tick from one that has stopped
            // masking. This is #576/rev-126's actual subject — a relayed report
            // sitting in the tail, reading as a live dialog — and
            // `prompt_wait_detected` must see it as one until the record
            // claims it.
            last = format!(
                "[orch] delivery {d} to {name} reports blocked: shall I continue? (y/n)"
            );
            reg.record_delivered_prompt(pty, &last, Delivery::MidSession);
        }
        out.last_delivered.insert(a.id.clone(), last);
        out.pty_of.insert(a.id.clone(), pty);
        out.session_of.insert(a.id.clone(), session);
        out.outputs.insert(a.id.clone(), 100 + i as u64);
        out.agent_ids.push(a.id);
    }
    out
}

/// How big a fabricated session is, on every axis a long-lived orchestrator
/// session actually grows along (#1702 P4).
///
/// Every field is a COUNT rather than a duration, because nothing here can
/// make a test wait a day: what "24 hours old" means to this codebase is not
/// elapsed time, it is the state a day of orchestrating leaves behind. Each
/// field below is one growth row from the #1702 plan's size table, and the
/// mapping is stated per field so a reader can check the fixture against a
/// real session rather than take the word "realistic" on trust.
#[derive(Clone, Copy, Debug)]
pub struct SessionScale {
    /// Agents still RUNNING and driving a pane. Bounded in the field by
    /// `max_agents` (ceiling 12), so this is small by construction — and it is
    /// the axis that matters: #1702's trigger needs ONE of these, and the
    /// suite had none.
    pub live_bound: usize,
    /// Agents that have exited. Plan row 1: `mark_dead` flips a status and
    /// nothing ever removes the entry, so this grows for the life of the
    /// process and every `agents.values()` scan pays for it — the attention
    /// tick every 3 s, the watchdog every 30 s, the publisher once a second
    /// per group.
    pub dead: usize,
    /// Prompt deliveries recorded against EACH live agent's session. Plan row
    /// 2: the record is drop-oldest and capped where it is written, so this is
    /// the axis the issue's own diagnosis got wrong, and the fixture carries a
    /// field-sized value precisely so the cap is exercised rather than assumed.
    pub deliveries_per_agent: usize,
    /// Audit rows written beyond the ones the spawns, binds and deaths below
    /// write themselves. Plan row 5: `audit.jsonl` is re-read and re-parsed
    /// whole by the viewer's follow mode.
    pub audit_rows: usize,
    /// Board rows. Plan row 6: `tasks.json` is unbounded (#1472), whole-file
    /// parsed, and read by `attention_tick` once per group per 3 s.
    pub board_rows: usize,
    /// Pending `ask_human` rows. Plan row 7: bounded by `humanq::PENDING_MAX`,
    /// and in the fixture for exactly that reason — a bounded axis is a claim,
    /// and a fixture that never fills it never checks the claim.
    pub pending_questions: usize,
    /// Rendered tail per live pane, in bytes.
    pub tail_bytes: usize,
}

impl Default for SessionScale {
    /// A day-old orchestrator session, sized from the plan's §3 figures.
    ///
    /// `dead: 300` is a day of an orchestrator spawning and reaping delegates
    /// at a dozen an hour. `deliveries_per_agent: 4_000` is the pane #1702 was
    /// reported on. `pending_questions` is read off `humanq::PENDING_MAX`
    /// rather than written as a literal: a fixture that seeds a bounded axis
    /// to a number of its own starts silently under-filling the moment the
    /// bound moves.
    fn default() -> Self {
        SessionScale {
            live_bound: 6,
            dead: 300,
            deliveries_per_agent: 4_000,
            audit_rows: 4_000,
            board_rows: 400,
            pending_questions: loomux_lib::orchestration::humanq::PENDING_MAX,
            tail_bytes: 16 * 1024,
        }
    }
}

/// A fabricated day-old session: the live fleet, plus everything a day of
/// orchestrating leaves behind.
///
/// Every count below is what the registry ACTUALLY holds, read back after
/// seeding rather than copied from the [`SessionScale`] that was asked for — a
/// guardrail refusal, a cap, or a rate backstop makes those two different, and
/// a fixture that reported the request would let an assertion claim a scale it
/// never ran at.
pub struct DayOldSession {
    /// The live, pty-bound, session-bound fleet — the #1702 trigger state.
    pub live: LongLivedSession,
    /// Agent ids that were spawned, bound and then killed. Still in the
    /// `agents` map, because nothing removes them.
    pub dead_ids: Vec<String>,
    /// The scale this was asked for.
    pub scale: SessionScale,
    /// Rendered pane tail per LIVE agent, keyed by agent id — the `tails`
    /// argument `attention_tick` takes.
    pub tails: HashMap<String, String>,
    /// Board rows the group's own `tasks()` returns.
    pub board_rows: usize,
    /// Pending question rows `questions()` returns.
    pub pending_questions: usize,
    /// Audit entries the viewer's window returns, and whether that window
    /// TRUNCATED — the second is the checkable half: `true` means the log is
    /// past `AUDIT_VIEW_LIMIT` and the viewer is no longer seeing all of it,
    /// which is the state a day-old session is really in.
    pub audit_window: (usize, bool),
}

impl DayOldSession {
    /// Every agent in the roster, live and dead. The population every
    /// `agents.values()` scan walks — plan row 1's cost, made assertable.
    pub fn roster_size(&self) -> usize {
        self.live.agent_ids.len() + self.dead_ids.len()
    }
}

/// Build a registry state equivalent to a long-lived orchestration session.
///
/// **Everything here goes through a production write path.** `spawn_agent` +
/// [`loomux_lib::orchestration::OrchRegistry::bind_pane_for_test`] +
/// `mark_dead` for the roster, `record_delivered_prompt` for the record,
/// `audit` for the log, `upsert_task` for the board, `ask_human` for the
/// questions. That is the difference between a fixture and a model, and it is
/// load-bearing here rather than stylistic: #1702's tick reaches the mask only
/// for an agent that is `Running` AND pty-bound AND `by_pty`-mapped, so a
/// fixture that set those three fields itself would have been exactly as green
/// on the broken tick as on the fixed one if it got any of them wrong — which
/// is the shape of the four-beta miss this whole slice exists to close.
///
/// **What it deliberately does NOT do is make `audit.jsonl` rotate.** The log
/// rotates at 8 MB keeping one generation, and the plan's row 5 is about the
/// cost of reading two of them — but writing 16 MB through `audit()` costs
/// more wall clock than every other liveness row put together, and the
/// property L7 is about is that a TICK does not hold a registry lock while
/// something reads a file, which does not vary with how many bytes there are
/// to read. What the fixture does instead is push the log past the viewer's
/// own window and REPORT that it did (`audit_window.1`), so the size axis is a
/// checked figure rather than an unstated one.
///
/// `group`'s guardrails must admit the fleet: `max_agents` at least
/// `live_bound`, and `max_spawns_per_hour` 0 (disabled), since the rate
/// backstop counts every admitted spawn in a rolling hour and the whole
/// fixture is built inside one second. A guardrail refusal shows up as a
/// smaller `dead_ids` / `live.agent_ids` than asked for, which a caller should
/// assert on rather than discover downstream.
pub fn seed_day_old_session(
    reg: &Arc<OrchRegistry>,
    group: &GroupId,
    scale: SessionScale,
) -> DayOldSession {
    use loomux_lib::orchestration::{humanq, TaskPatch};

    // --- the dead fleet (plan row 1) --------------------------------------
    //
    // Spawned, BOUND and then killed, rather than spawned and killed: a dead
    // agent that never held a pane is a smaller subject than the real thing.
    // `mark_dead`'s own cleanup of `by_pty` and the per-pane delivery lock is
    // part of what a day-old roster has been through, and a fixture that
    // skipped the bind would never exercise it — nor leave the roster in the
    // state a real one is in, where a `Dead` entry is one whose reverse-index
    // entry has already gone.
    let mut dead_ids = Vec::new();
    for i in 0..scale.dead {
        let name = format!("d{i}");
        let Ok(a) = reg.spawn_agent(group, Role::Worker, &name, "a finished task", false, None)
        else {
            break;
        };
        reg.bind_pane_for_test(group, &a.id, DEAD_PTY_BASE + i as u32);
        reg.mark_dead(&a.id, Some(0));
        dead_ids.push(a.id);
    }

    // --- the live fleet: the #1702 trigger state --------------------------
    let live =
        fabricate_long_lived_session(reg, group, scale.live_bound, scale.deliveries_per_agent);

    // Pane 0's tail ends on the very line loomux last delivered into its
    // session — the #576 case the record exists to mask, so that pane must NOT
    // flag. Every other pane ends on a real CLI dialog and must. A fixture
    // whose panes all answer the same way cannot tell a masking tick from one
    // that has stopped masking.
    let mut tails: HashMap<String, String> = HashMap::new();
    for (i, id) in live.agent_ids.iter().enumerate() {
        let ending =
            if i == 0 { live.last_delivered[id].clone() } else { DIALOG_ENDING.to_string() };
        tails.insert(id.clone(), padded_tail(scale.tail_bytes, &ending));
    }

    // --- the board (plan row 6) -------------------------------------------
    for i in 0..scale.board_rows {
        let _ = reg.upsert_task(
            group,
            "orch-1",
            None,
            TaskPatch {
                title: Some(format!("t{i}: a task a day-old board is carrying")),
                // A realistic MIX rather than N identical rows: the readers
                // that matter filter on status (`attention_tick`'s gate rows,
                // `LIST_TASKS_DONE_CAP`), so a board of one status is a much
                // smaller subject than its row count suggests.
                status: Some(BOARD_STATUSES[i % BOARD_STATUSES.len()].to_string()),
                ..Default::default()
            },
        );
    }

    // --- pending questions (plan row 7) -----------------------------------
    for i in 0..scale.pending_questions {
        let _ = reg.ask_human(
            group,
            "orch-1",
            humanq::AskRequest {
                text: format!("question {i}: which of these two shapes do you want?"),
                allow_free_text: Some(true),
                ..Default::default()
            },
        );
    }

    // --- the audit log (plan row 5) ---------------------------------------
    for i in 0..scale.audit_rows {
        reg.audit(
            group,
            "orch-1",
            "fixture-row",
            serde_json::json!({ "n": i, "note": "one row of a day's orchestrating" }),
        );
    }

    let (entries, truncated) = reg.audit_log_windowed(group);
    let pending_questions = reg
        .questions(group)
        .map(|qs| qs.iter().filter(|q| !q.status.is_settled()).count())
        .unwrap_or(0);

    DayOldSession {
        live,
        dead_ids,
        scale,
        tails,
        board_rows: reg.tasks(group).len(),
        pending_questions,
        audit_window: (entries.len(), truncated),
    }
}

/// Pty ids for the dead fleet. Far from the live fleet's 9000-block so a
/// failure message says which fleet a pane came from, and far from any real
/// pty id in the same binary.
const DEAD_PTY_BASE: u32 = 20_000;

/// The statuses a day-old board actually carries, in roughly the proportion a
/// real one does: mostly finished, a few in flight, a couple at a gate.
const BOARD_STATUSES: [&str; 6] = ["done", "done", "done", "in-progress", "review", "pr"];

/// A rendered pane tail of at least `bytes`, ending in `ending`.
///
/// The padding is line-shaped rather than one long line, because everything
/// that reads a tail here reads it as ROWS — `mask_loomux_notices_with_record`
/// compares row by row and `prompt_wait_detected` looks at the last few
/// non-empty ones — so a single 8 KB line would be a smaller subject than it
/// looks, not a bigger one.
pub fn padded_tail(bytes: usize, ending: &str) -> String {
    let mut s = String::with_capacity(bytes + ending.len() + 2);
    let mut row = 0usize;
    while s.len() < bytes {
        s.push_str(&format!(
            "  Compiling loomux v1.2.0 (row {row}) ... building the dependency graph\n"
        ));
        row += 1;
    }
    s.push_str(ending);
    s
}

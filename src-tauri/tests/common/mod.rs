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
        // past its first `?`, and `set_pty_for_test` writes the `by_pty` entry
        // that makes `session_for_pty` reach its second lock at all.
        let session = format!("00000000-0000-4000-8000-{i:012}");
        reg.set_session_for_test(&a.id, &session);
        reg.set_pty_for_test(&a.id, pty);

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

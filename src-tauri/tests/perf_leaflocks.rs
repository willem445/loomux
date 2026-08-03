//! Leaf-lock and request-sized-read pins for the orchestration poll paths
//! (#743 S7) — the enforcement `doc/design/performance.md` INV-5 says a source
//! scan cannot give it, because no scan can see lock *scope*.
//!
//! Must be an integration test, not a unit test (CLAUDE.md constraint 4 — the
//! Windows test exe needs build.rs's comctl32-v6 manifest). Everything here
//! drives the SHIPPED functions against a real `PtyManager` backed by a real
//! ConPTY pair and a real (trivial, immediately-exiting) child, with no Tauri
//! `AppHandle` (unavailable headless) and no real agent CLI (constraint 3).
//!
//! The technique is `ptywrite.rs`'s, inverted onto the read side: a pane whose
//! output ring is HELD by the test makes every `output_tail` of that pane park
//! *inside* the read — deterministically, with no sleep and no race against how
//! fast a 256 KiB clone happens to run on the host. The question each lock test
//! then asks is the one the invariant is actually about: **while one pane's
//! ring is being read, does everything else still move?**
//!
//! Reverting either lock fix (holding the `agents` guard across the pty reads
//! again) reds the two `does_not_hold_the_agents_lock` tests by timeout, which
//! is the shape the defect has. Reverting the statusline bound (a whole-ring
//! `output_tail` instead of `output_tail_bounded`) reds the bounded-read test
//! on the value it produces.

use loomux_lib::orchestration::{statusline_cost, Guardrails, OrchRegistry, Role};
use loomux_lib::pty::PtyManager;
use std::sync::{mpsc, Arc};
use std::time::Duration;

/// Long enough that a loaded CI runner never trips it, short enough that a real
/// regression fails the job rather than hanging it. Every use is a "did this
/// make progress at all" question, not a latency measurement — the fix moves
/// the wait from unbounded to nothing, so there is no near-miss to tune around.
const GRACE: Duration = Duration::from_secs(10);

/// How long a parked read is given to *be* parked before the property is
/// probed. It is asserted as a NEGATIVE (`recv_timeout` must time out), so this
/// is not a guess about scheduling: a call that has not returned after 300 ms,
/// when its unparked cost is microseconds, is parked at the one place in it
/// that can block — the ring we are holding. Same idiom, same duration, as
/// `ptywrite.rs`'s "the write has not completed" check.
const SETTLE: Duration = Duration::from_millis(300);

/// Run `f` on its own thread; report whether it finished within `t`.
///
/// A `false` here means the call is still blocked. The thread is left parked
/// deliberately: it is waiting on a lock this test still holds, and the harness
/// exits the process when the run ends.
fn completes_within<T: Send + 'static>(t: Duration, f: impl FnOnce() -> T + Send + 'static) -> bool {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(t).is_ok()
}

fn rails() -> Guardrails {
    Guardrails {
        max_agents: 4,
        agent_cli: "claude".into(),
        auto_ops: false,
        advanced_orchestrator: false,
        idle_kill_minutes: 0,
        max_spawns_per_hour: 0,
        watchdog_stall_minutes: 0,
        ..Guardrails::default()
    }
}

/// #464: every registry construction in this file goes through here, so no
/// spawn can write a generated custom-agent file into the developer's REAL
/// `~/.claude/agents` or `~/.copilot/agents`.
fn test_registry() -> (Arc<OrchRegistry>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let reg = OrchRegistry::new(dir.path().to_path_buf());
    reg.set_port(45999);
    reg.set_claude_agents_dir_override(dir.path().join("claude-agents"));
    reg.set_copilot_agents_dir_override(dir.path().join("copilot-agents"));
    reg.set_compact_hook_dir_override(dir.path().join("compacthook"));
    reg.set_copilot_hooks_dir_override(dir.path().join("copilot-hooks"));
    (Arc::new(reg), dir)
}

/// One group, one worker, bound to `pty_id`. Returns the agent id.
fn agent_on_pane(reg: &OrchRegistry, pty_id: u32) -> String {
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "t", false, None).unwrap();
    reg.set_pty_for_test(&w.id, pty_id);
    w.id
}

// ---------- INV-5: the `agents` lock is a leaf on the compact-nudge cadence ----------

#[test]
fn the_compact_signal_tail_read_does_not_hold_the_agents_lock() {
    const PANE: u32 = 74301;
    let pm = Arc::new(PtyManager::default());
    pm.register_fake_for_test(PANE, b"a pane tail\n");
    let (reg, _d) = test_registry();
    let agent_id = agent_on_pane(&reg, PANE);

    // Hold this pane's output ring: every `output_tail` of it now parks inside
    // the read. This is what a genuinely slow pane looks like from the caller's
    // side, made deterministic.
    let ring = pm.output_ring_for_test(PANE).expect("pane is registered");
    let _wedge = ring.lock().unwrap();

    let (tx, rx) = mpsc::channel();
    {
        let reg = reg.clone();
        let pm = pm.clone();
        std::thread::spawn(move || {
            let _ = tx.send(reg.compact_signals_from(&pm));
        });
    }
    assert!(
        rx.recv_timeout(SETTLE).is_err(),
        "setup: the compact-signal read did not park on the held ring — it cannot have \
         reached the tail read, so the assertion below would prove nothing"
    );

    // THE PROPERTY. The `agents` lock must have been snapshotted and released
    // before the pty read, so a parked read holds nothing but the pane itself.
    // Every reader of the registry is behind this — `agent()` is what the MCP
    // handlers, the attention scan and the usage poll all call.
    let probe = reg.clone();
    assert!(
        completes_within(GRACE, move || probe.agent(&agent_id)),
        "the `agents` lock is held across the compact-nudge tail read: one slow pane stalls \
         every reader of the agent registry (#743 S7, performance.md INV-5)"
    );
}

#[test]
fn the_output_total_read_does_not_hold_the_agents_lock() {
    const PANE: u32 = 74302;
    let pm = Arc::new(PtyManager::default());
    pm.register_fake_for_test(PANE, b"a pane tail\n");
    let (reg, _d) = test_registry();
    let agent_id = agent_on_pane(&reg, PANE);

    let ring = pm.output_ring_for_test(PANE).expect("pane is registered");
    let _wedge = ring.lock().unwrap();

    let (tx, rx) = mpsc::channel();
    {
        let reg = reg.clone();
        let pm = pm.clone();
        std::thread::spawn(move || {
            let _ = tx.send(reg.output_totals_from(&pm));
        });
    }
    assert!(
        rx.recv_timeout(SETTLE).is_err(),
        "setup: the output-total read did not park on the held ring"
    );

    let probe = reg.clone();
    assert!(
        completes_within(GRACE, move || probe.agent(&agent_id)),
        "the `agents` lock is held across the watchdog's pty counter read — the sibling of \
         the compact-signal read, on the same wake (#743 S7, performance.md INV-5)"
    );
}

#[test]
fn the_compact_signal_read_still_reports_every_pane_it_used_to() {
    // The lock move must not change WHAT the cadence sees: same agents, same
    // ANSI-stripped tails. A leaf-lock fix that quietly dropped a pane would
    // disarm the manual-`/compact` detector rather than speed it up.
    const PANE: u32 = 74303;
    let pm = Arc::new(PtyManager::default());
    pm.register_fake_for_test(PANE, b"\x1b[32mthe human typed\x1b[0m /compact\r\n");
    let (reg, _d) = test_registry();
    let agent_id = agent_on_pane(&reg, PANE);

    let signals = reg.compact_signals_from(&pm);

    let (tail, _last_input) = signals.get(&agent_id).expect("the bound pane must be reported");
    assert!(
        tail.contains("the human typed /compact"),
        "the tail must arrive ANSI-stripped, exactly as the detectors read it: {tail:?}"
    );
    assert_eq!(signals.len(), 1, "only the pane-bound agent is reported: {signals:?}");
}

// ---------- INV-5: request-sized reads on the usage poll ----------

/// Filler with no `$` in it, so only the planted figure can ever be parsed.
fn filler(bytes: usize) -> Vec<u8> {
    let line = b"streaming output with no cost figure on it at all\r\n";
    line.iter().copied().cycle().take(bytes).collect()
}

#[test]
fn the_statusline_read_is_bounded_to_what_the_pane_is_showing() {
    // The usage poll (`orch_group_usage`, 2 s + 4 s per group-bound view, per
    // agent) took the WHOLE ≤256 KiB ring and ANSI-stripped it to find one
    // dollar figure that is by construction the last thing painted. A figure
    // buried 128 KiB back is not on screen and is not what the CLI is
    // reporting now — reading that far is cost with no consumer.
    const PANE: u32 = 74304;
    let pm = PtyManager::default();
    let mut ring = b"session cost so far: $9.99\r\n".to_vec();
    ring.extend_from_slice(&filler(128 * 1024));
    pm.register_fake_for_test(PANE, &ring);

    assert_eq!(
        statusline_cost(&pm, PANE),
        None,
        "the statusline read must be request-sized: a figure 128 KiB behind the visible \
         tail is scrollback, not a statusline (#743 S7, performance.md INV-5)"
    );
}

#[test]
fn the_statusline_read_still_finds_the_figure_the_pane_is_showing() {
    // The other half of the bound, and the one that matters for correctness:
    // narrowing the window must not stop the poll seeing the live statusline.
    const PANE: u32 = 74305;
    let pm = PtyManager::default();
    let mut ring = filler(128 * 1024);
    ring.extend_from_slice(b"\x1b[2m total cost: $1.23 \x1b[0m\r\n");
    pm.register_fake_for_test(PANE, &ring);

    assert_eq!(
        statusline_cost(&pm, PANE),
        Some(1.23),
        "the figure the CLI is painting right now must still be read"
    );
}

// ---------- INV-5: marker IO out from under the toggle guards ----------

#[test]
fn the_notify_toggle_does_its_io_once_per_actual_transition() {
    // #743 S7 moved the marker write and the audit append out from under the
    // `notify_groups` guard (`pause_group`'s shape, in the same file). The
    // guard's `insert`/`remove` is what still decides that the IO happens at
    // all, so this pins the property the move must not lose: repeating a toggle
    // that changes nothing writes nothing and audits nothing.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let marker = reg.state_root().join(&g.id).join("notify");

    assert!(!reg.notify_enabled(&g.id), "off is the default");
    reg.set_notify(&g.id, true).unwrap();
    reg.set_notify(&g.id, true).unwrap();
    assert!(reg.notify_enabled(&g.id));
    assert!(marker.exists(), "the durable marker must survive a restart");
    assert_eq!(
        reg.audit_log(&g.id).iter().filter(|e| e.action == "notify-on").count(),
        1,
        "a repeat of a no-op toggle must not audit a second transition"
    );

    reg.set_notify(&g.id, false).unwrap();
    reg.set_notify(&g.id, false).unwrap();
    assert!(!reg.notify_enabled(&g.id));
    assert!(!marker.exists(), "disabling must remove the marker, not just the memory");
    assert_eq!(
        reg.audit_log(&g.id).iter().filter(|e| e.action == "notify-off").count(),
        1
    );
}

#[test]
fn concurrent_notify_toggles_never_leave_the_marker_disagreeing_with_memory() {
    // rev-231 N1. The marker file is LOAD-TIME TRUTH — it is what rebuilds the
    // in-memory set at startup — so a set mutation and its file write that can
    // interleave do not merely race for an instant: the file can end up saying
    // ON while memory says OFF, and the next restart believes the file. That is
    // a divergence which survives the process, which is why `marker_io` orders
    // the toggle structurally instead of the code resting on both callers
    // happening to be sync commands on the webview thread.
    //
    // Honest about what this is: a **deterministic green, a probabilistic red**.
    // With the ordering lock the invariant cannot be violated, so this never
    // flakes; without it the interleave has to be hit, so it would not fail
    // every run. It is a regression guard, not the red-before-green evidence —
    // the argument in `marker_io`'s doc is the primary defence, and this test
    // is what stops a future edit quietly removing it.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let marker = reg.state_root().join(&g.id).join("notify");

    let threads: Vec<_> = (0..8)
        .map(|t| {
            let reg = reg.clone();
            let group = g.id.clone();
            std::thread::spawn(move || {
                for i in 0..25 {
                    reg.set_notify(&group, (t + i) % 2 == 0).unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().expect("toggle thread");
    }

    // Whatever the last toggle was, the durable answer and the live answer are
    // the same answer. They are read by different things — the marker by the
    // next process, the set by this one — and a user who turns notifications
    // off must not find them on after a restart.
    assert_eq!(
        marker.exists(),
        reg.notify_enabled(&g.id),
        "the notify marker and the in-memory set disagree: a restart would silently \
         resurrect the setting the human last turned off (#743 S7 rev-231 N1)"
    );
}

#[test]
fn a_pane_that_is_gone_reports_no_statusline_cost() {
    // The fail-safe direction, unchanged by the bound: no pane, no figure —
    // never a stale or fabricated one.
    let pm = PtyManager::default();
    assert_eq!(statusline_cost(&pm, 74306), None);
}

//! The answer a MUTATING MCP tool's caller gets when its helper thread DIED
//! (#1702).
//!
//! An integration test rather than an inline one because it links the lib
//! (CLAUDE.md constraint 4). It drives `mcp::await_mutate_result` — the seam
//! `dispatch_bounded` itself calls, so this proves the shipped decision rather
//! than a re-implementation of it — with a channel whose sender is dropped,
//! which is exactly the state a panicked helper thread leaves behind.
//!
//! Why the seam exists at all: the wait is inseparable from the thread that
//! needs a live `Arc<OrchRegistry>`, but the DECISION — which of three answers
//! a caller gets — needs neither a registry nor a thread, and welding it into
//! the spawn site is what left the `Disconnected` arm untested and answering
//! "it WILL complete" about work that had already failed.

use loomux_lib::orchestration::mcp;
use loomux_lib::orchestration::GroupId;
use serde_json::{json, Value};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn group() -> GroupId {
    GroupId::parse("mcpmutate-g1").expect("a valid group id")
}

/// The `text` of a tool answer, and whether it is flagged as an error.
fn answer(v: &Value) -> (String, bool) {
    let text = v["content"][0]["text"].as_str().unwrap_or_default().to_string();
    (text, v["isError"].as_bool().unwrap_or(false))
}

#[test]
fn a_completed_mutating_tool_is_answered_with_its_own_result() {
    // The discriminating half, and it goes first: without it every assertion
    // below would pass just as well against a seam that answered `isError` to
    // everything, which would be a seam nobody could ship.
    let (tx, rx) = mpsc::channel();
    tx.send(Ok(json!({ "content": [{ "type": "text", "text": "done" }] }))).unwrap();
    let out = mcp::await_mutate_result(&rx, "upsert_task", Duration::from_secs(5), &group(), "w-1")
        .expect("a completed tool's own Ok must come back");
    let (text, is_error) = answer(&out);
    assert_eq!(text, "done", "the tool's own result must be untouched");
    assert!(!is_error, "a completed tool is not an error");
}

#[test]
fn a_slow_mutating_tool_is_told_it_is_running_and_not_to_re_issue() {
    // The `Timeout` arm, still a DIFFERENCE from the `Disconnected` arm below
    // rather than the only behaviour this seam has. `_tx` is held so the
    // channel is open and empty, which is what a tool that is genuinely still
    // running looks like.
    //
    // **This test used to ENFORCE the claim #1702 retracted** (#1713 review
    // N7). The at-most-once sweep corrected seven doc surfaces and did not
    // reach the message itself, so the function's doc said a tool "can now
    // fail to complete at all" while the string it returned still said "It
    // WILL complete" — and this assertion held that string in place, which is
    // CLAUDE.md's "a TEST is one of those surfaces" exactly: correcting the
    // claim reddens a test whose assertion quotes it, and the pressure is to
    // revert the fix rather than the pin.
    //
    // What survives is the load-bearing half: do NOT re-issue, because a
    // second call runs a non-idempotent tool twice. That is at-most-once,
    // which is true. The completion promise is gone, and the negative below
    // is what stops it coming back.
    let (_tx, rx) = mpsc::channel::<Result<Value, (i64, String)>>();
    let out =
        mcp::await_mutate_result(&rx, "spawn_agent", Duration::from_millis(50), &group(), "w-1")
            .expect("the busy answer is a tool result, not a protocol error");
    let (text, is_error) = answer(&out);
    assert!(is_error, "a caller must be able to see this is not the tool's answer");
    assert!(text.contains("still executing"), "{text}");
    assert!(text.contains("do NOT re-issue it"), "the anti-double-execution half: {text}");
    assert!(text.contains("run it twice"), "and its reason: {text}");
    assert!(text.contains("list_agents"), "the read tool to verify with: {text}");
}

#[test]
fn the_slow_tool_message_no_longer_promises_completion() {
    // **Split from the row above deliberately** (#1713 review N7 evidence). A
    // red evidences only the assertion it REACHED: with the negative sitting
    // last in that test, restoring the pre-#1702 wording reddened on
    // `run it twice` — the positive — and the negative, which is the pin N7 is
    // actually about, was never exercised. One neuter now reddens BOTH rows,
    // because the old message lacks "run it twice" AND carries the promise.
    //
    // This is the one assertion that stops the retracted claim coming back,
    // and it is the reason this row exists rather than a `contains` on the new
    // wording: a rewording that dropped the promise by accident would satisfy
    // the positives, and only this says the promise itself is gone.
    let (_tx, rx) = mpsc::channel::<Result<Value, (i64, String)>>();
    let out =
        mcp::await_mutate_result(&rx, "spawn_agent", Duration::from_millis(50), &group(), "w-1")
            .expect("the busy answer is a tool result, not a protocol error");
    let (text, _) = answer(&out);
    assert!(
        !text.contains("WILL complete"),
        "the retracted completion promise came back: {text}"
    );
}

#[test]
fn the_slow_tool_message_is_one_paragraph() {
    // `still_executing_text` had no shape pin, which is how a `\n` escape got
    // into it while N7 was being fixed — caught by `od`, not by the suite.
    // Pinned on the VALUE beside the content, like its died-helper sibling.
    let (_tx, rx) = mpsc::channel::<Result<Value, (i64, String)>>();
    let out =
        mcp::await_mutate_result(&rx, "upsert_task", Duration::from_millis(50), &group(), "w-1")
            .expect("the busy answer is a tool result");
    let (text, _) = answer(&out);
    assert!(!text.contains('\n'), "a user-facing message is one paragraph: {text:?}");
    assert!(!text.contains("          "), "a source indent leaked into the message: {text:?}");
    assert!(text.contains("still executing"), "and it is the slow-tool message: {text}");
}

#[test]
fn a_died_mutating_tool_answers_at_once_instead_of_waiting_out_the_deadline() {
    // The #1702 arm. Dropping the sender without sending is precisely what a
    // helper thread that panicked leaves behind — and a re-entrant `lock_safe`
    // on a mutate helper thread now panics rather than parking, which is what
    // makes this reachable rather than theoretical.
    let (tx, rx) = mpsc::channel::<Result<Value, (i64, String)>>();
    drop(tx);

    // A deadline 60x the answer this must give. If the `Disconnected` arm is
    // removed, `recv_timeout` still returns — with `Disconnected`, immediately
    // — so the timing assertion alone would NOT discriminate; what separates
    // the two implementations is the TEXT, and both are asserted.
    let started = Instant::now();
    let out = mcp::await_mutate_result(&rx, "upsert_task", Duration::from_secs(30), &group(), "w-1")
        .expect("a died helper is answered as a tool result, not a protocol error");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the answer must not wait out the mutate deadline: took {:?}",
        started.elapsed()
    );

    let (text, is_error) = answer(&out);
    assert!(is_error, "a caller must be able to see this is not the tool's answer");
    assert!(text.contains("internal error"), "{text}");
    // The one thing this answer may NOT say. The tool panicked at an unknown
    // point, so "nothing was executed" — which is what every `loomux busy:`
    // answer says, and what the old `still executing` text implied by telling
    // the agent it would finish — is the claim that cannot be made.
    assert!(!text.contains("WILL complete"), "a died tool will not complete: {text}");
    assert!(!text.contains("still executing"), "a died tool is not still executing: {text}");
    assert!(text.contains("may have partially executed"), "{text}");
    assert!(text.contains("list_tasks"), "the read tool to verify with: {text}");
}

#[test]
fn a_died_tool_with_no_read_tool_says_verify_without_naming_one() {
    // `verify_with` has no honest answer for every tool, and the fallback must
    // not name one that would not tell the caller anything.
    // `message_orchestrator` is a real registered tool, classified Mutate by
    // `tool_kind`'s default arm, with no row in `verify_with` — chosen over an
    // invented name because a dead row classifies nothing (#1609 review N2).
    let (tx, rx) = mpsc::channel::<Result<Value, (i64, String)>>();
    drop(tx);
    let tool = "message_orchestrator";
    let out = mcp::await_mutate_result(&rx, tool, Duration::from_secs(30), &group(), "w-1")
        .expect("a died helper is answered as a tool result");
    let (text, _) = answer(&out);
    // **This must be the DIED answer's fallback, not any answer's.** Both
    // messages route through the same `verify_with` miss, so the two clauses
    // below are shared between them and hold under an implementation that
    // never distinguishes a died helper at all — measured, not feared: scratch
    // round 2 collapsed both `recv_timeout` arms back to `still_executing_text`
    // and this test stayed GREEN while its sibling reddened. Two candidate
    // outputs have to DIVERGE for a fixture to witness anything, so the
    // identity of the answer is pinned first and the fallback second.
    assert!(text.contains("internal error"), "this must be the died answer: {text}");
    assert!(!text.contains("still executing"), "a died tool is not still executing: {text}");
    assert!(text.contains("verify before re-issuing"), "{text}");
    assert!(!text.contains("verify with"), "no read tool may be named here: {text}");
}

#[test]
fn the_died_answer_is_one_paragraph() {
    // A user-facing message reaches an agent's context as one blob; a `\n` plus
    // the source's indentation ships that indentation to the reader, and a
    // collapsed line-continuation leaves the run of spaces with no newline at
    // all. Both shapes are pinned, not just the content (CLAUDE.md).
    let (tx, rx) = mpsc::channel::<Result<Value, (i64, String)>>();
    drop(tx);
    let out = mcp::await_mutate_result(&rx, "upsert_task", Duration::from_secs(30), &group(), "w-1")
        .expect("a died helper is answered as a tool result");
    let (text, _) = answer(&out);
    // Pinned as the DIED answer's shape, for the reason the test above states:
    // the one-paragraph property is shared with `still_executing_text`, so
    // without this line the assertions below hold under an implementation that
    // never produces this message at all.
    assert!(text.contains("internal error"), "this must be the died answer: {text}");
    assert!(!text.contains('\n'), "a user-facing message is one paragraph: {text:?}");
    assert!(!text.contains("          "), "a source indent leaked into the message: {text:?}");
}

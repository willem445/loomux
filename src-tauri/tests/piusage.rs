//! pi usage snapshot (#2126 slice P3): the pi line folder
//! (`usage::parse_pi_transcript`), the incremental cursor generalised over a
//! per-CLI folder (`usage::TranscriptKind`), the on-disk reader
//! (`usage::pi_session_usage_in`), and the `compute_usage_snapshot` arm that
//! decides a pi agent's usage comes from the group's own pi store.
//!
//! An integration test, not inline `#[cfg(test)]`, per repo constraint #4 — a
//! unit-test binary linking the full lib misses the comctl32-v6 manifest
//! `build.rs` only embeds for integration-test targets.
//!
//! **No pi is ever run** (constraint 3). Every fixture below is built here from
//! pi's own published TypeScript types at the 0.84.4 install the plan pins
//! (`@earendil-works/pi-coding-agent`):
//!
//! - `dist/core/session-manager.d.ts` — `SessionHeader`, `SessionMessageEntry`,
//!   `ModelChangeEntry`, `CompactionEntry` (`usage?: Usage`),
//!   `BranchSummaryEntry` (`usage?: Usage`), `CustomEntry`, `LabelEntry`;
//! - `@earendil-works/pi-ai` `dist/types.d.ts` — `AssistantMessage`
//!   (`provider`, `model`, `usage`), `ToolResultMessage` (`usage?`), and
//!   `interface Usage` itself, whose `reasoning` field is documented *"a subset
//!   of `output`: `output` already includes these tokens"*.
//!
//! If pi ever changes those shapes these tests keep passing against a record
//! that no longer exists — which is exactly why the production fold contributes
//! nothing for a line it does not recognise rather than trusting the keys to be
//! there.

use loomux_lib::orchestration::{workflow, Guardrails, OrchRegistry, Role};
use loomux_lib::usage::{
    self, parse_pi_transcript, pi_session_usage_in, TranscriptCursors, TranscriptKind,
};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A loomux-minted session id, the shape `--session-id` gets (`CliCaps::
/// premints_session_id` is `true` for pi, so a pane carries one from spawn).
const SES: &str = "01a0653b-ae92-725e-b76e-ef04ce46d94b";

// ---------------------------------------------------------------------------
// Fixture builders — pi's record shapes, from the types cited in the module doc
// ---------------------------------------------------------------------------

/// One turn's numbers. `reasoning` and `cost` are `Option` because pi's `Usage`
/// declares `reasoning?` and a `cost` object that a provider without a price
/// table can leave unpopulated.
#[derive(Clone, Copy)]
struct Turn {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    reasoning: Option<u64>,
    cost: Option<f64>,
}

impl Default for Turn {
    fn default() -> Self {
        Turn { input: 0, output: 0, cache_read: 0, cache_write: 0, reasoning: None, cost: None }
    }
}

/// pi's `Usage` object. `totalTokens` is written the way pi computes it —
/// `input + output + cacheRead + cacheWrite`, with `reasoning` deliberately
/// outside the sum — so a fixture cannot quietly disagree with the vendor about
/// what a total is.
fn usage_json(t: &Turn) -> Value {
    let mut u = json!({
        "input": t.input,
        "output": t.output,
        "cacheRead": t.cache_read,
        "cacheWrite": t.cache_write,
        "totalTokens": t.input + t.output + t.cache_read + t.cache_write,
    });
    if let Some(r) = t.reasoning {
        u["reasoning"] = json!(r);
    }
    if let Some(c) = t.cost {
        // pi's `calculateCost` prices input/output/cacheRead/cacheWrite and
        // sums them into `total`; only `total` is read by the fold, so the
        // per-bucket split here is illustrative and `total` is the fact.
        u["cost"] = json!({
            "input": c, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": c,
        });
    }
    u
}

fn entry(id: &str, parent: Option<&str>, ty: &str) -> Value {
    json!({
        "type": ty,
        "id": id,
        "parentId": parent,
        "timestamp": "2026-09-03T03:06:45.266Z",
    })
}

fn assistant(id: &str, provider: &str, model: &str, t: Turn) -> String {
    let mut e = entry(id, None, "message");
    e["message"] = json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": "ok" }],
        "api": "openai-completions",
        "provider": provider,
        "model": model,
        "usage": usage_json(&t),
        "stopReason": "stop",
        "timestamp": 1_772_000_000_000u64,
    });
    e.to_string()
}

/// A `toolResult` whose optional `usage` is present — a tool that called a
/// model of its own.
fn tool_result_with_usage(id: &str, t: Turn) -> String {
    let mut e = entry(id, None, "message");
    e["message"] = json!({
        "role": "toolResult",
        "toolCallId": "tc-1",
        "toolName": "websearch",
        "content": [{ "type": "text", "text": "result" }],
        "isError": false,
        "usage": usage_json(&t),
        "timestamp": 1_772_000_000_000u64,
    });
    e.to_string()
}

/// The ordinary `toolResult`, which carries no `usage` at all.
fn tool_result(id: &str) -> String {
    let mut e = entry(id, None, "message");
    e["message"] = json!({
        "role": "toolResult",
        "toolCallId": "tc-1",
        "toolName": "read",
        "content": [{ "type": "text", "text": "file body" }],
        "isError": false,
        "timestamp": 1_772_000_000_000u64,
    });
    e.to_string()
}

fn user(id: &str, text: &str) -> String {
    let mut e = entry(id, None, "message");
    e["message"] = json!({
        "role": "user",
        "content": [{ "type": "text", "text": text }],
        "timestamp": 1_772_000_000_000u64,
    });
    e.to_string()
}

fn compaction(id: &str, t: Option<Turn>) -> String {
    let mut e = entry(id, None, "compaction");
    e["summary"] = json!("…so far");
    e["firstKeptEntryId"] = json!("e1");
    e["tokensBefore"] = json!(120_000);
    if let Some(t) = t {
        e["usage"] = usage_json(&t);
    }
    e.to_string()
}

fn branch_summary(id: &str, t: Option<Turn>) -> String {
    let mut e = entry(id, None, "branch_summary");
    e["fromId"] = json!("e1");
    e["summary"] = json!("that branch tried X");
    if let Some(t) = t {
        e["usage"] = usage_json(&t);
    }
    e.to_string()
}

fn header(id: &str, cwd: &str) -> String {
    json!({ "type": "session", "version": 3, "id": id, "timestamp": "2026-09-03T03:06:45.266Z", "cwd": cwd })
        .to_string()
}

fn model_change(id: &str, provider: &str, model_id: &str) -> String {
    let mut e = entry(id, None, "model_change");
    e["provider"] = json!(provider);
    e["modelId"] = json!(model_id);
    e.to_string()
}

fn custom(id: &str) -> String {
    let mut e = entry(id, None, "custom");
    e["customType"] = json!("some-extension");
    e["data"] = json!({ "anything": 1 });
    e.to_string()
}

fn label(id: &str) -> String {
    let mut e = entry(id, None, "label");
    e["targetId"] = json!("e1");
    e["label"] = json!("checkpoint");
    e.to_string()
}

/// A whole session file: LF-joined with a trailing newline, the way a JSONL
/// writer that has finished a record leaves it.
fn file(lines: &[String]) -> String {
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

/// Write a pi session file for `id` into `dir`, under the `<timestamp>_<id>`
/// name pi gives it.
fn write_session(dir: &Path, id: &str, text: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join(format!("2026-09-03T03-06-45-266Z_{id}.jsonl"));
    fs::write(&path, text).unwrap();
    path
}

// ---------------------------------------------------------------------------
// The fold: which records carry spend
// ---------------------------------------------------------------------------

#[test]
fn every_kind_of_pi_record_that_carries_usage_is_summed_into_one_total() {
    // Two assistant turns, a tool result that spent on a model of its own, a
    // compaction and a branch summary — pi's four `usage`-carrying shapes.
    let text = file(&[
        header(SES, "C:/tmp/repo"),
        user("e1", "go"),
        assistant("e2", "openrouter", "z-ai/glm-5.3-flash", Turn {
            input: 1000, output: 100, cache_read: 20, cache_write: 5, cost: Some(0.01),
            ..Turn::default()
        }),
        tool_result("e3"),
        assistant("e4", "openrouter", "z-ai/glm-5.3-flash", Turn {
            input: 2000, output: 200, cache_read: 40, cache_write: 10, cost: Some(0.02),
            ..Turn::default()
        }),
        tool_result_with_usage("e5", Turn { input: 7, output: 3, cost: Some(0.001), ..Turn::default() }),
        compaction("e6", Some(Turn { input: 300, output: 30, cost: Some(0.003), ..Turn::default() })),
        branch_summary("e7", Some(Turn { input: 500, output: 50, cost: Some(0.005), ..Turn::default() })),
    ]);

    let u = parse_pi_transcript(&text);
    assert_eq!(u.tokens.input_tokens, 1000 + 2000 + 7 + 300 + 500);
    assert_eq!(u.tokens.output_tokens, 100 + 200 + 3 + 30 + 50);
    assert_eq!(u.tokens.cache_read_tokens, 20 + 40);
    assert_eq!(u.tokens.cache_creation_tokens, 5 + 10);
    // Reported by pi, summed here — never derived from `price_for`.
    let cost = u.cost_usd.expect("every turn carried a cost.total");
    assert!((cost - 0.039).abs() < 1e-9, "cost was {cost}");
    assert_eq!(u.model.as_deref(), Some("openrouter/z-ai/glm-5.3-flash"));
}

#[test]
fn a_branch_the_leaf_has_navigated_away_from_still_counts_because_the_tokens_were_bought() {
    // pi's session file is a TREE. `branch_summary` marks a branch the current
    // leaf left behind, and the assistant turns on that branch stay in the
    // file. They were paid for either way, so the fold is over the FILE, not
    // over the active path — the discriminating fixture is one where the two
    // answers DIFFER, which is why the abandoned branch's turn is 900 tokens
    // rather than zero.
    let text = file(&[
        header(SES, "C:/tmp/repo"),
        assistant("e1", "anthropic", "claude-opus-4-8", Turn { input: 100, cost: Some(0.001), ..Turn::default() }),
        // the abandoned branch
        assistant("e2", "anthropic", "claude-opus-4-8", Turn { input: 900, cost: Some(0.009), ..Turn::default() }),
        branch_summary("e3", None),
        // back on the trunk
        assistant("e4", "anthropic", "claude-opus-4-8", Turn { input: 50, cost: Some(0.0005), ..Turn::default() }),
    ]);

    let u = parse_pi_transcript(&text);
    assert_eq!(u.tokens.input_tokens, 1050, "the abandoned branch's spend is still spend");
    assert_ne!(u.tokens.input_tokens, 150, "…and is not what an active-path fold would report");
}

#[test]
fn reasoning_tokens_are_not_added_to_output_because_pi_already_counts_them_inside_it() {
    // The mapping decision this arm most easily gets wrong, and the one that
    // differs from opencode's: pi's `Usage` documents `reasoning` as "a subset
    // of `output`: `output` already includes these tokens", and its own
    // `totalTokens` and `calculateCost` both price `output` alone. Folding it
    // in the way `opencode_session_usage` folds ITS reasoning bucket would
    // double-count.
    //
    // Non-vacuous by construction: `reasoning` is non-zero and DIFFERENT from
    // `output`, so an implementation that added it would report 240 here.
    let t = Turn { input: 500, output: 140, reasoning: Some(100), cost: Some(0.01), ..Turn::default() };
    let text = file(&[assistant("e1", "anthropic", "claude-opus-4-8", t)]);

    let u = parse_pi_transcript(&text);
    assert_eq!(u.tokens.output_tokens, 140, "output is pi's `output`, untouched");
    assert_ne!(u.tokens.output_tokens, 240, "a fold of `reasoning` would double-count it");
    // The four buckets must agree with pi's own `totalTokens` for the same
    // turn — the invariant the fixture builder writes and this asserts back.
    assert_eq!(u.tokens.total(), 640);
}

#[test]
fn cache_write_and_cache_read_land_in_loomuxs_two_cache_buckets_and_not_in_each_others() {
    // `cacheWrite` -> `cache_creation_tokens` is a rename across two vendors'
    // names for one quantity. The fixture makes the two numbers DIFFERENT so a
    // swapped mapping cannot pass.
    let t = Turn { cache_read: 900, cache_write: 11, cost: Some(0.0), ..Turn::default() };
    let u = parse_pi_transcript(&file(&[assistant("e1", "anthropic", "claude-opus-4-8", t)]));
    assert_eq!(u.tokens.cache_creation_tokens, 11);
    assert_eq!(u.tokens.cache_read_tokens, 900);
}

#[test]
fn the_model_is_the_last_assistant_turn_not_the_one_that_spent_the_most() {
    // claude's fold names the best-priced-by-output model because that is what
    // its dollar ESTIMATE was computed against. pi reports its own dollars, so
    // this field answers a different question — "which model is this pane on" —
    // and the fixture is built so the two answers disagree: the big spender is
    // first and a cheaper model runs last.
    let text = file(&[
        header(SES, "C:/tmp/repo"),
        assistant("e1", "anthropic", "claude-opus-4-8", Turn { output: 5000, cost: Some(1.0), ..Turn::default() }),
        model_change("e2", "openrouter", "z-ai/glm-5.3-flash"),
        assistant("e3", "openrouter", "z-ai/glm-5.3-flash", Turn { output: 3, cost: Some(0.0001), ..Turn::default() }),
    ]);

    let u = parse_pi_transcript(&text);
    assert_eq!(u.model.as_deref(), Some("openrouter/z-ai/glm-5.3-flash"));
    assert_ne!(u.model.as_deref(), Some("anthropic/claude-opus-4-8"));
}

#[test]
fn an_errored_turn_still_names_the_model_even_though_it_spent_nothing() {
    // pi writes an assistant entry with an all-zero `usage` and an
    // `errorMessage` when a turn fails. The pane is still ON that model, which
    // is what this field is for.
    let text = file(&[
        header(SES, "C:/tmp/repo"),
        assistant("e1", "moonshotai", "kimi-k2.6", Turn::default()),
    ]);
    let u = parse_pi_transcript(&text);
    assert_eq!(u.model.as_deref(), Some("moonshotai/kimi-k2.6"));
    assert_eq!(u.tokens.total(), 0);
}

#[test]
fn a_free_models_zero_cost_is_reported_as_zero_and_a_priceless_turn_leaves_it_unknown() {
    // The same distinction `opencodeusage.rs` pins: `Some(0.0)` is a real
    // answer, `None` is "pi never priced this".
    let free = parse_pi_transcript(&file(&[assistant(
        "e1", "openrouter", "z-ai/glm-5.3-flash",
        Turn { input: 10, output: 2, cost: Some(0.0), ..Turn::default() },
    )]));
    assert_eq!(free.cost_usd, Some(0.0), "a free model really did cost zero");
    assert_eq!(free.tokens.total(), 12);

    let unpriced = parse_pi_transcript(&file(&[assistant(
        "e1", "openrouter", "z-ai/glm-5.3-flash",
        Turn { input: 10, output: 2, cost: None, ..Turn::default() },
    )]));
    assert_eq!(unpriced.cost_usd, None, "no `cost` object is not a zero dollar figure");
    assert_eq!(unpriced.tokens.total(), 12, "…and the tokens are still exact");
}

#[test]
fn records_that_carry_no_usage_contribute_nothing_and_do_not_stop_the_fold() {
    // Positive control first: the two spending turns really are seen, so the
    // zeros below are "these records contributed nothing" and not "the fold
    // never ran".
    let noise = [
        header(SES, "C:/tmp/repo"),
        user("e1", "go"),
        model_change("e2", "anthropic", "claude-opus-4-8"),
        custom("e3"),
        label("e4"),
        tool_result("e5"),
        compaction("e6", None),
        branch_summary("e7", None),
        json!({ "type": "thinking_level_change", "id": "e8", "parentId": null, "thinkingLevel": "high" }).to_string(),
        // A shape this build has never seen — a future pi entry type.
        json!({ "type": "some_future_entry", "id": "e9", "parentId": null }).to_string(),
    ];
    let only_noise = parse_pi_transcript(&file(&noise));
    assert_eq!(only_noise.tokens.total(), 0);
    assert_eq!(only_noise.cost_usd, None);
    assert_eq!(only_noise.model, None);

    let mut with_spend = noise.to_vec();
    with_spend.push(assistant("e10", "anthropic", "claude-opus-4-8", Turn { input: 7, cost: Some(0.1), ..Turn::default() }));
    with_spend.push(label("e11"));
    let mixed = parse_pi_transcript(&file(&with_spend));
    assert_eq!(mixed.tokens.input_tokens, 7, "control: a spending turn among the noise IS counted");
    assert_eq!(mixed.model.as_deref(), Some("anthropic/claude-opus-4-8"));
}

#[test]
fn a_blank_or_unparseable_line_is_skipped_rather_than_ending_the_fold() {
    let text = format!(
        "{}\n\n{{not json at all\n{}\n",
        assistant("e1", "anthropic", "claude-opus-4-8", Turn { input: 10, cost: Some(0.1), ..Turn::default() }),
        assistant("e2", "anthropic", "claude-opus-4-8", Turn { input: 20, cost: Some(0.2), ..Turn::default() }),
    );
    let u = parse_pi_transcript(&text);
    assert_eq!(u.tokens.input_tokens, 30, "the turn AFTER the bad line must still be folded");
}

// ---------------------------------------------------------------------------
// The on-disk reader: which file, and what a partial write does
// ---------------------------------------------------------------------------

#[test]
fn a_pi_session_is_found_by_its_id_suffix_in_the_flat_group_store() {
    let dir = tempfile::tempdir().unwrap();
    // A sibling pane's file in the same flat directory, and one whose
    // timestamp ends in the digits of our id — the two ways a looser match
    // would answer for the wrong session.
    write_session(dir.path(), "11111111-2222-3333-4444-555555555555", &file(&[
        header("11111111-2222-3333-4444-555555555555", "C:/tmp/other"),
        assistant("x1", "anthropic", "claude-opus-4-8", Turn { input: 99_999, cost: Some(9.9), ..Turn::default() }),
    ]));
    write_session(dir.path(), SES, &file(&[
        header(SES, "C:/tmp/repo"),
        assistant("e1", "anthropic", "claude-opus-4-8", Turn { input: 12, output: 3, cost: Some(0.5), ..Turn::default() }),
    ]));

    let u = pi_session_usage_in(dir.path(), SES).expect("the session's own file");
    assert_eq!(u.tokens.input_tokens, 12);
    assert_eq!(u.tokens.output_tokens, 3);
    assert_eq!(u.cost_usd, Some(0.5));
}

#[test]
fn an_absent_file_and_an_absent_directory_are_both_no_usage_rather_than_a_zero_reading() {
    let dir = tempfile::tempdir().unwrap();
    assert!(pi_session_usage_in(&dir.path().join("never-created"), SES).is_none());
    fs::create_dir_all(dir.path()).unwrap();
    assert!(pi_session_usage_in(dir.path(), SES).is_none(), "an empty store has no such session");
}

#[test]
fn an_id_that_is_not_a_path_component_never_reaches_the_store() {
    // #925's refusal, shared with every other id-to-file lookup: the answer is
    // the existing `None`, not a lookup and not an error.
    let dir = tempfile::tempdir().unwrap();
    write_session(dir.path(), SES, &file(&[header(SES, "C:/tmp/repo")]));
    for bad in ["../escape", "a/b", "..", "", "C:evil"] {
        assert!(pi_session_usage_in(dir.path(), bad).is_none(), "{bad:?} must not be looked up");
    }
    // Control: the same directory DOES answer for a well-formed id, so the
    // refusals above are the id being rejected and not an unreadable store.
    assert!(pi_session_usage_in(dir.path(), SES).is_some());
}

#[test]
fn a_torn_last_line_keeps_everything_written_before_it() {
    // A JSONL writer appends the record and its newline separately, so a poll
    // can land between them. The partial line is held back, not folded.
    let dir = tempfile::tempdir().unwrap();
    let complete = file(&[
        header(SES, "C:/tmp/repo"),
        assistant("e1", "anthropic", "claude-opus-4-8", Turn { input: 100, output: 10, cost: Some(0.1), ..Turn::default() }),
    ]);
    let torn = assistant("e2", "anthropic", "claude-opus-4-8", Turn { input: 500, output: 50, cost: Some(0.5), ..Turn::default() });
    write_session(dir.path(), SES, &format!("{complete}{}", &torn[..torn.len() / 2]));

    let u = pi_session_usage_in(dir.path(), SES).expect("read");
    assert_eq!(u.tokens.input_tokens, 100, "the complete turn survives");
    assert_eq!(u.tokens.output_tokens, 10, "and the torn one contributes nothing");
}

// ---------------------------------------------------------------------------
// The cursor, generalised: one cache, a per-CLI folder
// ---------------------------------------------------------------------------

/// Bytes of a file, for the work assertions below.
fn len_of(p: &Path) -> u64 {
    fs::metadata(p).unwrap().len()
}

#[test]
fn a_pi_transcript_is_read_incrementally_the_way_a_claude_one_is() {
    // The point of generalising the cursor rather than adding a second cache:
    // #1239's work bound applies to pi with no second implementation of it.
    let dir = tempfile::tempdir().unwrap();
    // Several turns, so "the tick cost less than the file" is an unambiguous
    // margin rather than a coin flip against one record's length.
    let first = file(&[
        header(SES, "C:/tmp/repo"),
        assistant("e1", "anthropic", "claude-opus-4-8", Turn { input: 25, cost: Some(0.1), ..Turn::default() }),
        assistant("e2", "anthropic", "claude-opus-4-8", Turn { input: 25, cost: Some(0.1), ..Turn::default() }),
        assistant("e3", "anthropic", "claude-opus-4-8", Turn { input: 25, cost: Some(0.1), ..Turn::default() }),
        assistant("e4", "anthropic", "claude-opus-4-8", Turn { input: 25, cost: Some(0.1), ..Turn::default() }),
    ]);
    let path = write_session(dir.path(), SES, &first);

    let cursors = TranscriptCursors::with_revalidate_after(Duration::from_secs(3600));
    let (u1, w1) = cursors
        .session_usage_measured(TranscriptKind::Pi, dir.path(), SES)
        .expect("first read");
    assert_eq!(u1.tokens.input_tokens, 100);
    assert!(w1.bytes_read >= len_of(&path), "a first read parses the whole file");
    assert!(!w1.reset, "there was no cursor to throw away");

    // Append one turn, exactly as pi does.
    let appended = format!(
        "{}\n",
        assistant("e5", "anthropic", "claude-opus-4-8", Turn { input: 7, cost: Some(0.01), ..Turn::default() })
    );
    let before = len_of(&path);
    fs::write(&path, format!("{first}{appended}")).unwrap();

    let (u2, w2) = cursors
        .session_usage_measured(TranscriptKind::Pi, dir.path(), SES)
        .expect("second read");
    assert_eq!(u2.tokens.input_tokens, 107, "the append is folded on");
    assert!(!w2.reset, "an append is not a reason to re-parse from zero");
    assert!(
        w2.bytes_read <= appended.len() as u64 + 64,
        "a tick must cost the appended region plus the 64-byte anchor, not the whole \
         file: read {} for a {}-byte append",
        w2.bytes_read,
        appended.len()
    );
    assert!(
        w2.bytes_read < before,
        "…which is strictly less than the {before}-byte prefix already consumed \
         (read {})",
        w2.bytes_read
    );
}

#[test]
fn a_rewritten_pi_file_refolds_from_scratch_rather_than_folding_onto_a_stale_position() {
    // pi's `_rewriteFile` opens the session file with mode "w" — a truncate —
    // and writes every entry back, which is what a `/tree` navigation or a
    // label edit does. The cursor's shared guards are what catch it: the
    // rewritten file is shorter, so `stat_verdict`'s `len < self.len` arm
    // resets, and the totals describe the file as it now is.
    let dir = tempfile::tempdir().unwrap();
    let long = file(&[
        header(SES, "C:/tmp/repo"),
        assistant("e1", "anthropic", "claude-opus-4-8", Turn { input: 100, cost: Some(0.1), ..Turn::default() }),
        assistant("e2", "anthropic", "claude-opus-4-8", Turn { input: 200, cost: Some(0.2), ..Turn::default() }),
        assistant("e3", "anthropic", "claude-opus-4-8", Turn { input: 300, cost: Some(0.3), ..Turn::default() }),
    ]);
    let path = write_session(dir.path(), SES, &long);

    let cursors = TranscriptCursors::with_revalidate_after(Duration::from_secs(3600));
    let (before, _) = cursors
        .session_usage_measured(TranscriptKind::Pi, dir.path(), SES)
        .expect("first read");
    assert_eq!(before.tokens.input_tokens, 600, "control: the whole file was folded");

    // The rewrite drops the last two entries.
    let rewritten = file(&[
        header(SES, "C:/tmp/repo"),
        assistant("e1", "anthropic", "claude-opus-4-8", Turn { input: 100, cost: Some(0.1), ..Turn::default() }),
    ]);
    assert!(rewritten.len() < long.len(), "fixture: the rewrite really is shorter");
    fs::write(&path, &rewritten).unwrap();

    let (after, w) = cursors
        .session_usage_measured(TranscriptKind::Pi, dir.path(), SES)
        .expect("second read");
    assert!(w.reset, "the cursor must be thrown away, not resumed at a stale offset");
    assert_eq!(after.tokens.input_tokens, 100, "the totals describe the file as it now is");
}

#[test]
fn a_non_shrinking_pi_rewrite_is_caught_by_the_anchor_not_by_the_length_arm() {
    // The OTHER half of `_rewriteFile`, and the half a shrink-only framing
    // misses (rev-final round 2, W4). pi is the only harness here that rewrites
    // its whole session file, so unlike claude this cursor meets rewrites that
    // do NOT shorten the file — those sail past `stat_verdict`'s
    // `len < self.len` arm and reach `Extend`, where the only thing standing
    // between them and a fold onto a stale offset is the 64-byte anchor
    // re-read.
    //
    // The fixture GROWS rather than staying the same length, deliberately: a
    // same-length rewrite has to move the mtime to reach `Extend` at all, and
    // on a coarse filesystem clock that is a race. Growth reaches `Extend`
    // whatever the clock says, and it is the same discrimination — `len` is
    // strictly greater, so the length arm cannot be what fires.
    let dir = tempfile::tempdir().unwrap();
    let before = file(&[
        header(SES, "C:/tmp/repo"),
        assistant("e1", "anthropic", "claude-opus-4-8", Turn { input: 100, cost: Some(0.1), ..Turn::default() }),
        assistant("e2", "anthropic", "claude-opus-4-8", Turn { input: 200, cost: Some(0.2), ..Turn::default() }),
        assistant("e3", "anthropic", "claude-opus-4-8", Turn { input: 300, cost: Some(0.3), ..Turn::default() }),
    ]);
    let path = write_session(dir.path(), SES, &before);

    let cursors = TranscriptCursors::with_revalidate_after(Duration::from_secs(3600));
    let (first, _) = cursors
        .session_usage_measured(TranscriptKind::Pi, dir.path(), SES)
        .expect("first read");
    assert_eq!(first.tokens.input_tokens, 600, "control: the whole file was folded");

    // A `/tree` navigation that lands on a different branch: every entry is
    // rewritten, the amounts differ, and the file ends up LONGER.
    let after = file(&[
        header(SES, "C:/tmp/repo"),
        assistant("f1", "openrouter", "moonshotai/kimi-k2.6", Turn { input: 11, cost: Some(0.01), ..Turn::default() }),
        assistant("f2", "openrouter", "moonshotai/kimi-k2.6", Turn { input: 22, cost: Some(0.02), ..Turn::default() }),
        assistant("f3", "openrouter", "moonshotai/kimi-k2.6", Turn { input: 33, cost: Some(0.03), ..Turn::default() }),
        assistant("f4", "openrouter", "moonshotai/kimi-k2.6", Turn { input: 44, cost: Some(0.04), ..Turn::default() }),
    ]);
    assert!(
        after.len() > before.len(),
        "fixture: this test is about the NON-shrinking rewrite — if it ever shrinks, the \
         length arm fires and the anchor is no longer what is being pinned ({} vs {})",
        after.len(),
        before.len()
    );
    fs::write(&path, &after).unwrap();

    let (now, w) = cursors
        .session_usage_measured(TranscriptKind::Pi, dir.path(), SES)
        .expect("second read");
    assert!(
        w.reset,
        "a grown-but-rewritten file must throw the cursor away; folding its tail onto the \
         old offset would add the new entries to totals that still hold the old ones"
    );
    assert_eq!(
        now.tokens.input_tokens, 110,
        "the totals describe the file as it now is, not the old fold plus the new tail"
    );
    assert_ne!(
        now.tokens.input_tokens, 710,
        "710 is what an append-onto-a-stale-offset would report (600 + the new tail)"
    );
    assert_eq!(now.model.as_deref(), Some("openrouter/moonshotai/kimi-k2.6"));
}

#[test]
fn a_pi_cursor_and_a_claude_cursor_never_serve_each_others_totals() {
    // The cache key carries the harness. Two stores that happened to share a
    // root would otherwise answer out of one cursor, which is the one way this
    // design could produce a WRONG total rather than a slow tick — so the pin
    // is a single directory holding BOTH shapes under one id.
    let dir = tempfile::tempdir().unwrap();
    // pi's file for `SES`, in `dir`.
    write_session(dir.path(), SES, &file(&[
        header(SES, "C:/tmp/repo"),
        assistant("e1", "anthropic", "claude-opus-4-8", Turn { input: 100, cost: Some(0.1), ..Turn::default() }),
    ]));
    // A claude projects tree, also rooted at `dir`, whose one project folder
    // holds `<SES>.jsonl` — the name claude's scan looks for.
    let proj = dir.path().join("C--tmp-repo");
    fs::create_dir_all(&proj).unwrap();
    fs::write(
        proj.join(format!("{SES}.jsonl")),
        format!(
            "{}\n",
            json!({"type":"assistant","message":{"id":"m1","model":"claude-opus-4-8",
                "usage":{"input_tokens":4242,"output_tokens":0,
                         "cache_creation_input_tokens":0,"cache_read_input_tokens":0}}})
        ),
    )
    .unwrap();

    let cursors = TranscriptCursors::with_revalidate_after(Duration::from_secs(3600));
    let pi = cursors.session_usage(TranscriptKind::Pi, dir.path(), SES).expect("pi read");
    let claude = cursors.session_usage(TranscriptKind::Claude, dir.path(), SES).expect("claude read");
    assert_eq!(pi.tokens.input_tokens, 100);
    assert_eq!(claude.tokens.input_tokens, 4242);
    // And in the other order, so neither is merely "whichever ran first".
    let pi_again = cursors.session_usage(TranscriptKind::Pi, dir.path(), SES).expect("pi again");
    assert_eq!(pi_again.tokens.input_tokens, 100);
}

#[test]
fn the_whole_file_reader_and_the_cursor_agree_on_the_same_pi_file() {
    // Both go through `fold_appended`, so this is a pin on that staying true
    // rather than on two readers being kept in step by review.
    let dir = tempfile::tempdir().unwrap();
    write_session(dir.path(), SES, &file(&[
        header(SES, "C:/tmp/repo"),
        assistant("e1", "openrouter", "moonshotai/kimi-k2.6", Turn {
            input: 21_650, output: 121, cache_read: 1_328, cache_write: 64,
            reasoning: Some(78), cost: Some(0.021_263_98),
        }),
        compaction("e2", Some(Turn { input: 900, output: 90, cost: Some(0.001), ..Turn::default() })),
    ]));

    let whole = pi_session_usage_in(dir.path(), SES).expect("whole-file read");
    let cursors = TranscriptCursors::default();
    let incremental = cursors
        .session_usage(TranscriptKind::Pi, dir.path(), SES)
        .expect("cursor read");
    assert_eq!(whole.tokens, incremental.tokens);
    assert_eq!(whole.cost_usd, incremental.cost_usd);
    assert_eq!(whole.model, incremental.model);
    assert_eq!(whole.tokens.total(), 21_650 + 121 + 1_328 + 64 + 900 + 90);
}

// ---------------------------------------------------------------------------
// The wiring: which source a pi agent's usage comes from
// ---------------------------------------------------------------------------

fn rails(cli: &str) -> Guardrails {
    Guardrails {
        max_agents: 4,
        agent_cli: cli.into(),
        blocks: workflow::default_roster(&[
            (Role::Orchestrator, "", ""),
            (Role::Worker, "", ""),
            (Role::Reviewer, "", ""),
        ]),
        auto_ops: false,
        idle_kill_minutes: 0,
        max_spawns_per_hour: 0,
        watchdog_stall_minutes: 0,
        ..Guardrails::default()
    }
}

/// A roster whose worker CLASS defaults to claude and whose SECOND worker block
/// runs pi — the #2167 shape, in the direction this slice adds.
fn rails_second_worker_block_is_pi() -> Guardrails {
    let mut g = rails("claude");
    g.blocks.push(workflow::Block {
        id: "worker-pi".into(),
        name: "worker-pi".into(),
        kind: Role::Worker,
        cli: "pi".into(),
        model: String::new(),
        prompt: None,
        profile: None,
        allow: vec![],
        role_hint: None,
        effort: String::new(),
        context: String::new(),
        remote: None,
    });
    g
}

fn test_registry() -> (OrchRegistry, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let reg = OrchRegistry::new(dir.path().to_path_buf());
    reg.set_port(45993);
    // `the_pi_arm_never_fires_for_another_cli` drives the claude arm
    // deliberately — point every per-user root at this disposable tree so no
    // test here reads the maintainer's real `~/.claude`.
    reg.set_claude_projects_dir(dir.path().join("claude-projects"));
    reg.set_claude_agents_dir_override(dir.path().join("claude-agents"));
    reg.set_copilot_agents_dir_override(dir.path().join("copilot-agents"));
    reg.set_compact_hook_dir_override(dir.path().join("compacthook"));
    reg.set_copilot_hooks_dir_override(dir.path().join("copilot-hooks"));
    (reg, dir)
}

/// The proof the #464 allowlist row for this file names (`tests/orchestration.rs`,
/// `no_registry_construction_bypasses_the_test_agent_dir_overrides`). That row
/// permits ONE raw `OrchRegistry::new` here, and it permits it only because
/// `test_registry` applies every agent/hook dir override — without which a spawn
/// in this file writes a generated agent file into the maintainer's real
/// `~/.claude` or `~/.copilot`. A textual scan cannot see that, so the row would
/// otherwise be pure trust; this makes it a claim that fails in THIS binary the
/// moment the helper stops applying one.
#[test]
fn its_registry_helper_applies_every_override_this_allowlist_row_assumes() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/piusage.rs"),
    )
    .expect("this file reads itself");

    // The helper's body: from its signature to the first line that closes it at
    // column 0 — narrow enough that an override applied by some OTHER function
    // in this file cannot satisfy the assertions below.
    let start = src
        .find("fn test_registry() -> (OrchRegistry, tempfile::TempDir) {")
        .expect("the sanctioned helper must exist, under the name the row names");
    let body = &src[start..];
    let end = body.find("\n}").expect("the helper must terminate") + 2;
    let body = &body[..end];

    for needed in [
        "set_claude_agents_dir_override",
        "set_copilot_agents_dir_override",
        "set_compact_hook_dir_override",
        "set_copilot_hooks_dir_override",
    ] {
        assert!(
            body.contains(needed),
            "the #464 allowlist row for tests/piusage.rs assumes this helper applies every \
             override; it no longer applies {needed}, so a registry built through it can reach \
             the real agent dirs and the row's premise is gone"
        );
    }

    // The population control: the extraction really did isolate the helper, so
    // the four assertions above are about ITS body and not about the whole file
    // — which contains those same names in prose above.
    assert!(
        body.len() < 1_200,
        "the helper's body extraction ran away ({} chars); the assertions above would then be \
         satisfied by any other function in this file",
        body.len()
    );
    assert!(
        !body.contains("#[test]"),
        "the extraction swallowed a test, so it is no longer reading only the helper"
    );
}

#[test]
fn a_pi_agents_usage_comes_from_the_group_store_and_is_reported_not_estimated() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/pi-repo", rails("pi")).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "task", false, None).unwrap();
    // pi premints, so unlike opencode the id is there from spawn and this arm
    // never sits idle waiting for the pane to announce one.
    let sid = w.session_id.clone().expect("a pi pane carries a preminted session id");

    // The store goes exactly where the spawn points `--session-dir` — the same
    // function, so this cannot pass against a path the pane never writes.
    write_session(&reg.pi_sessions_dir(&g.id), &sid, &file(&[
        header(&sid, "C:/tmp/pi-repo"),
        assistant("e1", "openrouter", "moonshotai/kimi-k2.6", Turn {
            input: 21_650, output: 121, cache_read: 1_328, cache_write: 64,
            reasoning: Some(78), cost: Some(0.02),
        }),
        compaction("e2", Some(Turn { input: 900, output: 90, cost: Some(0.001), ..Turn::default() })),
    ]));

    let snap = reg.compute_usage_snapshot(&w, "pi");
    assert_eq!(snap.source, "pi-transcript", "usage must come from the group's pi store");
    assert_eq!(snap.input_tokens, 21_650 + 900);
    assert_eq!(snap.output_tokens, 121 + 90, "and `reasoning` is not added to it");
    assert_eq!(snap.cache_read_tokens, 1_328);
    assert_eq!(snap.cache_creation_tokens, 64);
    let cost = snap.cost_usd.expect("pi priced these turns");
    assert!((cost - 0.021).abs() < 1e-9, "cost was {cost}");
    assert!(
        !snap.estimated,
        "pi priced this itself — labelling it `estimated` would blend a reported \
         figure into a total the UI calls a price-table guess"
    );
    assert_eq!(snap.model.as_deref(), Some("openrouter/moonshotai/kimi-k2.6"));
}

#[test]
fn a_pi_agent_whose_session_file_holds_only_a_header_reports_no_usage_rather_than_a_zero_row() {
    // pi defers creating the file to the first assistant response and writes
    // the header first, so "spawned and never prompted" is the ordinary case.
    // A zero must not pre-empt the statusline fallback or overwrite history.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/pi-repo", rails("pi")).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "task", false, None).unwrap();
    let sid = w.session_id.clone().unwrap();
    write_session(&reg.pi_sessions_dir(&g.id), &sid, &file(&[header(&sid, "C:/tmp/pi-repo")]));

    let snap = reg.compute_usage_snapshot(&w, "pi");
    assert_eq!(snap.source, "none");
    assert_eq!(snap.input_tokens, 0);
    assert_eq!(snap.cost_usd, None);
}

#[test]
fn a_missing_pi_store_leaves_the_agent_at_zero_rather_than_wedging() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/pi-repo", rails("pi")).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "task", false, None).unwrap();
    fs::remove_dir_all(reg.pi_sessions_dir(&g.id)).ok();

    let snap = reg.compute_usage_snapshot(&w, "pi");
    assert_eq!(snap.source, "none");
    assert_eq!(snap.cost_usd, None);
}

#[test]
fn the_pi_arm_never_fires_for_another_cli() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/mixed-repo", rails("claude")).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "task", false, None).unwrap();
    let sid = w.session_id.clone().expect("claude premints too");
    // A pi store sitting in the group dir under this very session id — a
    // mixed-CLI group is the normal case, so a claude agent must not be charged
    // out of it just because the ids line up.
    write_session(&reg.pi_sessions_dir(&g.id), &sid, &file(&[
        header(&sid, "C:/tmp/mixed-repo"),
        assistant("e1", "anthropic", "claude-opus-4-8", Turn { input: 55_555, cost: Some(5.5), ..Turn::default() }),
    ]));

    let snap = reg.compute_usage_snapshot(&w, "claude");
    assert_ne!(snap.source, "pi-transcript");
    assert_eq!(snap.input_tokens, 0);
}

#[test]
fn a_pi_pane_in_a_second_block_of_its_class_is_read_as_pi_not_as_the_class_default() {
    // #2167's shape, in this slice's direction. The class default for Worker is
    // claude; the pane's own block runs pi. Resolving the CLI from the ROLE
    // would hand `compute_usage_snapshot` "claude", its pi arm would never run,
    // and the row would land as `none` with four zero counters — for a session
    // file sitting in the group store the whole time.
    //
    // This arm inherits the fix rather than re-implementing it: both callers
    // (`compute_group_usage` via `cli_for_block`, `mark_dead` via
    // `cli_for_agent`) already resolve per-block, so what is pinned here is
    // that the pi arm is reached THROUGH them.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/pi-second-block", rails_second_worker_block_is_pi()).unwrap();

    let rails = reg.group(&g.id).unwrap().guardrails;
    assert_eq!(rails.cli_for(Role::Worker), "claude", "fixture: the class default disagrees");
    assert_eq!(rails.cli_for_block("worker-pi", Role::Worker), "pi");

    let w = reg
        .spawn_agent_ex(&g.id, Role::Worker, Some("worker-pi".into()), "w", "task", false, None, None, None, None, None)
        .unwrap();
    assert_eq!(w.block, "worker-pi");
    let sid = w.session_id.clone().expect("a pi block premints a session id");

    write_session(&reg.pi_sessions_dir(&g.id), &sid, &file(&[
        header(&sid, "C:/tmp/pi-second-block"),
        assistant("e1", "openrouter", "z-ai/glm-5.3-flash", Turn {
            input: 1_000, output: 500, cost: Some(0.03), ..Turn::default()
        }),
    ]));

    // The live path and the exit path resolve the CLI separately, so both are
    // pinned — the same pair `a_claude_pane_reads_its_transcript_when_the_class_
    // default_block_runs_another_cli` pins for claude.
    let live = reg.group_usage(&g.id);
    let live_row = live["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == w.id.as_str())
        .expect("live worker on the roster");
    assert_eq!(live_row["source"], "pi-transcript", "the live poll must read the pi store");
    assert_eq!(live_row["tokens"]["total"].as_u64(), Some(1_500));

    reg.mark_dead(&w.id, Some(0));
    let usage = reg.group_usage(&g.id);
    let row = usage["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == w.id.as_str())
        .expect("dead worker captured");
    assert_eq!(row["source"], "pi-transcript", "mark_dead must read it too");
    assert_eq!(row["tokens"]["total"].as_u64(), Some(1_500));
    assert_eq!(usage["lifetime_tokens"].as_u64(), Some(1_500));
}

#[test]
fn a_pi_turn_with_tokens_but_no_dollars_still_wins_the_arm_and_leaves_the_cost_unknown() {
    // rev-final round 2, premortem 1: the fold-level distinction between
    // `Some(0.0)` and `None` is pinned above, but its SNAPSHOT-level
    // consequence was not — and that consequence is a decision, not an
    // accident, so it is pinned here rather than left to be rediscovered.
    //
    // A pi pane on a local/self-hosted or unpriced provider writes `usage` with
    // no `cost` object. Tokens are then real and dollars are unknown, and the
    // arm returns on `u.tokens.total() > 0` — so the statusline fallback is NOT
    // consulted and the row shows tokens against a blank cost, labelled
    // reported.
    //
    // That is deliberate and it is the same rule the claude arm already
    // follows: a claude transcript whose model is absent from `price_for`
    // returns `cost_usd: None` from its own arm too, because exact tokens beat
    // a scraped dollar figure that is empty on a subscription account anyway.
    // Tokens are the honest metric; a missing dollar figure is reported as
    // missing rather than backfilled from a less reliable source.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/pi-repo", rails("pi")).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "task", false, None).unwrap();
    let sid = w.session_id.clone().unwrap();
    write_session(&reg.pi_sessions_dir(&g.id), &sid, &file(&[
        header(&sid, "C:/tmp/pi-repo"),
        assistant("e1", "ollama", "qwen3-coder", Turn {
            input: 4_000, output: 250, cost: None, ..Turn::default()
        }),
    ]));

    let snap = reg.compute_usage_snapshot(&w, "pi");
    assert_eq!(snap.source, "pi-transcript", "exact tokens still win the arm");
    assert_eq!(snap.input_tokens, 4_000);
    assert_eq!(snap.output_tokens, 250);
    assert_eq!(snap.cost_usd, None, "unpriced is reported as unknown, never as $0");
    assert!(!snap.estimated, "and it is still not a price-table guess");
    assert_ne!(
        snap.source, "statusline",
        "the fallback must not be reached: it would attach a scraped dollar figure to \
         tokens this arm already read exactly"
    );
}

#[test]
fn the_pi_dollars_reach_the_group_total_on_the_reported_basis_not_the_estimated_one() {
    // `group_usage` splits its dollar total by basis so the UI never adds a
    // vendor-priced figure to a price-table guess. pi belongs on the reported
    // side, with opencode.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/pi-repo", rails("pi")).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "task", false, None).unwrap();
    let sid = w.session_id.clone().unwrap();
    write_session(&reg.pi_sessions_dir(&g.id), &sid, &file(&[
        header(&sid, "C:/tmp/pi-repo"),
        assistant("e1", "openrouter", "z-ai/glm-5.3-flash", Turn {
            input: 1_000, output: 500, cost: Some(0.25), ..Turn::default()
        }),
    ]));

    let usage = reg.group_usage(&g.id);
    let row = usage["agents"].as_array().unwrap().iter()
        .find(|a| a["id"] == w.id.as_str()).expect("worker on the roster");
    assert_eq!(row["source"], "pi-transcript");
    assert_eq!(row["estimated"], Value::Bool(false));
    assert_eq!(row["tokens"]["total"].as_u64(), Some(1_500));
    // The basis fields are the point: a lone pi group is wholly REPORTED, never
    // "estimated" and never "mixed".
    assert_eq!(usage["live_cost_basis"], "reported");
    assert_eq!(usage["lifetime_cost_basis"], "reported");
}

// ---------------------------------------------------------------------------
// The module's own claim about what it never does
// ---------------------------------------------------------------------------

#[test]
fn nothing_here_prices_a_pi_turn_from_the_claude_price_table() {
    // `price_for` exists for claude's token-derived ESTIMATE. A pi model id is
    // not in it, and a pi turn must never be routed through it even if one day
    // a pi provider serves a model whose id contains "opus" — the dollars are
    // pi's, and the fixture uses exactly that id to make the point testable.
    let dir = tempfile::tempdir().unwrap();
    write_session(dir.path(), SES, &file(&[
        header(SES, "C:/tmp/repo"),
        assistant("e1", "anthropic", "claude-opus-4-8", Turn {
            input: 1_000_000, output: 1_000_000, cost: Some(0.02), ..Turn::default()
        }),
    ]));
    assert!(usage::price_for("claude-opus-4-8").is_some(), "control: this id IS priced for claude");

    let u = pi_session_usage_in(dir.path(), SES).expect("read");
    assert_eq!(u.cost_usd, Some(0.02), "pi's own figure, not 30.0 from the price table");
}

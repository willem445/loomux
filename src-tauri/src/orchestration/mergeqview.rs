//! Read-only merge-queue **view** for the human's chrome (#581 slice F).
//!
//! Design note: `doc/design/merge-queue.md` — §11.3 is the `merge_queue.json`
//! schema this reads, §11.6 is this surface. The note's own words: *one
//! read-only Tauri command `orch_merge_queue` … feeding a DOM-free
//! `src/mergequeue.ts` model*. This file is the backend half of that sentence.
//!
//! # What this is, and what it deliberately is not
//!
//! It **reads one file and projects it**. There is no `git`, no `gh`, no
//! network, no write of any kind, and no decision: the queue's decisions are
//! `mergeq.rs`'s (pure core, slice C) and its actions are the driver's
//! (slice D). A view that could act would be a second, quieter driver.
//!
//! It also does not consult `.loomux/workflow.yml`. Whether the queue is
//! *enabled* is policy that slice E's `merge_queue_status()` MCP tool reports;
//! what this answers is the narrower question the chrome actually asks — "is
//! there a queue here, and what is in it right now" — and the honest answer for
//! a repo that never enabled the feature is [`status = "absent"`](project),
//! because no `merge_queue.json` was ever written. One file, one read.
//!
//! # Three properties this file exists to hold
//!
//! 1. **No state renders as unknown or blank.** `mergeq::EntryState` has no
//!    catch-all variant on purpose, so a state word this build does not know
//!    fails the parse — and that failure is projected as a *loud* `unreadable`
//!    status carrying the parser's own message, never as an empty queue. An
//!    empty queue and an unreadable one look identical to a human, which is
//!    exactly why they must not share a wire shape.
//! 2. **Truncation is surfaced, never silent** (the #608/#579 convention).
//!    `entries_total` is the count in the *file* and `truncated` is reported by
//!    the reader that knows it cut something — the `audit_log_windowed`
//!    precedent — so a capped list can never read as a complete one.
//! 3. **A version this build does not understand is refused, not guessed at.**
//!    Unknown *fields* are tolerated (that is `MergeQueueState`'s job); an
//!    unknown *schema* means the fields this build recognizes may no longer
//!    mean what it thinks, and rendering them anyway would put a confident
//!    wrong sentence in front of a human.

use super::mergeq::{MergeQueueState, MAX_ENTRIES, MERGE_QUEUE_VERSION};
use serde_json::{json, Value};
use std::path::Path;

/// The group-dir file this reads, beside `state.json` / `tasks.json` /
/// `audit.jsonl` (§4).
pub const MERGE_QUEUE_FILE: &str = "merge_queue.json";

/// How many entries this command will hand the webview.
///
/// Deliberately the queue's own cap (§10) rather than a display number: this is
/// a bound on the wire payload, not a UI decision — how many rows the chrome
/// draws is `src/mergequeue.ts`'s call, and it surfaces its own cut separately.
/// With both caps equal it is unreachable today, and that is fine: it becomes
/// reachable the moment a newer build raises `MAX_ENTRIES` and writes a file
/// this one reads, which is precisely when a silently short list would lie.
pub const VIEW_ENTRY_LIMIT: usize = MAX_ENTRIES;

/// Cap on the `detail` sentence. Parser messages are short, but they quote the
/// offending input, and an unbounded string from a file goes straight into
/// chrome.
const MAX_DETAIL_CHARS: usize = 300;

/// Read the group's `merge_queue.json` and project it for the chrome.
///
/// `dir` is the group state dir. **Constraint 6 posture, unchanged:** the
/// `group_id` that built this path is trusted as a path segment exactly as
/// every sibling `orch_*` command trusts it — safe only because the webview is
/// trusted, and this command adds no new agent-reachable input (it takes no
/// argument but the group).
pub fn merge_queue_view(dir: &Path) -> Value {
    match std::fs::read_to_string(dir.join(MERGE_QUEUE_FILE)) {
        Ok(text) => project(&text),
        // No file: this group has never enqueued anything (or the feature is
        // off, which is the product default, §12). Not a problem to report.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => absent(),
        // A file that is there and unreadable is a *fact*, and the one thing
        // this command must never do is let it read as "nothing queued".
        Err(e) => problem("unreadable", None, &format!("{MERGE_QUEUE_FILE} could not be read: {e}")),
    }
}

/// The projection itself — pure, so every property above is testable without a
/// filesystem.
///
/// `status` is a **closed vocabulary**: `absent` · `unreadable` ·
/// `unsupported-version` · `ok`. The frontend model switches on it exhaustively
/// and fails loud on a word it does not know, the same posture this module
/// takes on an entry state.
pub fn project(text: &str) -> Value {
    let state: MergeQueueState = match serde_json::from_str(text) {
        Ok(s) => s,
        Err(e) => {
            return problem("unreadable", None, &format!("{MERGE_QUEUE_FILE} did not parse: {e}"))
        }
    };
    if !state.version_supported() {
        return problem(
            "unsupported-version",
            Some(state.version),
            &format!(
                "{MERGE_QUEUE_FILE} is version {} and this build understands version \
                 {MERGE_QUEUE_VERSION} — not rendering a file it may misread",
                state.version
            ),
        );
    }
    // The count in the FILE, taken before the cap — `truncated` is then a fact
    // the reader states rather than something a consumer has to infer from
    // `entries.len() == VIEW_ENTRY_LIMIT`, which cannot distinguish a list that
    // was cut from one that happened to hold exactly the cap (#579's argument,
    // and `audit_log_windowed`'s shape).
    let total = state.entries.len();
    let entries: Vec<Value> = state
        .entries
        .iter()
        .take(VIEW_ENTRY_LIMIT)
        .map(|e| {
            json!({
                "pr": e.pr,
                // `EntryState::as_str` — the same word serde writes, so the
                // wire and the file can never disagree about a state's name.
                "state": e.state().as_str(),
                "blocked_reason": e.blocked_reason,
                "head": e.head,
                "enqueued_ms": e.enqueued_ms,
                "batch": e.batch,
            })
        })
        .collect();
    json!({
        "status": "ok",
        "detail": Value::Null,
        "version": state.version,
        "target": state.target,
        "entries": entries,
        "entries_total": total,
        "truncated": total > entries.len(),
        "batch": state.batch.as_ref().map(|b| json!({
            "id": b.id,
            "prs": b.prs,
            "state": b.state().as_str(),
            "draft_pr": b.draft_pr,
            "scratch_sha": b.scratch_sha,
            "started_ms": b.started_ms,
        })),
    })
}

/// No `merge_queue.json` at all — the product default (§12). Every collection
/// field is present and empty so the frontend never has to guard for a missing
/// key on this path.
fn absent() -> Value {
    json!({
        "status": "absent",
        "detail": Value::Null,
        "version": Value::Null,
        "target": "",
        "entries": [],
        "entries_total": 0,
        "truncated": false,
        "batch": Value::Null,
    })
}

/// A file that exists and cannot be rendered. Same shape as [`absent`] — same
/// keys, same emptiness — with the `status` and `detail` that make it a
/// different thing to a reader, human or model.
fn problem(status: &str, version: Option<u32>, detail: &str) -> Value {
    json!({
        "status": status,
        "detail": clip(detail),
        "version": version,
        "target": "",
        "entries": [],
        "entries_total": 0,
        "truncated": false,
        "batch": Value::Null,
    })
}

/// Char-bounded, not byte-bounded: slicing a `String` at a byte offset panics
/// mid-codepoint, and a parser message can quote anything the file held.
fn clip(s: &str) -> String {
    if s.chars().count() <= MAX_DETAIL_CHARS {
        return s.to_string();
    }
    let head: String = s.chars().take(MAX_DETAIL_CHARS).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::mergeq::EntryState;
    use std::path::PathBuf;

    /// Every state word the core defines, as it appears in the file.
    const ALL_STATES: [EntryState; 8] = [
        EntryState::Queued,
        EntryState::Batching,
        EntryState::CiWait,
        EntryState::Landing,
        EntryState::Bisecting,
        EntryState::Landed,
        EntryState::KickedBack,
        EntryState::Cancelled,
    ];

    fn file_with_states(states: &[EntryState]) -> String {
        let entries: Vec<String> = states
            .iter()
            .enumerate()
            .map(|(i, s)| {
                format!(
                    r#"{{"pr":{},"head":"sha{}","state":"{}","blocked_reason":null,"enqueued_ms":{},"batch":null}}"#,
                    600 + i,
                    i,
                    s.as_str(),
                    i
                )
            })
            .collect();
        format!(r#"{{"version":1,"target":"feat/int","entries":[{}]}}"#, entries.join(","))
    }

    #[test]
    fn no_file_is_absent_not_an_empty_queue() {
        let v = merge_queue_view(&PathBuf::from("no-such-group-dir-for-#581-slice-f"));
        assert_eq!(v["status"], "absent");
        assert_eq!(v["entries"].as_array().unwrap().len(), 0);
        assert_eq!(v["detail"], Value::Null);
    }

    #[test]
    fn every_state_the_core_defines_projects_to_its_own_word() {
        let v = project(&file_with_states(&ALL_STATES));
        assert_eq!(v["status"], "ok");
        let got: Vec<&str> =
            v["entries"].as_array().unwrap().iter().map(|e| e["state"].as_str().unwrap()).collect();
        let want: Vec<&str> = ALL_STATES.iter().map(|s| s.as_str()).collect();
        assert_eq!(got, want, "a state must reach the wire as the word the core spells it with");
        assert_eq!(v["target"], "feat/int");
        assert_eq!(v["entries_total"], 8);
        assert_eq!(v["truncated"], false);
    }

    #[test]
    fn a_ninth_state_is_loud_never_an_empty_queue() {
        // The case the frontend can only ever see if this build and the file's
        // writer disagree about the eight. It must NOT read as "nothing
        // queued": an empty queue and an unrenderable one are the same picture.
        let text = r#"{"version":1,"target":"feat/int","entries":[
            {"pr":612,"head":"a","state":"frobnicating","blocked_reason":null,"enqueued_ms":0,"batch":null}]}"#;
        let v = project(text);
        assert_eq!(v["status"], "unreadable");
        assert!(
            v["detail"].as_str().unwrap().contains("did not parse"),
            "detail must say what happened: {}",
            v["detail"]
        );
        assert_eq!(v["entries"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_schema_this_build_does_not_understand_is_refused_not_rendered() {
        let text = r#"{"version":2,"target":"feat/int","entries":[]}"#;
        let v = project(text);
        assert_eq!(v["status"], "unsupported-version");
        assert_eq!(v["version"], 2);
        assert!(v["detail"].as_str().unwrap().contains("version 2"), "{}", v["detail"]);
    }

    #[test]
    fn unknown_fields_do_not_make_a_readable_file_unreadable() {
        // §11.2's asymmetry: state degrades gracefully where policy fails loud.
        let text = r#"{"version":1,"target":"feat/int","entries":[
            {"pr":612,"head":"a","state":"queued","blocked_reason":null,"enqueued_ms":0,
             "batch":null,"landed_by":"a-newer-build"}],"lane":"experimental"}"#;
        let v = project(text);
        assert_eq!(v["status"], "ok");
        assert_eq!(v["entries"][0]["pr"], 612);
    }

    #[test]
    fn a_cut_list_says_so_rather_than_reading_as_complete() {
        let states: Vec<EntryState> = (0..VIEW_ENTRY_LIMIT + 5).map(|_| EntryState::Queued).collect();
        let v = project(&file_with_states(&states));
        assert_eq!(v["status"], "ok");
        assert_eq!(v["entries"].as_array().unwrap().len(), VIEW_ENTRY_LIMIT);
        assert_eq!(
            v["entries_total"],
            (VIEW_ENTRY_LIMIT + 5) as u64,
            "the total is the count in the FILE, not the count that survived the cap"
        );
        assert_eq!(v["truncated"], true);
    }

    #[test]
    fn a_full_but_uncut_list_is_not_reported_as_truncated() {
        // The boundary `entries.len() == VIEW_ENTRY_LIMIT` cannot distinguish
        // cut from exactly-full — which is why the reader reports it and no
        // consumer infers it.
        let states: Vec<EntryState> = (0..VIEW_ENTRY_LIMIT).map(|_| EntryState::Queued).collect();
        let v = project(&file_with_states(&states));
        assert_eq!(v["entries"].as_array().unwrap().len(), VIEW_ENTRY_LIMIT);
        assert_eq!(v["truncated"], false);
    }

    #[test]
    fn the_in_flight_batch_reaches_the_wire_whole() {
        let text = r#"{"version":1,"target":"feat/int",
            "entries":[{"pr":612,"head":"a","state":"ci-wait","blocked_reason":null,"enqueued_ms":0,"batch":"mq-7f3a"}],
            "batch":{"id":"mq-7f3a","prs":[612,613],"scratch_sha":"deadbeef","draft_pr":640,
                     "state":"ci-wait","started_ms":42}}"#;
        let v = project(text);
        assert_eq!(v["batch"]["id"], "mq-7f3a");
        assert_eq!(v["batch"]["state"], "ci-wait");
        assert_eq!(v["batch"]["draft_pr"], 640);
        assert_eq!(v["batch"]["prs"][1], 613);
        assert_eq!(v["entries"][0]["batch"], "mq-7f3a");
    }

    #[test]
    fn a_blocked_entry_carries_its_reason() {
        // §4: "paused" is a live predicate, not a ninth state — so the reason
        // is the only thing that tells a human why a queued entry is not moving.
        let text = r#"{"version":1,"target":"feat/int","entries":[
            {"pr":612,"head":"a","state":"queued","blocked_reason":"head moved; verdicts stale",
             "enqueued_ms":0,"batch":null}]}"#;
        let v = project(text);
        assert_eq!(v["entries"][0]["state"], "queued");
        assert_eq!(v["entries"][0]["blocked_reason"], "head moved; verdicts stale");
    }

    #[test]
    fn a_long_detail_is_clipped_on_a_char_boundary() {
        // Multi-byte on purpose: a byte-offset slice would panic here, and a
        // panic in a read-only view is a worse outcome than the long string.
        let long = "é".repeat(2000);
        let clipped = clip(&long);
        assert_eq!(clipped.chars().count(), MAX_DETAIL_CHARS + 1, "clipped + the ellipsis");
        assert!(clipped.ends_with('…'));
        assert_eq!(clip("short"), "short", "a message inside the bound is left alone");
    }

    #[test]
    fn a_torn_file_is_unreadable_rather_than_empty() {
        let v = project(r#"{"version":1,"entries":[{"pr":612,"#);
        assert_eq!(v["status"], "unreadable");
        assert!(v["detail"].as_str().is_some(), "an unreadable file must say why");
    }
}

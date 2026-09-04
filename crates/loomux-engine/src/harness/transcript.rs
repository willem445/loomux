//! The VT renderer: [`HarnessEvent`] in, terminal bytes out
//! (`doc/design/harness-adapters.md` §5).
//!
//! # Why a structured pane still writes to a terminal
//!
//! A structured pane has no PTY, so nothing produces bytes for its xterm surface
//! on its own. loomux produces them: this module turns the event stream into VT
//! output, and those bytes go into that pane's `OutputBuf` ring through the same
//! coalescer that feeds `pty-output` today.
//!
//! That choice is what keeps `get_output`, termgrid replay, thumbnails,
//! `last_exit_tail` and #888's replay-on-attach working with **no API change**.
//! The alternative — a DOM transcript view beside the terminal — breaks all five
//! for exactly the panes the feature is for, which is why §5.1 rejects it.
//!
//! # The one rule this module must not break
//!
//! > **The renderer never rewrites bytes it has already emitted.**
//!
//! Lines are wrapped to the pane's columns at emit time; a later width change
//! re-wraps nothing, and reflow of already-emitted output is xterm's own,
//! exactly as for a PTY pane.
//!
//! CLAUDE.md constraint 1 forbids resizing a PTY for a UI feature because the
//! repaint pollutes scrollback. A structured pane has no PTY to resize, so the
//! constraint cannot be violated the usual way — but a renderer that re-emitted
//! its transcript on a width change would produce **the same damage by a
//! different road**, and a worse one, because grepping for the resize call would
//! not find it. So this module emits forward only: no cursor movement, no
//! erase, and no **bare** carriage return — a `CR` that is not part of a `CRLF`
//! is the one that returns the cursor without advancing.
//! [`no_output_can_rewrite_the_screen`] is the pin, and it bans that shape
//! specifically; an earlier version banned every `CR`, which forbade the line
//! ending this module is required to emit (see [`NEWLINE`]).
//!
//! # What it deliberately does not do
//!
//! Wrapping counts **characters, not display columns**. A CJK or emoji glyph
//! occupies two cells and is counted as one here, so a line of wide characters
//! wraps late by up to its own width. Correct wrapping needs a Unicode width
//! table, which is a dependency; this is an engine leaf whose dependency budget
//! is `serde_json` and `std` (§8.1). The residual is stated rather than hidden,
//! and it degrades to "a line wraps in a slightly different place", never to
//! corrupted output — the bytes are still a valid stream, and xterm reflows them
//! itself.

use super::{
    CompactTrigger, Cost, Decision, DecisionSource, HarnessEvent, StopReason, Usage,
};

/// How much of a tool call's arguments is drawn.
///
/// A tool call is **one collapsed line**: the human is watching what the agent
/// is doing, not reading its arguments, and an unbounded `input` (a whole file
/// in a `Write`) would push everything else off the screen. The full value is in
/// the pane's event log either way.
pub const TOOL_PREVIEW_BYTES: usize = 120;

/// The line ending every emit site in this module uses — **`CRLF`, not a bare
/// `LF`**, and it is load-bearing rather than pedantic.
///
/// These bytes end at `this.term.write(chunk)` (`src/pane.ts`) on a `Terminal`
/// built without `convertEol`, which xterm.js defaults to `false`. With it
/// false a bare `LF` is an **INDEX** — down one row, column untouched — so
/// every line staircases rightward and the transcript composes into something
/// no terminal would show. A PTY never exposes this, because ConPTY and a POSIX
/// pty in `ONLCR` both deliver `CRLF`; a *synthesized* stream has to do it
/// itself. `painted()` in `src-tauri/tests/orchestration.rs` states the same
/// rule for the same reason on the fixture side, and the two existing
/// loomux-authored injections into a pane go through `term.writeln`, which
/// appends `CRLF` for them.
///
/// **This is not in tension with the forward-only rule above.** A `CR` that is
/// part of a `CRLF` is a newline; only a *bare* `CR` — one not followed by `LF`
/// — returns the cursor without advancing, which is what can repaint a line.
/// [`no_output_can_rewrite_the_screen`] bans exactly that, and an earlier
/// version of it banned every `CR`, which forbade this fix and would have left
/// a green suite asserting the defect as the contract.
///
/// Fixing this by setting `convertEol: true` on the shared `Terminal` was
/// rejected: that terminal is every pane's, so it would change how a real PTY
/// pane parses a lone `LF` too.
const NEWLINE: &str = "\r\n";

/// Dim, for loomux's own annotations.
const DIM: &str = "\x1b[2m";
/// Reset. Every sequence this module opens is closed on the same line, so a
/// truncated stream can never leave the pane's terminal in an attribute state.
const RESET: &str = "\x1b[0m";

/// Renders one pane's event stream.
///
/// Stateful in exactly one respect — the current column — because [`Text`] is a
/// **delta**: consecutive events continue a line rather than each starting one,
/// and wrapping is only correct if the renderer remembers how far along that
/// line it is.
///
/// [`Text`]: HarnessEvent::Text
#[derive(Debug)]
pub struct Renderer {
    cols: usize,
    col: usize,
}

impl Renderer {
    /// `cols` is the pane's width at the time the pane was created. It is not
    /// updated on resize, and that is the rule above rather than an oversight.
    ///
    /// A `0` — which a UI really does report mid-layout — is treated as **1**,
    /// the narrowest real terminal, not as a guessed comfortable width. The
    /// floor exists only so the wrapper cannot be asked to fit a character into
    /// no columns; anything above it is the caller's number and is honoured.
    /// An earlier draft clamped to 20 and CI caught it immediately: that floor
    /// silently overrode every genuinely narrow pane, which is a made-up width
    /// rendered as though it were the pane's own.
    pub fn new(cols: u16) -> Self {
        Renderer {
            cols: (cols as usize).max(1),
            col: 0,
        }
    }

    /// Render one event. Returns the bytes to append to the pane's ring — empty
    /// when the event draws nothing.
    pub fn render(&mut self, ev: &HarnessEvent) -> Vec<u8> {
        let mut out = String::new();
        match ev {
            HarnessEvent::Booted {
                session,
                model,
                capabilities: _,
            } => {
                let model = model.as_deref().unwrap_or("model unknown");
                let session = session.as_deref().unwrap_or("session not yet known");
                self.meta(&mut out, &format!("claude · {model} · {session}"));
            }
            HarnessEvent::TurnStarted { .. } => {
                // A blank line between turns and nothing else. The turn number
                // is loomux's own counter (see `TurnId`); printing it would put
                // an internal identity on the human's screen.
                self.newline(&mut out);
                self.newline(&mut out);
            }
            HarnessEvent::Text { delta, .. } => self.text(&mut out, delta),
            HarnessEvent::ToolCall { name, input, .. } => {
                let preview = preview_input(input);
                self.meta(&mut out, &format!("> {name}({preview})"));
            }
            HarnessEvent::ToolResult { ok, .. } => {
                self.meta(&mut out, if *ok { "  ok" } else { "  failed" });
            }
            HarnessEvent::PermissionRequest { tool, input, .. } => {
                let preview = preview_input(input);
                // Not dim: this is the one line that is waiting for a human, and
                // dimming it would put the thing the pane is blocked on in the
                // least visible style on the screen.
                self.newline(&mut out);
                self.line(&mut out, &format!("[permission] {tool}({preview})"));
            }
            HarnessEvent::PermissionSettled { decision, by, .. } => {
                let d = match decision {
                    Decision::Allow => "allowed",
                    Decision::Deny => "denied",
                };
                let by = match by {
                    DecisionSource::Policy => "by policy",
                    DecisionSource::Human => "by the human",
                    DecisionSource::PaneExited => "— the pane exited first",
                };
                self.meta(&mut out, &format!("  {d} {by}"));
            }
            HarnessEvent::TurnEnded {
                usage, cost, stop, ..
            } => {
                self.meta(&mut out, &turn_summary(usage.as_ref(), cost.as_ref(), stop));
            }
            HarnessEvent::Compacted {
                trigger,
                pre_tokens,
            } => {
                let t = match trigger {
                    CompactTrigger::Manual => "manual",
                    CompactTrigger::Auto => "auto",
                };
                let before = pre_tokens
                    .map(|n| format!(", {n} tokens before"))
                    .unwrap_or_default();
                self.meta(&mut out, &format!("-- compacted ({t}{before})"));
            }
            HarnessEvent::Exited { code } => {
                let code = code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".into());
                self.meta(&mut out, &format!("-- exited ({code})"));
            }
            // A structured pane never emits one, and a PTY pane's bytes are its
            // own — rendering inferred evidence into a transcript would put a
            // heuristic on screen in the same style as a reported fact, which is
            // the confusion `ObservedEvent` exists to prevent.
            HarnessEvent::Observed(_) => {}
        }
        out.into_bytes()
    }

    /// A loomux annotation: its own line, dimmed, wrapped.
    fn meta(&mut self, out: &mut String, s: &str) {
        self.newline(out);
        out.push_str(DIM);
        self.wrapped(out, s);
        out.push_str(RESET);
        self.newline(out);
    }

    /// A line of loomux's own, undimmed.
    fn line(&mut self, out: &mut String, s: &str) {
        self.newline(out);
        self.wrapped(out, s);
        self.newline(out);
    }

    /// Model text: continues the current line, wraps, honours its own newlines.
    fn text(&mut self, out: &mut String, s: &str) {
        for (i, part) in s.split('\n').enumerate() {
            if i > 0 {
                self.hard_newline(out);
            }
            self.wrapped(out, part);
        }
    }

    /// Emit `s`, breaking at `cols`.
    ///
    /// Breaks at the column, not at a word boundary. Word wrapping would need to
    /// buffer a word before deciding, and this renderer is fed **deltas** — a
    /// word routinely arrives split across two events, so a word-wrapper would
    /// either hold text back (latency the human sees as stalling) or break in
    /// the wrong place anyway.
    fn wrapped(&mut self, out: &mut String, s: &str) {
        for c in s.chars() {
            if self.col >= self.cols {
                out.push_str(NEWLINE);
                self.col = 0;
            }
            out.push(c);
            self.col += 1;
        }
    }

    /// Start a new line unless already at column 0.
    fn newline(&mut self, out: &mut String) {
        if self.col > 0 {
            out.push_str(NEWLINE);
            self.col = 0;
        }
    }

    /// Start a new line even at column 0 — a blank line the model asked for.
    fn hard_newline(&mut self, out: &mut String) {
        out.push_str(NEWLINE);
        self.col = 0;
    }
}

/// A tool call's arguments, on one line, bounded.
///
/// A `command` string is shown as itself, because for `Bash` it is the whole
/// point and the surrounding JSON is noise. Anything else is compact JSON.
fn preview_input(input: &serde_json::Value) -> String {
    let raw = match input.get("command").and_then(serde_json::Value::as_str) {
        Some(cmd) => cmd.to_string(),
        None => match input {
            serde_json::Value::Null => String::new(),
            other => other.to_string(),
        },
    };
    // One line, always: a newline inside a preview would break the "one
    // collapsed line each" property that keeps a busy pane readable.
    let raw = raw.replace(['\n', '\r'], " ");
    let (cut, truncated) = super::truncate_on_char_boundary(&raw, TOOL_PREVIEW_BYTES);
    if truncated {
        format!("{cut}…")
    } else {
        cut.to_string()
    }
}

/// The one-line summary a turn ends on.
///
/// Reads [`Usage::call_cumulative`] and says **cumulative**, because that figure
/// is the running total for the whole call and not this turn's spend — a summary
/// that printed it as a per-turn number would be the exact misreading §7's traps
/// exist to prevent, on the most-read surface there is.
fn turn_summary(usage: Option<&Usage>, cost: Option<&Cost>, stop: &StopReason) -> String {
    let stop = match stop {
        StopReason::Completed => "done".to_string(),
        StopReason::MaxTurns => "stopped: turn limit".to_string(),
        StopReason::Aborted => "stopped: aborted".to_string(),
        StopReason::Error => "stopped: error".to_string(),
        StopReason::Other(s) if s.is_empty() => "stopped".to_string(),
        StopReason::Other(s) => format!("stopped: {s}"),
    };
    let mut parts = vec![format!("-- {stop}")];
    if let Some(u) = usage {
        parts.push(format!("{} tokens (call total)", u.call_cumulative.total()));
    }
    if let Some(c) = cost {
        // "est." is not decoration: the vendor calls this figure a client-side
        // estimate and says not to make financial decisions from it, so the
        // screen says estimate too.
        parts.push(format!("${:.4} est.", c.usd));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{ObservedEvent, RequestId, Tokens, ToolUseId, TurnId};

    fn render_all(cols: u16, evs: &[HarnessEvent]) -> String {
        let mut r = Renderer::new(cols);
        let mut out = Vec::new();
        for ev in evs {
            out.extend(r.render(ev));
        }
        String::from_utf8(out).expect("the renderer must emit valid UTF-8")
    }

    #[test]
    fn no_output_can_rewrite_the_screen() {
        // §5.2's rule, as the assertion the module header names. The renderer is
        // forward-only: any cursor movement, erase, or bare carriage return
        // would let it repaint, which is CLAUDE.md constraint 1's damage
        // arriving without a ConPTY resize to grep for.
        let out = render_all(
            40,
            &[
                HarnessEvent::Booted {
                    session: Some("s".into()),
                    model: Some("opus".into()),
                    capabilities: vec![],
                },
                HarnessEvent::TurnStarted { turn: TurnId(0) },
                HarnessEvent::Text {
                    turn: TurnId(0),
                    delta: "a very long line that certainly wraps past forty columns".into(),
                },
                HarnessEvent::ToolCall {
                    turn: TurnId(0),
                    id: ToolUseId("t".into()),
                    name: "Bash".into(),
                    input: serde_json::json!({"command": "git status"}),
                },
                HarnessEvent::TurnEnded {
                    turn: TurnId(0),
                    usage: None,
                    cost: None,
                    stop: StopReason::Completed,
                },
            ],
        );
        // A BARE carriage return — one not followed by `LF` — is what can
        // repaint a line, and it is what this bans. An earlier version banned
        // every `\r`, which is the wrong rule stated with the right words: it
        // forbade the CRLF this module must emit (see `NEWLINE`), so it would
        // have reddened on the fix and left a green suite asserting the defect
        // as the contract.
        let bytes: Vec<char> = out.chars().collect();
        for (i, c) in bytes.iter().enumerate() {
            if *c == '\r' {
                assert_eq!(
                    bytes.get(i + 1),
                    Some(&'\n'),
                    "a bare CR at {i} can return the cursor without advancing, \
                     which repaints the line: {out:?}"
                );
            }
        }
        // The other direction, and the one that actually broke: a lone `LF` is
        // an INDEX on a terminal without `convertEol`, so every line after it
        // staircases. Every newline this module emits must be a full CRLF.
        for (i, c) in bytes.iter().enumerate() {
            if *c == '\n' {
                assert_eq!(
                    if i == 0 { None } else { bytes.get(i - 1) },
                    Some(&'\r'),
                    "a lone LF at {i} — the transcript staircases from here: {out:?}"
                );
            }
        }
        // Positive control for the pair above: this output really does contain
        // newlines, so neither loop is passing over a string with none.
        assert!(
            out.matches("\r\n").count() >= 3,
            "the two assertions above must have had CRLFs to inspect: {out:?}"
        );
        for seq in ["\x1b[A", "\x1b[B", "\x1b[C", "\x1b[D", "\x1b[H", "\x1b[J", "\x1b[K", "\x1b[s", "\x1b[u"] {
            assert!(
                !out.contains(seq),
                "the renderer emitted {seq:?}, which can rewrite what it already \
                 drew: {out:?}"
            );
        }
        // The positive control: it did emit SOMETHING, so the absences above are
        // about the content and not about an empty string.
        assert!(out.contains("git status"), "{out:?}");
        // Every attribute it opens, it closes on the same pass.
        assert_eq!(
            out.matches(DIM).count(),
            out.matches(RESET).count(),
            "an unbalanced SGR leaves the pane's terminal in an attribute state: {out:?}"
        );
    }

    #[test]
    fn text_deltas_continue_one_line_and_wrap_at_the_column() {
        // The property that makes `Text` a delta rather than a line: three
        // events, one paragraph. A renderer that started a line per event would
        // produce three lines here and pass any test that only checked content.
        let out = render_all(
            10,
            &[
                HarnessEvent::Text {
                    turn: TurnId(0),
                    delta: "abcde".into(),
                },
                HarnessEvent::Text {
                    turn: TurnId(0),
                    delta: "fghij".into(),
                },
                HarnessEvent::Text {
                    turn: TurnId(0),
                    delta: "klmno".into(),
                },
            ],
        );
        assert_eq!(
            out, "abcdefghij\r\nklmno",
            "wrapped at 10, not per event, and the break is a CRLF: {out:?}"
        );
    }

    #[test]
    fn a_newline_inside_model_text_is_honoured_and_a_wrap_is_not_a_paragraph() {
        let out = render_all(
            80,
            &[HarnessEvent::Text {
                turn: TurnId(0),
                delta: "one\n\ntwo".into(),
            }],
        );
        assert_eq!(out, "one\r\n\r\ntwo");
    }

    #[test]
    fn a_tool_call_is_one_line_however_long_its_input_is() {
        // The collapsed-line property, tested on the input that actually breaks
        // it: a `command` string carrying REAL newlines. A structured argument
        // would not — `Value::to_string` escapes newlines to `\n`, so that
        // fixture is already one line and the test would pass with the flattening
        // deleted. Length alone would not either; truncation hides it.
        let out = render_all(
            200,
            &[HarnessEvent::ToolCall {
                turn: TurnId(0),
                id: ToolUseId("t".into()),
                name: "Bash".into(),
                input: serde_json::json!({"command": "echo one\necho two\necho three"}),
            }],
        );
        let body = out.replace(DIM, "").replace(RESET, "");
        // Split on the CRLF this module emits, not on a bare `\n`: splitting on
        // `\n` leaves a `\r` on every part, so an empty line reads as non-empty
        // and the count below silently stops meaning "lines drawn".
        let drawn: Vec<&str> = body.split(NEWLINE).filter(|l| !l.is_empty()).collect();
        assert_eq!(drawn.len(), 1, "a tool call must draw exactly one line: {drawn:?}");
        assert!(drawn[0].starts_with("> Bash("), "{drawn:?}");
        assert!(
            drawn[0].contains("echo one echo two"),
            "the newlines must become spaces, not line breaks: {drawn:?}"
        );
    }

    #[test]
    fn a_long_tool_preview_is_truncated_and_says_so() {
        let long = "x".repeat(TOOL_PREVIEW_BYTES + 50);
        let out = render_all(
            500,
            &[HarnessEvent::ToolCall {
                turn: TurnId(0),
                id: ToolUseId("t".into()),
                name: "Bash".into(),
                input: serde_json::json!({ "command": long }),
            }],
        );
        assert!(out.contains('…'), "truncation must be visible: {out:?}");
        let xs = out.matches('x').count();
        assert_eq!(xs, TOOL_PREVIEW_BYTES, "truncated to the cap, not to a guess");
    }

    #[test]
    fn a_permission_request_is_the_one_line_that_is_not_dimmed() {
        // It is what the pane is blocked on, so it must not be drawn in the
        // least visible style on the screen. Asserted as a CONTRAST against a
        // neighbouring meta line, so a change that dimmed everything fails here
        // rather than passing because nothing is dim.
        let out = render_all(
            80,
            &[
                HarnessEvent::ToolCall {
                    turn: TurnId(0),
                    id: ToolUseId("t".into()),
                    name: "Bash".into(),
                    input: serde_json::json!({"command": "rm -rf /"}),
                },
                HarnessEvent::PermissionRequest {
                    id: RequestId("r".into()),
                    tool: "Bash".into(),
                    input: serde_json::json!({"command": "rm -rf /"}),
                },
            ],
        );
        let perm_line = out
            .lines()
            .find(|l| l.contains("[permission]"))
            .expect("the request must be drawn");
        assert!(
            !perm_line.contains(DIM),
            "the blocked-on line must not be dimmed: {perm_line:?}"
        );
        let tool_line = out
            .lines()
            .find(|l| l.contains("> Bash("))
            .expect("the tool call must be drawn");
        assert!(
            tool_line.contains(DIM),
            "the control: an ordinary meta line IS dimmed, so the assertion \
             above is about contrast rather than about nothing being dim: {tool_line:?}"
        );
    }

    #[test]
    fn the_turn_summary_says_the_token_figure_is_a_call_total_and_the_cost_an_estimate() {
        // §7's two traps on the most-read surface there is. A summary that
        // printed the cumulative figure as this turn's spend, or a vendor
        // estimate as a bill, would be wrong in a way only prose catches.
        let out = render_all(
            120,
            &[HarnessEvent::TurnEnded {
                turn: TurnId(0),
                usage: Some(Usage {
                    call_cumulative: Tokens {
                        input: 100,
                        output: 20,
                        cache_read: 5,
                        cache_creation: 1,
                    },
                    this_turn_main_loop: Some(Tokens {
                        input: 10,
                        output: 2,
                        cache_read: 0,
                        cache_creation: 0,
                    }),
                    per_model: vec![],
                }),
                cost: Some(Cost {
                    usd: 0.0731,
                    basis: crate::harness::CostBasis::HarnessEstimate,
                }),
                stop: StopReason::Completed,
            }],
        );
        assert!(out.contains("126 tokens (call total)"), "{out:?}");
        assert!(
            !out.contains("12 tokens"),
            "the per-turn figure must not be the one drawn: {out:?}"
        );
        assert!(out.contains("$0.0731 est."), "{out:?}");
    }

    #[test]
    fn an_unknown_stop_reason_reaches_the_screen_instead_of_being_flattened() {
        let out = render_all(
            120,
            &[HarnessEvent::TurnEnded {
                turn: TurnId(0),
                usage: None,
                cost: None,
                stop: StopReason::Other("rapid_refill_breaker".into()),
            }],
        );
        assert!(
            out.contains("rapid_refill_breaker"),
            "a reason this build does not know is exactly what a human \
             debugging a stuck pane needs: {out:?}"
        );
    }

    #[test]
    fn inferred_evidence_is_never_drawn() {
        // A structured pane emits none of these; the pin is that if one ever
        // reached this renderer it would not be painted in the same style as a
        // reported fact.
        let out = render_all(
            80,
            &[
                HarnessEvent::Observed(ObservedEvent::ReadyMarker),
                HarnessEvent::Observed(ObservedEvent::QuestionSuspected {
                    matched: "Do you want to proceed?".into(),
                }),
                HarnessEvent::Observed(ObservedEvent::Quiet),
            ],
        );
        assert!(out.is_empty(), "inferred evidence must draw nothing: {out:?}");
    }

    #[test]
    fn a_zero_width_pane_terminates_and_is_not_widened_to_a_guess() {
        // Two properties, and the second is the one an earlier draft got wrong.
        // A zero must not make the wrapper unable to place a character — and it
        // must not be silently replaced by a comfortable width either, because
        // that renders a number the pane never reported.
        let out = render_all(
            0,
            &[HarnessEvent::Text {
                turn: TurnId(0),
                delta: "abc".into(),
            }],
        );
        assert_eq!(
            out, "a\r\nb\r\nc",
            "zero is treated as one column, not as twenty"
        );
        // The control: a real narrow width is honoured exactly, so the floor
        // above cannot be creeping upward unnoticed.
        let out = render_all(
            3,
            &[HarnessEvent::Text {
                turn: TurnId(0),
                delta: "abcdef".into(),
            }],
        );
        assert_eq!(out, "abc\r\ndef");
    }
}

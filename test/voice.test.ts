// Unit tests for the voice-prompt pure logic (#58): the insertion-target
// decision and the push-to-talk state machine. DOM wiring (voicecontrol.ts) is
// exercised by hand. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  resolveVoiceTargetKind,
  nextVoiceState,
  type VoiceMachineState,
} from "../src/voice.ts";

test("resolveVoiceTargetKind: focused compose box wins", () => {
  assert.equal(
    resolveVoiceTargetKind({ composeFocused: true, hasActivePane: true, paneHasTerminal: true }),
    "compose",
  );
  // Even with no active pane, an explicitly focused compose box is the target.
  assert.equal(
    resolveVoiceTargetKind({ composeFocused: true, hasActivePane: false, paneHasTerminal: false }),
    "compose",
  );
});

test("resolveVoiceTargetKind: falls back to the active pane's terminal", () => {
  assert.equal(
    resolveVoiceTargetKind({ composeFocused: false, hasActivePane: true, paneHasTerminal: true }),
    "terminal",
  );
});

test("resolveVoiceTargetKind: nothing focusable → none", () => {
  assert.equal(
    resolveVoiceTargetKind({ composeFocused: false, hasActivePane: false, paneHasTerminal: false }),
    "none",
  );
});

test("resolveVoiceTargetKind: a pane with no terminal (file explorer) refuses the capture", () => {
  // #214: a files pane has no PTY, and a welcome/dormant pane hasn't opened one.
  // Without this the capture would run to completion and paste the transcript into
  // an xterm that was never opened — the words silently vanish. Refuse up front.
  assert.equal(
    resolveVoiceTargetKind({ composeFocused: false, hasActivePane: true, paneHasTerminal: false }),
    "none",
  );
});

test("voice machine: full happy-path capture cycle", () => {
  let s: VoiceMachineState = "idle";
  s = nextVoiceState(s, "toggle"); // press to record
  assert.equal(s, "starting"); // awaiting backend start
  s = nextVoiceState(s, "ackRecording"); // mic confirmed live
  assert.equal(s, "recording");
  s = nextVoiceState(s, "toggle"); // press to stop
  assert.equal(s, "transcribing"); // local transcription running
  s = nextVoiceState(s, "settle"); // transcript delivered
  assert.equal(s, "idle");
});

test("voice machine: failed start settles back to idle", () => {
  let s: VoiceMachineState = nextVoiceState("idle", "toggle");
  assert.equal(s, "starting");
  s = nextVoiceState(s, "settle"); // start rejected (no mic, etc.)
  assert.equal(s, "idle");
});

test("voice machine: Esc cancels an active recording immediately", () => {
  assert.equal(nextVoiceState("recording", "cancel"), "idle");
});

test("voice machine: Esc during transcribing cancels (kills subprocess)", () => {
  assert.equal(nextVoiceState("transcribing", "cancel"), "idle");
});

test("voice machine: toggle is ignored while transcribing (no interrupt)", () => {
  assert.equal(nextVoiceState("transcribing", "toggle"), "transcribing");
});

test("voice machine: toggle is ignored while starting (no double-start)", () => {
  assert.equal(nextVoiceState("starting", "toggle"), "starting");
});

test("voice machine: Esc while starting aborts to idle", () => {
  assert.equal(nextVoiceState("starting", "cancel"), "idle");
});

test("voice machine: idle ignores non-toggle events", () => {
  assert.equal(nextVoiceState("idle", "settle"), "idle");
  assert.equal(nextVoiceState("idle", "cancel"), "idle");
  assert.equal(nextVoiceState("idle", "ackRecording"), "idle");
});

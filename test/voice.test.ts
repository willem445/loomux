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
    resolveVoiceTargetKind({ composeFocused: true, hasActivePane: true }),
    "compose",
  );
  // Even with no active pane, an explicitly focused compose box is the target.
  assert.equal(
    resolveVoiceTargetKind({ composeFocused: true, hasActivePane: false }),
    "compose",
  );
});

test("resolveVoiceTargetKind: falls back to the active pane's terminal", () => {
  assert.equal(
    resolveVoiceTargetKind({ composeFocused: false, hasActivePane: true }),
    "terminal",
  );
});

test("resolveVoiceTargetKind: nothing focusable → none", () => {
  assert.equal(
    resolveVoiceTargetKind({ composeFocused: false, hasActivePane: false }),
    "none",
  );
});

test("voice machine: full happy-path capture cycle", () => {
  let s: VoiceMachineState = "idle";
  s = nextVoiceState(s, "toggle"); // press to record
  assert.equal(s, "busy"); // awaiting backend start
  s = nextVoiceState(s, "ackRecording"); // mic confirmed live
  assert.equal(s, "recording");
  s = nextVoiceState(s, "toggle"); // press to stop
  assert.equal(s, "busy"); // transcribing
  s = nextVoiceState(s, "settle"); // transcript delivered
  assert.equal(s, "idle");
});

test("voice machine: failed start settles back to idle", () => {
  let s: VoiceMachineState = nextVoiceState("idle", "toggle");
  assert.equal(s, "busy");
  s = nextVoiceState(s, "settle"); // start rejected (no mic, etc.)
  assert.equal(s, "idle");
});

test("voice machine: Esc cancels an active recording", () => {
  let s: VoiceMachineState = "recording";
  s = nextVoiceState(s, "cancel");
  assert.equal(s, "busy"); // cancelling
  s = nextVoiceState(s, "settle");
  assert.equal(s, "idle");
});

test("voice machine: busy swallows stray toggles (no double-start)", () => {
  assert.equal(nextVoiceState("busy", "toggle"), "busy");
  // Esc while busy is also ignored until the in-flight op settles.
  assert.equal(nextVoiceState("busy", "cancel"), "busy");
});

test("voice machine: idle ignores non-toggle events", () => {
  assert.equal(nextVoiceState("idle", "settle"), "idle");
  assert.equal(nextVoiceState("idle", "cancel"), "idle");
  assert.equal(nextVoiceState("idle", "ackRecording"), "idle");
});

// Pure, DOM-free logic for the voice-prompt feature (#58): the insertion-target
// decision and the push-to-talk state machine. Kept here so both are unit-
// testable in Node (test/voice.test.ts); the DOM wiring lives in voicecontrol.ts.

/** Where a finished transcript should land, decided at capture START from what
 *  currently holds focus:
 *   - "compose"  → the focused steer/compose textbox (insert at caret);
 *   - "terminal" → the active pane's terminal (paste into its PTY, no newline);
 *   - "none"     → nothing focusable to receive it (no active pane) → don't start. */
export type VoiceTargetKind = "compose" | "terminal" | "none";

/** Decide the target kind. A focused compose box wins (the human is clearly
 *  composing there); otherwise the active pane's terminal receives it; with no
 *  active pane there's nowhere to put it. */
export function resolveVoiceTargetKind(opts: {
  composeFocused: boolean;
  hasActivePane: boolean;
}): VoiceTargetKind {
  if (opts.composeFocused) return "compose";
  if (opts.hasActivePane) return "terminal";
  return "none";
}

/** Push-to-talk lifecycle. `busy` is the transient state while an async
 *  backend round-trip (start, or stop+transcribe, or cancel) is in flight —
 *  toggles are ignored there so a double-tap can't double-start or race. */
export type VoiceMachineState = "idle" | "recording" | "busy";

/** Events driving the machine:
 *   - "toggle"        the hotkey / mic button was pressed;
 *   - "ackRecording"  the backend confirmed the mic is live;
 *   - "cancel"        Esc — abandon an in-flight recording;
 *   - "settle"        the async op finished (started-failed, transcribed, or
 *                     cancelled) → back to idle. */
export type VoiceEvent = "toggle" | "ackRecording" | "cancel" | "settle";

/** Pure transition function. Unknown (state, event) pairs are no-ops (return the
 *  current state), which is what makes `busy` swallow stray toggles. */
export function nextVoiceState(state: VoiceMachineState, event: VoiceEvent): VoiceMachineState {
  switch (state) {
    case "idle":
      // Only a deliberate toggle starts a capture; it goes through `busy`
      // until the backend acks the live mic.
      return event === "toggle" ? "busy" : "idle";
    case "recording":
      // Toggling again stops (→ transcribe); Esc cancels. Both go via `busy`.
      return event === "toggle" || event === "cancel" ? "busy" : "recording";
    case "busy":
      // `ackRecording` promotes a successful start to live; `settle` ends any
      // in-flight op. Toggles are ignored while busy.
      if (event === "ackRecording") return "recording";
      if (event === "settle") return "idle";
      return "busy";
  }
}

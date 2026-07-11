// Pure, DOM-free logic for the voice-prompt feature (#58): the insertion-target
// decision and the push-to-talk state machine. Kept here so both are unit-
// testable in Node (test/voice.test.ts); the DOM wiring lives in voicecontrol.ts.

/** Where a finished transcript should land, decided at capture START from what
 *  currently holds focus:
 *   - "compose"  → the focused steer/compose textbox (insert at caret);
 *   - "terminal" → the active pane's terminal (paste into its PTY, no newline);
 *   - "none"     → nothing focusable to receive it → don't start. */
export type VoiceTargetKind = "compose" | "terminal" | "none";

/** Decide the target kind. A focused compose box wins (the human is clearly
 *  composing there); otherwise the active pane's terminal receives it; with
 *  nowhere to put it, we refuse to start (rather than record into the void). */
export function resolveVoiceTargetKind(opts: {
  composeFocused: boolean;
  hasActivePane: boolean;
  /** Does the active pane have an OPEN terminal to paste into? Not every pane
   *  does: a file-explorer pane (#214) never has one, and a welcome / dormant pane
   *  hasn't opened one yet. Aiming a transcript at those means pasting into an
   *  unopened xterm — the words just vanish. Better to refuse the capture up front
   *  than to record, transcribe, and silently drop it. Ignored when a compose box
   *  is focused (that path doesn't touch the terminal). */
  paneHasTerminal: boolean;
}): VoiceTargetKind {
  if (opts.composeFocused) return "compose";
  if (opts.hasActivePane && opts.paneHasTerminal) return "terminal";
  return "none";
}

/** Push-to-talk lifecycle:
 *   - "idle"         nothing happening;
 *   - "starting"     voice_start round-trip in flight (awaiting the mic-live ack);
 *   - "recording"    mic is live and capturing;
 *   - "transcribing" mic stopped; the (possibly multi-minute) local transcription
 *                    is running — the webview stays responsive because the backend
 *                    command is async, and this state drives a "Transcribing…"
 *                    indicator.
 *  A toggle (hotkey / button) is ignored while `starting` or `transcribing`; Esc
 *  (`cancel`) abandons any active phase (and, in `transcribing`, kills the
 *  subprocess). */
export type VoiceMachineState = "idle" | "starting" | "recording" | "transcribing";

/** Events driving the machine:
 *   - "toggle"        the hotkey / mic button was pressed;
 *   - "ackRecording"  the backend confirmed the mic is live;
 *   - "cancel"        Esc — abandon the active phase;
 *   - "settle"        the async op finished (start failed, or transcript ready). */
export type VoiceEvent = "toggle" | "ackRecording" | "cancel" | "settle";

/** Pure transition function. Unknown (state, event) pairs are no-ops (return the
 *  current state) — that's what makes `starting`/`transcribing` swallow stray
 *  toggles so a double-tap can't double-start or interrupt a transcription. */
export function nextVoiceState(state: VoiceMachineState, event: VoiceEvent): VoiceMachineState {
  switch (state) {
    case "idle":
      // Only a deliberate toggle starts a capture.
      return event === "toggle" ? "starting" : "idle";
    case "starting":
      // Mic confirmed live → recording; a failed start or a cancel → idle.
      if (event === "ackRecording") return "recording";
      if (event === "settle" || event === "cancel") return "idle";
      return "starting"; // toggle ignored mid-start
    case "recording":
      if (event === "toggle") return "transcribing"; // stop → transcribe
      if (event === "cancel") return "idle"; // Esc → discard
      return "recording";
    case "transcribing":
      // Transcript delivered, or Esc cancelled (subprocess killed) → idle.
      if (event === "settle" || event === "cancel") return "idle";
      return "transcribing"; // toggle ignored while transcribing
  }
}

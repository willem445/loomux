// Global voice-capture controller (#58). Exactly ONE capture may be in flight
// across the whole app, so this is a singleton that owns the push-to-talk state
// machine (pure logic in voice.ts) and routes the finished transcript to the
// target chosen at capture start: a focused compose box (insert at caret) or the
// active pane's terminal (paste into its PTY — bracketed, never auto-submitted).
//
// Transcription is async on the backend (the whisper subprocess can run for
// minutes on a large model), so the flow has a visible "transcribing" phase and
// Esc can cancel it. A generation counter guards against a late transcript
// landing after the user cancelled or closed the pane.

import { voiceStart, voiceStop, voiceCancel } from "./pty";
import { showToast } from "./toast";
import {
  resolveVoiceTargetKind,
  nextVoiceState,
  type VoiceMachineState,
} from "./voice";

/** Visual phase shown on the target while a capture is live. */
export type VoicePhase = "recording" | "transcribing" | "off";

/** The pane-side surface the controller drives. `Pane` implements this; the
 *  controller depends only on this interface so there's no import cycle. */
export interface VoiceTargetPane {
  /** Is this pane's steer/compose box the focused element right now? */
  isComposeFocused(): boolean;
  /** Insert transcribed text into the compose box at the caret (no submit). */
  insertTranscript(text: string): void;
  /** Paste transcribed text into the terminal's PTY (bracketed, no newline). */
  pasteToTerminal(text: string): void;
  /** Reflect the capture phase on the target's indicator (mic button for a
   *  compose target, overlay badge for a terminal target). */
  setVoicePhase(kind: "compose" | "terminal", phase: VoicePhase): void;
  /** Show a transient status/error line on the strip (compose panes only). */
  showVoiceStatus(msg: string): void;
}

type Target = { pane: VoiceTargetPane; kind: "compose" | "terminal" };

class VoiceController {
  private state: VoiceMachineState = "idle";
  private target: Target | null = null;
  private getActivePane: () => VoiceTargetPane | null = () => null;
  private escHandler: ((e: KeyboardEvent) => void) | null = null;
  /** Bumped on every cancel/dispose so a late begin()/stop() result is dropped
   *  instead of landing in a target the user has moved on from. */
  private gen = 0;

  /** Wire the controller to the grid so the hotkey can find the active pane. */
  init(getActivePane: () => VoiceTargetPane | null): void {
    this.getActivePane = getActivePane;
  }

  /** Alt+S from anywhere: start a capture aimed at whatever holds focus, stop a
   *  live recording (→ transcribe), or — while starting/transcribing — do
   *  nothing (Esc is how you bail out of those). */
  toggleFromHotkey(): void {
    this.toggle(() => {
      const pane = this.getActivePane();
      const kind = resolveVoiceTargetKind({
        composeFocused: !!pane?.isComposeFocused(),
        hasActivePane: !!pane,
      });
      if (kind === "none" || !pane) {
        showToast("Voice: focus a pane or the compose box first.");
        return null;
      }
      return { pane, kind };
    });
  }

  /** Mic button on a compose strip: same toggle, but the target is always this
   *  strip's compose box. */
  toggleForCompose(pane: VoiceTargetPane): void {
    this.toggle(() => ({ pane, kind: "compose" }));
  }

  /** A pane is going away — abandon the capture if it was the target so the mic
   *  stream / whisper subprocess is released and no transcript lands in a dead
   *  pane. */
  notifyPaneDisposed(pane: VoiceTargetPane): void {
    if (this.target?.pane === pane && this.state !== "idle") {
      this.gen++;
      this.removeEsc();
      this.state = "idle";
      this.target = null;
      void voiceCancel().catch(() => {});
    }
  }

  // ----- internals -----

  /** Shared toggle: begin from idle, stop from recording, ignore otherwise. */
  private toggle(resolve: () => Target | null): void {
    if (this.state === "idle") {
      const t = resolve();
      if (t) void this.begin(t);
    } else if (this.state === "recording") {
      this.stop();
    }
    // starting / transcribing → ignored (use Esc to cancel)
  }

  /** Start a capture toward `t`. Errors (no mic, permission) settle back to idle
   *  with a message on the target. */
  private async begin(t: Target): Promise<void> {
    const myGen = this.gen;
    this.state = nextVoiceState(this.state, "toggle"); // → starting
    this.target = t;
    try {
      await voiceStart();
      if (this.gen !== myGen) return; // cancelled during start
      this.state = nextVoiceState(this.state, "ackRecording"); // → recording
      this.setPhase(t, "recording");
      this.installEsc(); // Esc now cancels; stays through transcribing
    } catch (err) {
      if (this.gen !== myGen) return;
      this.state = nextVoiceState(this.state, "settle"); // → idle
      this.target = null;
      this.status(t, `Mic: ${String(err)}`);
    }
  }

  /** Stop recording and run the (async) transcription, showing a transcribing
   *  indicator meanwhile. */
  private stop(): void {
    const t = this.target;
    this.state = nextVoiceState(this.state, "toggle"); // recording → transcribing
    if (t) this.setPhase(t, "transcribing");
    void this.runStop(t);
  }

  private async runStop(t: Target | null): Promise<void> {
    const myGen = this.gen;
    try {
      const text = await voiceStop();
      if (this.gen !== myGen) return; // cancelled/disposed mid-transcribe
      if (t) {
        this.setPhase(t, "off");
        if (text) this.deliver(t, text);
        else this.status(t, "Voice: no speech detected.");
      }
    } catch (err) {
      if (this.gen !== myGen) return;
      if (t) {
        this.setPhase(t, "off");
        this.status(t, `Transcription: ${String(err)}`);
      }
    } finally {
      if (this.gen === myGen) {
        this.removeEsc();
        this.state = nextVoiceState(this.state, "settle"); // → idle
        this.target = null;
      }
    }
  }

  /** Esc while recording (discard) or transcribing (kill the subprocess). */
  private cancel(): void {
    if (this.state === "idle") return;
    const t = this.target;
    this.gen++; // invalidate any in-flight begin()/runStop()
    this.removeEsc();
    if (t) this.setPhase(t, "off");
    this.state = nextVoiceState(this.state, "cancel"); // → idle
    this.target = null;
    void voiceCancel().catch(() => {}); // stops the mic and/or kills whisper
    if (t) this.status(t, "Voice: cancelled.");
  }

  private deliver(t: Target, text: string): void {
    if (t.kind === "compose") t.pane.insertTranscript(text);
    else t.pane.pasteToTerminal(text);
  }

  private setPhase(t: Target, phase: VoicePhase): void {
    t.pane.setVoicePhase(t.kind, phase);
  }

  /** Errors/status go to the strip for compose targets (it has a status line)
   *  and to a toast for terminal targets (no strip to write to). */
  private status(t: Target, msg: string): void {
    if (t.kind === "compose") t.pane.showVoiceStatus(msg);
    else showToast(msg);
  }

  /** While recording or transcribing, Esc cancels instead of reaching the shell
   *  or the compose box. Capture-phase so it wins over both. Removed once idle. */
  private installEsc(): void {
    if (this.escHandler) return;
    this.escHandler = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      this.cancel();
    };
    document.addEventListener("keydown", this.escHandler, { capture: true });
  }

  private removeEsc(): void {
    if (!this.escHandler) return;
    document.removeEventListener("keydown", this.escHandler, { capture: true });
    this.escHandler = null;
  }
}

/** The one and only voice controller (single global capture). */
export const voiceController = new VoiceController();

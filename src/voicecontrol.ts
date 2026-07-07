// Global voice-capture controller (#58). Exactly ONE capture may be in flight
// across the whole app, so this is a singleton that owns the push-to-talk state
// machine (pure logic in voice.ts) and routes the finished transcript to the
// target chosen at capture start: a focused compose box (insert at caret) or the
// active pane's terminal (paste into its PTY — bracketed, never auto-submitted).
//
// It stays out of pane.ts to keep that file focused and to make the single-
// capture invariant obvious: there is one controller, one state, one target.

import { voiceStart, voiceStop, voiceCancel } from "./pty";
import { showToast } from "./toast";
import {
  resolveVoiceTargetKind,
  nextVoiceState,
  type VoiceMachineState,
} from "./voice";

/** The pane-side surface the controller drives. `Pane` implements this; the
 *  controller depends only on this interface so there's no import cycle. */
export interface VoiceTargetPane {
  /** Is this pane's steer/compose box the focused element right now? */
  isComposeFocused(): boolean;
  /** Insert transcribed text into the compose box at the caret (no submit). */
  insertTranscript(text: string): void;
  /** Paste transcribed text into the terminal's PTY (bracketed, no newline). */
  pasteToTerminal(text: string): void;
  /** Toggle the mic-button "recording" pulse (compose-target indicator). */
  setMicRecording(on: boolean): void;
  /** Toggle the terminal-capture overlay badge (terminal-target indicator). */
  setTerminalRecording(on: boolean): void;
  /** Show a transient status/error line on the strip (compose panes only). */
  showVoiceStatus(msg: string): void;
}

type Target = { pane: VoiceTargetPane; kind: "compose" | "terminal" };

class VoiceController {
  private state: VoiceMachineState = "idle";
  private target: Target | null = null;
  private getActivePane: () => VoiceTargetPane | null = () => null;
  private escHandler: ((e: KeyboardEvent) => void) | null = null;

  /** Wire the controller to the grid so the hotkey can find the active pane. */
  init(getActivePane: () => VoiceTargetPane | null): void {
    this.getActivePane = getActivePane;
  }

  /** Alt+V from anywhere: stop if a capture is running, else start one aimed at
   *  whatever holds focus (compose box → caret, else active terminal). */
  toggleFromHotkey(): void {
    if (this.state !== "idle") {
      this.stopOrCancel(/* viaEsc */ false);
      return;
    }
    const pane = this.getActivePane();
    const kind = resolveVoiceTargetKind({
      composeFocused: !!pane?.isComposeFocused(),
      hasActivePane: !!pane,
    });
    if (kind === "none" || !pane) {
      showToast("Voice: focus a pane or the compose box first.");
      return;
    }
    void this.begin({ pane, kind });
  }

  /** Mic button on a compose strip: stop a running capture, else start one
   *  targeting this strip's compose box. */
  toggleForCompose(pane: VoiceTargetPane): void {
    if (this.state !== "idle") {
      this.stopOrCancel(false);
      return;
    }
    void this.begin({ pane, kind: "compose" });
  }

  /** A pane is going away — abandon the capture if it was the target so the
   *  backend mic stream is released and no transcript lands in a dead pane. */
  notifyPaneDisposed(pane: VoiceTargetPane): void {
    if (this.target?.pane === pane && this.state !== "idle") {
      this.removeEsc();
      this.state = "idle";
      this.target = null;
      void voiceCancel().catch(() => {});
    }
  }

  // ----- internals -----

  /** Start a capture toward `t`. Errors (no mic, permission) settle back to
   *  idle with a message on the target. */
  private async begin(t: Target): Promise<void> {
    this.state = nextVoiceState(this.state, "toggle"); // → busy
    this.target = t;
    try {
      await voiceStart();
      this.state = nextVoiceState(this.state, "ackRecording"); // → recording
      this.setIndicator(t, true);
      this.installEsc();
    } catch (err) {
      this.state = nextVoiceState(this.state, "settle"); // → idle
      this.target = null;
      this.status(t, `Mic: ${String(err)}`);
    }
  }

  /** Toggle/Esc while active: stop-and-transcribe (viaEsc=false) or cancel
   *  (viaEsc=true). No-op unless we're actually recording. */
  private stopOrCancel(viaEsc: boolean): void {
    if (this.state !== "recording") return; // busy → ignore
    const t = this.target;
    this.state = nextVoiceState(this.state, viaEsc ? "cancel" : "toggle"); // → busy
    if (t) this.setIndicator(t, false);
    this.removeEsc();
    if (viaEsc) void this.doCancel(t);
    else void this.doStop(t);
  }

  private async doStop(t: Target | null): Promise<void> {
    try {
      const text = await voiceStop();
      if (!t) return;
      if (text) this.deliver(t, text);
      else this.status(t, "Voice: no speech detected.");
    } catch (err) {
      if (t) this.status(t, `Transcription: ${String(err)}`);
    } finally {
      this.state = nextVoiceState(this.state, "settle"); // → idle
      this.target = null;
    }
  }

  private async doCancel(t: Target | null): Promise<void> {
    try {
      await voiceCancel();
    } catch {
      /* best-effort */
    } finally {
      this.state = nextVoiceState(this.state, "settle"); // → idle
      this.target = null;
    }
    if (t) this.status(t, "Voice: cancelled.");
  }

  private deliver(t: Target, text: string): void {
    if (t.kind === "compose") t.pane.insertTranscript(text);
    else t.pane.pasteToTerminal(text);
  }

  private setIndicator(t: Target, on: boolean): void {
    if (t.kind === "compose") t.pane.setMicRecording(on);
    else t.pane.setTerminalRecording(on);
  }

  /** Errors/status go to the strip for compose targets (it has a status line)
   *  and to a toast for terminal targets (no strip to write to). */
  private status(t: Target, msg: string): void {
    if (t.kind === "compose") t.pane.showVoiceStatus(msg);
    else showToast(msg);
  }

  /** While recording, Esc cancels the capture instead of reaching the shell or
   *  the compose box. Capture-phase so it wins over both. Removed on stop. */
  private installEsc(): void {
    if (this.escHandler) return;
    this.escHandler = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      this.stopOrCancel(true);
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

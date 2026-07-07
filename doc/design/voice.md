# Voice prompts (issue #58)

Local, open-source speech-to-text into any focused target. Push-to-talk mic
capture → on-device [whisper.cpp](https://github.com/ggml-org/whisper.cpp)
transcription → text dropped at the current focus point, never auto-submitted.
User-facing usage lives in the README ([Voice prompts](../../README.md#voice-prompts));
this note is the architecture and the decisions behind it.

## Flow

```
 Alt+V / 🎤 ─▶ voiceController.toggle ─▶ voice_start (Rust)
 (main.ts /                                └─ cpal opens the default mic on a
  mic button)                                 dedicated thread; buffers mono f32
 Alt+V / 🎤 ─▶ voiceController.stop  ─▶ voice_stop(app, state)
                                          ├─ join capture thread
                                          ├─ resample → 16 kHz mono
                                          ├─ encode WAV (scratch temp)
                                          ├─ resolve whisper runtime (below)
                                          ├─ run whisper-cli.exe (subprocess)
                                          └─ parse stdout → transcript
   deliver to target ◀── transcript ─────────┘
     compose box → insert at caret   |   terminal → xterm paste (bracketed, no \n)
```

## Frontend

- **`src/voice.ts`** — pure, DOM-free, unit-tested (`test/voice.test.ts`):
  - `resolveVoiceTargetKind` — the insertion-target decision: a focused compose
    box wins; otherwise the active pane's terminal; otherwise nothing.
  - `nextVoiceState` — the push-to-talk state machine
    (`idle → busy → recording → busy → idle`). `busy` is the async in-flight
    guard that makes a double-tap a no-op, so only one capture ever runs.
- **`src/voicecontrol.ts`** — one global `voiceController` (the single-capture
  invariant is "one controller, one state, one target"). It owns the state
  machine, calls the backend, routes the transcript, drives the recording
  indicator, and installs a capture-phase `Esc` handler *only while recording*
  (so Esc cancels voice without otherwise stealing Esc from the shell/compose
  box). Depends on panes only through the `VoiceTargetPane` interface — no import
  cycle.
- **`src/pane.ts`** implements `VoiceTargetPane`: `isComposeFocused()`,
  `insertTranscript()` (caret insert), `pasteToTerminal()` (`xterm.paste`, which
  applies bracketed-paste semantics and adds no newline), plus the two indicator
  toggles. The mic button (orchestrator strip only) and the `Alt+V` hotkey
  (any pane) both go through the controller.

**Hotkey — `Alt+V`.** Chosen over `Ctrl+Shift+V` (the terminal paste combo,
handled per-pane) and to match loomux's `Alt+<letter>` app-shortcut family
(`shortcuts.ts`). Terminals decline app shortcuts (`isAppShortcut`) so `Alt+V`
bubbles to the document handler from any focused pane.

**No PTY resize.** The terminal-capture indicator is an absolutely-positioned
overlay badge inside `.pane-term` (which is `position: relative`); it floats over
xterm and never changes the terminal's box, honoring the project's
never-resize-the-PTY rule.

## Backend (`src-tauri/src/voice.rs`)

Windows-only capture (`#[cfg(windows)]`); off Windows the three commands return a
graceful "only available on Windows" error and no audio crates are pulled. See
the cross-platform path below.

### Capture — `cpal` (native WASAPI)

- Vetted **getrandom-clean at runtime** (`cargo tree -i getrandom`): getrandom
  appears only under build-dependencies / Android-only `oboe-sys`, never in
  cpal's Windows runtime tree — so it does not import `ProcessPrng` and is safe
  on the Windows-10 baseline (see `Cargo.toml`). Pure Rust WASAPI, no C++/cmake.
- Native capture also sidesteps the WebView2 `getUserMedia` mic-permission dance,
  so it's deterministic.
- The cpal `Stream` is `!Send` on Windows, so it lives entirely on a dedicated
  capture thread controlled by a stop channel; frames are downmixed to mono and
  pushed into a shared buffer that `voice_stop` drains and resamples to 16 kHz.
- A shared error slot records input-stream faults (mic unplugged mid-record);
  `voice_stop` uses it to tell "you didn't say anything" (empty → `""`) apart
  from "the device died" (empty + error → surfaced).

#### Real-time safety of the capture buffer (long-recording bug)

The audio callback runs under a hard real-time deadline (WASAPI shared-mode
buffers are ~10 ms). The first implementation appended into a single growing
`Vec<f32>` from inside that callback — fine for short clips, but on long
recordings the `Vec`'s doubling reallocations copy multi-MB **inside the
callback**, blowing the deadline, starving/glitching the stream, and producing
audio whisper heard as silence. That was the "long recording → empty transcript"
bug: short words worked, long dictation returned nothing.

The fix is `ChunkedBuffer` (pure, unit-tested): samples accumulate in fixed-size
16,384-sample blocks that are **never reallocated** — a filled block is retired
and a fresh one started, so the per-callback cost is bounded (O(samples) plus at
most one small constant allocation per block boundary). The single contiguous
`Vec` is materialized once, at stop time, off the audio thread. The buffer's
`Mutex` is uncontended during capture (`voice_stop` only locks after the stream
is dropped). A **max-duration guard** (`MAX_RECORDING_SECS`, 5 min) caps growth
and appends a "recording capped" note to the transcript rather than growing
memory without bound.

**Diagnostics:** `LOOMUX_VOICE_KEEP_WAV=1` preserves the scratch WAV and logs its
path, duration, and RMS level (via the pure `rms` / `duration_secs` helpers) — a
near-zero RMS on a long capture is the fingerprint of a silent/starved capture.
This is the tool that was missing while first chasing the bug.

WASAPI callback timing itself can't be unit-tested; `ChunkedBuffer` accumulation,
capping, and the RMS/duration helpers are covered hermetically, and the fix is
verified by hand (record 60–90 s; the transcript is non-empty and the kept WAV's
RMS is sane).

### Transcription — subprocess to prebuilt `whisper-cli.exe`

git.rs-style `Command` with `CREATE_NO_WINDOW`, `-nt -l en`; stdout parsed into
one prompt line (timestamps and `[BLANK_AUDIO]`-style markers stripped).

**Why subprocess, not the `whisper-rs` crate.** `whisper-rs` is itself
getrandom-clean at runtime, but `whisper-rs-sys` builds whisper.cpp from C++ via
cmake, which would drag a cmake + MSVC toolchain requirement into
`cargo check --locked` for every contributor and CI. Shelling out to a prebuilt
binary keeps the gate a pure-Rust check and mirrors how git.rs already integrates
an external CLI. The trade-off — shipping/fetching the binary — is handled by the
bundling slice (whisper-cli.exe + DLLs + `base.en` as Tauri resources). If the
team later accepts the C++ build, `whisper-rs` behind a Cargo feature is a clean
swap; the frontend contract wouldn't change.

### Runtime resolution order

`voice_stop` receives the `AppHandle` and resolves the CLI and model
independently, each in this priority:

1. **Bundled resources** — `<tauri resource dir>/whisper/whisper-cli.exe`
   (+ its DLLs) and `<resource dir>/whisper/models/ggml-base.en.bin`. This is the
   frozen convention the bundling slice builds to; it makes voice work out of the
   box from the installer.
2. **Env overrides** — `LOOMUX_WHISPER_CLI` / `LOOMUX_WHISPER_MODEL` (power users
   / a custom whisper build or model).
3. **`%LOCALAPPDATA%\loomux\whisper\`** — `whisper-cli.exe` (or legacy `main.exe`)
   and `models\` (prefers `ggml-base.en.bin`, then `ggml-tiny.en.bin`, then the
   first `*.bin`).

Every failure names all three locations so the message is actionable.

### Error surfaces

- No input device → "no microphone / input device found".
- Stream build failure (often OS mic-permission denial) → "couldn't open the
  microphone … check Windows microphone privacy settings".
- Device lost mid-record → "microphone stopped mid-recording: …".
- **Missing DLLs** (the failure the human hit in the demo: `whisper-cli.exe`
  copied without its whisper.cpp/ggml DLLs) → detected via `CreateProcess`
  `ERROR_MOD_NOT_FOUND` (126) and the DLL-load NTSTATUS exit codes
  (`0xC0000135` / `0xC000007B` / `0xC0000139`, plus shell `127`; see the pure,
  tested `is_dll_load_failure`) → "whisper-cli.exe is missing its DLLs — copy the
  .dll files … next to whisper-cli.exe".

### Tested pure logic

`resample_linear`, `encode_wav_pcm16`, `parse_whisper_output`, and
`is_dll_load_failure` have inline unit tests (cross-platform). Live capture and
the subprocess call are validated by hand (they need a real mic + the whisper
runtime), per the repo's no-DOM-sim / no-live-agent test policy.

## Cross-platform path (not yet built)

The `#[cfg(windows)]` gate exists to keep CI cheap — cpal pulls `alsa-sys` on
Linux (needs `libasound2-dev`) and CoreAudio on macOS. cpal itself builds on both
(macOS needs no extra packages; Linux needs the ALSA dev headers on the runner).
To generalize: lift the gate on `voice::win`, and make the whisper-resolution
paths OS-agnostic (drop the `.exe` suffix, keep using `data_local_dir()`
per-OS). The capture and transcription logic is otherwise portable.

# Voice prompts (issue #58)

Local, open-source speech-to-text into any focused target. Push-to-talk mic
capture → on-device [whisper.cpp](https://github.com/ggml-org/whisper.cpp)
transcription → text dropped at the current focus point, never auto-submitted.
User-facing usage lives in the README ([Voice prompts](../../README.md#voice-prompts));
this note is the architecture and the decisions behind it.

## Flow

```
 Alt+S / 🎤 ─▶ voiceController.toggle ─▶ voice_start (Rust)
 (main.ts /                                └─ cpal opens the default mic on a
  mic button)                                 dedicated thread; buffers mono f32
 Alt+S / 🎤 ─▶ voiceController.stop  ─▶ voice_stop(app, state)  [async]
                                          └─ spawn_blocking:
                                             ├─ join capture thread
                                             ├─ resample → 16 kHz mono
                                             ├─ encode WAV (scratch temp)
                                             ├─ resolve whisper runtime (below)
                                             ├─ spawn whisper-cli.exe (killable)
                                             └─ parse stdout → transcript
   deliver to target ◀── transcript ─────────┘
     compose box → insert at caret   |   terminal → xterm paste (bracketed, no \n)
```

## Frontend

- **`src/voice.ts`** — pure, DOM-free, unit-tested (`test/voice.test.ts`):
  - `resolveVoiceTargetKind` — the insertion-target decision: a focused compose
    box wins; otherwise the active pane's terminal; otherwise nothing.
  - `nextVoiceState` — the push-to-talk state machine
    (`idle → starting → recording → transcribing → idle`). `starting` and
    `transcribing` swallow stray toggles so a double-tap can't double-start or
    interrupt a transcription; only one capture ever runs.
- **`src/voicecontrol.ts`** — one global `voiceController` (the single-capture
  invariant is "one controller, one state, one target"). It owns the state
  machine, calls the backend, routes the transcript, drives the phase indicator,
  and installs a capture-phase `Esc` handler while recording *or transcribing*
  (so Esc cancels without otherwise stealing Esc from the shell/compose box). A
  **generation counter** invalidates a late `voiceStart`/`voiceStop` result when
  the user cancelled or closed the pane mid-flight, so a stale transcript never
  lands. Depends on panes only through the `VoiceTargetPane` interface — no
  import cycle.
- **`src/pane.ts`** implements `VoiceTargetPane`: `isComposeFocused()`,
  `insertTranscript()` (caret insert), `pasteToTerminal()` (`xterm.paste`, which
  applies bracketed-paste semantics and adds no newline), plus `setVoicePhase()`
  (mic-button pulse/spin for compose targets, overlay badge for terminal
  targets). The mic button (orchestrator strip only) and the `Alt+S` hotkey (any
  pane) both go through the controller.

**Transcribing phase & responsiveness.** Stopping a recording enters
`transcribing`, which shows a "Transcribing…" spinner (amber) distinct from the
red "Recording" state. The backend `voice_stop` is **async** and runs the whisper
subprocess on `spawn_blocking`, so the webview stays responsive even on a
multi-minute large-model transcription (the original UI froze because the
subprocess ran inline on the command path). A toggle during `transcribing` is
ignored; **Esc cancels** it (see backend kill, below).

**Hotkey — `Alt+S`** ("speak"). Changed from `Alt+V`, which is Claude Code's
paste-image binding — and because loomux intercepts app shortcuts before the
terminal (`isAppShortcut` → the pane declines them so they bubble to the document
handler), `Alt+V` was *stolen* from agents in a pane. `Alt+M` (the obvious "mic")
is already minimize-pane. `Alt+S` is free in loomux, unused by Claude Code
(verified against its keybindings docs), and not a readline word-motion binding.

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

**Async + cancellable.** `voice_stop` is an `async` command that clones the state
`Arc`s and runs the join + subprocess on `tauri::async_runtime::spawn_blocking`,
so a multi-minute transcription never blocks the webview (the original freeze).
The subprocess is `spawn()`ed (not `output()`) and **enrolled in a kill-on-close
Job Object** — reusing `pty::assign_kill_on_close_job` (#78). The `JobHandle`
lives in `VoiceState.transcribing`; dropping it fires `KILL_ON_JOB_CLOSE`, so:

- **Esc while transcribing** → `voice_cancel` takes/drops the handle → whisper is
  killed → the blocking `wait_with_output` returns and stops burning CPU. (Esc
  while *recording* just discards the audio.)
- **Pane close** → the frontend calls `voice_cancel` (same kill path).
- **App exit / crash** → loomux's death closes its last handle to the job, so the
  OS reaps whisper too — never orphaned. Fail-soft: if the job can't be created,
  transcription still runs, just without the cancel/orphan guarantee.

A `spawn_blocking` closure holds only `Send` values (the two `Arc`s + the bundled
path), so the `State` guard is never held across an await.

**Why subprocess, not the `whisper-rs` crate.** `whisper-rs` is itself
getrandom-clean at runtime, but `whisper-rs-sys` builds whisper.cpp from C++ via
cmake, which would drag a cmake + MSVC toolchain requirement into
`cargo check --locked` for every contributor and CI. Shelling out to a prebuilt
binary keeps the gate a pure-Rust check and mirrors how git.rs already integrates
an external CLI. The trade-off — the user fetches the binary — is deliberate:
voice is an **opt-in** feature (the runtime and model are NOT shipped with the
installer; the human chose download-it-yourself over +150 MB installers). The
pinned, checksum-verified `scripts/stage-whisper.ps1` makes the opt-in a
one-liner. If the team later accepts the C++ build, `whisper-rs` behind a Cargo
feature is a clean swap; the frontend contract wouldn't change.

### Runtime resolution order

`voice_stop` receives the `AppHandle` and resolves the CLI and model
independently, each in this priority:

1. **Bundled resources** — `<tauri resource dir>/whisper/whisper-cli.exe`
   (+ its DLLs) and `<resource dir>/whisper/models/ggml-base.en.bin`. Nothing
   ships there today (voice is opt-in); the probe is kept so a future decision
   to bundle needs zero backend changes.
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

`resample_linear`, `encode_wav_pcm16`, `parse_whisper_output`,
`is_dll_load_failure`, and `ChunkedBuffer`/`rms`/`duration_secs` have inline unit
tests (cross-platform); the push-to-talk state machine (`nextVoiceState`,
including the `transcribing` state and Esc-cancel transitions) is tested in
`test/voice.test.ts`. Live capture, the async command path, and the
subprocess-kill (which need a real mic, a Tauri runtime, and the whisper runtime)
are validated by hand — record 60–90 s, confirm a responsive UI during
transcription, and confirm Esc/pane-close terminate the whisper process — per the
repo's no-DOM-sim / no-live-agent test policy.

## Cross-platform path (not yet built)

The `#[cfg(windows)]` gate exists to keep CI cheap — cpal pulls `alsa-sys` on
Linux (needs `libasound2-dev`) and CoreAudio on macOS. cpal itself builds on both
(macOS needs no extra packages; Linux needs the ALSA dev headers on the runner).
To generalize: lift the gate on `voice::win`, and make the whisper-resolution
paths OS-agnostic (drop the `.exe` suffix, keep using `data_local_dir()`
per-OS). The capture and transcription logic is otherwise portable.

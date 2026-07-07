# Voice-prompt prototype (issue #58) — demo walkthrough

**Status: PROTOTYPE / DRAFT PR — do not merge.** This is a working direction
demo: push-to-talk on the orchestrator steer strip → local, open-source Whisper
transcription → text lands in the compose box for the human to review and send.
Nothing is auto-submitted; the transcript is inserted exactly as if you'd typed
it, and you press Enter yourself.

---

## What it does

A microphone button sits on the steering strip next to the paperclip. Click it
to start recording (it turns red and pulses); click again to stop. Loomux
captures your mic natively, transcribes it locally with whisper.cpp, and drops
the recognized text into the compose box at the caret. Review, edit, hit Enter.

- **Local + open source.** Transcription is [whisper.cpp](https://github.com/ggml-org/whisper.cpp)
  (MIT), run entirely on your machine. No audio leaves the box; no cloud STT.
- **Latency:** usable, not streaming. A few seconds of speech on the `base.en`
  model transcribes in ~1–3 s on CPU. `tiny.en` is faster and less accurate.

---

## One-time setup (model + binary download)

Neither the whisper.cpp binary nor the model weights are committed to the repo.
Download them once into `%LOCALAPPDATA%\loomux\whisper\`:

```
%LOCALAPPDATA%\loomux\whisper\
├─ whisper-cli.exe          ← prebuilt whisper.cpp CLI (or main.exe from older zips)
└─ models\
   └─ ggml-base.en.bin      ← the model weights
```

### 1. Get the whisper.cpp Windows binary

Download a prebuilt release zip from
<https://github.com/ggml-org/whisper.cpp/releases> (the `whisper-bin-x64.zip`
asset), and copy `whisper-cli.exe` (older releases name it `main.exe` — both are
accepted) into `%LOCALAPPDATA%\loomux\whisper\`.

> Prefer to build it yourself? `cmake -B build && cmake --build build --config Release`
> in a whisper.cpp checkout produces the same `whisper-cli.exe`.

### 2. Get a model

Download a ggml model into the `models\` subfolder. Good starting points:

| Model | File | Size | Notes |
| --- | --- | --- | --- |
| tiny (English) | `ggml-tiny.en.bin` | ~75 MB | fastest, roughest |
| base (English) | `ggml-base.en.bin` | ~142 MB | **recommended default** |

Direct download, e.g.:

```powershell
$dir = "$env:LOCALAPPDATA\loomux\whisper\models"
New-Item -ItemType Directory -Force $dir | Out-Null
Invoke-WebRequest `
  "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin" `
  -OutFile "$dir\ggml-base.en.bin"
```

### Override locations (optional)

If you keep whisper elsewhere, point loomux at it with environment variables —
these take priority over the default folder:

- `LOOMUX_WHISPER_CLI` → full path to `whisper-cli.exe`
- `LOOMUX_WHISPER_MODEL` → full path to a `.bin` model

If the binary or a model is missing, the mic button shows an actionable error in
the strip's status line (naming the folder it looked in) instead of failing
silently.

---

## Demo script (what to show)

1. `npm run tauri dev`, start an orchestrator group so a steering strip is
   visible under the orchestrator pane.
2. Click the 🎤 button on the strip — it turns red and pulses (mic is live).
3. Say a prompt, e.g. *"spawn a worker to fix the failing layout test."*
4. Click 🎤 again. After ~1–3 s the transcription appears in the compose box.
5. Edit if needed, press **Enter** to send it to the orchestrator — exactly the
   normal steer path (loomux never auto-submits voice).

Also worth showing: click the mic with no whisper installed → the strip reports
where it looked, so the degradation is graceful.

---

## Architecture (as built)

```
 steer strip 🎤  ──click──▶  voiceStart()  ──IPC──▶  voice_start   (Rust)
   (pane.ts)                  (pty.ts)                 └─ cpal opens default mic on a
                                                          dedicated thread; buffers f32
 steer strip 🎤  ──click──▶  voiceStop()   ──IPC──▶  voice_stop
                                                       ├─ resample → 16 kHz mono
                                                       ├─ encode WAV (scratch temp)
                                                       ├─ run whisper-cli.exe (subprocess)
                                                       └─ parse stdout → transcript
   insertTranscript(text)  ◀──── transcript ─────────────┘
   (into compose box, NOT submitted)
```

- **Capture — `cpal` (native WASAPI).** The backend owns the microphone, so
  there is **no WebView2 `getUserMedia` permission** to negotiate — the demo is
  deterministic. The cpal `Stream` is `!Send` on Windows, so it lives entirely
  on a dedicated capture thread controlled by a stop channel; samples flow into
  a shared buffer that `voice_stop` drains and resamples.
- **Transcription — subprocess to prebuilt whisper.cpp.** git.rs-style
  `Command` with `CREATE_NO_WINDOW`, `-nt -l en`. stdout is parsed into a single
  prompt line (timestamps and `[BLANK_AUDIO]`-style markers stripped).
- **Boundaries honored:** frontend↔backend only through `#[tauri::command]` +
  typed `pty.ts` wrappers; no PTY resize (the strip is existing chrome, the mic
  is one more button in the row); model weights are downloaded, not committed.

Files: `src-tauri/src/voice.rs` (capture + transcription + pure helpers),
`src-tauri/src/lib.rs` (command registration + `VoiceState`), `src/pty.ts`
(wrappers), `src/pane.ts` (mic button + push-to-talk), `src/styles.css`.

### What's tested

Pure logic in `voice.rs` has inline unit tests: linear resampling (identity /
empty / down / up), WAV PCM-16 header correctness + clamping, and whisper-output
parsing (timestamp stripping, blank-marker dropping). Live capture and the
subprocess call are validated by hand (they need a real mic + the downloaded
binary) — consistent with the repo's "no DOM sim / no live-agent" test policy.

### What's stubbed / prototype-grade

- **No streaming / partial results.** One shot: record, then transcribe.
- **Linear resampler**, not a polyphase/anti-aliased one — fine for speech STT.
- **No device picker** — uses the OS default input device.
- **No packaging of whisper.** The user downloads the binary + model (documented
  above). A production build would bundle or auto-fetch them.
- **English-forced** (`-l en`); language selection is a config knob later.

---

## Crate-vetting results (the getrandom / ProcessPrng constraint)

The Windows-10 baseline forbids any crate that imports
`bcryptprimitives.dll!ProcessPrng` in the **shipped binary** (getrandom-based:
uuid v4, rand, tempfile default features, and many audio/ML crates). Vetted with
`cargo tree -i getrandom --target x86_64-pc-windows-msvc`:

| Crate | Verdict | Detail |
| --- | --- | --- |
| **cpal 0.15** | ✅ **safe, used** | getrandom appears only under *build-dependencies* (jobserver→cc) and the *Android-only* `oboe-sys`. Its Windows runtime tree is getrandom-free — pure WASAPI via `windows-sys`. Adding cpal introduced **zero** new getrandom edges to loomux's runtime tree. |
| **whisper-rs 0.14 / -sys** | ⚠️ runtime-clean but **rejected for the gate** | getrandom is only a *build-dependency* (via `cmake`→`cc`→`jobserver`), so the linked binary is safe. **But** `whisper-rs-sys` builds whisper.cpp from C++ via cmake, dragging a **cmake + MSVC C++ toolchain** requirement into `cargo check --locked` for every contributor and CI. On this machine cmake couldn't even find `cl.exe` without a vcvars environment. Too heavy for a prototype gate — hence the subprocess route. |
| hound (WAV) | ➖ not needed | WAV encoding is ~15 lines, hand-rolled in `voice.rs` to avoid a dependency. |

**Pre-existing getrandom in the tree (not from this PR):** `tauri` pulls
getrandom 0.3.x on a normal edge and `uuid`/proc-macro codegen pulls 0.4.x — both
predate this change and ship in loomux today, so they are not a regression this
PR introduces.

---

## Recommended production architecture

1. **Keep native cpal capture.** It's getrandom-clean, deterministic, and avoids
   the WebView2 mic-permission problem entirely. Add a device picker and a
   simple input-level meter so the user can see the mic is hot.
2. **Two viable transcription backends, pick per appetite:**
   - *Subprocess (this prototype)* — keeps `cargo check` a pure-Rust gate; the
     cost is shipping/fetching `whisper-cli.exe`. Best if you don't want a C++
     build in CI. Harden by bundling the binary as a Tauri sidecar resource and
     auto-downloading the model on first use (with a progress UI).
   - *Link `whisper-rs`* — no external binary, cleaner single-process flow, and
     runtime getrandom-clean. The cost is a cmake + MSVC C++ build in CI and
     locally. Worth it once the team accepts that build dependency; gate it
     behind a Cargo feature so the default check stays light.
3. **Model management:** ship `base.en` by default, offer `tiny.en` for speed and
   larger multilingual models opt-in. Cache under `%LOCALAPPDATA%\loomux\whisper`.
4. **UX:** push-to-talk hold (spacebar-style) in addition to click-toggle; a
   global hotkey to record without focusing the strip; interim "…listening"
   feedback. Consider whisper.cpp streaming mode for near-real-time partials.
5. **Rebase note:** #100 turns the steer strip's `<input>` into a multi-line
   `<textarea>` (on an unmerged batch branch). This prototype targets `main`'s
   single-line input; on rebase, `insertTranscript` and the mic button move to
   the textarea unchanged (both use `value` / `selectionStart`).

---
title: Voice prompts
layout: default
parent: Features
nav_order: 3
---

# Voice prompts
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

---

Dictate a prompt instead of typing it. Press **`Alt+S`** to start recording
(push-to-talk), speak, and press **`Alt+S`** again to stop — loomux transcribes
your speech **locally** and drops the text at your current focus. **`Esc`**
cancels at any point (including mid-transcription). Transcription is never
auto-submitted: you review it and press Enter yourself.

> **Windows only** for now. On other platforms the hotkey reports that voice
> capture isn't available. The cross-platform path is sketched in
> [`doc/design/voice.md`](https://github.com/willem445/loomux/blob/main/doc/design/voice.md).

## Where the text lands

The target is decided when you *start*, and follows focus:

- a **compose/steer box** focused → inserted at the caret;
- any **terminal pane** focused (an agent, the orchestrator pane, a plain shell)
  → pasted into that pane's input as if typed (bracketed paste, no trailing
  newline).

The orchestrator [steer strip](steering.html) also has a **🎤 button** that records
into the strip; the hotkey works from any pane, with or without the button.

Only **one recording** runs at a time. While a capture targets a terminal, a
badge floats over that pane — red "Recording", then amber "Transcribing…" while
the speech is being converted. A large model can take a while; the app stays
responsive and you can `Esc` to abort.

## Local & open source, opt-in

Speech-to-text is [whisper.cpp](https://github.com/ggml-org/whisper.cpp) (MIT)
running entirely on your machine — no audio leaves the box, no cloud STT. Loomux
does **not** ship the whisper runtime (it would add ~150 MB to the installer), so
voice is opt-in: install a whisper build and a model once and loomux picks them
up automatically.

## Set it up (Windows)

### The easy way — the staging script

Run the convenience script from a checkout. It downloads a pinned,
checksum-verified runtime **plus** the `base.en` model into the location loomux
auto-detects (`%LOCALAPPDATA%\loomux\whisper`):

```powershell
powershell -ExecutionPolicy Bypass -File scripts\stage-whisper.ps1
```

Restart loomux and press `Alt+S`.

### By hand

1. From a
   [whisper.cpp release](https://github.com/ggml-org/whisper.cpp/releases),
   download the CPU `whisper-bin-x64.zip` and extract **`whisper-cli.exe`
   together with _all_ of its DLLs** — `whisper.dll`, `ggml.dll`,
   `ggml-base.dll`, and every `ggml-cpu-*.dll`.

   > **Missing DLLs are the usual cause of a silent "whisper failed to run."**
   > `ggml` loads the `ggml-cpu-*.dll` matching your CPU at runtime, so keep the
   > whole set together next to `whisper-cli.exe`. See
   > [Troubleshooting](../troubleshooting.html#voice-whisper-failed-to-run).

2. Download a ggml model — e.g. `ggml-base.en.bin` from
   [ggerganov/whisper.cpp](https://huggingface.co/ggerganov/whisper.cpp).
3. Place them where loomux looks by default:

   ```
   %LOCALAPPDATA%\loomux\whisper\whisper-cli.exe      (with the DLLs beside it)
   %LOCALAPPDATA%\loomux\whisper\models\ggml-base.en.bin
   ```

Restart loomux and press `Alt+S`.

### Custom locations (env overrides)

To keep the runtime or model elsewhere, point loomux at them:

- `LOOMUX_WHISPER_CLI` → a `whisper-cli.exe` (with its DLLs beside it)
- `LOOMUX_WHISPER_MODEL` → any ggml `.bin` (e.g. a larger multilingual model)

Resolution order is **bundled resources → env vars → `%LOCALAPPDATA%`**. Nothing
ships in the bundled slot today (voice is opt-in), so in practice it's your env
vars, then the `%LOCALAPPDATA%` default. Every failure message names all the
locations it checked, so it's actionable.

## Performance & tuning

- **Threads.** Loomux passes `-t` capped at your CPU's parallelism (max 8) —
  whisper.cpp otherwise defaults to 4, so this is a 2× win on a many-core machine,
  without oversubscribing (its CPU inference is memory-bandwidth-bound and gains
  flatten past ~8 threads).
- **Model choice** (the biggest speed/quality lever). `base.en` is a fine
  default; for noticeably better accuracy at similar speed, use
  **`large-v3-turbo`** quantized — **`q8_0`** is the quality/speed sweet spot
  (`q5_0` is smaller/faster). Drop the `.bin` in `models\` or point
  `LOOMUX_WHISPER_MODEL` at it.
- **GPU.** NVIDIA owners get a large speed-up from a **cuBLAS/CUDA** whisper.cpp
  build — download a `cublas` release asset and point `LOOMUX_WHISPER_CLI` at it.
- **Extra flags.** `LOOMUX_WHISPER_ARGS` is appended verbatim to the whisper
  command (whitespace-split, no shell quoting) for power users — e.g.
  `LOOMUX_WHISPER_ARGS="-t 12 -bs 5"`. It comes *after* loomux's args and whisper
  takes the last value of a flag, so your overrides win.

## Vocabulary biasing

Bias recognition toward your own jargon with an optional
`%LOCALAPPDATA%\loomux\whisper\vocab.txt` — one term or phrase per line, `#` for
comments. Loomux assembles it into whisper's `--prompt` (an initial-prompt hint):

```
# loomux project terms
loomux
ConPTY
tmux
gh
xterm
WASAPI
cpal
Tauri
ggml
orchestrator
```

Keep it a **short curated list**: whisper's initial prompt is capped (~224
tokens) and only a curated list is reliably honored — loomux truncates to a
conservative budget and logs a warning if `vocab.txt` is over-long. Set
`LOOMUX_WHISPER_PROMPT` to a raw prompt string to override the file entirely.

> Fine-tuning a model is out of scope — it needs a GPU, a labeled dataset, and a
> multi-GB output; `--prompt` gets most of the domain-term benefit for none of
> the cost.

## Limits & diagnostics

- Recordings are **capped at 5 minutes**; loomux appends a "recording capped"
  note rather than growing memory without bound.
- If the mic can't be opened (no device, or Windows microphone privacy blocks
  it), or the whisper runtime is missing/misconfigured, loomux surfaces a
  **specific** message rather than failing silently.
- To debug a capture, set `LOOMUX_VOICE_KEEP_WAV=1` — loomux keeps the scratch
  WAV and logs its path, duration, and level. A near-zero level on a long capture
  is the fingerprint of a silent/starved mic.

See [`doc/design/voice.md`](https://github.com/willem445/loomux/blob/main/doc/design/voice.md)
for the architecture and the cross-platform path.

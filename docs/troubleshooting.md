---
title: Troubleshooting
layout: default
nav_order: 7
---

# Troubleshooting
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

---

Loomux tries to fail *loud and specific* — most problems surface as an inline
message or toast that names the cause. This page collects the recurring ones.

## Voice: whisper failed to run

Almost always **missing DLLs**. `whisper-cli.exe` needs its whole DLL set beside
it — `whisper.dll`, `ggml.dll`, `ggml-base.dll`, and every `ggml-cpu-*.dll`.
`ggml` loads the `ggml-cpu-*.dll` matching your CPU at runtime, so if you copied
just the `.exe`, it dies before transcribing anything.

Loomux detects this specific failure (the Windows DLL-load error codes) and tells
you to copy the `.dll` files next to `whisper-cli.exe`. Fix:

- Re-extract **all** files from the whisper.cpp `whisper-bin-x64.zip` into the
  same folder as `whisper-cli.exe`, **or**
- just run the staging script, which places a complete, checksum-verified set for
  you:

  ```powershell
  powershell -ExecutionPolicy Bypass -File scripts\stage-whisper.ps1
  ```

See [Voice prompts → Set it up](features/voice-prompts.html#set-it-up-windows).

## Voice: no transcript / "you didn't say anything"

- **Mic permission.** If the microphone can't be opened, loomux reports "couldn't
  open the microphone … check Windows microphone privacy settings." Open
  **Settings → Privacy & security → Microphone** and allow desktop apps to use
  the mic.
- **No input device.** "No microphone / input device found" means Windows sees no
  capture device — check it's plugged in and set as default.
- **Long recording returned nothing.** Set `LOOMUX_VOICE_KEEP_WAV=1` and record
  again; loomux logs the kept WAV's path, duration, and level. A near-zero level
  is the fingerprint of a silent/starved capture.
- Recordings are **capped at 5 minutes**; past that, loomux appends a "recording
  capped" note.

## Voice: which model / it's slow

`base.en` is the default. For better accuracy at similar speed, use
`large-v3-turbo` quantized (`q8_0` is the sweet spot). NVIDIA owners can point
`LOOMUX_WHISPER_CLI` at a **cuBLAS/CUDA** whisper build for a large speed-up. Full
tuning knobs are on the [Voice prompts](features/voice-prompts.html#performance--tuning)
page.

## `gh` not found or not authenticated

The [GitHub issues view](features/github-issues.html) and the orchestration PR
workflow both go through the `gh` CLI. If the panel says `gh` is missing or you're
not logged in:

- Install the [GitHub CLI](https://cli.github.com/).
- Run `gh auth login` and complete the browser flow.

Loomux stores no token — it uses whatever `gh auth login` you already have. The
panel shows a one-line hint instead of failing calls, so a broken `gh` never
looks like a loomux bug.

## An agent CLI isn't found

Orchestration and agent panes drive the `claude` and/or `copilot` CLIs — loomux
doesn't bundle them. The launcher warns inline when a selected role's CLI isn't
installed. Make sure the CLI is on your `PATH` (open a fresh terminal and run
`claude --version` / `copilot --version`).

An agent pane that dies with an error **stays open** so you can read what
happened — it isn't closed out from under you.

## macOS: "app is damaged and can't be opened"

Builds are **unsigned** for now, so macOS quarantines them. Clear the attribute:

```sh
xattr -cr /Applications/Loomux.app
```

The install script does this for you; if you dragged the app from a `.dmg`
manually, run it yourself.

## Disk & data locations

Loomux keeps durable state and logs under your platform data dir
(`%LOCALAPPDATA%\loomux\` on Windows; the equivalent app-data dir elsewhere):

- `orchestration/<group>/` — per-group `state.json`, `audit.jsonl`,
  `agents.json`, and rendered role instructions.
- `logs/` — crash forensics and a rotating breadcrumb log (see below).
- `whisper/` — the opt-in voice runtime and models, if you installed them.

If a group's `audit.jsonl` grows large, note that loomux **rotates** it (the
prior generation is `audit.1.jsonl`, read alongside the current one in the audit
viewer). Ending a group can optionally remove each agent's worktree to reclaim
disk (branches are always kept).

Durable files (the task board, group state, and friends) are written
**atomically** — a same-directory temp file renamed over the original — so a
failed write (full disk, crash) can never destroy the previous good copy.
Worker worktrees in Rust repos share one cargo build cache
(`<repo>\.loomux-target`, gitignored) instead of a multi-GB `target/` each, and
loomux warns each group's orchestrator once when the workspace drive drops
below ~5 GB free. Details in
[doc/design/durability-and-disk.md](https://github.com/willem445/loomux/blob/main/doc/design/durability-and-disk.md).

## Crash logs

If loomux exits uncleanly, the next launch surfaces a toast naming the newest
crash log. Forensics live under `<data dir>/loomux/logs/`:

- `crash-<timestamp>.log` — panic message, thread, and backtrace.
- `breadcrumbs.log` — a rotating record of lifecycle events (pane/PTY open/close,
  agent spawn/exit, delivery outcomes) with **no prompt content**.

Attach these when reporting a bug. Design details:
[`doc/design/crash-observability.md`](https://github.com/willem445/loomux/blob/main/doc/design/crash-observability.md).

## Still stuck?

Open an issue at
[github.com/willem445/loomux/issues](https://github.com/willem445/loomux/issues)
with your platform, what you did, and any crash-log or breadcrumb output.

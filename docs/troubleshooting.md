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

Orrerix tries to fail *loud and specific* — most problems surface as an inline
message or toast that names the cause. This page collects the recurring ones.

## Voice: whisper failed to run

Almost always **missing DLLs**. `whisper-cli.exe` needs its whole DLL set beside
it — `whisper.dll`, `ggml.dll`, `ggml-base.dll`, and every `ggml-cpu-*.dll`.
`ggml` loads the `ggml-cpu-*.dll` matching your CPU at runtime, so if you copied
just the `.exe`, it dies before transcribing anything.

Orrerix detects this specific failure (the Windows DLL-load error codes) and tells
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

- **Mic permission.** If the microphone can't be opened, orrerix reports "couldn't
  open the microphone … check Windows microphone privacy settings." Open
  **Settings → Privacy & security → Microphone** and allow desktop apps to use
  the mic.
- **No input device.** "No microphone / input device found" means Windows sees no
  capture device — check it's plugged in and set as default.
- **Long recording returned nothing.** Set `ORRERIX_VOICE_KEEP_WAV=1` and record
  again; orrerix logs the kept WAV's path, duration, and level. A near-zero level
  is the fingerprint of a silent/starved capture.
- Recordings are **capped at 5 minutes**; past that, orrerix appends a "recording
  capped" note.

## Voice: which model / it's slow

`base.en` is the default. For better accuracy at similar speed, use
`large-v3-turbo` quantized (`q8_0` is the sweet spot). NVIDIA owners can point
`ORRERIX_WHISPER_CLI` at a **cuBLAS/CUDA** whisper build for a large speed-up. Full
tuning knobs are on the [Voice prompts](features/voice-prompts.html#performance--tuning)
page.

## `gh` not found or not authenticated

The [GitHub issues view](features/github-issues.html) and the orchestration PR
workflow both go through the `gh` CLI. If the panel says `gh` is missing or you're
not logged in:

- Install the [GitHub CLI](https://cli.github.com/).
- Run `gh auth login` and complete the browser flow.

Orrerix stores no token — it uses whatever `gh auth login` you already have. The
panel shows a one-line hint instead of failing calls, so a broken `gh` never
looks like an orrerix bug.

## An agent CLI isn't found

Orchestration and agent panes drive the `claude`, `copilot`, and `opencode`
CLIs — orrerix doesn't bundle them. The launcher warns inline when a selected
role's CLI isn't installed. Make sure the CLI is on your `PATH` (open a fresh
terminal and run `claude --version` / `copilot --version` / `opencode
--version`).

An agent pane that dies with an error **stays open** so you can read what
happened — it isn't closed out from under you.

## A Copilot agent can't use its loomux tools

If a Copilot pane lists the `loomux` MCP server but the agent says it has no
permission to use its tools, check `~/.copilot/permissions-config.json` (or
`%USERPROFILE%\.copilot\permissions-config.json`). orrerix records two grants
there when it spawns a Copilot pane, keyed by the repository's git root:

- your agent's workspace under `allowed_directories`, and
- `{ "kind": "mcp", "serverName": "loomux", "toolName": null }` under
  `tool_approvals`, which approves every loomux tool for that repository.

If the file is missing or the entry isn't there, orrerix couldn't write it —
usually because `~/.copilot` isn't writable, or because `COPILOT_HOME` points
somewhere orrerix can't reach. Copilot will then prompt in-pane for each tool
instead, which an unattended agent has no one to answer.

Note that a Copilot Business or Enterprise administrator can block
allow-all-permissions options outright; orrerix can't override that policy, and
the targeted grants above are what keep such a pane usable at all.

## macOS: "app is damaged and can't be opened"

Builds are **unsigned** for now, so macOS quarantines them. Clear the attribute:

```sh
xattr -cr /Applications/Loomux.app
```

The install script does this for you; if you dragged the app from a `.dmg`
manually, run it yourself.

## Disk & data locations

Orrerix keeps durable state and logs under your platform data dir
(`%LOCALAPPDATA%\loomux\` on Windows; the equivalent app-data dir elsewhere):

- `orchestration/<group>/` — per-group `state.json`, `audit.jsonl`,
  `agents.json`, and rendered role instructions.
- `logs/` — crash forensics and a rotating breadcrumb log (see below).
- `whisper/` — the opt-in voice runtime and models, if you installed them.

If a group's `audit.jsonl` grows large, note that orrerix **rotates** it (the
prior generation is `audit.1.jsonl`, read alongside the current one in the audit
viewer). Ending a group can optionally remove each agent's worktree to reclaim
disk (branches are always kept).

Durable files (the task board, group state, and friends) are written
**atomically** — a same-directory temp file renamed over the original — so a
failed write (full disk, crash) can never destroy the previous good copy.
Each worker worktree keeps its own build cache (e.g. a multi-GB `target/` in a
Rust repo) — orrerix does not share or dedup build caches across worktrees, and
warns each group's orchestrator once when the workspace drive drops below ~5 GB
free. Details in
[doc/design/durability-and-disk.md](https://github.com/willem445/loomux/blob/main/doc/design/durability-and-disk.md).

## Crash logs

If orrerix exits uncleanly, the next launch surfaces a toast naming the newest
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

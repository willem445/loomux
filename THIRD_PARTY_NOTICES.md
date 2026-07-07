# Third-party notices

Loomux ships one third-party component inside its Windows installer (the ConPTY
host, below). It also documents an **opt-in** component — the whisper.cpp voice
runtime — which loomux does **not** distribute: users install it themselves if
they want voice input. Each component is used under its own license.

## whisper.cpp voice runtime — MIT (opt-in; not shipped)

Applies only when a user opts into voice input (issue #58) and installs the
runtime themselves — via `scripts/stage-whisper.ps1` or by hand. Loomux does not
bundle or redistribute these files.

- Upstream: https://github.com/ggml-org/whisper.cpp
- Version: **v1.9.1** (prebuilt `whisper-bin-x64.zip`, CPU/x64), pinned +
  sha256-verified by `scripts/stage-whisper.ps1`
- Files: `whisper-cli.exe`, `whisper.dll`, `ggml.dll`, `ggml-base.dll`,
  `ggml-cpu-*.dll`
- License: MIT (Copyright (c) 2023-2026 The ggml authors)

## Whisper base.en model weights — MIT (opt-in; not shipped)

Applies under the same opt-in condition as the runtime above.

- Source: https://huggingface.co/ggerganov/whisper.cpp (`ggml-base.en.bin`)
- Revision: `5359861c739e955e79d9a303bcbc70fb988958b1`
- sha256: `a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002`
- The OpenAI Whisper models are released by OpenAI under the MIT License
  (Copyright (c) 2022 OpenAI), converted to ggml format by the whisper.cpp
  project.

## ConPTY host (terminal resize behavior) — MIT (shipped)

Bundled in the Windows installer for clean terminal-resize behavior.

- Upstream: https://github.com/microsoft/terminal (built via
  https://github.com/wezterm/wezterm/tree/main/assets/windows/conhost)
- Bundled files: `conpty.dll`, `OpenConsole.exe`
  (`src-tauri/resources/conhost/`)
- License: MIT. Provenance notes in `src-tauri/resources/conhost/README.md`.
- Shipping the upstream `LICENSE` text alongside these binaries is still open:
  issue #2. This file is the project-wide notice that issue asks for.

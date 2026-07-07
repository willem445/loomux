# Third-party notices

Loomux distributes the following third-party components inside its Windows
installer. Each is redistributed under its own license; full license texts ship
next to the binaries in the installed app and in the source tree.

## whisper.cpp (voice runtime) — MIT

- Upstream: https://github.com/ggml-org/whisper.cpp
- Version: **v1.9.1** (prebuilt `whisper-bin-x64.zip`, CPU/x64)
- Bundled files: `whisper-cli.exe`, `whisper.dll`, `ggml.dll`, `ggml-base.dll`,
  `ggml-cpu-*.dll`
- License: MIT — see `src-tauri/resources/whisper/LICENSE-whisper.cpp.txt`
- Acquired at build time (pinned + sha256-verified) by
  `scripts/stage-whisper.ps1`; not committed to this repo (issue #58).

## Whisper base.en model weights — MIT

- Source: https://huggingface.co/ggerganov/whisper.cpp (`ggml-base.en.bin`)
- Revision: `5359861c739e955e79d9a303bcbc70fb988958b1`
- sha256: `a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002`
- The OpenAI Whisper models are released by OpenAI under the MIT License,
  converted to ggml format by the whisper.cpp project.
- License + provenance: `src-tauri/resources/whisper/MODEL-CARD.txt`

## ConPTY host (terminal resize behavior) — MIT

- Upstream: https://github.com/microsoft/terminal (built via
  https://github.com/wezterm/wezterm/tree/main/assets/windows/conhost)
- Bundled files: `conpty.dll`, `OpenConsole.exe`
  (`src-tauri/resources/conhost/`)
- License: MIT. Provenance notes in `src-tauri/resources/conhost/README.md`.
- Tracking issue for shipping the upstream `LICENSE` alongside these binaries:
  #2. This file is the project-wide notice that issue asks for.

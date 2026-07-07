# Bundled whisper.cpp voice runtime (Windows)

Loomux's push-to-talk voice input (issue #58) transcribes locally with
[whisper.cpp](https://github.com/ggml-org/whisper.cpp) — no network, no cloud
STT. On Windows the runtime and a default model ship inside the installer so
voice works out of the box.

## Frozen layout

The feature code resolves the runtime at this exact convention, relative to the
Tauri **resource directory** (next to the installed `Loomux.exe`):

    <resource dir>/whisper/whisper-cli.exe
    <resource dir>/whisper/*.dll            (whisper.dll, ggml.dll, ggml-base.dll, ggml-cpu-*.dll)
    <resource dir>/whisper/models/ggml-base.en.bin

`ggml` loads the `ggml-cpu-*.dll` matching the host CPU at runtime, so every CPU
variant from the upstream zip is shipped. `SDL2.dll` and `parakeet.*` from that
zip are intentionally omitted — `whisper-cli` transcribes a wav we hand it.

## Nothing here is committed

The binaries and the ~141 MiB model are **not** in git (ignored via the repo
root `.gitignore`). `scripts/stage-whisper.ps1` downloads PINNED, sha256-verified
artifacts into this directory at build time, and `.github/workflows/release.yml`
runs it on the Windows leg before bundling. Only these docs/licenses are
committed and ride along in the bundle:

- `README.md` — this file
- `LICENSE-whisper.cpp.txt` — upstream MIT license for the runtime binaries
- `MODEL-CARD.txt` — model provenance (source, revision, sha256) + MIT license

See `THIRD_PARTY_NOTICES.md` at the repo root for the project-wide summary.

## Why the bundle references this whole directory

`tauri.windows.conf.json` maps `resources/whisper` (the directory) — not
individual globs — to `whisper/`. Tauri's resource walker copies a directory's
current contents and **skips silently when the expected files are absent**,
whereas an explicit file path or a `*.dll` glob is treated as required and
fails the build (even `cargo check`) when nothing matches. Referencing the
directory is what lets regular PR CI — which never stages these artifacts —
stay green while a release build ships the full runtime. The trade-off is that
the committed docs above ship in the bundle too, matching how
`resources/conhost` already ships its README.

## Resolution order & graceful degradation

The feature resolves in order: **bundled** (here) → `LOOMUX_WHISPER_*` env
overrides → `%LOCALAPPDATA%\loomux\whisper\…`. So a dev build with nothing
staged (`npm run tauri dev`) does not fail — voice simply uses a
`%LOCALAPPDATA%` copy if the power-user placed one there, or reports the runtime
as unavailable. `cargo check`/`cargo test` and regular PR CI never touch these
files; only `tauri build` (release/bundle) does.

## Reproducing a bundled build locally

    pwsh ./scripts/stage-whisper.ps1     # from the repo root
    npm run tauri build

To bump the pinned whisper.cpp version or model, edit the pins at the top of
`scripts/stage-whisper.ps1` (single source of truth) and update
`THIRD_PARTY_NOTICES.md` + `MODEL-CARD.txt`.

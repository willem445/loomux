# Third-party notices

Loomux ships one third-party component inside its Windows installer (the ConPTY
host, below). It also documents an **opt-in** component — the whisper.cpp voice
runtime — which loomux does **not** distribute: users install it themselves if
they want voice input. A third class is **shipped in-repo**: content vendored
verbatim into this repository (the Lucide and Primer Octicons icon artwork and the
frontend-design and impeccable agent skills, below). Each component is used under its own license.

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

- Upstream: https://github.com/microsoft/terminal, version `1.22.250204002`
  (win10 x64), via the `Microsoft.Windows.Console.ConPTY` NuGet package built
  by microsoft/terminal's own release pipeline
- Vendored from wezterm commit
  [`4accc376f341`](https://github.com/wezterm/wezterm/commit/4accc376f3411f2cbf4f92ca46f79f7bc47688a1)
  ("update bundled conpty build", 2025-02-08)
- Bundled files: `conpty.dll`, `OpenConsole.exe`, `LICENSE`
  (`src-tauri/resources/conhost/`) — the `resources/conhost/*` glob in
  `tauri.conf.json` ships all three in every installer
- License: MIT (Copyright (c) Microsoft Corporation), full text in
  `src-tauri/resources/conhost/LICENSE`. Provenance notes in
  `src-tauri/resources/conhost/README.md`.

## Lucide icons — ISC, with some icons MIT (shipped in-repo)

`src/icons.ts` carries a curated set of [Lucide](https://github.com/lucide-icons/lucide)
glyphs as inline SVG string constants — the artwork the app's icons are drawn
from. It is a vendored copy, not an npm dependency: the icons ship inside the
frontend bundle, so the notice belongs here rather than in the opt-in class.

- Upstream: https://github.com/lucide-icons/lucide, version **1.31.0**
  @ [`b7b6ecf1316d0af64c97a6b0392abe5e816a8e30`](https://github.com/lucide-icons/lucide/commit/b7b6ecf1316d0af64c97a6b0392abe5e816a8e30)
- Vendored files: the 32 glyph bodies listed by `ICON_NAMES` in `src/icons.ts`,
  each the inner markup of the upstream `icons/<name>.svg` at that commit,
  copied verbatim. Only icons a surface actually renders are vendored, and
  `test/icons.test.ts` fails on one that nothing uses.
- License: **ISC** (Copyright (c) 2026 Lucide Icons and Contributors), full text
  in `src/vendor/lucide/LICENSE`. Provenance and the re-vendoring procedure are
  in the sibling `src/vendor/lucide/README.md`.
- **Two of the vendored glyphs are Feather-derived and additionally carry the
  MIT license** (Copyright (c) 2013-present Cole Bemis, text in the same
  `LICENSE` file): `arrow-up` and `trash-2`.

## Primer Octicons — MIT (shipped in-repo)

`src/agenticons.ts` carries the per-agent pane marks — the glyphs that say which agent CLI
is running in a pane (#992) — as inline SVG string constants. Like the Lucide artwork above
it is a vendored copy, not an npm dependency, and it ships inside the frontend bundle.

- Upstream: https://github.com/primer/octicons, version **19.33.0**
  @ [`cc4e12df6ff8292447ba9141eaa2a6f6e1c59a85`](https://github.com/primer/octicons/commit/cc4e12df6ff8292447ba9141eaa2a6f6e1c59a85)
- Vendored files: one glyph — `copilot-16` — the inner markup of the upstream
  `icons/copilot-16.svg` at that commit, copied verbatim. `test/agenticons.test.ts` fails if
  this notice stops naming a glyph the table vendors.
- License: **MIT** (Copyright (c) 2026 GitHub Inc.), full text in
  `src/vendor/octicons/LICENSE`. Provenance, the re-vendoring procedure, and the
  trademark position (nominative use of an unmodified mark; no affiliation implied) are in
  the sibling `src/vendor/octicons/README.md`.
- Agent CLIs whose vendors publish no such grant are drawn as generated letter badges
  instead — loomux redistributes no brand mark it has not been licensed for.

## frontend-design agent skill — Apache-2.0 (shipped in-repo)

`.claude/skills/frontend-design/` is vendored verbatim from
[anthropics/skills](https://github.com/anthropics/skills)
@ `2235be7c60b551f5de82ade908fd3816455afcda`
(`skills/frontend-design/`: `SKILL.md` + `LICENSE.txt`). The license ships
alongside the skill. Do not edit these files in place — re-vendor from
upstream and update the pin here (see the sibling `README.md`).

## impeccable agent skill — Apache-2.0 (shipped in-repo)

`.claude/skills/impeccable/` and the four `.claude/agents/impeccable-*.md`
subagent definitions are vendored verbatim from
[pbakaus/impeccable](https://github.com/pbakaus/impeccable)
@ tag `skill-v4.1.1`
([`5a149f3fdb1b5793f10567233b1dcab98fc305fd`](https://github.com/pbakaus/impeccable/commit/5a149f3fdb1b5793f10567233b1dcab98fc305fd)) —
153 files, the whole of that tag's `.claude/skills/impeccable/**` tree plus its
`.claude/agents/impeccable-*.md` siblings. It is a design-review skill (`shape`,
`audit`, `critique`, `polish`) plus a standalone deterministic anti-pattern
detector, and is itself derived from the `frontend-design` skill vendored above.

- **Not** vendored from the `impeccable` npm package: that package ships only
  the `detect` CLI and downloads the skill content from `impeccable.style` at
  install time, which pins nothing a reviewer can re-derive. The git tag above
  is byte-verifiable with `gh api`. `npx impeccable detect` still runs the npm
  CLI (**3.6.0**), which is invoked ad hoc and is not a repo dependency — it is
  absent from `package.json` on purpose.
- License: **Apache-2.0** (Copyright (c) Paul Bakaus), full text in
  `.claude/skills/impeccable/LICENSE.txt`; upstream's own third-party notice is
  carried beside it as `NOTICE.md`, per Apache-2.0 §4(d).
- Upstream's `.claude/settings.json` is deliberately **not** vendored — it arms
  a `PostToolUse`/`Stop` hook on every session. Rationale in the sibling
  `README.md`, which also carries the re-vendoring procedure.

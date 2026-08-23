# Vendored skill — do not edit in place

Everything in this directory except this `README.md` is vendored **verbatim**
from [pbakaus/impeccable](https://github.com/pbakaus/impeccable) @ tag
`skill-v4.1.1` (`5a149f3fdb1b5793f10567233b1dcab98fc305fd`), Apache-2.0.
The four `impeccable-*.md` files in `.claude/agents/` come from the same tree
and the same pin. See `THIRD_PARTY_NOTICES.md` for the notice entry.

- The prose-normalization rule in `CLAUDE.md` does **not** apply here:
  editing these files silently forks the vendor. To change anything,
  re-vendor from upstream and update the pin in `THIRD_PARTY_NOTICES.md`.
- `LICENSE.txt` and `NOTICE.md` are upstream's own, carried because
  Apache-2.0 §4(a)/§4(d) requires redistributions to include both.
- `.gitattributes` pins this tree to `eol=lf` so the worktree bytes match
  upstream's. Verify pristineness blob-vs-blob, never worktree-vs-upstream:
  `gh api repos/pbakaus/impeccable/contents/.claude/skills/impeccable/SKILL.md?ref=<sha>`.

## What is deliberately NOT vendored

- **`.claude/settings.json`** — upstream's copy is a hook manifest that runs
  `scripts/hook.mjs` after every `Edit`/`Write`/`MultiEdit` and again on every
  `Stop`. Taking it would arm a design detector on every agent session in this
  repo, most of which never touch a UI file, and would collide with this repo's
  own settings. Invoke the detector explicitly instead — `npx impeccable detect
  <path>` — or opt in per-machine via the gitignored
  `.claude/settings.local.json`.

## Repo-local adaptations when following the skill

- Its screenshot/visual-iteration loops must not launch the GUI unattended
  (`npm run tauri dev` never exits — see `CLAUDE.md` Commands). Use the E2E
  harness under `e2e/` for screenshots.
- `live` mode (`scripts/live/`, `scripts/live-server.mjs`) drives a browser
  against a dev server and carries framework adapters (Svelte, Next, Nuxt,
  TanStack) this repo has no use for; it is vendored only because the tree is
  taken whole. The frontend here is vanilla TS with no dev-server UI route.
- Scratch space is this repo's `./.scratch/`.

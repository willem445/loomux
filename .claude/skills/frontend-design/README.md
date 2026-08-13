# Vendored skill — do not edit in place

`SKILL.md` and `LICENSE.txt` are vendored **verbatim** from
[anthropics/skills](https://github.com/anthropics/skills)
@ `2235be7c60b551f5de82ade908fd3816455afcda` (`skills/frontend-design/`).

- The prose-normalization rule in `CLAUDE.md` does **not** apply here:
  editing these files silently forks the vendor. To change anything,
  re-vendor from upstream and update the pin in `THIRD_PARTY_NOTICES.md`.
- Two of the skill's suggestions need repo-local adaptation when followed:
  its screenshot self-critique loop must not launch the GUI unattended
  (`npm run tauri dev` never exits — see `CLAUDE.md` Commands), and its
  "jot down notes" scratch space is this repo's `./.scratch/`.

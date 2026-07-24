---
name: agent-cli-reference
description: When a change depends on Claude Code, GitHub Copilot CLI, or any other agent CLI's behavior (flags, hooks, settings, session files, transcripts, agent definitions), consult the official reference documentation directly — never infer from changelogs, memory, code comments, or prior assumption. Includes the doc index and the citation discipline.
---

# Agent-CLI facts come from reference docs, not inference

Loomux orchestrates other vendors' agent CLIs. Their capabilities change
under us, their flags have precise semantics we don't control, and a wrong
assumption about them ships as a loomux bug. Twice in one week this repo
paid for guessing (lineage below). The rule that replaces guessing:

> Before designing or reviewing ANYTHING that depends on an agent CLI's
> behavior — a flag, a hook event, a settings-merge rule, a session-file
> format, an agent-definition mechanism — fetch the official reference page
> for that exact surface and read it. If the network or the fetch tool is
> unavailable, say so and mark the dependency UNVERIFIED in the PR/design
> note; do not substitute recall or a changelog skim.

## Doc index

Claude Code (root: https://code.claude.com/docs/en/overview):

- Hooks (events, payloads, blocking semantics, merge behavior):
  https://code.claude.com/docs/en/hooks
- Settings files and precedence: https://code.claude.com/docs/en/settings
- CLI flags (`--agents`, `--append-system-prompt`, `--settings`,
  `--mcp-config`, `--resume`, ...): https://code.claude.com/docs/en/cli-reference
- Memory / CLAUDE.md loading: https://code.claude.com/docs/en/memory
- Slash commands (incl. `/compact`): https://code.claude.com/docs/en/slash-commands

GitHub Copilot (root: https://docs.github.com/en/copilot/reference):

- Hooks (events, payloads, config locations, merge behavior):
  https://docs.github.com/en/copilot/reference/hooks-reference
- CLI reference (flags incl. `--agent`, custom agent files):
  https://docs.github.com/en/copilot/reference — navigate to the CLI section
  for the current layout rather than trusting a deep link.

Other agent CLIs (Ante, hermes, future tiers): find the vendor's official
reference before wiring anything, and ADD its root URL to this index in the
same PR that introduces the dependency. A CLI with no reference docs gets
its observed behavior recorded in `doc/design/` with the version it was
observed against — labeled observation, never presented as contract.

## Citation discipline

- PR bodies and design notes that rest on a CLI fact cite the page (and
  section) the fact came from. "Per the hooks reference, `PreCompact`
  carries `transcriptPath`" — not "Copilot supports hooks".
- Distinguish three states explicitly, and never let one masquerade as
  another:
  1. **Docs say X** — cite it.
  2. **Docs are silent on X** — name it as a residual/open question (and
     make it a demo/live check if it matters).
  3. **Docs say NOT-X** — cite that too; absence claims need a source as
     much as presence claims.
- Changelogs, blog posts, release announcements, and community threads are
  leads, not sources. They may prompt a docs lookup; they never close one.
- When docs and observed behavior disagree, the observation wins for the
  code path (with a comment noting the divergence + doc link) and the
  divergence gets flagged in the PR for the human.

## Lineage (why this skill exists)

- PR #418 round 1 declared "Copilot has no compact hooks upstream" from a
  changelog read. The hooks REFERENCE showed 14 events including
  `preCompact` — the user had to supply the link. A full correction round
  followed (#417).
- The same PR hedged Claude's `--settings` hook-merge semantics as
  "unverified" through three review rounds; the hooks reference documents
  the additive merge model directly. The hedge cost a demo-script step and
  reviewer time.
- Same-family precedent: #329 round 7's context-window fix initially
  hardcoded a model table where the transcript itself carried the model id
  (the price-table convention already knew this).

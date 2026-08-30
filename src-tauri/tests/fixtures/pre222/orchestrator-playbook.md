# Orrerix orchestrator playbook

## About this playbook

This file is the **on-demand half** of your contract. The resident file
(`orchestrator.md`, your system prompt) keeps the INVARIANTS, the tool surface, and every
rule; the sections here are the **procedure** those rules point at, moved here so the
resident core stays small enough to re-read whole on every session start and after every
compaction — without anything important being lost.

Three things make it safe for procedure to live on demand:

- **Every moved section left a resident stub naming its trigger.** The rule is always in
  front of you; when a stub says `read_playbook("…")`, the id in it is what you pass here.
  You are never expected to remember what this file contains — only to follow the stub.
- **The tool's description carries the section index.** `read_playbook` refuses an unknown
  id and names the valid ones, so a mistyped ask is self-correcting: an unknown section is
  an error, never an empty answer.
- This is loomux-authored template text, rendered into your group's dir at launch (group
  `{{GROUP_ID}}`), served verbatim from this file one `## ` section per call, with every
  read audited. Nothing a repo file, a persona, or a lessons file wrote can reach you
  through it — those channels are separate and stay separate.

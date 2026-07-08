---
title: Steering & attachments
layout: default
parent: Features
nav_order: 4
---

# Steering & attachments
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

---

An orchestrator pane has a thin **compose field** docked under its terminal
(styled like the task board's *Add a task* field). This is the collision-proof
way to talk to a running orchestrator — steer its plans, answer a question, or
redirect it — without fighting the CLI's own input box.

## Steering strip

Type steering in the strip and press **Enter** — loomux enqueues it to the
orchestrator through the **same serialized delivery path** worker reports use, so
your message and an incoming report can never land in each other's text: the
pane's input has exactly one writer.

- The field wraps and grows to a few lines as you type; **Shift+Enter** inserts a
  newline for a multi-line message, and past a few lines it scrolls internally
  rather than pushing the terminal (the strip floats over it, so the PTY is never
  resized).
- Focus the strip with **`Alt+P`** (or click it); **`Esc`** hands focus back to
  the terminal.
- Because it's a loomux field and not the CLI's own input box, it never steals
  the terminal's keys — type freely in the terminal and the strip stays out of
  the way.
- Steering a **paused** group, or a pane with no live orchestrator, is reported
  inline rather than silently dropped.

You can still type directly into the CLI if you prefer; loomux holds an incoming
report for a few seconds while it sees you typing there, but the strip is the
collision-proof path.

## Attach a screenshot

Paste an image with **`Ctrl+V`** (or click the paperclip to pick files) and it
joins the message as a thumbnail chip — remove one with its **✕**, or queue
several.

On send, loomux saves each image to the group's scratch dir and adds an
`Attached image:` reference line to the message — formatted the way the
orchestrator's CLI reads it (a plain path for Claude Code, an `@<path>` mention
for Copilot) — so the agent opens the screenshot.

- Accepted formats: **PNG, JPEG, GIF, WebP, BMP**.
- Up to **10 MB each** and **8 per message**.
- The saved files are cleaned up when the group ends.

## Related

- The [voice prompts](voice-prompts.html) 🎤 button records straight into this strip.
- The [task board's](../orchestration.html#the-task-board) merge-gate and **▶ Start**
  buttons deliver through the same one-writer path, so board actions and typed
  steering never collide.

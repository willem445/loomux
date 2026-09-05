---
title: Agents tab
layout: default
parent: Features
nav_order: 11
---

# Agents tab
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

---

## What it is

The **agents** button in the top bar opens the same left panel the session
browser lives in, on its second tab: one row per pane in this window, saying
what each one is doing right now.

It is an overview of the *live* window — not a history and not a fleet-wide
report. Every row is derived from what the pane already knows, so opening the
tab costs nothing on the wire and asks no agent anything.

The panel has two tabs and they share one column. Clicking **agents** when the
Agents tab is already showing closes the panel; clicking it while the Sessions
tab is showing switches without moving the column, so your terminals are not
resized by changing tabs. `Ctrl+Shift+P` still opens the Sessions tab.

## A row

Each row carries the agent CLI's mark, the pane's name, its state, and a quiet
line naming whichever of these the pane has: the agent CLI it is running, its
orchestration role or workflow block, and its group.

The mark is the same one the pane header wears, resolved from the same reading —
GitHub's Copilot glyph where a vendor publishes a licensed one, a lettered badge
otherwise, and for a remote pane whose profile does not say which agent runs on
the far end, a neutral badge that says orrerix does not know.

A pane that is not an agent is never given a guess. A pane with no launch line at
all — one you opened and typed into yourself — carries no mark; a pane launched
with a shell or a transport (`bash`, `pwsh`, `ssh`) carries the same neutral
badge its header does, because naming it "Agent CLI: bash" would be a confident
wrong answer.

**Click a row to go to that pane** — it switches to the pane's project tab,
makes it the active pane there, and focuses the terminal.

It *reveals* the pane rather than just focusing it, so a row always gets you
somewhere you can see. If the pane is parked in the dock, it comes back into
the grid; if another pane is filling the window, that fullscreen drops first.
Clicking the row of the pane that is *already* fullscreen leaves it
fullscreen — you are looking at it, so there is nothing to reveal.

## Groups, and the order

Rows are grouped under the project tab they live in, with the tab's name as the
header. A tab with no panes in the list shows no header — including a tab whose
rows have all been filtered out by a chip.

The pair of buttons beside the **Agents** heading chooses which group comes
first:

| Order | Groups come in this order |
| --- | --- |
| **most wants you** | The tab holding the most urgent pane first. Two tabs whose worst pane is in the same state stay in your tab-strip order. |
| **by tab** | Your tab-strip order — the arrangement you dragged the tabs into. Never alphabetical, so renaming a tab does not reshuffle the list. |

Inside a group the order is the same either way: most wants you first, then by
name. Your choice is remembered on this machine, and changing it does not resize
anything.

## The states

| State | What it means |
| --- | --- |
| **needs you** | The pane is wedged and will not un-wedge itself — blocked, stranded on an unsubmitted prompt, or held on a dialog. |
| **question** | The agent is asking you something: a question or a gate. |
| **reported** | The agent has called in — its report is waiting on its orchestrator, not on you. The same word the pane header's chip uses. |
| **held** | Orrerix is withholding a delivery to this pane because its input box looks occupied. It clears itself. |
| **turn done** | The agent finished and is parked at its prompt. See the caveat below. |
| **working** | No evidence of a prompt. This is the default reading, and it is honest about being one — see below. |
| **idle** | For an orchestration pane, the roster holds no assignment for it; for any other pane, nobody has ever typed into it. Either way it is also quiet. |
| **dormant** | A restore placeholder. Nothing is running yet — click the pane to bring it back. |
| **exited** | The pane had a process and lost it. |

A pane carrying several of these at once is reported by the one that most wants
you: the list above is in precedence order, top first.

### "working" means *no evidence of a prompt*

Orrerix does not ask an agent whether it is busy — there is no such question to
ask, and inventing one would mean typing into a pane you are watching. `working`
is what is left when nothing else applies. A pane genuinely thinking and a pane
sitting at a prompt orrerix has not recognised both read `working`, and that is
the direction this deliberately fails in: it will under-claim a finished turn
before it will claim one that has not happened.

### "turn done" is a different confidence per CLI

`turn done` is the only state that makes a claim about the *agent* rather than
about the pane, so how far to trust it depends on which CLI is running.

| CLI | Trust |
| --- | --- |
| **Claude Code** | Trusted. Its idle input box is a shape orrerix measures directly. |
| **Copilot CLI** | Trusted after the first turn. Its boot-time terminal chatter is excluded from what counts as your input, and its permission prompt shows up as **question** rather than as a finished turn. |
| **OpenCode** | Trusted, with one unmeasured edge: whether its footer repaints are big enough to read as work has not been measured. If they are, the pane reads **working** — never a finished turn that has not happened. |
| **Anything else** | The generic prompt reading still runs; **working** is the default. |

Two more things worth knowing about it:

- **Clicking a parked pane does not un-park it.** Focusing a pane clears its
  attention chip, and reading that as "the agent resumed" would flip a finished
  turn back to `working` the instant you looked at it. The state clears on
  evidence instead: you typing, or the pane painting something substantial.
- **An orchestration agent being *idle* is not the same as it being at a
  prompt.** The roster's idle reading means "this agent holds no assignment",
  which is what feeds the **idle** state and nothing else.

## Filtering, and the count

The chips under the heading filter the list to one state, with a live count on
each. A chip is only offered for a state something is currently in — except the
one you have selected, which stays so you can always get back out of it.

The number on the **agents** button and on the tab is how many panes are in
**needs you** or **question**: the two states where a person has to do
something. It is visible with the panel closed, which is the point of counting
it. `held` is orrerix's own doing and clears itself; a **reported** pane is
waiting on its orchestrator, not on you; a finished turn is not blocking
anyone; and nothing is waiting on a dormant or exited pane.

## The spinner

The **working** row wears a small animated pixel-dot glyph — the same
character-cell style a terminal agent draws for itself. It is one inline SVG
sprite stepped by CSS, so it costs nothing per frame beyond what the compositor
does, and the row's state word carries the meaning on its own.

If your system asks for reduced motion, the glyph is drawn still. The word does
not change.

## What it does not do

- **No new probing.** Nothing here reads a terminal's screen on your behalf,
  starts a background poll of its own, or sends a keystroke to find out what a
  pane is doing.
- **No resize.** Opening and closing the panel resizes the panes exactly as it
  always did; switching between its two tabs, filtering, and changing the group
  order do not resize anything.
- **This window only.** A pane in another orrerix window, or an agent with no
  pane open, is not listed. The [session browser](session-browser.html) is where
  work you are not currently looking at lives.

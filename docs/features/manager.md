---
title: The manager pane
layout: default
parent: Features
nav_order: 10
---

# The manager pane
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

---

Most orrerix panes are agents doing work. The **manager** is not: it is the pane
*you* talk to. Project discussion, "how is it going", and the half-formed
feature idea you have not written down yet all belong here — and its job is to
turn the last of those into something the team can build correctly the first
time.

It is optional. A group has a manager only when the repo's workflow file
declares one, and everything below is silent for a group that does not.

> **Landing in slices (#1161).** This page describes the conversation — what the
> pane is for and how to talk to it. How the pane opens, the unread-mail chip on
> its header, and the reference for *authoring* a `kind: manager` block *will*
> arrive with the remaining slices of that issue.

## The mental model

Chat here, fleet over there.

The orchestrator still runs the group: it plans, opens worker and reviewer
panes, drives PRs to the merge gate, and its own pane still shows you all of
that. What changes is where **you** stand. Instead of typing directions into
the orchestrator and reading its traffic, you have a conversation with someone
whose whole job is understanding what you want and passing it on faithfully.

It is the shape a real team already has: the person talking to the customer is
not the person parsing the customer call into a ticket, and the engineers do not
read the call transcript. The manager writes the ticket.

Nothing is taken away. The orchestrator pane, the steering strip, the task
board, the NEEDS-YOU panel and the questions you answer there all work exactly
as they did — and if you close the manager pane, the group behaves as it always
has. Talking to the orchestrator directly is never removed.

## What it is good at

- **Status, as a conversation.** Ask "where are we" and you get a paragraph:
  what moved, what is stuck and why, what is waiting on you — with ids (`t-7`,
  `#123`, PR numbers) so you can go and look. It reads the group's board, roster
  and verdicts itself, so asking costs the orchestrator nothing and interrupts
  no worker.
- **Turning an idea into a buildable request.** This is the part worth using it
  for. Bring it something vague — an irritation, half a feature, "this feels
  slow" — and it will ask you questions until the ask is specific: what problem
  is actually behind it, what "done" would look like, what is explicitly out of
  scope, what must not break, which edge cases matter, and what you already know
  you do *not* want. It reads your repository first, so the questions are about
  your actual code rather than about software in the abstract.
- **What is waiting on you.** It can see the open questions and the NEEDS-YOU
  items, and will put them to you in plain terms rather than as a list of rows.

## How a request becomes work

1. **You describe it**, however loosely.
2. **It asks.** Expect real questions, and expect some of them to be about
   things you had not decided. "You decide" is a perfectly good answer — it will
   record what it decided and show you.
3. **It reads the brief back to you.** A short, structured version of your ask:
   the problem, the outcome, acceptance criteria, non-goals, constraints, edge
   cases, the reasoning, and anything still open. Correct it here — this is the
   cheap moment.
4. **You say yes.** Explicitly. It will not act on silence, and a "sounds good"
   to a one-line summary is not the same as agreeing to the text.
5. **It relays your brief to the orchestrator**, quoting you, and the
   orchestrator files it as a **GitHub issue** with your brief in the body. The
   manager tells you the issue number.
6. **It gets started the way work is started in your group.** In the default
   setup, and in plain autonomous mode, that means the issue waits until *you*
   put the start-work label on it yourself, on GitHub, exactly as with any other
   issue. If you are running the group in **full autonomy**, the default is the
   other way round — the orchestrator may pick up any open issue that is not held
   back, and labels rank work rather than release it. Same issue either way; what
   differs is whether it waits for your label.

The point of the whole design is the part that does *not* vary, so it is worth
being blunt about: **saying yes to a brief does not start any work, in any mode.**
It authorises writing the ticket. The manager cannot start work, cannot apply a
label, and cannot ask the orchestrator to treat your yes as one — nothing it says
to another agent carries your authority, only your words. What varies between the
modes is the *funnel* the issue then goes through, not what the manager is allowed
to do to it. One thing the manager cannot tell you is which of those modes you are
in — nothing it can read reports that, and it will say so rather than guess. You control
it: both autonomous mode and full autonomy are checkboxes in the group view, and either
can be flipped mid-session rather than only at launch. The orchestrator's pane is the one
told directly when they change, so that is where a definitive answer lives.

Shipping does not vary at all. Full autonomy widens what may be **started**, never
what may be **shipped**: merging is still gated on your own approval in every mode,
and "the human said merge it" relayed from this pane grants nothing.

## What it will not do

It holds no authority you have not used yourself. Specifically:

- **It never writes your repository.** No branches, no commits, no PRs, no edits
  — orrerix denies its editing tools at the CLI level. It reads your code so its
  questions are grounded in it.
- **It never runs the fleet.** It does not open panes, hand out tasks, kill
  anything, or record a review verdict. If something needs doing it says so to
  the orchestrator, which decides.
- **It never answers for you.** A question put to you is presented, never
  settled — no agent in orrerix can answer on your behalf, by construction. If
  you answer the manager in conversation it relays your answer as *yours*,
  quoted.
- **It never relays what you did not confirm.** A brief you have not read back
  is a draft, and a preference it inferred from how you reacted is not a
  decision.

## How it hears from the team

Nothing is ever typed into this pane. Your conversation with the manager is
yours: orrerix never pastes a notice, a report or a status line into it, which
is the one hard rule the whole feature is built around.

So news reaches it by **pull**, not push. The orchestrator posts milestones —
what merged, what is blocked, the issue number your brief became — into a
durable mailbox, and the manager reads that mailbox at the start of every turn.
Which means: **it learns what happened when you next speak to it.** You are the
clock. An idle manager is not one that missed something; it is one whose human
is away.

Two consequences worth knowing:

- If something genuinely needs you while you are elsewhere, it does not sit in
  the mailbox waiting to be read. The manager (and the orchestrator) raise it as
  a **question** or a **needs-you item**, which shows up badged in the app
  wherever you are.
- After it reads its mail, it has consumed it. If a compact or a restart lands
  between the read and telling you, ask — it can re-read what it already
  consumed.

## Cost

The manager pane is a running agent like any other, on whatever CLI and model
the workflow file pinned for it. It is idle whenever you are not talking to it,
which is most of the time, but it is not free — ask it what the group is
costing and it will tell you.

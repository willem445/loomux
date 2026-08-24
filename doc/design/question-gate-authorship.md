# The question gate and authorship (#576 / #871 / #903)

The interactive-question guard (#420) asks one question of a pane: *is a live dialog on
screen, so that an Enter would select an option rather than submit text?* Everything in
this note follows from a single fact about that question — **it cannot be answered from
the shape of a row.** A menu's highlighted choice and a line of ordinary prose that
happens to lead with `❯` are the same row. The only thing that separates them is who
wrote it, and authorship is not something a reading of the screen can recover.

`doc/design/orchestration.md` carries the guard itself: the detector, the composed-screen
reading (#534), the idle-composer release and the bounded override (#903). This note
carries the part that is about **provenance** — what loomux is allowed to remember having
written into a pane, what that memory may be used for, and what an adversary can do with
it.

## The two rows that cannot be told apart

```
❯ 1. Yes, allow once                                       <- a live permission dialog
❯ [orch] Round 3 (cap) re-record for #1429 at new head …   <- a resumed transcript
```

Both lead with a pointer glyph at content. Both sit above an empty composer. Both are
static for as long as the pane is idle. The first must hold a delivery forever; the second
must not hold one at all. `h4` in `src-tauri/tests/orchestration.rs` pins the first as
MUST-HOLD, and `j1` pins that the second is byte-for-byte the same shape.

The second row is not hypothetical. It is what the live #903 wedge was: pane `rev-1277`
resumed session `39b611be`, and `claude --resume` replayed the prior turn — whose last
user message was **loomux's own kickoff prompt to `rev-1262`**, the same session, the same
application run, delivered ninety minutes earlier. For thirty minutes every drainer poll
recorded `signal:"pointer-option"`, `grid:"still-rendered"` and `idle_row:true`. loomux
could see the empty composer and refused to act on it, because the row vetoing the release
was one loomux had written itself.

So the guard needs a record of what loomux wrote. That is the whole of #576, and the whole
of this note is what may safely go into it.

## Two records, admitted two different ways

| | what it holds | keyed by | how a line gets in |
| --- | --- | --- | --- |
| `DeliveredNotices` (#576/#661) | marker-led notice rows | **pane** | a producer opts in (`mark_notice_maskable`), then a write delivers it |
| `DeliveredPrompts` (#903) | prompt bodies — kickoff briefs, `send_prompt` bodies | **session** | provenance alone: `delivered_prompt_lines` admits it, a write records it |

Both are written on the delivery side, from the bytes loomux pasted, after the write
returns `Ok`. A pane cannot add to either: what a pane *prints* never reaches a record, so
an agent printing a marker row — or a row that looks like a brief — widens nothing.

Two differences are load-bearing.

**The prompt record is keyed by SESSION, and that is the fix rather than an optimisation.**
A resumed pane is a new pty replaying an old transcript, so a per-pane record is empty at
exactly the moment the screen is fullest. The text outlives the pane, so the record has to
as well. `j7` pins it: a line delivered into one pane is claimable from a second pane that
resumes the same session, and from no other session.

**The prompt record has no producer opt-in, because its admission rule is the promise.**
`DeliveredNotices` parks a line until a producer promises the pane's own agent did not
author any span of it. There is no producer to ask for a brief. What stands in its place is
a filter on what may enter at all — and that filter is the subject of the next section.

## The attack shape that stays closed

`OrchRegistry::mark_notice_maskable` documents why the notice record is default-closed, and
it is worth restating because it is the reason #903 was **not** fixed by simply widening
that record:

> `notify_when(kind, pr, note)` takes an agent-supplied `note` (up to `NOTICE_FIELD_CAP`),
> and `notify_tick` delivers the fired notice **to the registering agent's own pane**.
> `sanitize_gh_text` maps `[`/`]`
> to parens and strips control characters — it does not touch ordinary words, `?`, `(`,
> `)` or `❯`, which is everything a dialog row is made of.

So one agent, with one tool call it makes itself, could put a line of its own choosing into
its own pane's record; print that line's head into its own pane; and let its CLI paint a
permission dialog whose rows are exactly that line's remainder. The run reconstructs, the
dialog rows are masked away, the gate reads clear, and loomux presses Enter into a live
question. **One party, no coincidences, deterministic.**

`delivered_prompt_lines` closes that route by construction: it drops every marker-led line,
so notice text never enters the prompt record.

**What makes that sufficient, checkably.** The claim the record rests on is that no agent can
address an admitted delivery to its own pane, and it is enforced in three independent places
rather than observed:

| admitted delivery | why an agent cannot aim it at itself |
| --- | --- |
| `send_prompt` body (`MidSession`) | the dispatch refuses `a.id == caller.agent_id` — *"cannot send a prompt to yourself"* |
| kickoff brief (`FreshKickoff`/`ResumeKickoff`) | its target is an agent `spawn_agent` has just created; the caller cannot be it |
| everything else | refused by `prompt_record_admits_kind`, which has no wildcard arm |

The two routes that really did reach an agent's own pane — the post-compact re-grounding notice
and `resume_kickoff_notice`, both of which paste that agent's own directive ledger verbatim, and
both reachable on demand through the self-callable `request_compact` — are refused by the
delivery-kind term and by the first-line term respectively. `note_directive(text, replace: true)`
writes the ledger **raw**: no sanitize, no cap, not even the `[ts]` prefix its append branch adds.
That is what made this a one-party capability rather than a two-party coincidence, and it is the
reason the rule is about the authorship of the DELIVERY rather than of each line — a notice is one
marker-led line above a body that is not, so a per-line filter admitted every continuation row. `j6` pins it, with a positive control so the assertion cannot pass by the record
simply always being empty.

## The attack shape that is bounded, not closed

What the prompt record does admit is a kickoff brief and an orchestrator's `send_prompt`
body. That is a real widening and it deserves a plain statement of what it buys an
adversary.

**The shape.** An orchestrator chooses the text of a `send_prompt` into a worker's pane. If
it sends a line that is byte-identical to a row a real permission dialog will later paint
there, and the worker's CLI then paints that dialog, the mask can claim the dialog's row as
loomux's own and the gate can read clear.

**What bounds it.**

- **Reconstruct-to-end.** A run is claimed only when the rows account for the recorded line
  *to its end*. A run that merely starts one proves nothing about the rows after it, so the
  last claimed row is the line's last row and nothing painted below it is swallowed.
- **`dialog_header_above`.** As of #903 **every** record claim owes this, not only the
  pointer-stripped ones: the claim is refused when a dialog's own question row heads the
  block above the anchor. Since a real dialog's option list is headed by the question it is
  asking, the masked-away option cannot be one a human is being asked about. `j3` is the
  pin, with a control proving the same line IS claimed without the header — so the refusal
  is the guard and not an accident.

  **A chain of claims can no longer walk that scan past the header.** The scan used to test
  `keep` before testing for a header, so a row already claimed was stepped over — and two
  recorded lines were enough to defeat the bound: claim the dialog's question row with the
  first, and the second's option row then reads as having nothing above it. The header test
  now runs first, so a header vetoes whether or not its own row was claimed, which is a term
  the record cannot buy at any number of claims. `j14` pins it with the two-line chain, and
  `j3`'s single-line record is exactly why that gap survived the first round.
- **The trust asymmetry.** `send_prompt` is the orchestrator's tool. The party this residual
  is available to already holds spawn and prompt powers over the pane in question: it can
  start the agent, replace it, and dictate what it is told to do. A capability to influence
  one masking decision in that pane is not a new tier of authority for it — which is
  precisely what the `notify_when` route WOULD have been, since it is available to any
  agent about itself.

  **One more admitted route, and it is not one-party either — stated because it is the kind of
  thing a residual section is for.** `review_verdict`'s multi-line `summary` reaches the
  ORCHESTRATOR's pane through the relay, and its continuation lines are `Lines::Keep`, so a
  reviewer's own words can enter the orchestrator's session record. It is two-party in the same
  sense as the row above: the reviewer chooses the text, but it lands in somebody else's pane,
  and the harm needs that pane's CLI to paint a dialog whose rows the reviewer also chose. What
  it is NOT is symmetrical with `notify_when` — a reviewer cannot aim it at its own pane, which
  is the property the whole record now rests on.

  That asymmetry, not a probability argument, is why one was taken and
  the other refused.

**What it does NOT bound, honestly.** Two gaps, both real:

- **A dialog with no question row of its own.** `dialog_header_above` needs a `?`-shaped row
  to veto on. The Claude MCP approval capture (`claude-mcp-approval.txt`) has none — it is
  kept a question today by its confirm footer, not by a header — so a claim against one of
  its option rows would not be refused by this term.
- **A header the same party controls.** The scan vetoes on the first question row above the
  anchor. Where the text above the option block is *also* something the orchestrator put
  there, there is nothing independent for the scan to find.

Neither is closed here, and neither is closed by a probability estimate. They are the price
of the widening, recorded so that a future reader deciding whether to widen further starts
from what is actually open.

## The Enter, and what #903's override now does with it

Two further readings changed with #903, and both touch this note's subject.

**A collapsed paste is still our own text.** A CLI that replaces a multi-line paste with a
placeholder of its own (`[Pasted text #1 +6 lines]`) leaves `mask_own_paste` nothing to
match, so the composer reads as neither empty nor ours and `idle_row` flips true→false at
exactly the checkpoint that decides the Enter — on an otherwise unchanged screen. The live
incident shows the flip in two consecutive audit records. `mask_own_paste` now claims such a
row, on two narrowing terms: the paste must be **multi-line** (a single line is not one a
CLI collapses, so a placeholder beside one is not ours — `j5`), and no dialog header may sit
above it (`j10`, with a control that the same placeholder IS claimed once the question row
is gone — that term went unpinned in the first cut of this work, and a residual claiming it
bounded the claim was therefore a claim rather than evidence). The evidence being claimed on is the CLI's, not a shape invented here: a pane that
took our bytes into a free-text composer is a pane that was not showing a modal. The
placeholder's `#N`/`+M lines` parts are deliberately not parsed — that text is the CLI's to
change and this repo has no citable specification of it.

**A granted override carries its grant to the Enter.** This supersedes the pre-#903 claim
that the override "can paste into a live menu; it cannot press Enter into one".

The override existed to move a queue that a fifteen-minute false positive had stopped. But
it skipped only the pre-paste gate, so the pre-Enter checkpoint re-read the same unchanged
screen, reached the same false positive *by construction* — the grant's own precondition is
that this reading is wrong — and aborted with the text already in the box. The result was
strictly worse than never overriding: a stranded paste that every later delivery queues
behind, and a pane a human has to kill. That is what the live incident did, twice.

So an attempt the drainer granted an override to now presses its Enter, on three terms: the
grant is the caller's (never re-derived at this site), the evidence is a **fresh** sample
taken twice at that moment (never a latched flag), and the bar is the **weak** idleness
reading — the same one the grant was decided on, because the strong one is exactly what is
wrong on this pane class and requiring it would make the code unreachable.

The decision is `override_enter_admits`, split from the pane reading
(`preenter_override_admits`) for the reason `question_hold_predicate_sampled`'s own doc gives
about rev-15 B4: a rule welded to a live `PtyManager` is one no test here can drive, so it
ends up pinned by nothing. Its two terms — *enough* reads, and *every* read admitting — are
pinned in both orders by `j9`, so an implementation that consulted only the first or only the
last read fails.

**The residual this leaves, stated as its own thing.** A dialog painted *above* a composer
that holds our paste, showing no menu-structure token evidence, inside the override window,
would get the Enter. It is narrower than it first sounds: `h13`'s `AskUserQuestion` — the
dialog shape that motivated the menu-structure conjunct — is caught by the **token** clause
(numbered options and a selection footer), not the pointer one, and is therefore not in this
residual. What is in it is a pointer-led dialog with no numbered options and no selection
footer, painted above a live composer, fifteen minutes into a hold the human has been badged
about for five of them. The human directed this trade explicitly ("self-healing panes"); it
is recorded here as a trade and not as an absence.

## Limits

- **Losing a record is fail-closed.** Both records are in memory and neither is snapshotted.
  A restart degrades the prompt record to nothing, which is the pre-#903 behaviour: the gate
  holds and `QuestionStale` badges it at ten minutes.
- **A prompt line longer than `DELIVERED_PROMPT_CHARS` is dropped, not truncated.** A
  recorded prefix could only be claimed by accident — a run whose rows happen to end exactly
  where the truncation did — so the record would carry entries whose meaning depended on a
  pane's width. Dropping costs a hold.
- **The record does not survive a coalesced flush's own framing.** Only the constituent
  payloads are prompt bodies; the flush header is marker-led and stays on the notice rule.
- **Nothing here makes the detector quieter.** Every fix in this note is about recognising
  loomux's own text. A genuinely question-shaped row that loomux did not write still holds a
  delivery, exactly as it did before — which is the direction this guard is always allowed to
  be wrong in.

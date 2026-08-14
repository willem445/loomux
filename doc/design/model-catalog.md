# The model catalog and the shared model picker (#935)

Two surfaces let a human pin a model: the launcher's per-role selector
(orchestrator mode) and the workflow pane's block editor. Until #935 they
answered "which models can this CLI take?" differently — the launcher with a
dropdown built from a curated list merged with a backend probe, the block editor
with a free-text box. The pain the issue opens with is the free-text box: the
human has to remember each CLI's exact ids, and a typo is only discovered when
the group fails to spawn.

The fix is not a second dropdown. It is **one** catalog (`src/modelcatalog.ts`,
DOM-free and unit-tested) and **one** component (`src/modelpicker.ts`, DOM wiring
only), because the second copy of a control is the second place a fix has to be
remembered.

## Two kinds of claim, and which one leads

| source | what it is | how it ages |
| --- | --- | --- |
| curated (`orchclis.ts`) | a suggestion list written down in this repo | badly (#329) |
| probed (`probe_agent_cli`) | what the CLI on THIS machine reports about itself | current by construction |

So **the probe leads and the curated list backs it.** A machine's own answer beats
a suggestion — opencode's enumerator reports the providers the human actually
configured, which no list in this repo can know — and a CLI that reports nothing
(a parse miss, an older build, not installed) degrades to the suggestions rather
than to an empty dropdown.

The merge never *replaces* curated entries. Each role's default is drawn from
them, and `pickerSelection` falls to its `custom…` branch for a value outside the
list: a dropped default would open the form on a hand-typed id, looking exactly
like a choice the human made themselves.

## The inherit row is pinned first

`INHERIT_MODEL` is the empty id — the "send no `--model` at all" row, which on
opencode is the only honest default (#722): every other value would silently
override the model the human's own config selects.

A real enumerator can report dozens of ids. Appending the curated list after them
would put the one row that means *don't choose* at the bottom of the menu, which
is the row a human is least likely to find and the one loomux most wants them to
keep. It is **pinned, not sorted in**: everything else keeps the probe-leads
order.

The same value drives a second rule. `pickerSelection` tests menu membership
*before* it tests whether the current value is empty, because inherit is both an
empty string and a deliberate choice; with the tests the other way round, a
human's "inherit" reads as "nothing chosen" and falls through to whatever happens
to be first in the list.

And a blank id arriving *from a probe* is dropped rather than rendered: an empty
option displays as "(none) — the model your own CLI config selects", so
manufacturing one for a CLI whose curated row has no inherit entry would advertise
an inheritance the spawn path then overrides with `default_model`.

## No curated fallback for a CLI with no row

`curatedModels` returns `[]` for a CLI this repo has no row for, deliberately not
reusing `orchCliFor`'s fall-back-to-the-first-row. That fallback exists for the
launcher's Agent field, where orchestrator mode restricts the select to
`ORCH_CLIS` ids so it never fires. The block editor's list is wider — `gemini` is
a `WORKFLOW_CLIS` member with no `ORCH_CLIS` row — and falling back there would
offer claude's aliases as gemini models: a wrong answer wearing a right one's
clothes. Nothing to suggest is an honest answer; the CLI is still probed, and
`custom…` is still there.

## The `custom…` escape is not a nicety

A dropdown that could only offer ids loomux already knew would be a **narrower**
field than the free text it replaces. Bedrock inference profiles, gateway
deployment names and any model newer than this build all arrive as ids no curated
list or probe carries. The picker fails open on them, the same posture
`selectorknobs.ts` takes on an id it cannot place. (Which is also why the picker
is not a `<datalist>`: browsers filter a datalist's suggestions by the input's
current text, so a pre-filled default hides every other option.)

## What this module does NOT state

No vendor facts. Which models a CLI accepts is the CLI's to report and
`orchclis.ts`' to suggest; whether a knob applies to the selected model is
`selectorknobs.ts`', narrowed by the backend's `CLI_CAPS` via `agent_cli_knobs`.
A model table here would be a third copy of the thing #329 says not to keep one
of.

## The block editor is the second host, and it is not a copy

The workflow pane's block form renders the same `ModelPicker`, against the same
catalog, styled with its own classes. Two rules are its own, and both come from
what a *file* is as against what a *launch* is.

**The blank row is offered on every CLI.** A block's `model:` is optional, and
leaving it out is a declared state: `model_of` (workflow.rs) resolves it to
`default_model(cli, kind)`. The launcher has no equivalent — every role there
starts on a real default drawn from the curated row — so its list for claude
carries no empty entry, and `pickerSelection` would fall a block with no `model:`
through to `sonnet`. That is a choice nobody made, displayed as if they had, and
it would leave no way back to "leave it to loomux" once anything was picked: a
field *narrower* than the free text it replaced, which is the one thing this
issue may not produce. `blockModelOptions` prepends `INHERIT_MODEL` — but only
when there is a menu to put it in front of, since a CLI with no curated row and
nothing back from its probe (`gemini`, today — it is probed like any other; the
reply is what carries nothing) opens straight onto the custom input, where an
empty box already means what the blank row means.

**And it reads differently.** `INHERIT_MODEL_LABEL` says "the model your own CLI
config selects", which is true of an empty `--model` on a pane loomux launches
and false of a blank `model:` on three of the four CLIs a block may name
(`sonnet`/`opus` on claude, `auto` on copilot, `pro` on gemini — only opencode's
`default_model` is genuinely empty). So the picker takes the blank row's text as
an option, and the block editor passes one that names the *rule* rather than one
CLI's outcome.

## The knobs follow the model, keystroke by keystroke

`context` is available only where the selected model has a documented `[1m]` form
(#687/#709). The block form derives its controls once, at render, and its model
control edits with the form re-render suppressed — it has to, because
re-rendering rebuilds the input under the human's caret. Those two facts together
were the bug: type `sonnet` over `haiku` and the context select stayed disabled,
quoting a reason about a model that was no longer selected, until you clicked
away from the block and back.

The fix is not to re-render. `workflowknobs.ts` holds the model and re-derives on
`setModel`, so the two rows can be **repainted in place** from a fresh
`KnobFieldSpec` while everything else on the form stands still. `ModelPicker`
already fires `onChange` for a dropdown pick *and* for every keystroke in its
`custom…` box — the typed case a `change` listener on a `<select>` never sees,
which is how the bug survived. The same repaint serves the other input that moves
under a form which cannot re-render: the `agent_cli_knobs` reply, which lands
whenever the IPC resolves and turns both rows from "reading this CLI's
capabilities…" into real options with no model having moved at all.

Two rules there are the editor's own, against the launcher's. A declared value
the CLI cannot deliver still shows, marked — dropping it would rewrite the
human's file the moment any other field was touched — and the control stays
**enabled**, because a value you can see and cannot remove is worse than one that
is merely wrong. The launcher resets such a value instead (`knobValue`): there
the control's job is to decide a payload, and a stale pick must never reach it.

## Seams

- The probe arrives as an **injected function** (`pty.ts`'s `probeAgentCli`), so
  every rule above is testable with no browser and no Tauri host.
  `ModelCatalog.probe` memoizes on the in-flight promise (two forms opening at
  once make one call) and never rejects — a rejected promise reaching a form's
  render path turns "we couldn't ask" into a broken field.
- `ModelCatalog.models(cli)` is **synchronous**: a form paints on its first frame
  and cannot await a probe whose worst case is the backend's 8s timeout. It
  re-paints when the probe resolves.
- `ModelPicker` takes its CSS classes — and its blank row's text — as options. A
  shared control that hardcoded one host's classes would be unstyled in the
  other, and one that hardcoded the blank row's wording would state a vendor
  outcome that is false on the other host's CLIs.
- The catalog **instance** is app-wide (`modelprobe.ts`), not per-form. The memo
  is the point: a launcher pane *becomes* a workflow pane when "Edit workflow…"
  is pressed, and a per-form catalog would re-probe every CLI across that
  handover. It lives in its own module because `modelcatalog.ts` must stay
  reachable from `node --test`, and the instance has to be wired to `pty.ts`.
  What that promotion costs, and how it is paid, is the section below.
- `workflowknobs.ts` takes the capability answer as an injected `KnobLookup` —
  the same seam `analyzeWorkflow` takes — so the knob rules are testable with no
  browser, no Tauri host and no backend.
- Slice A's opencode enumerator (`opencode models`, shipped in #939) plugged in
  with no change here: it only makes `CliProbe.models` non-empty for a CLI that
  reported nothing before, which is the case every rule above was written for. It
  did add one thing this note has to answer, and the section below is that answer.

## A memo in front of the backend inherits the backend's caching rule

`probe_agent_cli` (cliprobe.rs) caches **complete** probes for the app run and
deliberately does not cache the rest:

> failures and partial answers are NOT — a CLI installed while loomux is running
> must become launchable on the next probe … and by the same argument an opencode
> whose `models` run failed — a network blip, a provider configured or
> `opencode auth login` completed a minute later — must be able to report its real
> list without a restart.

While `ModelCatalog` was a field on each `WelcomeForm` that rule was somebody
else's problem: the front memo died with the pane, so "open a new pane" reached
the backend again and the recovery worked. Promoting the instance to app scope
removes that expiry — and a front memo with no expiry, holding an answer the
backend refused to hold, does not *duplicate* the cache. It makes it
**unreachable**. Install gemini mid-session and every surface goes on reporting it
missing until loomux restarts.

So the front memo keeps only what the backend would have kept. `worthKeeping` is
that test, and it reads completeness off the reply because completeness is
deliberately not a wire field ("a caching fact, not a wire field"): an answer that
carries **no list** is exactly the answer a later probe might improve on. For an
enumerator CLI that is the backend's own predicate verbatim (`complete =
!listed.is_empty()`); for a help-parsed one it is stricter, and stricter in the
safe direction — the cost of being wrong is one IPC to a backend holding the
answer in a HashMap, against a session-long "this CLI has nothing".

Freshness is then bounded from the caller's side, because "never cached" must not
become "asked on every paint":

| caller | asks | so recovery is |
| --- | --- | --- |
| launcher | per CLI change (`applyRoleModels`) | change the CLI select |
| workflow pane | once per CLI per **pane** (`modelProbes`) | open a workflow pane |

The block form re-renders on every knob edit, so probing from its render path
would be a subprocess per paint for exactly the CLIs that have nothing to say.
Per pane is also precisely the granularity the per-form memo used to give, which
is what makes this a restoration rather than a new policy.

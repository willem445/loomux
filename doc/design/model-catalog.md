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

## Seams

- The probe arrives as an **injected function** (`pty.ts`'s `probeAgentCli`), so
  every rule above is testable with no browser and no Tauri host.
  `ModelCatalog.probe` memoizes on the in-flight promise (two forms opening at
  once make one call) and never rejects — a rejected promise reaching a form's
  render path turns "we couldn't ask" into a broken field.
- `ModelCatalog.models(cli)` is **synchronous**: a form paints on its first frame
  and cannot await a probe whose worst case is the backend's 8s timeout. It
  re-paints when the probe resolves.
- `ModelPicker` takes its CSS classes as options. A shared control that hardcoded
  one host's classes would be unstyled in the other.
- Slice A's opencode enumerator (`opencode models`) plugs in with no change here:
  it only makes `CliProbe.models` non-empty for a CLI that reported nothing
  before, which is the case every rule above was written for.

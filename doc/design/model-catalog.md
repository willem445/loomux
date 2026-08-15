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

---

# Asking the CLI itself: per-agent model detection (#993)

Everything above answers "which model strings may this CLI be given?" from what
a CLI prints *unprompted* — its `--help` prose, or a list subcommand where one
exists. Three things a human needs are not in that answer, and cannot be:

- **which models this host actually offers** — an account's entitlements, a
  provider the human configured, a plan tier;
- **each model's reasoning-effort levels** — per model and per account, so a
  table in this repo would be a third copy of the thing #329 says not to keep
  one of;
- **the numeric context window** — which the CLIs emit only as banner prose.

#993 closes the first two by asking the CLI in its own control protocol, and the
third with a small table that cites a page.

## The live probe is a third kind of claim

| source | what it is | how it ages |
| --- | --- | --- |
| curated (`orchclis.ts`) | a suggestion written down in this repo | badly (#329) |
| probed (`probe_agent_cli`) | what the CLI advertises it *accepts* | current by construction |
| **detected (`list_cli_models`)** | **what the CLI reports about this host** | **current, and specific to the account** |

Claude Code has no list subcommand (`claude models` opens a chat), but it speaks
a control protocol on its `stream-json` input stream. One `list_models` control
request returns the model picker the human's own install would show — real ids,
plus each model's `supportsEffort` / `supportedEffortLevels`.

Detection **adds rows and re-orders them; it never removes one.** It feeds the
same `mergeModelOptions` the probe reply does, so the machine's answer leads and
the curated entries stay behind it — which keeps the property `orchclis.test.ts`
names, that a role's default is still on the menu afterwards.

## Credit safety: why detection was a human gesture, and why it is now automatic

This is the load-bearing decision in the feature, and it is a constraint-3 call.
It was decided one way in #993 and the other way in #1002; both readings are
recorded here, because the second is only defensible given what the first was
waiting for.

Anthropic **types the control envelope** (`SDKControlRequest`,
`SDKControlListModelsRequest = { subtype: 'list_models' }`, `ModelInfo`) in
`@anthropic-ai/claude-agent-sdk`'s `sdk.d.ts`, and publishes `ModelInfo` at
<https://docs.claude.com/en/api/agent-sdk/typescript>. Every CLI flag loomux
passes is documented at <https://code.claude.com/docs/en/cli-reference>.

But the string `list_models` appears **nowhere** in Anthropic's documentation
corpus. The claim that it is a metadata message which does **not consume
completion credits** is third-party (`stablyai/orca`, MIT) and therefore
UNVERIFIED by the vendor — docs-are-silent, not docs-say-X. **That is still true
and nothing below changes it.**

#993 took the conservative reading: if the claim is wrong, an automatic probe
spends the human's money every time a model picker paints, so `list_cli_models`
was reachable only from a `detect` button, and the automatic model list stayed
exactly what `cliprobe.rs` made it. The flip was left as a follow-up gated on the
**human** live-validating the cost — that validation being theirs, never an
agent's.

**#1002 is that validation, and the direction that followed it.** The human
validated the cost themselves and directed that detection become automatic and
interaction-free, with **no re-ask affordance** — a restart re-detects. This is
recorded as **human-directed** rather than as an engineering conclusion, and the
distinction matters: no agent may re-derive it, widen it to another CLI's control
protocol, or restore a re-ask button on its own reasoning. A new protocol row is
a new cost question, and it goes back to the human.

### Why the command cannot spawn

Making detection automatic is not the same as making the *command* automatic,
and #1020 deliberately did the smaller thing. Two shapes were available:

**A — keep `list_cli_models` an ASK, ration it with a memo.** The startup sweep
primes the memo; a picker that opens before the sweep has answered starts its
own ask, which the memo and a single-flight gate collapse. Its attraction is
mid-run recovery: a CLI installed after boot is picked up on the next paint.

**C — make `list_cli_models` a LOOKUP that cannot spawn at all.** The startup
sweep is the only spawn site; a picker that opens early is told there is nothing
yet, and the answer reaches it on the `models-detected` event moments later.

**C shipped.** A puts a subprocess spawn back on a render path — precisely the
boundary #993 drew — and then needs two separate guards to keep it safe (the
gate, and a frontend once-per-program bound), each of which is a thing a later
edit can quietly remove. C deletes the path instead, so the property "no render
path can spend the human's money" is enforced by there being no code that could,
not by a guard that has to keep being right. When the underlying claim is
UNVERIFIED, the shape whose safety does not depend on a guard is the one to
take.

C's cost is A's attraction: **a CLI installed after loomux started is not
detected until the next restart.** That is acceptable only because the human
already accepted it in the same direction that asked for no re-ask affordance.
It is not a general licence to trade recovery for simplicity elsewhere.

### What still bounds it

- One sweep per app run, started from Tauri `setup`, sequential
  (`modelwire::start_startup_sweep`).
- Only CLIs with a `PROTOCOLS` row — one today (`claude`). A CLI without one is
  never spawned for a list it has no way to give.
- Failures and empty replies are never memoized, so the memo cannot serve a bad
  moment as this CLI's answer for the rest of the session.
- `ModelCatalog.detect` issues at most one lookup per CLI per app run, so a form
  that repaints costs no IPC — the bound that replaced the click.

## The backend transports; the frontend parses

`src-tauri/src/modelwire.rs` writes one request line to the CLI's stdin, reads
stdout, and hands the bytes over IPC. It understands nothing about the reply.
`src/modelwire.ts` parses it.

That split is deliberate on two counts. The parsing is the fragile part — it is
written against a payload key (`response.response.models`) that the vendor does
not document — so it belongs where a `node --test` round costs a second rather
than a CI build. And the backend stays a few dozen lines: per-CLI differences
live in a `PROTOCOLS` table as DATA, the `ENUMERATORS`/`CLI_CAPS` pattern this
repo already uses, so adding Copilot or opencode is a row, not a branch.

The subprocess plumbing is **shared, not copied**: `cliprobe.rs`'s `run_cli`
gained an optional stdin payload rather than being duplicated, because the
fresh-PATH resolution, the hidden-window flag, the two drain threads and the
deadline poll are the parts that are easy to get subtly wrong on Windows.

The correlation id travels **on the reply** rather than being spelled in both
languages. A `control_response` to some other control request must not be read
as the answer to this one, and an id that drifted between sender and reader
would silently stop correlating with nothing looking wrong.

## Never manufacture

`modelwire.ts` inherits the rule `cliprobe.rs`'s parsers already carry: **it may
under-recognise, but it must never manufacture.** Every id it returns is a
verbatim `value` string from the CLI's own JSON. It does not repair a truncated
line, does not infer an id from a display name, and — the tempting one — does
not read a context window out of the description prose, which really does say
"Opus 5 with 1M context".

A payload it does not recognise yields no models, which looks exactly like a CLI
too old for the request: every surface keeps its seed list.

## Three states, not two

`ModelInfo` marks every capability field optional, and a real `haiku` row comes
back carrying none of them. So `supportsEffort` is `boolean | null`:

| value | meaning | what the effort knob does |
| --- | --- | --- |
| `null` | the CLI did not raise the subject | **unchanged** — `CLI_CAPS`'s levels |
| `false` | the CLI says this model has no effort setting | disabled, quoting the model |
| `true` + levels | the CLI listed them | offers exactly those, in its order |

Collapsing `null` into `false` would remove a capability on no evidence. This is
the docs-say-X / docs-are-silent / docs-say-NOT-X rule (`agent-cli-reference`)
applied to a reply instead of a page.

**Narrowing only ever narrows.** `caps` stays the outer bound: a CLI with no
seam for the knob — copilot's effort lives in `~/.copilot/settings.json`, not on
a flag — has none regardless of what a model reports about itself, or loomux
would put a flag on the wire that the CLI cannot take.

## A detection reply owes every surface `agent_cli_knobs` owes

Detection is not just another source for the dropdown — it is the reply that
makes `knobLookup` answer differently. So the surfaces it invalidates are the
same ones the sibling `agent_cli_knobs` reply invalidates, and both hosts owe
them all: the model menu, the knob rows, and (in the workflow pane) the analysis
pass and its findings.

Repainting only the dropdown is a real defect rather than a cosmetic one, and it
shipped in the first cut on one of the two surfaces (#997 review). The block
editor gained its rows and its summary line — so the human had every reason to
believe detection had landed — while *Thinking level* went on offering
`low/medium/high/xhigh/max` for a model whose reply said it had no effort
setting. Picking `xhigh` wrote it, the next render brought the row back disabled,
and the findings pane flagged the block: **the editor offered a value its own
validator rejects.** The launcher never had the bug (`applyRoleModels` →
`applyRoleKnobs`), and that asymmetry is what identified it as an oversight.

Because this is DOM wiring — which this repo validates by hand rather than by
simulating a DOM — the pin is a **source scan** (`test/detectrefresh.test.ts`),
in the tradition of `transport.test.ts`'s one-importer rule and `groupid.rs`'s
two scans. It asserts that **every** call site in each host reaches the
refreshers by name, and carries a vacuity guard so a broken extraction fails
loudly instead of passing. The next such handler will be written by copy-paste
from a neighbour rather than by reading this note; a scan is what notices.

Since #1020 the reply arrives on two routes rather than from one button — the
lookup a picker fires when it paints, and the sweep's push
(`ModelCatalog.onReport`) — so each host funnels **both** into one method
(`applyDetection`, `refreshRoleFromDetection`) and the scan pins that funnel plus
the two routes into it. Two properties are new with the automatic architecture,
and neither existed to be got wrong before:

- **The lookup fires from a render path and its refresh ends in a re-render**, so
  an unguarded handler is an infinite loop rather than a stale row. Two
  independent exits are required and both are pinned: an early-out on an answer
  that carried nothing (which never sets `report()`, so the other guard would
  never fire), and a guard on `report()` being absent (which a good answer would
  otherwise pass on every repaint).
- **A push subscription must be released, and asking whether it is dead must be
  free.** The catalog is app-scoped and the hosts are not, so a subscription
  nobody releases retains its whole host — a `WorkflowView`, its analysis, its
  detached DOM — for the life of the process. `onReport` therefore takes
  liveness as a **separate, side-effect-free predicate** rather than as the
  delivery callback's return value, and prunes on **registration**.

  Both halves are the fix for a leak that shipped in the first cut of this slice
  and was caught in review, so the reasoning is recorded rather than the rule
  alone. Liveness-as-return-value meant asking required *delivering*, so pruning
  could only run when a report changed state — and the producer changes state at
  most once per program per app run, and **zero** times in the ordering where the
  pull wins the race (`acceptReport` refuses before reaching the listeners). The
  prune therefore ran approximately never, and every host built after the sweep
  was retained forever: precisely the leak the mechanism was introduced to
  prevent. Registration is the right event to hang it on because it is the only
  one that keeps recurring — every new host subscribes, so the list cannot grow
  past the live hosts plus one.

  A host that *has* a teardown still releases there (`WorkflowView.dispose`)
  rather than waiting to be pruned. `WelcomeForm` deliberately does not: `fire()`
  is the only candidate and `reopenAfterLaunchFailure` revives the form after it,
  so a form released there would come back deaf to the sweep. The prune is what
  covers it, which is the case the prune exists for.

  `ModelCatalog.liveReportListeners` exists solely so a test can see retention,
  and that is a deliberate exception to this repo's rule against readers with no
  product caller: the leak above kept the whole suite green because nothing could
  observe the list. A leak no test can see is a leak that comes back.

The file's header enumerates what the instrument cannot do — reachability, not
behaviour; source, not module graph — because a structural test that reads as
broader coverage than it has is worse than none.

**Nothing in that file is a behaviour test, `runWhenNotEditing` included.** An
earlier round claimed otherwise on three surfaces, and the claim was false: the
`runWhenNotEditing` pins are source scans of a second file, one level down. A
behaviour pin genuinely is not available — the method reads
`document.activeElement` and attaches a listener, and this repo forbids
simulating a DOM — so whether a deferral actually defers is hand-validated like
the rest of the DOM wiring (#997 review B-1).

Three limits found the same way, by somebody mutating the subject and watching
the suite stay green — which is the only way a structural test's claims ever get
checked:

- **Scanning only the first `onDetect`.** One handler per host today, so it was
  sound as written; fixed before a second picker made it silently wrong (NB-4).
- **Matching inside comments.** These handlers are two-thirds comment by volume
  and the prose names every refresher, so deleting the real calls and leaving a
  comment kept the suite green — the round-1 blocking regression passing its own
  pin. Full-line comments are stripped before matching now (B-2), the discipline
  `test/workspacelayout.test.ts` already uses — a *trailing* comment after code
  is not stripped and could still satisfy an assertion, an accepted residual
  (stripping those safely needs a tokenizer, not a regex) with no live instance
  in either handler today (#997 review R4-2).
- **A positive match that stopped discriminating.** Once the handler reached the
  live knob hook in *two* places, asserting its presence survived a mutation of
  either. The pin now also asserts the form-local closure is absent — the
  negative half is the half that discriminates. Re-deriving the red table after
  an unrelated fix is what caught it.

Two further rules follow from the reply being **slow**. An ask spawns a CLI, so
it is in flight for seconds, and a human does things in seconds.

**Repaint through the live hook, never a captured one.** `renderInspector()` clears
`repaintBlockKnobs` before rebuilding, precisely so a late `agent_cli_knobs`
reply cannot paint into a row it has just detached — and `ensureCliKnobs` calls
`this.repaintBlockKnobs?.()` for that reason. A detect handler holding its own
form's repainter in a closure walks straight around that guard: select another
block while the ask is in flight and the reply paints the *previous* form's
detached rows, leaving the one on screen stale (#997 review NB-1). The handler
also takes the probe reply's `formPane.contains(picker.root)` early-out before
touching any of this form's DOM. The findings are recomputed either way — they
belong to the pane, not to the form.

**Defer the mid-type rebuild; never drop it.** Rebuilding the menu under a
half-typed id resolves it to the dropdown branch and hides the input beneath the
caret, so `ModelPicker.editingCustom` is the question both hosts ask. But
*refusing* is only half an answer: nothing schedules another attempt, and on the
launcher that was permanent — `applyRoleModels` is otherwise reachable only from
the role's CLI `change` listener and the seed pass, so a detection landing
mid-type never reached that role's dropdown again for the dialog's life, and the
human saw detection do nothing (#997 review NB-3). `runWhenNotEditing`
owns both halves now: run it, or run it on the input's next `blur`. The
predicate stays deliberately narrower than "focus is somewhere in this picker":
the hazard is the text input specifically, and a human whose focus is merely on
the `<select>` loses nothing to a rebuild that re-selects the same value.

#1020 **widens** this window rather than closing it. Under #993 the reply landed
a second or two after a click the human had just made; now it arrives on the
startup sweep's schedule, so they are more likely to be mid-type when it does,
not less — which is why the deferral survived the button that motivated it.

## The context-window table, and why one exists at all

`src/modelcontext.ts` is the one model fact loomux states on its own authority.
It is there because no artifact carries the number: the CLIs emit it as prose,
and the one machine-readable source — Anthropic's Models API `max_input_tokens`
— needs an API key loomux does not have, and wiring one vendor's API into a
generic tool is the host special-casing constraint 8 forbids.

Three rules keep it from aging the way #329 warns about:

1. **Keyed by CLI, then by id.** `sonnet` on a copilot row gets no window:
   GitHub's reference does not say which Sonnet it serves or with what window,
   and borrowing Anthropic's number would be loomux inventing a vendor fact
   (#687's rule, applied to a number instead of a description).
2. **Family aliases first.** `sonnet` means "the latest Sonnet model", so its
   row stays right across a release that a pinned version row would not.
   Versioned ids get rows only where the vendor documents that exact model
   today.
3. **Silence is an answer.** No row means no number — never a family's figure
   applied to a version that may not share it. `claude-sonnet-4-5` is the case
   that rule is for.

The lookup uses `ModelInfo.resolvedModel` when the reply carried one: it is the
canonical wire id an alias resolves to on *this* install, which turns a moving
alias into the exact model the account is being served — the one id a static
table can be sure about. It falls back to the picked id only when that field is
**absent**, which Anthropic documents as the case on any install older than
Claude Code v2.1.197.

**Absent and unknown are different states, and conflating them re-opened the
hole rule 3 exists to close (#997 review).** The first cut branched on the
resulting *label* being empty rather than on the *field* being absent, so a
reported `resolvedModel` with no row — `claude-sonnet-4-5`, or the
`us.anthropic.…` / ARN / gateway forms an enterprise install really produces —
fell through to the alias and printed the alias's number for it. Rule 3 held one
layer down and the composed path inherited the figure anyway, and only once
detection was on: the feature's own path re-opening the hole the table was built
to close. A resolved id loomux cannot place is the *more specific* statement, so
its silence is the answer; a missing field is not a statement at all, so the
picked id is what gets asked. `test/modelnames.test.ts` pins both halves.

## Seams added

| seam | shape | who calls it |
| --- | --- | --- |
| `list_cli_models` (`modelwire.rs`) | `{ output, request_id, error }` — a LOOKUP since #1020, never a spawn | `pty.ts`'s `listCliModels` |
| `parseListModelsReply` (`modelwire.ts`) | stdout + id to `ModelReport` | `readCliModelReply` |
| `ModelCatalog.detect` | one lookup per CLI per app run, never rejects | both hosts' picker paint path |
| `ModelCatalog.acceptReport` / `onReport` | the sweep's push, and liveness-pruned listeners | `modelprobe.ts`, both hosts |
| `models-detected` (event) | `{ program, reply }` | `pty.ts`'s `onModelsDetected` |
| `ModelCatalog.detail` | `(cli, id)` to a `ModelDetail` or `null` | the picker's labels, `knobState` |
| `knobState(caps, cli, model, detail?)` | optional 4th arg, defaults to today's behaviour | launcher + workflow pane |

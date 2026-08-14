// The workflow block editor's knob rows (#935 slice C).
//
// What these defend is one failure the human reported and one that shares its
// cause:
//
//   1. TYPING A MODEL MUST MOVE THE KNOB. `context` is available only where the
//      SELECTED model has a documented `[1m]` form (#687/#709 — there is no
//      `haiku[1m]`). The block form derives its controls once, at render, and the
//      model control edits with the form re-render suppressed (a re-render would
//      rebuild the input under the caret), so the context select stayed disabled
//      — quoting a reason about a model that was no longer selected — until the
//      human clicked away from the block and back. The knob rows have to be a
//      live function of the model, not of the model the form OPENED on.
//   2. A CAPABILITY REPLY THAT LANDS LATE MUST DO THE SAME. `agent_cli_knobs` is
//      an IPC call that resolves after the form is on screen; before it does,
//      both rows say "reading this CLI's capabilities…", and nothing about the
//      model has moved when the answer arrives.
//
// And two rules that make this an EDITOR of somebody's file rather than a form
// composing a command line: a declared value the CLI can't deliver still shows,
// marked — and stays removable.

import { test } from "node:test";
import assert from "node:assert/strict";
import { knobState, type CliKnobs } from "../src/selectorknobs.ts";
import type { KnobLookup } from "../src/workflowmodel.ts";
import { BlockKnobFields, CONTEXT_COPY, EFFORT_COPY, knobFieldSpec } from "../src/workflowknobs.ts";

/** The `agent_cli_knobs` reply for claude, verbatim from `CLI_CAPS` (mod.rs) —
 *  the same fixture `selectorknobs.test.ts` pins against. */
const CLAUDE: CliKnobs = {
  cli: "claude",
  known: true,
  effort: {
    values: ["low", "medium", "high", "xhigh", "max"],
    note: "--effort <level> is a session-scoped flag; a model that lacks a level falls back to the highest one it supports at or below it",
  },
  context: {
    values: ["1m"],
    note: "the [1m] model-alias suffix (sonnet[1m]) — access is plan- and credit-gated, so a tier the account cannot serve fails at the CLI, visibly in the pane",
  },
};

/** What the backend returns for a CLI it has never evaluated — `gemini`'s
 *  neighbours in `WORKFLOW_CLIS` are not all in `CLI_CAPS`. */
const UNKNOWN: CliKnobs = {
  cli: "gemini",
  known: false,
  effort: { values: [], note: "" },
  context: { values: [], note: "" },
};

/** The lookup the pane injects, with one CLI's record already in hand. */
const lookupFor =
  (caps: CliKnobs | null): KnobLookup =>
  (cli, model) =>
    knobState(caps, cli, model);

/** The lookup before any reply has landed: `null` is "not asked yet", which is a
 *  different state from "asked, and the answer is no". */
const notAsked: KnobLookup = () => null;

const values = (spec: { options: { value: string }[] }): string[] => spec.options.map((o) => o.value);

// ── the reported bug: the knob follows the model ─────────────────────────────

test("typing a model with a documented [1m] form activates the context knob (#935)", () => {
  // The form opened on `haiku`, which has no `haiku[1m]` alias at all, so the
  // control is correctly off. The human then types `sonnet` — which does — and
  // the knob has to come alive without the form re-rendering.
  const knobs = new BlockKnobFields(lookupFor(CLAUDE), "claude", "haiku", {});
  assert.equal(knobs.context.disabled, true, "haiku has no [1m] form, so the knob starts off");
  assert.match(knobs.context.hint, /not a Claude model alias/);

  knobs.setModel("sonnet");
  assert.equal(knobs.context.disabled, false, "a typed sonnet must ACTIVATE the context field");
  assert.deepEqual(values(knobs.context), ["", "1m"], "and offer the tier it now supports");
  assert.match(knobs.context.hint, /the model's context window/);
});

test("typing a model with no [1m] form puts the knob back, carrying the vendor's reason", () => {
  // The same move in reverse: a control that stays enabled after the model moves
  // under it is how `--model haiku[1m]` — a string the vendor documents no
  // meaning for — gets composed.
  const knobs = new BlockKnobFields(lookupFor(CLAUDE), "claude", "opus", {});
  assert.equal(knobs.context.disabled, false);

  knobs.setModel("fable");
  assert.equal(knobs.context.disabled, true);
  assert.match(knobs.context.hint, /Fable 5 already runs with the 1M window/);
});

test("a custom id loomux doesn't recognize leaves the knob enabled, not refused", () => {
  // The `custom…` branch of the picker is where a Bedrock inference profile or a
  // gateway deployment name arrives, and on those providers the suffix is exactly
  // how the 1M window is selected. Disabling what we merely don't know would
  // remove a real capability.
  const knobs = new BlockKnobFields(lookupFor(CLAUDE), "claude", "haiku", {});
  assert.equal(knobs.context.disabled, true);
  knobs.setModel("arn:aws:bedrock:us-east-1::inference-profile/my-profile");
  assert.equal(knobs.context.disabled, false);
});

test("the thinking level does NOT move with the model — only the context window does", () => {
  // Effort is a CLI-level seam: every claude model takes a level (one it lacks
  // falls back to the highest it supports at or below it). A model gate applied
  // to both would disable a knob the CLI can deliver.
  const knobs = new BlockKnobFields(lookupFor(CLAUDE), "claude", "sonnet", {});
  const before = knobs.effort;
  knobs.setModel("haiku");
  assert.deepEqual(knobs.effort, before, "the effort row is unchanged by a model that lacks [1m]");
  assert.equal(knobs.effort.disabled, false);
});

// ── the capability reply that lands late ─────────────────────────────────────

test("both rows are inert while the capability lookup is still in flight", () => {
  // Offering values before knowing whether they exist is how a form promises
  // something the spawn won't do.
  const knobs = new BlockKnobFields(notAsked, "claude", "sonnet", {});
  for (const spec of [knobs.effort, knobs.context]) {
    assert.equal(spec.disabled, true);
    assert.deepEqual(values(spec), [""], "nothing to offer yet");
    assert.match(spec.hint, /reading this CLI's capabilities/);
  }
});

test("the answer is re-asked on every read, so a reply that lands late is picked up", () => {
  // The reply arrives whenever the IPC happens to resolve — with no model having
  // moved at all. A field that cached its states at construction would sit on
  // "reading this CLI's capabilities…" until the human clicked elsewhere and back.
  let caps: CliKnobs | null = null;
  const knobs = new BlockKnobFields((cli, model) => knobState(caps, cli, model), "claude", "sonnet", {});
  assert.equal(knobs.context.disabled, true);
  caps = CLAUDE;
  assert.equal(knobs.context.disabled, false, "the landed reply must reach the row with no model change");
  assert.deepEqual(values(knobs.effort), ["", "low", "medium", "high", "xhigh", "max"]);
});

test("a CLI loomux has never evaluated claims nothing, and says so", () => {
  const knobs = new BlockKnobFields(lookupFor(UNKNOWN), "gemini", "pro", {});
  assert.equal(knobs.effort.disabled, true);
  assert.equal(knobs.context.disabled, true);
  assert.match(knobs.effort.hint, /never evaluated gemini/);
});

// ── the editor rules ─────────────────────────────────────────────────────────

test("a declared value the CLI can't deliver still shows, marked — and stays removable", () => {
  // This is an editor of the human's file. Dropping what the file says would
  // rewrite it the moment any other field was touched, and greying the control
  // out would leave them looking at a value they cannot delete. The validation
  // pass raises the finding; the control keeps the fix reachable.
  const knobs = new BlockKnobFields(lookupFor(CLAUDE), "claude", "haiku", { context: "1m" });
  assert.deepEqual(values(knobs.context), ["", "1m"]);
  assert.equal(knobs.context.options[1]?.label, "1m (not delivered)");
  assert.equal(knobs.context.selected, "1m", "the file's own value is what the control shows");
  assert.equal(knobs.context.disabled, false, "a declared value must stay removable");
});

test("the blank row leads every knob and is what an undeclared block selects", () => {
  const knobs = new BlockKnobFields(lookupFor(CLAUDE), "claude", "sonnet", {});
  assert.equal(knobs.context.options[0]?.value, "");
  assert.equal(knobs.context.options[0]?.label, "(the CLI's default)");
  assert.equal(knobs.context.selected, "");
});

test("a declared value the state DOES offer is not duplicated into a second row", () => {
  const knobs = new BlockKnobFields(lookupFor(CLAUDE), "claude", "sonnet", { effort: "high" });
  assert.deepEqual(values(knobs.effort), ["", "low", "medium", "high", "xhigh", "max"]);
  assert.equal(knobs.effort.selected, "high");
});

test("a declared value is read trimmed — the file's own whitespace is not a second value", () => {
  const spec = knobFieldSpec({ enabled: true, values: ["1m"], reason: "" }, " 1m ", CONTEXT_COPY);
  assert.equal(spec.selected, "1m");
  assert.deepEqual(values(spec), ["", "1m"], "no ' 1m ' row beside the real one");
});

test("each knob's hint names the file key it writes", () => {
  // The hint is what a human maps from the control back to the line they'd hand-
  // edit, so the two controls can never carry the same one.
  const state = { enabled: true, values: ["x"], reason: "" };
  assert.match(knobFieldSpec(state, "", EFFORT_COPY).hint, /^effort: —/);
  assert.match(knobFieldSpec(state, "", CONTEXT_COPY).hint, /^context: —/);
});

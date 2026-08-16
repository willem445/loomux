// The workflow block editor's two model knobs — `effort:` and `context:` — as a
// live function of the model the picker is holding RIGHT NOW (#935 slice C).
//
// DOM-free and I/O-free (`test/workflowknobs.test.ts`); the capability answer
// arrives as an INJECTED `KnobLookup`, the same seam `analyzeWorkflow` takes, so
// the rules below are testable with no browser, no Tauri host and no backend.
//
// **Why this is a live object and not one more pure function.** A knob's
// availability is not a property of the file alone: `context` exists only where
// the SELECTED model has a documented `[1m]` form (#687/#709 — there is no
// `haiku[1m]`), so the answer changes with every keystroke in the model field.
// The block form derives its controls once, at render, and the model control
// edits with the form re-render suppressed — it has to, since re-rendering would
// rebuild the input under the human's caret. The two together are the bug #935
// carries: type `sonnet` over `haiku` and the context select stays disabled,
// quoting a reason about a model that is no longer selected, until you click away
// from the block and back.
//
// Holding the model here, and re-deriving on `setModel`, is what lets the form
// repaint those two rows without re-rendering itself. The view owns the paint;
// this owns the answer.

import type { KnobLookup } from "./workflowmodel.ts";
import type { KnobState, KnobStates } from "./selectorknobs.ts";

/** One row of a knob `<select>`. The value is what the file would carry; the
 *  label is what the human reads. */
export interface KnobOption {
  value: string;
  label: string;
}

/** Everything one knob control must show. Recomputed whole rather than patched,
 *  so a repaint can never leave the options from one model beside the hint from
 *  another. */
export interface KnobFieldSpec {
  options: KnobOption[];
  /** The option to select — always one of {@link options}. */
  selected: string;
  disabled: boolean;
  /** The line under the control: the vendor's own reason when the knob is off. */
  hint: string;
}

/** The two strings a knob's hint is written from. Passed in rather than tabled
 *  here: they are the editor's copy, and this module states no vendor fact. */
export interface KnobCopy {
  /** The file key this control writes, e.g. `"context:"`. */
  key: string;
  /** What the knob does, in the editor's own words. */
  what: string;
}

export const EFFORT_COPY: KnobCopy = { key: "effort:", what: "how hard this block's agent thinks" };
export const CONTEXT_COPY: KnobCopy = { key: "context:", what: "the model's context window" };

/** What one knob control shows for a declared value against the capability state
 *  its CLI and model resolve to.
 *
 *  `state` is `null` while the `agent_cli_knobs` lookup is in flight — the
 *  control is inert then, because offering values before knowing whether they
 *  exist is how a form promises something the spawn won't do.
 *
 *  Two editor rules the launcher's equivalent does not have, and both follow from
 *  this being an EDITOR of somebody's file rather than a form composing a
 *  command:
 *
 *   - a DECLARED value the state doesn't offer still shows, marked. Dropping it
 *     would silently rewrite the file the moment any other field is touched.
 *   - and it keeps the control ENABLED, because a value the human can see and
 *     cannot remove is worse than one that is merely wrong. (The launcher resets
 *     such a value instead — `knobValue` — since there the control's job is to
 *     decide a payload, and a stale pick must never reach it.) */
export function knobFieldSpec(state: KnobState | null, declared: string, copy: KnobCopy): KnobFieldSpec {
  const current = declared.trim();
  const values = state?.values ?? [];
  const options: KnobOption[] = [{ value: "", label: "(the CLI's default)" }];
  for (const v of values) options.push({ value: v, label: v });
  if (current && !values.includes(current)) options.push({ value: current, label: `${current} (not delivered)` });
  return {
    options,
    selected: current,
    disabled: !state?.enabled && !current,
    hint: !state
      ? `${copy.key} — reading this CLI's capabilities…`
      : state.enabled
        ? `${copy.key} — ${copy.what}. Blank leaves the CLI's default.`
        : state.reason,
  };
}

/** The knob rows of ONE block's form, tracking the model as the human edits it.
 *
 *  Built per form render (the block's declared values are the ones on screen);
 *  `setModel` is called by the model picker's own change hook, on a dropdown pick
 *  and on every keystroke in its `custom…` input alike — the typed case being the
 *  one a plain `change` listener on a `<select>` never sees, which is how the bug
 *  survived. */
export class BlockKnobFields {
  private readonly lookup: KnobLookup;
  private readonly cli: string;
  private readonly declared: { effort: string; context: string };
  private model: string;

  constructor(
    lookup: KnobLookup,
    cli: string,
    model: string,
    declared: { effort?: string; context?: string }
  ) {
    this.lookup = lookup;
    this.cli = cli.trim();
    this.model = model;
    this.declared = { effort: declared.effort ?? "", context: declared.context ?? "" };
  }

  /** The model the knobs are answering for. */
  setModel(model: string): void {
    this.model = model;
  }

  get effort(): KnobFieldSpec {
    return knobFieldSpec(this.states()?.effort ?? null, this.declared.effort, EFFORT_COPY);
  }

  get context(): KnobFieldSpec {
    return knobFieldSpec(this.states()?.context ?? null, this.declared.context, CONTEXT_COPY);
  }

  /** Asked on every read, never cached: the lookup's answer changes under us too
   *  — `agent_cli_knobs` resolves after the form is already on screen, and that
   *  reply turns both rows from "reading this CLI's capabilities…" into real
   *  options with no model having moved at all. */
  private states(): KnobStates | null {
    return this.lookup(this.cli, this.model);
  }
}

// Per-knob availability for the launcher's model selector (#687).
//
// RED-BEFORE-GREEN PLACEHOLDER — this commit deliberately ships the BACKEND's
// current gating behind the new signature: a knob is offered whenever the CLI's
// capability row lists values for it, with no model gate at all. That is exactly
// the state #709's review left behind (`haiku` + `1m` composes `haiku[1m]`), so
// the red run reads as a statement about the gap, not about a missing file. The
// next commit implements the real states.

/** The `agent_cli_knobs` reply — the CLI's capability row, verbatim (mod.rs's
 *  `cli_knobs_json`). `note` is always populated for a CLI loomux has evaluated;
 *  an empty value set is a claim, and the note is why. */
export interface CliKnobValues {
  values: string[];
  note: string;
}

export interface CliKnobs {
  cli: string;
  known: boolean;
  effort: CliKnobValues;
  context: CliKnobValues;
}

export interface KnobState {
  enabled: boolean;
  values: string[];
  reason: string;
}

export interface KnobStates {
  effort: KnobState;
  context: KnobState;
}

export function knobState(caps: CliKnobs | null, _cli: string, _model: string): KnobStates {
  const of = (k: CliKnobValues | undefined): KnobState => ({
    enabled: !!k && k.values.length > 0,
    values: k ? k.values : [],
    reason: k && k.values.length === 0 ? k.note : "",
  });
  return { effort: of(caps?.effort), context: of(caps?.context) };
}

export function knobValue(_state: KnobState, picked: string): string {
  return picked;
}

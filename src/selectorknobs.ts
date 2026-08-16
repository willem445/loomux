// Per-knob availability for the launcher's model selector (#687).
//
// DOM-free and I/O-free (`test/selectorknobs.test.ts`), and it mirrors no vendor
// capability of its own: the value sets and the notes come from the backend's
// `agent_cli_knobs`, which reports `CLI_CAPS` verbatim, so the launcher, the
// workflow parser and the spawn path can never disagree about what a CLI can do.
//
// What this module adds on top of that is the half the backend cannot answer:
// **whether the knob makes sense for the MODEL that is selected**. `effort` does
// on every claude model (a model that lacks a level falls back to the highest it
// supports at or below it — model-config §Adjust effort level), but `context`
// does not: `[1m]` is a model-alias suffix, and the suffix is only defined where
// the underlying model has a 1M window.
//
// That is #709's carried review finding. `clamped_knob` checks `context_variants`
// for claude and stops, so `model: haiku` + `context: 1m` composes
// `--model haiku[1m]` — not a plan-gated alias the account can't serve (which
// loomux deliberately does not pre-judge), but a string the vendor documents no
// meaning for at all.
//
// Docs behind the model gate (fetched 2026-08-02, `agent-cli-reference`
// discipline — Claude Code model-config):
//
//   §Model aliases    lists `sonnet[1m]` and `opus[1m]`; §opusplan adds
//                     `opusplan[1m]`. There is no `haiku[1m]`, `fable[1m]`,
//                     `best[1m]` or `default[1m]` row.
//   §Extended context "Fable 5, Sonnet 5, Opus 4.6 and later, and Sonnet 4.6
//                     support a 1 million token context window" — no Haiku, in
//                     any version — and "You can also use the `[1m]` suffix with
//                     model aliases or full model names", with
//                     `/model claude-opus-4-8[1m]` as the example.
//                     The rule stated outright: "Only append `[1m]` when the
//                     underlying model supports 1M context."
//
// The gate is therefore by model FAMILY, not by version: families are stable,
// version tables age (#329). And it fails OPEN on an id it does not recognize —
// a Bedrock inference profile, a gateway deployment name, a custom option. On
// those providers the suffix is exactly how the 1M window is selected, so
// refusing what we merely don't recognize would remove a real capability; an id
// that turns out not to support it fails at the CLI, visibly in the pane, which
// is the same posture slice A took on plan-gated entitlements.

import type { ModelDetail } from "./modelcatalog.ts";

/** One knob's row in a CLI's capability record (`cli_knobs_json`, mod.rs).
 *  `note` is always populated for a CLI loomux has evaluated: an empty `values`
 *  is a CLAIM, and the note is the vendor fact behind it. */
export interface CliKnobValues {
  values: string[];
  note: string;
}

/** The `agent_cli_knobs` reply. `known: false` is a CLI loomux has never
 *  evaluated — both sets empty and no note to quote. */
export interface CliKnobs {
  cli: string;
  known: boolean;
  effort: CliKnobValues;
  context: CliKnobValues;
}

/** What a UI should render for one knob. A knob that is off is never simply
 *  absent: `reason` is non-empty whenever `enabled` is false, so the control can
 *  say WHY — a hidden knob reads as "loomux forgot", a disabled one that quotes
 *  the vendor states the fact. */
export interface KnobState {
  enabled: boolean;
  /** The values to offer, in the backend's order. Empty when disabled. */
  values: string[];
  /** Why it is off. `""` when it is on. */
  reason: string;
}

export interface KnobStates {
  effort: KnobState;
  context: KnobState;
}

const off = (reason: string): KnobState => ({ enabled: false, values: [], reason });

/** The claude model family an id names, or `null` for one we don't recognize.
 *  Handles both forms the docs allow: a bare alias, and a full model name
 *  (`claude-<family>-<version>`), since `[1m]` may be appended to either. */
function claudeFamily(model: string): string | null {
  const m = model.trim().toLowerCase();
  if (!m) return null;
  const aliases = ["opusplan", "sonnet", "opus", "haiku", "fable", "best", "default"];
  if (aliases.includes(m)) return m;
  if (m.startsWith("claude-")) {
    // `claude-opus-4-8`, `claude-sonnet-4.6`, `claude-haiku-4-5`: the family is
    // the segment after the vendor prefix.
    const family = m.split("-")[1] ?? "";
    if (aliases.includes(family)) return family;
  }
  return null;
}

/** Whether `[1m]` has a documented meaning on this model, and if not, the reason
 *  a human should be shown. Exported because two surfaces need exactly this
 *  question — the launcher's control and the workflow pane's validation pass —
 *  and a second copy of it is a second answer. */
export function contextModelState(model: string): { supported: boolean; reason: string } {
  const family = claudeFamily(model);
  switch (family) {
    case "sonnet":
    case "opus":
    case "opusplan":
      return { supported: true, reason: "" };
    case "haiku":
      return {
        supported: false,
        reason:
          "haiku[1m] is not a Claude model alias — the 1M context window is documented for " +
          "Fable 5, Sonnet 5, Opus 4.6 and later, and Sonnet 4.6, not for Haiku.",
      };
    case "fable":
      return {
        supported: false,
        reason: "Fable 5 already runs with the 1M window, and there is no fable[1m] alias to select it.",
      };
    case "best":
    case "default":
      return {
        supported: false,
        reason:
          `\`${family}\` resolves per account, so there is no alias to append [1m] to — the suffix is ` +
          "documented for sonnet, opus and opusplan (and full claude-* model names).",
      };
    default:
      // Unrecognized, or empty (= the CLI's own default model). Fail open — see
      // the module note.
      return { supported: true, reason: "" };
  }
}

/** Narrow a CLI's effort levels to the ones it reported for THIS model (#993).
 *
 *  `caps.effort.values` is what the CLI can deliver in general — `CLI_CAPS`,
 *  written down in this repo. `detail` is what the CLI's own list-models reply
 *  said about the selected model on the machine in front of the human, and it
 *  outranks the written-down set for the same reason a probe outranks a curated
 *  suggestion: it is specific and current.
 *
 *  Three states, and each does something different:
 *
 *    detail is null              nothing has been detected (nobody asked, the ask
 *                                failed, the reply did not mention this model) →
 *                                leave the knob exactly as it was. This is the
 *                                default and the common case.
 *    supportsEffort === false    the CLI said this model has no effort setting →
 *                                turn the knob OFF, quoting the CLI rather than
 *                                offering levels it just said do not apply.
 *    levels are listed           offer those, in the CLI's own order.
 *
 *  Note what the middle row is NOT: `supportsEffort: null` (the field was
 *  absent, which a real `haiku` row does) is the first row, not the second. An
 *  omitted field is silence, and silence must not disable anything. */
function narrowEffort(caps: CliKnobValues, detail: ModelDetail | null): KnobState {
  // `caps` is the outer bound and detection may only ever shrink it. A CLI with
  // no seam for the knob — copilot's effort lives in `~/.copilot/settings.json`,
  // not on a flag — has none regardless of what any model reports about itself,
  // and `knobValue` would happily pass a value through an `enabled` knob, so a
  // reply that re-enabled it here would put a flag on the wire that this CLI
  // cannot take.
  if (!caps.values.length) return off(caps.note);
  const fallback: KnobState = { enabled: true, values: caps.values, reason: "" };
  if (!detail) return fallback;
  if (detail.supportsEffort === false) {
    return off(`${detail.name || detail.id} reports no reasoning-effort setting, so loomux sets none on it.`);
  }
  if (!detail.effortLevels.length) return fallback;
  return { enabled: true, values: [...detail.effortLevels], reason: "" };
}

/** Per-knob state for one role/block: what the CLI can deliver (`caps`, from the
 *  backend) narrowed by what the selected `model` can carry.
 *
 *  `caps` is `null` before the async fetch lands, and mismatched when a reply for
 *  a CLI the human has since moved off arrives late. Both disable rather than
 *  assume: offering claude's five effort levels on a copilot row because a stale
 *  reply was still in hand is exactly the silent-wrong-answer this module is for.
 *
 *  `detail` (#993) is what the CLI itself reported about `model`, when a human
 *  has asked it. Optional and defaulting to `null`, which is exactly the
 *  behaviour every caller had before it existed — nobody has to opt out. */
export function knobState(
  caps: CliKnobs | null,
  cli: string,
  model: string,
  detail: ModelDetail | null = null
): KnobStates {
  const want = cli.trim();
  if (!caps || caps.cli !== want) {
    const reason = `loomux has not read ${want || "this CLI"}'s capabilities yet.`;
    return { effort: off(reason), context: off(reason) };
  }
  if (!caps.known) {
    const reason = `loomux has never evaluated ${want}, so it sets no thinking level or context window on it.`;
    return { effort: off(reason), context: off(reason) };
  }
  const effort = narrowEffort(caps.effort, detail);
  if (!caps.context.values.length) return { effort, context: off(caps.context.note) };
  const model_ = contextModelState(model);
  return {
    effort,
    context: model_.supported
      ? { enabled: true, values: caps.context.values, reason: "" }
      : off(model_.reason),
  };
}

/** The value a knob may actually put on the wire. `""` — the CLI's own default,
 *  i.e. no flag and no suffix — whenever the knob is disabled or the pick is not
 *  one of its values.
 *
 *  This is the payload rule, not a display rule: a control that has been disabled
 *  under the human (they picked `xhigh`, then switched the role to copilot; they
 *  picked `1m`, then switched the model to haiku) still holds its old value in
 *  the DOM, and that value must never reach `create_orchestration`. */
export function knobValue(state: KnobState, picked: string): string {
  const v = picked.trim();
  return state.enabled && v !== "" && state.values.includes(v) ? v : "";
}

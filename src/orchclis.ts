// The launcher's orchestrator-mode CLI catalog: which agent CLIs a group can be
// launched on, the curated model ids each one suggests, and the per-role default.
//
// DOM-free and I/O-free so `test/orchclis.test.ts` can pin it directly — it used
// to be a `const` inside launcher.ts, where the one thing worth testing about it
// (that a role's default is a model the picker actually offers, and that loomux
// never quietly pins a model the human didn't ask for) could not be.
//
// **This is a SUGGESTION list, not a capability claim.** Membership here says
// "the launcher can start a group on this CLI"; what a CLI can *do* is the
// backend's to state (`CLI_CAPS` / `cli_can_host`, and `agent_cli_knobs` for the
// knobs). The model lists are shortcuts for the ids this repo's issues actually
// target — the picker merges whatever the CLI's own `--help` reports and always
// keeps a `custom…` entry, so a missing id costs a human one line of typing, and
// a stale id in a hardcoded table would cost them a failed spawn (#329).
//
// That "shortcut" sizing holds for every row whose CLI can be asked something.
// It does NOT hold for copilot, whose row is a full catalog because nothing on
// the machine will answer for it — see the row itself for why, and for what
// retires it (#1020).
//
// Every id here is one PATH probe (`probe_agent_cli`) away from being refused at
// submit: the `id` IS the program name the launcher probes, which is why
// `orchclis.test.ts` pins it against `AGENTS`.

import type { OrchRole } from "./roster";

/** The curated-list entry that means **no `--model` at all**: the pane runs on
 *  whatever model the human's own CLI config selects.
 *
 *  Only offered where the CLI has no vendor-neutral alias to default to
 *  (opencode — see its row), because there it is the only honest default: any id
 *  loomux picked would silently override a human who had already chosen one, and
 *  the backend's `default_model("opencode", _)` returns exactly this for the same
 *  reason (#722). It is a real, selectable option rather than an absence, so the
 *  form can *say* "inherit" instead of leaving a role looking unset. */
export const INHERIT_MODEL = "";

export interface OrchCli {
  /** The CLI id AND the program name the launcher probes on PATH. */
  id: string;
  /** Curated model ids to suggest, in menu order. */
  models: string[];
  /** The model each capability class starts on. `INHERIT_MODEL` means "send
   *  nothing and let the CLI decide" — never a silent pick. Every value must be
   *  either `INHERIT_MODEL` or one of `models`, or the picker would open on its
   *  `custom…` branch with a prefilled id (pinned in `orchclis.test.ts`). */
  defaults: Record<OrchRole, string>;
}

export const ORCH_CLIS: OrchCli[] = [
  {
    id: "claude",
    models: ["sonnet", "opus", "haiku", "fable"],
    // Reasoning-heavy roles (orchestrator, planner) default to the strong
    // tier; executing roles (worker, reviewer) to the mid tier.
    defaults: { orchestrator: "opus", worker: "sonnet", reviewer: "sonnet", planner: "opus" },
  },
  {
    // The one row that carries a FULL vendor catalog rather than a shortcut, and
    // it is deliberate — human-directed (#1020), not a conclusion an agent may
    // re-derive or extend to another CLI.
    //
    // Every other row stays short because something on the machine will answer
    // for it: the picker puts the CLI's own reply in front of these suggestions
    // (`mergeModelOptions`), so a curated list only has to cover the defaults.
    // **Nothing answers for copilot.** It has no `ENUMERATORS` row (cliprobe.rs)
    // and no `PROTOCOLS` row (modelwire.rs), so its only live source is
    // `parse_models_from_help`, and copilot's `--help` no longer enumerates
    // models under `--model` — the parse comes back empty. The merge then has
    // nothing to lead with and this list IS the menu, which is why five ids left
    // a human retyping two dozen others from memory.
    //
    // The order is copilot's own, not sorted, and `auto` stays FIRST: it is
    // copilot's pick-for-me value and the default on every role.
    //
    // **INTERIM.** A static catalog standing in for a live answer ages exactly
    // the way #329 warns, and this one is meant to be retired rather than
    // maintained: when copilot gains a *supported* way to enumerate its models,
    // give it the `ENUMERATORS`/`PROTOCOLS` row and cut this back to the handful
    // the defaults need. No change here is required for that — the machine's
    // answer already sorts in front of these. (Today's `copilot … model list` is
    // rejected as unsupported and only spills a catalog on its ERROR path, which
    // is not a contract loomux may parse.)
    //
    // **What this list is NOT: an entitlement claim.** Copilot also reports a
    // much shorter per-account "Supported models" set, which varies by plan and
    // subscription. That is a fact about one machine, and baking it in is the
    // host special-casing constraint 8 forbids — these are the models copilot
    // offers as a product. An id a given account cannot use is refused at spawn,
    // which is a truthful failure; hiding it here would make loomux wrong for
    // everyone whose plan differs from the one that produced the list.
    id: "copilot",
    models: [
      "auto",
      "gpt-5.6-sol",
      "gpt-5.6-terra",
      "gpt-5.6-luna",
      "gpt-5.5",
      "gpt-5.4",
      "gpt-5.4-mini",
      "gpt-5.3-codex",
      "gpt-5-mini",
      "claude-sonnet-5",
      "claude-fable-5",
      "claude-opus-5",
      "claude-opus-4.8",
      "claude-opus-4.8-fast",
      "claude-opus-4.7",
      "claude-sonnet-4.6",
      "claude-sonnet-4.5",
      "claude-haiku-4.5",
      "mai-code-1-flash-picker",
      "gemini-3.7-flash",
      "gemini-3.6-flash",
      "gemini-3.5-flash",
      "gemini-3.1-pro-preview",
      "grok-4.5",
      "kimi-k3",
      "kimi-k2.7-code",
      "grok-4.6",
      "mai-code-1.1-flash",
    ],
    defaults: { orchestrator: "auto", worker: "auto", reviewer: "auto", planner: "auto" },
  },
  {
    // #722. Model ids here are `provider_id/model_id` — the `/` is part of the
    // id, and the backend's `sanitize_model` was widened to stop dropping it
    // (it silently turned `opencode/deepseek-v4-flash-free` into a model that
    // does not exist). Nothing on this path may re-mangle it: the picker's
    // option VALUE is the raw id, and only the label is prettified.
    //
    // The curated ids, and where each one comes from:
    //   opencode/deepseek-v4-flash-free  the free Zen model #722 exists for
    //                                    ("DeepSeek V4 Flash Free", Zen docs;
    //                                    provider key `opencode`, not `zen`,
    //                                    confirmed against the CLI's own model
    //                                    parser in the #722 verification memo)
    //   opencode/deepseek-v4-flash       its paid sibling, same memo
    //   opencode/gpt-5.1-codex           the models reference's own example id
    // The Zen free tier is broader than this, and deliberately not enumerated:
    // each extra row is another line of hardcoded model table to go stale
    // (#329), against a `custom…` entry that already accepts any id.
    id: "opencode",
    models: [INHERIT_MODEL, "opencode/deepseek-v4-flash-free", "opencode/deepseek-v4-flash", "opencode/gpt-5.1-codex"],
    // NO default, on every role — the one CLI where that is the honest answer.
    // opencode has no vendor-neutral alias at all (no `sonnet`, no `auto`, no
    // `pro`): its catalog spans dozens of providers, so any default loomux
    // picked would be both a hardcoded model table and a silent override of the
    // human's own `opencode.json` / `/models` choice. This mirrors the backend's
    // `default_model("opencode", _)` — the launcher and the spawn path have to
    // agree, or the form would advertise an inheritance it then overrode.
    defaults: {
      orchestrator: INHERIT_MODEL,
      worker: INHERIT_MODEL,
      reviewer: INHERIT_MODEL,
      planner: INHERIT_MODEL,
    },
  },
];

/** The catalog row for a CLI id, falling back to the first row — the launcher's
 *  Agent field can hold an id that orchestrator mode does not offer (a solo-only
 *  CLI, `custom`), and every caller here needs *a* row rather than a crash. */
export function orchCliFor(id: string): OrchCli {
  return ORCH_CLIS.find((c) => c.id === id) ?? ORCH_CLIS[0]!;
}

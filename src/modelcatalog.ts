// The one place that answers "which models may this CLI be pinned to?" (#935).
//
// DOM-free and I/O-free (`test/modelcatalog.test.ts`): the backend probe arrives
// as an INJECTED function, so every rule below is testable without a browser or a
// Tauri host, and both surfaces that ask the question — the launcher's per-role
// selector and the workflow pane's block editor — get the same answer from the
// same code rather than from two hand-copied merges.
//
// Two sources feed it, and they are different KINDS of claim:
//
//   curated (`orchclis.ts`)  a suggestion list, written down here in the repo.
//                            Stable, offline, and always wrong eventually (#329).
//   probed (`probe_agent_cli`) what the CLI on THIS machine reports about itself.
//                            Current by construction, and specific to the human's
//                            own install — opencode's `models` enumerates the
//                            providers they configured, which no curated list can
//                            know.
//
// So the probe leads and the curated list backs it: a machine's own answer beats
// a suggestion, and a CLI that reports nothing (parse miss, not installed, an
// older build) degrades to the suggestions rather than to an empty dropdown. The
// merge never REPLACES curated entries, because a role's default is drawn from
// them and a default outside the offered list opens the picker on its `custom…`
// branch with a prefilled id — which reads as a human's own typing
// (`orchclis.test.ts` names that failure).
//
// This module states no vendor fact of its own. Which models a CLI can be given
// is the CLI's to report and `orchclis.ts`' to suggest; whether a knob applies to
// the selected model is `selectorknobs.ts`' (narrowed by the backend's `CLI_CAPS`).
// Adding a model table here would be a third copy of the thing #329 says not to
// keep one of.

// `.ts` extension: node's `--test` resolves these specifiers itself (no bundler),
// so a pure module and everything it imports must be reachable as written — the
// same reason `panesetup.ts`/`sessionroute.ts` spell theirs out.
import { INHERIT_MODEL, ORCH_CLIS } from "./orchclis.ts";

/** Result of probing an agent CLI on this machine — the wire shape of the
 *  backend's `probe_agent_cli` (`src-tauri/src/cliprobe.rs`), owned here rather
 *  than in `pty.ts` for the reason `CliKnobs` is owned by `selectorknobs.ts`:
 *  the pure module that reasons about the reply is the one a test can import. */
export interface CliProbe {
  available: boolean;
  /** Model ids the CLI reported (may be empty — a parse miss is not an error). */
  models: string[];
  /** Human-readable failure reason when not available. */
  error: string | null;
}

/** The probe result for a program loomux could not reach at all. Never thrown
 *  onwards: a form that cannot ask the machine still has to render, and it
 *  renders the curated suggestions plus the `custom…` escape. */
export const probeFailure = (error: string): CliProbe => ({
  available: false,
  models: [],
  error,
});

/** The curated suggestions for a CLI id, or `[]` for one this repo has no row
 *  for.
 *
 *  Deliberately NOT `orchCliFor`, whose fallback-to-the-first-row exists for the
 *  launcher's Agent field (orchestrator mode restricts that select to `ORCH_CLIS`
 *  ids, so the fallback never fires there). The workflow block editor's CLI list
 *  is wider — `gemini` is a `WORKFLOW_CLIS` member with no `ORCH_CLIS` row — and
 *  falling back there would offer claude's aliases as gemini models: a wrong
 *  answer wearing the same clothes as a right one. Nothing to suggest is an
 *  honest answer; the picker still probes, and still keeps `custom…`. */
export function curatedModels(cli: string): string[] {
  return ORCH_CLIS.find((c) => c.id === cli.trim())?.models.slice() ?? [];
}

/** Merge a CLI's own reported models with the curated suggestions.
 *
 *  Order: the inherit row first when the curated list offers one, then everything
 *  the CLI reported, then the curated entries it did not.
 *
 *  **Why inherit is pinned.** `INHERIT_MODEL` is not a model — it is the "send no
 *  `--model` at all" row (#722), the honest default on a CLI where any id loomux
 *  picked would silently override the human's own config. A real enumerator
 *  (opencode's `models`) can report dozens of ids, and appending the curated list
 *  after them would bury the one row that means "don't choose" at the bottom of
 *  the menu — the choice a human is least likely to find is the one loomux most
 *  wants them to keep. It is pinned, not sorted in: everything else keeps the
 *  probe-leads order.
 *
 *  Ids are compared and emitted VERBATIM. opencode's are `provider_id/model_id`
 *  and the `/` is part of the id (#722) — nothing here may re-mangle one. The only
 *  entries dropped are blank ones from the probe: an empty id renders as the
 *  inherit row, and manufacturing one for a CLI whose curated row does not offer
 *  inheritance would advertise an inheritance the spawn path then overrides. */
export function mergeModelOptions(curated: readonly string[], probed: readonly string[]): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  const push = (id: string): void => {
    if (seen.has(id)) return;
    seen.add(id);
    out.push(id);
  };
  if (curated.includes(INHERIT_MODEL)) push(INHERIT_MODEL);
  for (const id of probed) if (id.trim() !== "") push(id);
  for (const id of curated) push(id);
  return out;
}

/** Everything a picker should offer for `cli`, given the probe reply in hand
 *  (`null` before it lands, or when it failed — both degrade to curated). */
export function modelOptions(cli: string, probe: CliProbe | null): string[] {
  return mergeModelOptions(curatedModels(cli), probe?.models ?? []);
}

/** The options a WORKFLOW BLOCK's picker offers, given what {@link modelOptions}
 *  merged for its CLI.
 *
 *  One rule on top of the launcher's list, and it is a property of the FILE
 *  rather than of the CLI: a block's `model:` is optional, and leaving it out is
 *  a declared state — `workflow.rs`'s `model_of` resolves it to
 *  `default_model(cli, kind)`. The launcher has no equivalent, because every role
 *  there starts on a real default drawn from the curated row. So the blank row is
 *  offered on EVERY CLI here, not only on the one whose curated row carries it —
 *  without it, a block that declares no model would open on whatever happens to
 *  be first in the menu (`sonnet`, on claude), which reads as a choice somebody
 *  made and takes away the only way to say "leave it to loomux" once a model has
 *  been picked. That is a field NARROWER than the free text it replaces, which is
 *  the one thing #935 may not do.
 *
 *  But only when there is a menu to put it in front of. A CLI with nothing
 *  curated and nothing probed (`gemini`, today) has no dropdown at all —
 *  `pickerSelection` opens such a picker straight onto its custom input, which IS
 *  the field then, and a one-row menu reading "(unset)" in front of it would be a
 *  menu whose only purpose is to be escaped from. An empty custom box already
 *  means exactly what the blank row means. */
export function blockModelOptions(models: readonly string[]): string[] {
  if (!models.length) return [];
  return models.includes(INHERIT_MODEL) ? [...models] : [INHERIT_MODEL, ...models];
}

/** The select value that means "let me type an id" — a sentinel, not a model, so
 *  it can never collide with one: every real id the pickers carry is either empty
 *  (inherit) or a vendor id, and none of them starts with `__`. */
export const CUSTOM_OPTION = "__custom";

/** What a picker should show for `current` against the options it now has. */
export interface PickerSelection {
  /** The `<select>`'s value: one of `models`, or `CUSTOM_OPTION`. */
  selected: string;
  /** The text the custom input should carry. Only meaningful when `showCustom`. */
  custom: string;
  /** Whether the custom input is visible. */
  showCustom: boolean;
}

/** Resolve a picker's state — extracted from the DOM component so the one thing
 *  worth pinning about it can be tested (the repo's DOM-free-module convention).
 *
 *  The membership test comes FIRST, before the "is it empty" test, and that order
 *  is the point: `INHERIT_MODEL` is the empty string AND a real menu row, so a
 *  falsiness check in front would treat a human's deliberate "inherit" as "nothing
 *  chosen" and fall through to whatever happened to be first in the list. On
 *  opencode that is the difference between inheriting the model the human
 *  configured and silently pinning a probed one (#722).
 *
 *  An id that is not on the menu is not refused — it opens the custom branch
 *  carrying that id. Bedrock ARNs, gateway deployment names and models newer than
 *  this build all arrive that way, and a dropdown that could only offer what
 *  loomux already knew would be a narrower field than the free text it replaced. */
export function pickerSelection(models: readonly string[], current: string): PickerSelection {
  if (models.includes(current)) return { selected: current, custom: "", showCustom: false };
  if (current) return { selected: CUSTOM_OPTION, custom: current, showCustom: true };
  const first = models[0];
  return first === undefined
    ? // No options at all (a CLI with no curated row whose probe reported
      // nothing): the custom input IS the field, so it is shown rather than left
      // hidden behind a menu whose only entry is `custom…`.
      { selected: CUSTOM_OPTION, custom: "", showCustom: true }
    : { selected: first, custom: "", showCustom: false };
}

/** The probe seam: one call per program per app run, shared by every surface.
 *
 *  Memoized on the PROMISE, not the result, so two forms opening at once make one
 *  backend call rather than two (the backend caches too — this keeps the IPC and
 *  the 8s worst case off the second caller as well).
 *
 *  Never rejects. A probe is loomux asking the machine a question it can live
 *  without an answer to: the surfaces all render curated suggestions plus a
 *  `custom…` escape either way, and a rejected promise reaching a form's render
 *  path turns "we couldn't ask" into a broken field. */
export class ModelCatalog {
  private inflight = new Map<string, Promise<CliProbe>>();
  private resolved = new Map<string, CliProbe>();

  /** The injected backend call (`pty.ts`'s `probeAgentCli`). Written out rather
   *  than as a constructor parameter property: node's `--test` runs these modules
   *  in strip-only mode, where a parameter property is a syntax error, so the
   *  shorthand would put this module out of reach of its own tests. */
  private readonly probeFn: (program: string) => Promise<CliProbe>;

  constructor(probeFn: (program: string) => Promise<CliProbe>) {
    this.probeFn = probeFn;
  }

  probe(program: string): Promise<CliProbe> {
    let p = this.inflight.get(program);
    if (!p) {
      p = this.probeFn(program)
        .catch((e: unknown) => probeFailure(String(e)))
        .then((r) => {
          this.resolved.set(program, r);
          return r;
        });
      this.inflight.set(program, p);
    }
    return p;
  }

  /** The probe reply already in hand, or `null` while one is still in flight —
   *  for the synchronous paths (a form's first paint) that must render before
   *  awaiting anything. */
  cached(program: string): CliProbe | null {
    return this.resolved.get(program) ?? null;
  }

  /** The options to offer for `cli` RIGHT NOW: curated, merged with the probe
   *  reply if one has landed. Synchronous by design — a form paints immediately
   *  and re-paints when {@link probe} resolves. */
  models(cli: string): string[] {
    return modelOptions(cli, this.cached(cli));
  }
}

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
 *  But only when there is a menu to put it in front of. A CLI with no curated
 *  row and nothing back from its probe (`gemini`, today — it is probed like any
 *  other, the reply is what carries nothing) has no dropdown at all —
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

/** Whether a probe reply is worth remembering for the rest of the app run.
 *
 *  **This mirrors the backend's own caching rule, and it has to.**
 *  `probe_agent_cli` (cliprobe.rs) caches only a COMPLETE probe: "failures and
 *  partial answers are NOT [cached] — a CLI installed while loomux is running
 *  must become launchable on the next probe … and by the same argument an
 *  opencode whose `models` run failed — a network blip, a provider configured or
 *  `opencode auth login` completed a minute later — must be able to report its
 *  real list without a restart." A memo in FRONT of that backend which keeps
 *  those answers anyway does not merely duplicate the cache; it deletes the
 *  recovery, because there is then no next probe to reach it.
 *
 *  Completeness is deliberately not a wire field ("a caching fact, not a wire
 *  field", cliprobe.rs), so this reads the same fact off the reply itself: an
 *  answer that carries no list is exactly the answer a later probe might improve
 *  on. For an enumerator CLI that is completeness verbatim (`complete =
 *  !listed.is_empty()`); for a help-parsed one it is stricter, and stricter in
 *  the safe direction — the cost is an extra IPC to a backend that has the answer
 *  in a HashMap, against a stale "this CLI has nothing" that would last the
 *  session. */
export function worthKeeping(probe: CliProbe): boolean {
  return probe.available && probe.models.length > 0;
}

/** What a CLI's own list-models reply said about ONE model (#993).
 *
 *  A different kind of claim from the two above it. `curated` is a suggestion
 *  this repo wrote down, and `probed` (`--help`, `opencode models`) is what the
 *  CLI advertises it *accepts*. This is what the CLI reports about a model's own
 *  capabilities **on the machine in front of the human** — which nothing else
 *  can answer, because an effort level is per-model and per-account. A table of
 *  them here would be the third copy of the thing #329 says not to keep one of.
 *
 *  The fields mirror Claude Code's own `ModelInfo`, which Anthropic types in
 *  `@anthropic-ai/claude-agent-sdk`'s `sdk.d.ts` and publishes at
 *  <https://docs.claude.com/en/api/agent-sdk/typescript> §ModelInfo (read
 *  2026-08-14 per the `agent-cli-reference` discipline):
 *
 *      value: string;                 // Model identifier to use in API calls
 *      resolvedModel?: string;        // Canonical wire model id this row's `value` resolves to
 *      displayName: string;           // Human-readable display name
 *      description: string;           // Description of the model's capabilities
 *      supportsEffort?: boolean;      // Whether this model supports effort levels
 *      supportedEffortLevels?: ('low' | 'medium' | 'high' | 'xhigh' | 'max')[];
 *
 *  Renamed rather than mirrored verbatim because this is loomux's own shape, not
 *  one vendor's: a Copilot or opencode probe will fill the same fields from
 *  whatever its own reply calls them. Every capability field on that type is
 *  OPTIONAL, and a real reply exercises that — `haiku` comes back carrying no
 *  effort fields at all — so absent has to stay distinguishable from false. */
export interface ModelDetail {
  /** The id VERBATIM as the CLI reported it (`ModelInfo.value`) — what `--model`
   *  would receive. May itself carry a `[1m]` suffix: `opus[1m]` is a row the
   *  CLI lists, not something loomux composes. */
  id: string;
  /** The canonical wire id `id` resolves to (`ModelInfo.resolvedModel`), or `""`
   *  when the CLI did not say. Anthropic documents it as requiring Claude Code
   *  v2.1.197 or later, so `""` is the ordinary answer from an older install and
   *  never an error. Worth carrying because it turns an alias into the exact
   *  model the account is really being served — the one id a context-window
   *  lookup can be sure about. */
  resolvedId: string;
  /** The CLI's own display name (`ModelInfo.displayName`), or `""`. */
  name: string;
  /** The CLI's own description (`ModelInfo.description`), or `""`. Reported
   *  prose, shown verbatim: loomux never parses a number out of it. */
  description: string;
  /** Whether the CLI said this model takes a reasoning-effort setting.
   *
   *  **`null` is a third state, not a synonym for `false`.** The docs-say-X /
   *  docs-are-silent / docs-say-NOT-X rule (`agent-cli-reference`) applied to a
   *  reply instead of a page: `false` is the CLI saying this model has no effort
   *  knob, and `null` is the CLI not raising the subject — an older build, a
   *  field that moved, a row like `haiku` that simply omits it. They must not
   *  collapse, because `false` turns the knob OFF and `null` has to leave it
   *  exactly as it was. */
  supportsEffort: boolean | null;
  /** The effort levels the CLI listed for this model, in its own order. Empty
   *  when it listed none — which, paired with `supportsEffort: null`, is simply
   *  "nothing was said". */
  effortLevels: string[];
}

/** A whole list-models reply. `models` empty is the ordinary failure: the CLI is
 *  not installed, is an older build without the control request, or answered in
 *  a shape this build does not recognise. Every one of those degrades to the
 *  seed rather than to an error a form has to render. */
export interface ModelReport {
  models: ModelDetail[];
  /** Human-readable reason the reply carried nothing, or `null`. Diagnostic
   *  only — no surface refuses anything on it. */
  error: string | null;
}

/** The report for a CLI loomux could not ask at all. */
export const reportFailure = (error: string): ModelReport => ({ models: [], error });

/** Strip the `[1m]` context suffix from an id, if it carries one.
 *
 *  The suffix selects a context window on an existing model (model-config
 *  §Extended context); it does not name a different model, and a CLI enumerating
 *  its models reports the base ids. So `sonnet[1m]` has to find `sonnet`'s row —
 *  otherwise a human who picked the 1M variant would silently lose the effort
 *  levels the plain variant shows, which reads as the suffix having disabled
 *  something. */
function withoutContextSuffix(id: string): string {
  const raw = id.trim();
  return raw.toLowerCase().endsWith("[1m]") ? raw.slice(0, -4) : raw;
}

/** The reported detail for `id`, or `null` when the reply said nothing about it.
 *
 *  Matching widens in one direction only, and each step is a statement loomux
 *  can defend: verbatim (the ids are the same string), then case-insensitively
 *  (a vendor id is not case-significant, and `modelnames.ts` already compares
 *  this way), then with `[1m]` stripped (the suffix is a context window, not a
 *  model). It never widens to a FAMILY: `claude-sonnet-4-5` must not pick up
 *  `sonnet`'s reported effort levels, because they are a different model's. */
export function detailFor(models: readonly ModelDetail[], id: string): ModelDetail | null {
  const raw = id.trim();
  if (!raw) return null;
  const exact = models.find((m) => m.id === raw);
  if (exact) return exact;
  const want = withoutContextSuffix(raw).toLowerCase();
  if (!want) return null;
  return models.find((m) => withoutContextSuffix(m.id).toLowerCase() === want) ?? null;
}

/** Whether a list-models reply is worth remembering for the rest of the app run.
 *
 *  The same rule as {@link worthKeeping} and for the same reason: a reply that
 *  carried nothing is exactly the reply a later ask might improve on — the CLI
 *  was installed a minute later, an upgrade added the control request, a login
 *  completed. Keeping it would delete the recovery rather than duplicate a
 *  cache. */
export function reportWorthKeeping(report: ModelReport): boolean {
  return report.models.length > 0;
}

/** The probe seam, shared by every surface: one call per program per app run for
 *  an answer worth keeping, and a fresh ask for one that is not.
 *
 *  Memoized on the PROMISE, not the result, so two forms opening at once make one
 *  backend call rather than two — which is also what bounds the re-ask: an answer
 *  that was not worth keeping costs one probe per *caller that asks again*, never
 *  a stampede, and callers ask per surface rather than per paint.
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

  constructor(
    probeFn: (program: string) => Promise<CliProbe>,
    detectFn: ((program: string) => Promise<ModelReport>) | null = null
  ) {
    this.probeFn = probeFn;
    this.detectFn = detectFn;
  }

  probe(program: string): Promise<CliProbe> {
    const kept = this.resolved.get(program);
    if (kept) return Promise.resolve(kept);
    let p = this.inflight.get(program);
    if (!p) {
      p = this.probeFn(program)
        .catch((e: unknown) => probeFailure(String(e)))
        .then((r) => {
          // Kept only if {@link worthKeeping}. The `inflight` entry is dropped
          // either way, and that is the whole fix: an answer this memo does not
          // keep leaves nothing behind, so the NEXT caller reaches the backend
          // and can be told that the CLI has since been installed, or that
          // opencode can enumerate now. Deleting it here rather than never
          // memoizing at all keeps concurrent callers on one call.
          if (worthKeeping(r)) this.resolved.set(program, r);
          this.inflight.delete(program);
          return r;
        });
      this.inflight.set(program, p);
    }
    return p;
  }

  /** The probe reply already in hand, or `null` when there is none worth having
   *  — one still in flight, or one that carried nothing — for the synchronous
   *  paths (a form's first paint) that must render before awaiting anything.
   *  Both `null` cases mean the same thing to a caller: render the curated
   *  suggestions, and re-paint if something better lands. */
  cached(program: string): CliProbe | null {
    return this.resolved.get(program) ?? null;
  }

  /** The options to offer for `cli` RIGHT NOW: curated, merged with the probe
   *  reply if one has landed, then with anything the CLI's own list-models reply
   *  named. Synchronous by design — a form paints immediately and re-paints when
   *  {@link probe} or {@link detect} resolves.
   *
   *  The list-models ids go through the same {@link mergeModelOptions} the probe
   *  reply does, and for the same reason: a machine's own answer beats a
   *  suggestion, so they lead, and the curated entries stay behind them rather
   *  than being replaced. Detection therefore only ever ADDS rows and re-orders
   *  them — a role's default is still on the menu afterwards, which is the
   *  property `orchclis.test.ts` names. */
  models(cli: string): string[] {
    const base = modelOptions(cli, this.cached(cli));
    const detected = this.report(cli);
    return detected ? mergeModelOptions(base, detected.models.map((m) => m.id)) : base;
  }

  // ---- the list-models reply (#993, made automatic by #1020) ---------------
  //
  // Still a SECOND seam rather than a widening of the first, but for a
  // different reason than it had under #993. Then, `detect` SPAWNED the CLI and
  // `probe` did not, so the two were kept apart by what they cost and this one
  // moved only when a human clicked. Now neither spawns from here at all: the
  // backend's startup sweep runs the control request once, unbidden, and
  // `detect` is a lookup against what it left behind (`src-tauri/src/modelwire.rs`).
  //
  // What still separates them is the ANSWER, not the cost. `probe` reports what
  // a CLI says it accepts; this reports what the CLI said about the models on
  // this machine — ids, display names, per-model effort levels. Merging them
  // would collapse two different kinds of claim into one map, which is the
  // thing the header of this module is about.

  /** Every program a lookup has been issued for, resolved or not.
   *
   *  **Kept forever, unlike {@link probe}'s, and the difference is the point.**
   *  A probe memo drops an answer that was not worth keeping so the NEXT caller
   *  re-asks and can be told the CLI has since appeared. That re-ask was
   *  affordable because it was one IPC; here, under #993, it was affordable
   *  because it cost a human gesture. #1020 removed the gesture — the callers
   *  are paints now, and a form that re-renders would re-issue on every paint.
   *
   *  So the bound moved into the memo: one lookup per program per app run. It
   *  costs nothing to be wrong about, because the lookup is not the only
   *  delivery — {@link acceptReport} overwrites whatever this remembered when
   *  the sweep's own answer arrives. */
  private detectAsked = new Map<string, Promise<ModelReport>>();
  /** Reports {@link reportWorthKeeping} judged keepable — what {@link report}
   *  serves. Separate from {@link detectAsked} because a barren answer must
   *  still leave `report()` null (a surface has to fall back to its seed) while
   *  bounding the ask. */
  private detectResolved = new Map<string, ModelReport>();
  /** Live listeners for {@link onReport}. */
  private reportListeners: ((program: string) => boolean)[] = [];

  /** The injected backend call (`pty.ts`'s `listCliModels`, read through
   *  `modelwire.ts`). Optional: the launcher's own catalog is constructed
   *  before this slice existed in it, and a catalog with no detector simply
   *  reports nothing rather than failing. */
  private readonly detectFn: ((program: string) => Promise<ModelReport>) | null;

  /** Look up what the backend's startup sweep found for `program`.
   *
   *  **Safe on a paint path, which is what #1020 changed.** The call it makes
   *  cannot spawn an agent CLI — the sweep already did that, once, at startup —
   *  so a picker fires this on open the way it fires {@link probe}. It resolves
   *  from the backend memo, normally immediately.
   *
   *  Issued at most once per program per app run (see {@link detectAsked}); a
   *  form that repaints re-reads the resolved promise and issues nothing. An
   *  answer that carried nothing is still not KEPT — {@link report} stays null
   *  and surfaces show their seed — it is simply not asked for again, because
   *  the sweep is what would have to change its mind, and the sweep pushes.
   *
   *  Never rejects: a lookup that failed leaves every surface exactly as it
   *  was. */
  detect(program: string): Promise<ModelReport> {
    const asked = this.detectAsked.get(program);
    if (asked) return asked;
    const detect = this.detectFn;
    if (!detect) return Promise.resolve(reportFailure("this catalog has no list-models detector wired"));
    const p = detect(program)
      .catch((e: unknown) => reportFailure(String(e)))
      .then((r) => {
        // Never downgrade: an event may have landed while this was in flight,
        // and it carries the same memo this was reading. Whichever arrived with
        // a real answer wins.
        if (reportWorthKeeping(r)) this.detectResolved.set(program, r);
        return this.detectResolved.get(program) ?? r;
      });
    this.detectAsked.set(program, p);
    return p;
  }

  /** Take a report the backend pushed, rather than one this catalog asked for
   *  (#1020 — `models-detected`).
   *
   *  The push half of detection. A picker painted while the sweep was still
   *  running has already read "nothing yet" and memoized that lookup, so
   *  without this its dropdown would keep the seed for the life of the app.
   *
   *  Two reports are refused, and each refusal is a repaint somebody does not
   *  owe:
   *
   *  - One that carries NOTHING. It says only that the sweep found nothing for
   *    that CLI, which is what every surface already assumes — and storing it
   *    would overwrite a real answer that arrived first.
   *  - One for a CLI an answer is ALREADY held for. The sweep runs once per app
   *    run and asks each CLI once, so a second answer for the same program is
   *    not a new fact: it is this same answer arriving by the other route, and
   *    the two routes racing is the ordinary case rather than the odd one. The
   *    picker that already painted it must not be rebuilt a second time —
   *    possibly under a caret, since a deferred rebuild fires on blur.
   *
   *  Returns whether anything changed, which is what makes "fired only for a
   *  report that changed something" true of {@link onReport} rather than merely
   *  intended. A design that ever re-sweeps mid-run has to revisit the second
   *  refusal — it is a statement about the producer, not about reports. */
  acceptReport(program: string, report: ModelReport): boolean {
    if (!reportWorthKeeping(report)) return false;
    if (this.detectResolved.has(program)) return false;
    this.detectResolved.set(program, report);
    // Also settles the pull side: a picker that opens later reads this instead
    // of issuing a lookup for an answer already in hand.
    this.detectAsked.set(program, Promise.resolve(report));
    // A listener that reports itself gone is dropped here rather than needing
    // its host to remember to unsubscribe — see `onReport`.
    this.reportListeners = this.reportListeners.filter((cb) => cb(program));
    return true;
  }

  /** Be told when a pushed report lands, so a form already on screen can
   *  refresh (#1020).
   *
   *  **The callback returns whether it is still alive**, and that is the whole
   *  lifecycle contract: `false` drops it. A host with an explicit teardown can
   *  use the returned unsubscribe instead, but neither surface here reliably
   *  has one — the launcher form is discarded with its pane, and a subscription
   *  that outlived it would repaint detached DOM forever. Liveness is a fact
   *  only the host knows (`this.disposed`, `el.isConnected`), and this module
   *  is DOM-free, so the host is asked rather than inspected.
   *
   *  Fired only for a report that CHANGED something ({@link acceptReport}), so
   *  a listener never has to ask whether it has already seen this one. */
  onReport(cb: (program: string) => boolean): () => void {
    this.reportListeners.push(cb);
    return () => {
      this.reportListeners = this.reportListeners.filter((x) => x !== cb);
    };
  }

  /** The list-models reply already in hand for `cli`, or `null`. */
  report(cli: string): ModelReport | null {
    return this.detectResolved.get(cli) ?? null;
  }

  /** What the CLI reported about one model, or `null` when it has not been
   *  asked, or asked and said nothing about this id. All three are the same
   *  thing to a surface: show what you already knew. */
  detail(cli: string, id: string): ModelDetail | null {
    const report = this.report(cli);
    return report ? detailFor(report.models, id) : null;
  }
}

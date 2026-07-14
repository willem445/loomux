// The agent roster a group will run — resolved, and DOM-free so it can be tested
// (#222, the advanced-orchestrator toggle).
//
// Two jobs, both of which used to be scattered:
//
// 1. **The canonical role table.** `OrchRole` and its labels were duplicated in
//    three places (launcher.ts's fixed four-row form table, groupview.ts's chip
//    map — which had gone stale and was missing `planner` entirely, so a planner
//    pane showed a generic "AGENT" chip — and orchbadge.ts's union). The union and
//    the badge text stay in orchbadge.ts (its own pure module, already correct);
//    everything ELSE about a role lives here, and both UIs read it.
//
// 2. **Roster resolution.** Given the toggle, the backend's preview of the repo's
//    workflow file, and the launcher's per-role picks, work out the roster the
//    group would actually run and how to describe it. This is the text the human
//    consents to before the group spawns, so it has to say the same thing the
//    backend will do — including the un-obvious cases: a broken workflow file
//    still launches (on the built-in roster), and turning the toggle on in a repo
//    that declares nothing is a no-op, not an error.
//
// The blocks themselves are RESOLVED BY THE BACKEND (`orch_workflow_preview` runs
// the same load + clamp that `create_group` runs). This module never parses YAML:
// a second parser is a second set of answers, and the only answer that matters is
// the engine's.

import type { OrchRole } from "./orchbadge";

export type { OrchRole };

/** The four capability classes, in roster order — the order the launcher lists
 *  its per-role CLI/model rows and the order a resolved roster reads best in.
 *  `label` is the form caption; the short chip text lives in orchbadge.ts. */
export const ORCH_ROLES: readonly { key: OrchRole; label: string }[] = [
  { key: "orchestrator", label: "Orchestrator" },
  { key: "worker", label: "Worker" },
  { key: "reviewer", label: "Reviewer" },
  { key: "planner", label: "Planner" },
];

/** How a block's repo-authored instructions (if any) reach its agent. `none` is
 *  every block of the built-in roster — and the only value for which a block is
 *  just a role with a different name. */
export type BlockPersona = "none" | "prompt" | "profile";

/** One resolved block: what a pane launched from it will actually run. Mirrors
 *  the backend's `orch_workflow_preview` rows and the group panel's agent rows. */
export interface RosterBlock {
  id: string;
  name: string;
  kind: OrchRole;
  cli: string;
  model: string;
  persona: BlockPersona;
}

/** The backend's read of `<repo>/.loomux/workflow.yml` (`orch_workflow_preview`).
 *  Never an error: a repo with no file, and a repo with a broken one, are both
 *  things the launcher has to be able to describe. */
export interface WorkflowPreview {
  /** `.loomux/workflow.yml` — from the backend, so the two can't drift. */
  path: string;
  /** Whether the repo has the file at all. */
  present: boolean;
  /** Whether it parsed and validated. `!present` is vacuously valid. */
  valid: boolean;
  /** The workflow's `name:`, or "". */
  name: string;
  /** Every validation finding, not just the first. Empty when `valid`. */
  errors: string[];
  /** Names of declared gates (`merge`). Enforcement is loomux's, not the UI's. */
  gates: string[];
  /** The resolved roster. Empty when the file is absent or invalid. */
  blocks: RosterBlock[];
  /** #255: the structural agent-capacity this roster + its merge gate need, or
   *  `null` when there's nothing declared to derive one from (the file is
   *  absent or invalid — the group would run the built-in roster instead). */
  min_agents: number | null;
  recommended_agents: number | null;
  /** The gate's own reviewer requirement folded into `min_agents` — NOT a
   *  count of reviewer blocks. Read this, never `blocks.filter(reviewer)`, to
   *  describe why `min_agents` is what it is: a `threshold: N` gate over a
   *  larger or different set of reviewer blocks makes those two numbers
   *  genuinely different, and recounting blocks to describe a gate-derived
   *  number was rev-1 of #255's review catching exactly that bug. */
  reviewers_needed: number | null;
  /** Which declared tiers `recommended_agents` adds over `min_agents` — short
   *  noun phrases (`"the planner"`, `"1 more worker tier"`), backend-computed
   *  (`workflow::extra_tiers`) for the same reason as `reviewers_needed`: so
   *  the launcher never re-derives which blocks are "extra" from the roster
   *  and the gate need separately. `[]` when nothing is (minimum ==
   *  recommended), `null` alongside the other `null`s above. */
  extra_tiers: string[] | null;
}

/** The launcher's own hard ceiling on `max_agents` (`numberInput(4, 1, 12)`
 *  below) — mirrors `MAX_AGENTS_CEILING` in `src-tauri/src/orchestration/mod.rs`.
 *  Kept as one named constant, rather than the `12` the form field already
 *  hardcoded, so the capacity advisory below can reason about "the most this
 *  cap could ever reach" instead of silently assuming the recommendation
 *  always fits under whatever the input's `max` happens to be. */
export const MAX_AGENTS_CEILING = 12;

/** #255: the agent-capacity a declared workflow needs, mirrored from the
 *  backend's `recommend_capacity` (`orch_workflow_preview` / the
 *  `workflow-loaded` audit record) so the launcher's warning can never say
 *  something the engine wouldn't compute the same way. */
export interface CapacityRecommendation {
  /** What one review round costs without evicting anything already live: the
   *  gate's reviewer requirement plus one worker slot. */
  minimum: number;
  /** What running every declared tier concurrently costs. May exceed
   *  {@link MAX_AGENTS_CEILING} — a workflow file is not bounded by it, only
   *  `max_agents` is. */
  recommended: number;
  /** The gate's reviewer requirement alone (see `WorkflowPreview.reviewers_needed`). */
  reviewersNeeded: number;
  /** Which declared tiers `recommended` adds over `minimum` (see
   *  `WorkflowPreview.extra_tiers`). */
  extraTiers: string[];
}

/** A launcher per-role pick: the CLI and model the form collected for a class. */
export interface RolePick {
  key: OrchRole;
  cli: string;
  model: string;
}

/** What the group will run, and why.
 *
 *  - `builtin`   — the toggle is off. Today's four roles; the file (if any) is
 *                  not read. THE DEFAULT.
 *  - `declared`  — the toggle is on and the repo's workflow file resolved.
 *  - `none`      — the toggle is on but the repo declares no workflow. A no-op,
 *                  not an error: it is how you launch before you write the file.
 *  - `invalid`   — the toggle is on and the file is broken. The group still
 *                  launches, on the built-in roster (a repo file may never stop a
 *                  group from starting) — so this is a warning, never a blocker.
 */
export type RosterStatus = "builtin" | "declared" | "none" | "invalid";

export interface ResolvedRoster {
  status: RosterStatus;
  /** The blocks that will run, whatever the status — so a caller can always just
   *  render this. For every status but `declared` it is the built-in four. */
  blocks: RosterBlock[];
  /** Validation findings to surface. Non-empty only for `invalid`. */
  errors: string[];
  /** One line for the human, stating what will happen — including the fallback. */
  summary: string;
  /** #255: non-null only for `declared` — the built-in four have no gate to
   *  derive a capacity recommendation from. */
  capacity: CapacityRecommendation | null;
}

/** The built-in roster the launcher's per-role picks describe: the four classes,
 *  each block id equal to its class name, no personas. This IS what loomux has
 *  always run — `default_roster` in the backend synthesizes exactly this — so the
 *  toggle-off preview isn't a mock-up of the default, it is the default.
 *
 *  `groupCli` fills in for a pick with no CLI of its own (the form seeds every
 *  role from the group default, but a caller need not). */
export function builtinRoster(picks: readonly RolePick[], groupCli: string): RosterBlock[] {
  const byKey = new Map(picks.map((p) => [p.key, p]));
  return ORCH_ROLES.map(({ key, label }) => {
    const pick = byKey.get(key);
    return {
      id: key,
      name: label,
      kind: key,
      cli: pick?.cli?.trim() || groupCli,
      model: pick?.model?.trim() ?? "",
      persona: "none" as const,
    };
  });
}

/** Resolve what a launch would run. `preview` is null when it hasn't been fetched
 *  (or the fetch failed) — treated as "we don't know what's in the repo", which
 *  with the toggle off is not a question anyone asked. */
export function resolveRoster(
  advanced: boolean,
  preview: WorkflowPreview | null,
  picks: readonly RolePick[],
  groupCli: string
): ResolvedRoster {
  const builtin = builtinRoster(picks, groupCli);
  if (!advanced) {
    return {
      status: "builtin",
      blocks: builtin,
      errors: [],
      // Say the file is being ignored only when there IS one — otherwise this
      // line would advertise a feature by describing a file the user has never
      // heard of and does not have.
      summary:
        preview?.present === true
          ? `Standard roster — ${preview.path} is present but will not be used.`
          : "Standard roster — orchestrator, worker, reviewer, planner.",
      capacity: null,
    };
  }
  if (!preview || !preview.present) {
    return {
      status: "none",
      blocks: builtin,
      errors: [],
      summary: `No ${preview?.path ?? ".loomux/workflow.yml"} in this repo — the standard roster will run. Create one to declare your own blocks.`,
      capacity: null,
    };
  }
  if (!preview.valid) {
    return {
      status: "invalid",
      blocks: builtin,
      errors: preview.errors,
      // NOT a blocker, and the wording must not imply one: the backend audits a
      // broken file and falls back, precisely so a repo file can never stop a
      // group from launching.
      summary: `${preview.path} has ${preview.errors.length === 1 ? "an error" : `${preview.errors.length} errors`} and will be skipped — the standard roster will run instead.`,
      capacity: null,
    };
  }
  return {
    status: "declared",
    blocks: preview.blocks,
    errors: [],
    summary: `${preview.name || preview.path} — ${describeRoster(preview.blocks)}${
      preview.gates.length ? `, gated on ${preview.gates.join(", ")}` : ""
    }.`,
    capacity:
      preview.min_agents != null && preview.recommended_agents != null && preview.reviewers_needed != null
        ? {
            minimum: preview.min_agents,
            recommended: preview.recommended_agents,
            reviewersNeeded: preview.reviewers_needed,
            extraTiers: preview.extra_tiers ?? [],
          }
        : null,
  };
}

/** "1 worker, 2 reviewers" — the delegate counts, orchestrator excluded (every
 *  group has exactly one and it is not a choice the roster makes). */
export function describeRoster(blocks: readonly RosterBlock[]): string {
  const parts: string[] = [];
  for (const { key, label } of ORCH_ROLES) {
    if (key === "orchestrator") continue;
    const n = blocks.filter((b) => b.kind === key).length;
    if (n) parts.push(`${n} ${label.toLowerCase()}${n > 1 ? "s" : ""}`);
  }
  return parts.length ? parts.join(", ") : "no delegates";
}

/** The one-line description of a block for the roster table: what it is and what
 *  it will run. A persona is called out because it is the part the human is
 *  really being asked to consent to — repo-authored text that becomes an agent's
 *  instructions. */
export function describeBlock(b: RosterBlock): string {
  const persona =
    b.persona === "profile"
      ? " · repo persona (file)"
      : b.persona === "prompt"
        ? " · repo persona"
        : "";
  return `${b.kind} · ${b.cli} · ${b.model || "default model"}${persona}`;
}

/** Whether the roster is worth showing the human before they launch. The built-in
 *  four are what they already expect; anything else is a change they should see. */
export function rosterNeedsReview(r: ResolvedRoster): boolean {
  return r.status !== "builtin";
}

/** English-join a short list of noun phrases: `"a"`, `"a and b"`, `"a, b, and
 *  c"` — mirrors the backend's `workflow::join_with_and` so `extraTiers` reads
 *  the same sentence wherever it's rendered. */
export function joinWithAnd(parts: readonly string[]): string {
  if (parts.length === 0) return "";
  if (parts.length === 1) return parts[0];
  if (parts.length === 2) return `${parts[0]} and ${parts[1]}`;
  return `${parts.slice(0, -1).join(", ")}, and ${parts[parts.length - 1]}`;
}

/** The "Raise to N" the launcher can actually offer: `recommended`, clamped to
 *  {@link MAX_AGENTS_CEILING}. A workflow's structural need isn't bounded by
 *  the ceiling, but the `max_agents` field is — offering a number the field
 *  (and `clamped()` at Create) would silently clip is offering a fix that
 *  doesn't land (#255 rev-1 NB2). `null` when there's nothing to raise to. */
export function capacityRaiseTarget(r: ResolvedRoster): number | null {
  return r.capacity ? Math.min(r.capacity.recommended, MAX_AGENTS_CEILING) : null;
}

/** #255: the launcher's advisory. Two tiers, matched to the backend's two
 *  audit records:
 *
 *  - `maxAgents < minimum` — HARD: not even one review round fits without
 *    evicting a live agent.
 *  - `minimum <= maxAgents < recommended` — SOFT: every review round
 *    completes, but named tiers (an extra worker lane, extra reviewers, the
 *    planner) can never be live *alongside* one. This is the #255 incident's
 *    own boundary — `max_agents == minimum` there, which the hard-only check
 *    rev-1 of this PR's review caught as silent on the exact run that thrashed.
 *
 *  `null` for a `builtin`/`none`/`invalid` roster (no gate to derive anything
 *  from) and whenever `maxAgents` already covers `recommended` — fully quiet
 *  there, matching the backend's `max-agents-below-recommended` boundary.
 *
 *  Advisory only: this never touches `maxAgents` itself, it only describes why
 *  raising it (the #56 on-the-fly cap, or just the number on this form before
 *  Create) would help. */
export function capacityWarning(r: ResolvedRoster, maxAgents: number): string | null {
  if (!r.capacity || maxAgents >= r.capacity.recommended) return null;
  const { minimum, recommended, reviewersNeeded, extraTiers } = r.capacity;
  // The gate's OWN requirement, never a recount of reviewer BLOCKS (rev-1 B1) —
  // the worker count is safe to read off the blocks, since `minimum`'s worker
  // slot is derived the same "any worker block at all" way on both sides.
  const workers = r.blocks.filter((b) => b.kind === "worker").length;
  const reviewerPart =
    reviewersNeeded > 0 ? `${reviewersNeeded} reviewer${reviewersNeeded > 1 ? "s" : ""}` : "its reviewers";
  const workerPart = workers > 0 ? " + a worker" : "";
  const target = capacityRaiseTarget(r)!;
  const overCeiling =
    recommended > MAX_AGENTS_CEILING
      ? ` (this workflow's full roster needs ${recommended}, above loomux's ${MAX_AGENTS_CEILING}-agent ` +
        `limit — ${MAX_AGENTS_CEILING} is as high as this cap can go)`
      : "";

  if (maxAgents < minimum) {
    return (
      `This workflow's merge gate needs ${reviewerPart}${workerPart} (minimum ${minimum} live agents) to run ` +
      `one review round without evicting a live agent — max_agents is ${maxAgents}. Raise it to at least ` +
      `${minimum}, or ${target} to run every declared tier at once${overCeiling}.`
    );
  }
  const extras = extraTiers.length ? joinWithAnd(extraTiers) : "some of its declared tiers";
  return (
    `This workflow's full roster needs ${recommended} live agents to run every declared tier at once — ` +
    `max_agents is ${maxAgents}, which covers one review round but not the rest, so ${extras} can never be ` +
    `live alongside a review round. Raise it to ${target}${overCeiling}.`
  );
}

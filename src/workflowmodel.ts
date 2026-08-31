// Pure model for the repo's `workflow.yml` — the user-defined agent workflow (#222).
// DOM-free and I/O-free: parse, validate, derive the graph, serialize. The pane
// (workflowview.ts) is a VIEW over this; the FILE is the source of truth (the
// Kestra pattern — a form edit rewrites the YAML, it does not become a second,
// competing copy of it).
//
// Three rules this module exists to keep, each earned from a documented failure of
// some other workflow tool (see the #222 investigation, §1a-v and §4):
//
//  1. `id` is the identity; `name` is display only. n8n keys its graph by the node's
//     DISPLAY NAME, so a rename silently breaks every edge and expression pointing at
//     it. Here an edge/gate names an `id`, ids are immutable once created, and a rename
//     touches nothing else.
//  2. No coordinates, ever. Dify/ComfyUI/Langflow all embed x/y in the semantic file,
//     so nudging a node churns the logic diff. Layout (if the view ever draws any) goes
//     in the `workflow.layout.json` beside it; this file is the workflow.
//  3. Validate BEFORE a run, not during one. Flowise, Langflow and Dify discover a
//     dangling reference at runtime; Dify will happily *publish* a workflow whose node
//     isn't installed. `validateWorkflow` is the whole pre-run pass, and it is pure
//     precisely so it is unit-tested without a DOM (test/workflowmodel.test.ts).
//
// A fourth rule is about how we FAIL: an unknown kind, an unknown CLI, a dangling edge
// — none of them stop the file from opening. They become findings, the block renders as
// a stub, and the human can fix it in the pane. Refusing to open a file you can't fully
// understand is ComfyUI's #1 import-failure class, and it is the one behavior guaranteed
// to leave someone stranded with no way to repair the thing that is broken.
//
// The YAML subset is hand-rolled rather than pulled from npm: the schema is small and
// CLOSED (block mappings, block sequences, flow seqs/maps, block scalars, comments,
// quoted scalars), and the alternative is a dependency in a project that has spent real
// effort keeping its dependency list short. Anything the subset can't read is a parse
// finding on a line number — the raw-text view still opens, so the file is still fixable.

// The one import, and it is TYPE-only: the capability answer is handed in by the
// caller (see `KnobLookup`), never imported, because this module mirrors no
// vendor fact of its own — see `validateWorkflow`'s knob pass.
import type { KnobStates } from "./selectorknobs";

// ---------- the closed enums ----------

/** The capability classes. CLOSED, deliberately (#222 §2c): a workflow file may define
 *  unlimited personas but may never invent a capability — `kind` picks one of these
 *  and inherits its structural guarantees (a planner gets no worktree and no write tools;
 *  a reviewer may `gh pr review` but can never push). A repo file that could grant itself
 *  write access would be a footgun with `auto_ops` on and nobody watching.
 *
 *  Order mirrors the backend's `kind_names()` (workflow.rs), which is also the order
 *  `src/workflow-schema.json` declares — the two are compared as ordered arrays by
 *  `the_workflow_schema_manifest_matches_the_engines_values_defaults_and_bounds`.
 *
 *  `manager` (#1161) is a class the workflow file may name and `spawn_agent` may not:
 *  it is the human's own interface pane, opened for them rather than spawned by the
 *  orchestrator, and at most one may be declared. */
export const BLOCK_KINDS = ["orchestrator", "worker", "reviewer", "planner", "manager"] as const;
export type BlockKind = (typeof BLOCK_KINDS)[number];

export function isBlockKind(v: string): v is BlockKind {
  return (BLOCK_KINDS as readonly string[]).includes(v);
}

/** The agent CLIs a block may run. Mirrors the backend's `SUPPORTED_CLIS`
 *  (crates/loomux-engine/src/model.rs) — a block naming anything else is a
 *  finding, not a spawn.
 *
 *  `gemini` (#267 stage 2) and `opencode` (#722) are what make "run one reviewer
 *  on a different CLI/model than the worker" buy a genuinely different model
 *  family rather than a second Claude-family opinion — opencode most of all,
 *  since its model ids name a provider (`opencode/deepseek-v4-flash-free`) and
 *  reach a catalog no other adapter here can. Which CLIs can host which *kinds*
 *  is a separate question the backend owns (`CLI_CAPS` / `cli_can_host`):
 *  membership here is spawnability, not capability. */
export const WORKFLOW_CLIS = ["claude", "copilot", "gemini", "opencode"] as const;
export type WorkflowCli = (typeof WORKFLOW_CLIS)[number];

export function isWorkflowCli(v: string): v is WorkflowCli {
  return (WORKFLOW_CLIS as readonly string[]).includes(v);
}

/** The role hints a block may declare (#250/#324, #891) — an OPTIONAL persona/
 *  template/badge marker. `kind` alone decides the deny-flags and the cwd rule;
 *  the MCP tool scope has a short enumerated list of hint-keyed exceptions
 *  (`doc/design/liaison.md`): two narrowing — `session_digest` to `process`,
 *  `review_verdict` away from `liaison` — and TWO widening toward that same
 *  `liaison`, both otherwise orchestrator-only: `group_usage`, and `ask_human`
 *  (the pose only — nothing answers on the human's behalf, and
 *  `withdraw_question` is deliberately not widened with it).
 *
 *  `liaison` is SUPERSEDED by `kind: manager`, the first-class human-interface
 *  class. It still parses and still runs, unchanged, and the exceptions above
 *  are still exactly what it gets; a manager is a capability class of its own
 *  with its own tools and its own structural rules. `validateWorkflow` warns on
 *  one so a file's author is told where the feature moved.
 *  Mirrors the backend's `role_hint_requires` (workflow.rs) so this pane's
 *  pre-run pass agrees with what the real parser would say. Each hint REQUIRES a
 *  specific `kind` — `advisor` needs `planner`, `process` needs `worker`,
 *  `liaison` needs `reviewer` — so a workflow can't spell a combination nothing
 *  downstream would honor. */
export const ROLE_HINTS = ["advisor", "process", "liaison"] as const;
export type RoleHint = (typeof ROLE_HINTS)[number];

/** The capability class a role_hint REQUIRES, or `undefined` for an unrecognized
 *  value — the caller turns that into a `role-hint-unknown` finding, the same
 *  "reject, never coerce" shape `isBlockKind` uses for `kind`.
 *
 *  Trims and lowercases before comparing, to match the backend's
 *  `role_hint_requires` (`raw.trim().to_ascii_lowercase()`, workflow.rs) — a file
 *  with `role_hint: Advisor` parses clean on the real engine, and this pane's
 *  pre-run validation pass must not disagree and flag it as unknown. */
export function roleHintRequires(hint: string): BlockKind | undefined {
  const h = hint.trim().toLowerCase();
  if (h === "advisor") return "planner";
  if (h === "process") return "worker";
  if (h === "liaison") return "reviewer";
  return undefined;
}

/** The role hints a block of THIS kind may legally declare — derived from
 *  {@link ROLE_HINTS} and {@link roleHintRequires}, never listed a second time.
 *
 *  That derivation is the whole point (#1020): the form that offers these must not
 *  be able to spell something the parser rejects, and the only way to guarantee it
 *  is for the offer and the rule to be the SAME statement. A hint added to
 *  `ROLE_HINTS` (with its pairing in `roleHintRequires`) shows up here — and so in
 *  the pane — with no edit at all; a hardcoded picker would silently keep offering
 *  the old two while `validateWorkflow` had moved on. */
export function roleHintsForKind(kind: string): RoleHint[] {
  return ROLE_HINTS.filter((h) => roleHintRequires(h) === kind);
}

/** Why a block of this kind may NOT declare a repo-authored PERSONA
 *  (`prompt:` / `profile:`), or `null` when it may — the mirror of
 *  `workflow::persona_allowed` and of `parse_workflow`'s refusal.
 *
 *  Separate from {@link allowDenialReason} because the two rules are not
 *  co-extensive and never were: a PLANNER may carry a persona and may not
 *  pre-approve tools, so folding them into one predicate would either ban a
 *  planner's persona or permit an orchestrator's. Two loomux-owned classes
 *  answer non-null here (#222 for the orchestrator, #1161 D1 for the manager);
 *  the argument for each is in `parse_workflow`.
 *
 *  Before #1161 the pane mirrored only the `allow:` half, so a workflow the
 *  engine refuses OUTRIGHT — persona on the trust root — could be authored in
 *  the pane, saved, and reported as clean; the launch then fell back to the
 *  built-in roster with no finding to explain why. Fail-closed, but silent, and
 *  `kind: manager` made it newly reachable through the kind picker. */
export function personaDenialReason(kind: string): string | null {
  if (kind === "orchestrator") {
    return (
      "the orchestrator is orrerix's trust root, and a repo file may not author its prompt — " +
      "put personas on the blocks it spawns"
    );
  }
  if (kind === "manager") {
    return (
      "a manager speaks to the human and relays their direction into the trust root, so a repo " +
      "file may not author its persona — put personas on the blocks the orchestrator spawns"
    );
  }
  return null;
}

/** Why a block of this kind may NOT declare `allow:`, or `null` when it may.
 *
 *  Mirrors the three REFUSALS in `parse_workflow` (workflow.rs), which are separate
 *  rules with separate reasons and are stated here as one answer so the pane's form
 *  and its validation pass cannot disagree about them:
 *
 *   - an ORCHESTRATOR block may not declare `prompt:`/`profile:`/`allow:` at all —
 *     it is the group's trust root, and a repo file that could pre-approve its
 *     tools would be a prompt-injection seam into the one agent running
 *     unsupervised;
 *   - a MANAGER block may not either (#1161, decision D1) — its whole output surface is
 *     persuading the human and relaying their direction into that same trust root, so a
 *     repo-authored persona there would launder the repo's own instructions into what the
 *     human is told;
 *   - a READ-ONLY class (today: `planner`, via `Role::containment`) may not, because
 *     `allow: Bash(python *)` hands it a shell that writes files while naming
 *     nothing on the deny list. Reviewers and workers keep `allow:` — a reviewer has
 *     its shell by design (running the tests is the job).
 *
 *  An UNRECOGNIZED kind answers `null`: `unknown-kind` already says what is wrong
 *  with that block, and stacking a second finding on top of it explains nothing. */
export function allowDenialReason(kind: string): string | null {
  if (kind === "orchestrator") {
    return (
      "the orchestrator is orrerix's trust root, and a repo file may not pre-approve its tools — " +
      "put personas and allow: patterns on the blocks it spawns"
    );
  }
  if (kind === "manager") {
    return (
      "a manager speaks to the human and relays their direction into the trust root, so a repo " +
      "file may not author its persona or pre-approve its tools — put personas and allow: " +
      "patterns on the blocks the orchestrator spawns"
    );
  }
  if (kind === "planner") {
    return (
      "a planner's class is read-only, and a pre-approved tool pattern could hand it a shell " +
      "that writes files — move the work to a worker block"
    );
  }
  return null;
}

/** Longest `remote:` label the engine accepts — `pathseg::MAX_SEGMENT_LEN`
 *  (crates/loomux-engine/src/pathseg.rs), the shared #925 identifier cap the
 *  engine validates a remote label with.
 *
 *  A hand-written mirror, like {@link WORKFLOW_CLIS}, and safe for the same
 *  reason — **engine -> manifest -> pane, with no step left to assumption**:
 *  `workflow_schema_field_facts()` states `block.remote.maxLength` from
 *  `MAX_SEGMENT_LEN` itself,
 *  `the_workflow_schema_manifest_matches_the_engines_values_defaults_and_bounds`
 *  pins `src/workflow-schema.json` against that fact in both directions, and
 *  `test/workflowschema.test.ts` pins THIS constant against the manifest row.
 *  Raise `MAX_SEGMENT_LEN` and the Rust test reddens; change this number alone
 *  and the pane test does.
 *
 *  It was NOT in that chain when the key shipped (#1457 review B3): the engine
 *  stated no fact for `block.remote`, so the manifest declared none, so the
 *  bidirectional pin had nothing to compare and this constant was pinned to
 *  nothing while a comment here said otherwise. Both sides tested their cap
 *  against their own constant, which is green and tautological at once. */
export const REMOTE_LABEL_MAX = 64;

/** Whether a `remote:` label is one the engine will accept — the pane's mirror of
 *  `pathseg::check_segment`, which `parse_workflow` validates the key with (#1457).
 *
 *  `[A-Za-z0-9_-]`, non-empty, at most {@link REMOTE_LABEL_MAX}, no leading `-`
 *  (a bare `-foo` is an OPTION to any command line the label is interpolated
 *  into), and no Windows reserved device name. REFUSED, never rewritten: two
 *  spellings must not be able to name one operator binding.
 *
 *  Not the same predicate as an id's: `sanitize_id` REWRITES, which is exactly
 *  what a label may not do. */
export function isRemoteLabel(label: string): boolean {
  if (!label || label.length > REMOTE_LABEL_MAX) return false;
  if (!/^[A-Za-z0-9_-]+$/.test(label)) return false;
  if (label.startsWith("-")) return false;
  const stem = (label.split(".")[0] ?? "").toUpperCase();
  return !/^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])$/.test(stem);
}

/** Why a block of this kind may NOT declare `remote:`, or `null` when it may
 *  (#1457) — the mirror of `parse_workflow`'s refusal.
 *
 *  The same two loomux-owned classes {@link personaDenialReason} answers for, and
 *  for a related but distinct reason: those two blocks are load-bearing LOCALLY.
 *  The orchestrator is the trust root and holds orchestration state, the `gh`
 *  operations and the merge gate; the manager pane is the human's own interface —
 *  the thing they type into. Neither is a remote-execution feature with a missing
 *  implementation.
 *
 *  An UNRECOGNIZED kind answers `null`, exactly like {@link allowDenialReason}:
 *  `unknown-kind` already says what is wrong with that block. */
export function remoteDenialReason(kind: string): string | null {
  if (kind === "orchestrator") {
    return (
      "the orchestrator is orrerix's trust root — orchestration state, the gh operations and " +
      "the merge gate stay on this machine — so put remote: on the blocks it spawns"
    );
  }
  if (kind === "manager") {
    return (
      "a manager pane is the human's own interface, the thing they type into, so it runs where " +
      "they are — put remote: on the blocks the orchestrator spawns"
    );
  }
  return null;
}

/** What the engine will actually apply for one `allow:` entry, or `null` when it
 *  drops the entry entirely. Mirrors `sanitize_allow` (profiles.rs): everything
 *  outside its alphabet is FILTERED OUT — silently, on the way to the CLI's
 *  `--allowedTools` / `--allow-tool` flag — so a pattern carrying a `$`, a `|` or a
 *  quote reaches the agent as a different pattern than the one in the file, and the
 *  human never hears about it. The pane says so instead (`allow-sanitized`). */
export function sanitizeAllowPattern(pattern: string): string | null {
  const cleaned = pattern
    .trim()
    .replace(/[^A-Za-z0-9():*_\-. ,/]/g, "")
    .trim();
  return cleaned || null;
}

/** Does this block actually REVIEW PRs? — reviewer-kind, minus the liaison
 *  (#891). Mirrors the backend's `is_reviewing_block` (workflow.rs), which the
 *  `{{REVIEWERS}}` fan-out, a reviewer's "one of N" lane and the class-default
 *  block resolution all ask.
 *
 *  `kind === "reviewer"` answers "which capability class does it ride", and that
 *  stops being the same question once a hint SUBTRACTS from its class: a liaison
 *  rides the reviewer posture and reviews nothing, is denied `review_verdict`,
 *  and is refused outright by `validateWorkflow` when a merge gate names it. So
 *  anywhere this pane means "the blocks that can satisfy a gate" — offering the
 *  gate's reviewer checkboxes, filling them in when the gate is switched on —
 *  asks THIS, or the editor writes a file its own validator immediately flags. */
export function isReviewingBlock(b: { kind?: string; role_hint?: string }): boolean {
  return b.kind === "reviewer" && b.role_hint?.trim().toLowerCase() !== "liaison";
}

/** The schema version this build reads and writes. */
export const WORKFLOW_VERSION = 1;

/** The repo's committed orrerix config dir, relative to the repo root. */
export const CONFIG_DIR = ".orrerix";

/** The pre-#1153 spelling of {@link CONFIG_DIR}, still read when `.orrerix/` is absent.
 *  NEVER renamed on the user's behalf — it is a tracked directory in their repository.
 *  See `doc/design/rebrand-filesystem.md`. */
export const LEGACY_CONFIG_DIR = ".loomux";

/** Where the workflow lives, relative to the repo root. */
export const WORKFLOW_FILE = `${CONFIG_DIR}/workflow.yml`;

/** The legacy location, tried when {@link WORKFLOW_FILE} is not there. */
export const LEGACY_WORKFLOW_FILE = `${LEGACY_CONFIG_DIR}/workflow.yml`;

/** After reading `tried` came back NOT-FOUND, the next path worth trying — or null when
 *  there is nothing left to try.
 *
 *  Deliberately narrow: only the DEFAULT workflow path falls back. A pane opened on an
 *  explicit file (the editor's `getFile`, a restored tab) is showing the file it was asked
 *  to show, and silently opening a different one because that one is missing would be the
 *  pane lying about what it has open. And the fallback happens exactly once — `tried` is
 *  the legacy path on the second call, which returns null — so a repo with neither file
 *  lands on the "no workflow yet" empty state after two reads, not a loop. */
export function legacyFallbackFor(tried: string): string | null {
  return tried === WORKFLOW_FILE ? LEGACY_WORKFLOW_FILE : null;
}

/** What a `merge` gate can require of its reviewers. `all-pass` = every named reviewer
 *  recorded PASS; `threshold` = at least N of them did. These are the CANONICAL
 *  spellings — what the pane offers and what it writes. */
export const GATE_REQUIRES = ["all-pass", "threshold"] as const;
export type GateRequire = (typeof GATE_REQUIRES)[number];

/** What the ENGINE accepts, which is a superset: `all` is a synonym for `all-pass` in
 *  `parse_workflow`'s gate match (workflow.rs). Validation reads THIS list, so a
 *  hand-written file using the synonym stops being flagged as an error it never was —
 *  a pane that refuses a file the engine loads is telling the same class of lie as one
 *  that blesses a file the engine refuses, just in the other direction (#880 review).
 *  The picker still offers `GATE_REQUIRES` only: there is no reason to offer a human
 *  two spellings of one thing. */
export const GATE_REQUIRES_ACCEPTED = ["all-pass", "all", "threshold"] as const;

/** Where autonomous work comes from (#382 P1). Mirrors the engine's
 *  `intake_source_from_str` (workflow.rs). An EMPTY `source:` is legal and means the
 *  built-in default, which is why "" is accepted by {@link isIntakeSource} but is not
 *  offered as a value: "inherit" is a different statement from "github-labels", and a
 *  picker that spelled it out would PIN what the file meant to inherit. */
export const INTAKE_SOURCES = ["github-labels", "board", "none"] as const;
export type IntakeSourceName = (typeof INTAKE_SOURCES)[number];

export function isIntakeSource(v: string): boolean {
  const s = v.trim().toLowerCase();
  return s === "" || (INTAKE_SOURCES as readonly string[]).includes(s);
}

/** How long an identifier the engine's `sanitize_id` will carry (`MAX_ID_CHARS`,
 *  workflow.rs). Longer is not truncated — it is REJECTED, because a resource called
 *  something the author didn't write is worse than one that fails to load. */
export const ID_MAX_CHARS = 48;

/** A label the intake profile may name. The engine rejects rather than rewrites
 *  (`sanitize_intake_label`): a label the repo's own GitHub labels no longer match is
 *  a silent no-op, so a space or a `#` is a hard error there and a finding here.
 *  A LEADING `-` is banned on top of the id alphabet — the hold spelling becomes a
 *  positional argument to `gh label create`, and a dash-leading positional is read as
 *  a flag. Empty is not a rejection: it means "inherit this one". */
export function isValidIntakeLabel(v: string): boolean {
  const s = v.trim();
  if (!s) return true;
  return s.length <= ID_MAX_CHARS && /^[A-Za-z0-9_][A-Za-z0-9_-]*$/.test(s);
}

/** A resource name (#858). Same alphabet as a block id at the engine
 *  (`sanitize_id`), and — like every other author-written identifier in that file —
 *  rejected rather than rewritten, so `heavy build` never becomes a resource called
 *  `heavybuild` that the author's own `acquire_lock` call cannot name. Unlike an
 *  intake label, a leading `-` is fine: nothing puts a resource name in an argv. */
export function isValidResourceName(v: string): boolean {
  const s = v.trim();
  return !!s && s.length <= ID_MAX_CHARS && /^[A-Za-z0-9_-]+$/.test(s);
}

/** The bounds `parse_workflow` enforces on the policy sections — mirrored here so the
 *  pane's forms cannot write a file the engine then refuses to load, and so a
 *  hand-written file that already carries one gets a finding rather than a clean bill
 *  of health. Every one of these is a REFUSAL on the engine (`RESOURCE_SLOTS_MAX`,
 *  `RESOURCE_MAX_HOLD_MINUTES_MAX`, `RESOURCES_MAX`, and `max_batch`'s floor of 1),
 *  with the single exception noted on the checks-timeout pair below. */
export const RESOURCE_SLOTS_MIN = 1;
export const RESOURCE_SLOTS_MAX = 64;
export const RESOURCE_MAX_HOLD_MINUTES_MIN = 1;
export const RESOURCE_MAX_HOLD_MINUTES_MAX = 480;
export const RESOURCES_MAX = 32;
export const MERGE_QUEUE_MAX_BATCH_MIN = 1;
/** A threshold gate needs at least one passing review — `fact("gate.threshold", "min", 1)`
 *  on the engine, and the floor `validateWorkflow` has always enforced. */
export const GATE_THRESHOLD_MIN = 1;
/** The small-batch clause's floor — `fact("gate.max_diff_lines", "min", 1)` on the
 *  engine. `0` is refused rather than read as "unlimited": the way to mean no limit is
 *  to omit the key, and a bound that bounds nothing is a typo (#1174). */
export const GATE_MAX_DIFF_LINES_MIN = 1;
/** #1176's caps — `fact("gate.routing", "max_entries", …)` and
 *  `fact("gate.routing.paths", "max_entries", …)` on the engine. Bounds on work the
 *  `gh` shim does on the merge path (every rule against every changed file), not on
 *  what this form can render. */
export const GATE_ROUTING_RULES_MAX = 32;
export const GATE_ROUTING_PATHS_MAX = 32;
/** Longest path glob the engine accepts (`MAX_GLOB_CHARS`) — past it `sanitize_glob`
 *  truncates, which the parse reads as a glob it had to change, which is a refusal. */
export const ROUTING_GLOB_MAX_CHARS = 200;

/** Is this a path glob the engine would accept — `sanitize_glob`, mirrored.
 *
 *  The alphabet is what a POSIX `case` pattern can carry with `*` as its ONLY
 *  metacharacter: no `[`, `\\`, `?`, brace, quote or space. The three refused shapes
 *  are the ones that could never fire against a repo-relative changed path, and a
 *  rule that never fires silently drops a reviewer the repo asked for. */
export function isRoutingGlob(p: string): boolean {
  return (
    p.length > 0 &&
    p.length <= ROUTING_GLOB_MAX_CHARS &&
    /^[A-Za-z0-9._\-/*]+$/.test(p) &&
    !p.startsWith("/") &&
    !p.endsWith("/") &&
    !p.split("/").includes("..")
  );
}
/** `checks_timeout_minutes` rides the notify-TTL clamp (`clamp_expires_minutes`) -
 *  one of the policy numbers the engine CLAMPS rather than refuses (the driver's
 *  three backstops are the others, #1778) - so a value outside this range is a
 *  warning here, not an error: the file loads, it just doesn't do what it says. */
export const MERGE_QUEUE_CHECKS_TIMEOUT_MIN = 5;
export const MERGE_QUEUE_CHECKS_TIMEOUT_MAX = 240;
/** INVARIANT 9's counters (#1778 §2.3) - the engine REFUSES values outside these
 *  closed ranges rather than clamping: a repo file may run a tighter review loop
 *  than the orchestrator template promises, never a looser one. */
export const DRIVER_MAX_REVIEW_ROUNDS_MIN = 1;
export const DRIVER_MAX_REVIEW_ROUNDS_MAX = 3;
export const DRIVER_MAX_CI_ATTEMPTS_MIN = 1;
export const DRIVER_MAX_CI_ATTEMPTS_MAX = 3;
export const DRIVER_MAX_REBASE_ATTEMPTS_MIN = 0;
export const DRIVER_MAX_REBASE_ATTEMPTS_MAX = 1;
/** The driver's three backstops ride the same notify-TTL clamp family the merge
 *  queue's checks timeout rides (`clamp_expires_minutes`) - CLAMPED, not refused:
 *  outside this range is a warning here, not an error. */
export const DRIVER_TIMEOUT_MIN = 5;
export const DRIVER_TIMEOUT_MAX = 240;

/** The board statuses a `board.wip:` cap may name (#1175), in board order — the pane's
 *  mirror of the engine's `RawWip` fields. `done` is deliberately absent: it is the
 *  relief valve every other cap depends on, so the engine has no field for it and
 *  `deny_unknown_fields` refuses one. */
export const WIP_STATUSES = [
  "queued",
  "in-progress",
  "review",
  "pr",
  "prototype",
  "human-testing",
  "blocked",
] as const;

/** `parse_workflow` refuses `0` — a cap of nothing is a stop, not a limit. */
export const WIP_LIMIT_MIN = 1;

/** One numeric field's range. `max` is OPTIONAL and its absence is a statement: the
 *  engine imposes no ceiling on that field, so neither may a form. */
export interface FieldBounds {
  min: number;
  max?: number;
}

/** Every bounded number a policy form writes, keyed by its manifest field id.
 *
 *  This table exists because a bound that lives as a literal at the point of use is a
 *  bound nothing can check (#1020 review, finding 2): `merge_queue.max_batch` was clamped
 *  to a hand-typed `64` in the form — a ceiling the engine does not impose and the
 *  manifest does not declare — so typing `100` silently wrote `64` into the file, and no
 *  test in the tree could see it. The table is the fix in kind rather than in degree: the
 *  forms read their bounds from HERE, `test/workflowschema.test.ts` pins every entry
 *  against `src/workflow-schema.json` (whose own `min`/`max` the Rust side pins against
 *  the engine's constants), and it pins in BOTH directions — a manifest bound missing from
 *  this table, and a table bound the manifest does not declare, both redden.
 *
 *  So "engine → manifest → pane, with no step left to assumption" is now enforced for the
 *  numbers rather than asserted about them: a `max` cannot enter a form without first
 *  existing in the engine. */
export const POLICY_BOUNDS: Readonly<Record<string, FieldBounds>> = {
  // No ceiling here either: a gate may name any number of reviewers, so `threshold` is
  // bounded above by the reviewer list rather than by a constant — which
  // `validateWorkflow` checks against the list itself, where the real answer is.
  "gate.threshold": { min: GATE_THRESHOLD_MIN },
  // No ceiling: "how big is too big" is the repo's call, and the engine invents no
  // upper bound for it either (#1174).
  "gate.max_diff_lines": { min: GATE_MAX_DIFF_LINES_MIN },
  // No ceiling, deliberately: `parse_workflow` refuses `max_batch: 0` and accepts every
  // integer above it, so the form must too.
  "merge_queue.max_batch": { min: MERGE_QUEUE_MAX_BATCH_MIN },
  "merge_queue.checks_timeout_minutes": {
    min: MERGE_QUEUE_CHECKS_TIMEOUT_MIN,
    max: MERGE_QUEUE_CHECKS_TIMEOUT_MAX,
  },
  "driver.max_review_rounds": {
    min: DRIVER_MAX_REVIEW_ROUNDS_MIN,
    max: DRIVER_MAX_REVIEW_ROUNDS_MAX,
  },
  "driver.max_ci_attempts": { min: DRIVER_MAX_CI_ATTEMPTS_MIN, max: DRIVER_MAX_CI_ATTEMPTS_MAX },
  "driver.max_rebase_attempts": {
    min: DRIVER_MAX_REBASE_ATTEMPTS_MIN,
    max: DRIVER_MAX_REBASE_ATTEMPTS_MAX,
  },
  "driver.lane_timeout_minutes": { min: DRIVER_TIMEOUT_MIN, max: DRIVER_TIMEOUT_MAX },
  "driver.fix_timeout_minutes": { min: DRIVER_TIMEOUT_MIN, max: DRIVER_TIMEOUT_MAX },
  "driver.drive_timeout_minutes": { min: DRIVER_TIMEOUT_MIN, max: DRIVER_TIMEOUT_MAX },
  "resource.slots": { min: RESOURCE_SLOTS_MIN, max: RESOURCE_SLOTS_MAX },
  "resource.max_hold_minutes": {
    min: RESOURCE_MAX_HOLD_MINUTES_MIN,
    max: RESOURCE_MAX_HOLD_MINUTES_MAX,
  },
  // No ceiling on any WIP cap, and one row per status rather than seven typed out:
  // a limit above the board's own size degenerates to "no limit", which is what the
  // author asked for, so there is nothing to refuse — and spreading `WIP_STATUSES`
  // means an eighth cappable status cannot arrive here bound-less.
  ...Object.fromEntries(
    WIP_STATUSES.map((s) => [`board.wip.${s}`, { min: WIP_LIMIT_MIN }] as const)
  ),
};

/** A legal block id: lowercase-ish, human-meaningful, safe as a filename fragment and as
 *  a shell-adjacent token. Deliberately strict — the id ends up in agent ids, pane names
 *  and (via the backend) command lines, and `sanitize_model` (mod.rs) is the precedent
 *  for keeping repo-authored strings out of a shell line. */
const BLOCK_ID_RE = /^[a-z][a-z0-9]*(?:[-_][a-z0-9]+)*$/;

export function isValidBlockId(id: string): boolean {
  return BLOCK_ID_RE.test(id);
}

// ---------- the schema ----------

/** Any value the YAML subset can hold. Blocks keep the keys they don't understand
 *  (`extra`) so a file written by a NEWER loomux survives a round-trip through an older
 *  pane instead of being silently stripped by it. */
export type YamlValue = string | number | boolean | null | YamlValue[] | { [k: string]: YamlValue };

/** One agent block: a persona (prompt or profile), a model, a CLI — and a `kind` that
 *  says which capability class it runs as.
 *
 *  `kind` and `cli` are typed as raw strings, not as the enums: a file naming
 *  `kind: superuser` must still LOAD (as a stub, with a finding) so the human can fix it
 *  in the pane. Narrowing them at the type level would force the parser to drop the very
 *  block the user needs to see. */
export interface WorkflowBlock {
  /** Immutable identity. Edges and gates reference THIS (never `name`). */
  id: string;
  /** Display label. Renaming it must never break a reference — that is its whole point. */
  name: string;
  /** One of BLOCK_KINDS; anything else is a finding + a stub. */
  kind: string;
  /** One of WORKFLOW_CLIS; anything else is a finding + a stub. */
  cli: string;
  /** Model to pin, or "" for the CLI's default. */
  model: string;
  /** Inline persona — compiled to `claude --agents '<json>'` (native, no file needed). */
  prompt?: string;
  /** Persona file — compiled to `copilot --agent <name>` against `.github/agents/`.
   *  Mutually exclusive with `prompt` (a block with both is a finding). */
  profile?: string;
  /** OPTIONAL persona/template marker (#250/#324, #891) — one of {@link ROLE_HINTS},
   *  or anything else (a finding + a stub, same as an unrecognized `kind`). Requires
   *  its matching `kind` (see {@link roleHintRequires}); absent is today's behavior,
   *  byte for byte. */
  role_hint?: string;
  /** Extra pre-approved tool patterns (`--allowedTools` / `--allow-tool`), e.g.
   *  `Bash(gh pr view --json title,body)`. A `RawBlock` field since #222 that this
   *  model never knew about until #880 — so a workflow that declared one showed up
   *  in the pane as a workflow that didn't. Quoted scalars matter here (the comma
   *  inside that pattern is CONTENT, not a separator); `emitScalar` already quotes
   *  for the flow context this emits into. */
  allow?: string[];
  /** Thinking level (#687) — one of the CLI's own `effort` values, or "" for its
   *  default. Which values exist, and whether the CLI has any seam for them at
   *  all, is capability data the BACKEND owns (`agent_cli_knobs`); this field is
   *  just the file's text, exactly like `cli` and `kind`. */
  effort?: string;
  /** Context-window variant (#687) — `1m`, or "" for the model's own window.
   *  Same ownership rule as {@link effort}. */
  context?: string;
  /** OPTIONAL remote LABEL (#1457) — the abstract name of a machine this block's
   *  agent runs on over SSH. A repo file SELECTS a label; the operator binds it
   *  to a host, an account and a remote clone path outside the repo, so a
   *  `host:`/`port:`/`identity_file:` key is not a field here — it is an unknown
   *  key, and an unknown key fails the whole file on the engine.
   *
   *  Absent is a local block, byte for byte. Declared, it is inert in this build:
   *  the operator binding (#1458) and the spawn path (#1459) are what make it do
   *  anything. */
  remote?: string;
  /** Keys this build doesn't know, preserved verbatim across a round-trip. */
  extra?: Record<string, YamlValue>;
}

/** One ADVISORY edge (#222 §2g): the declared happy path. The orchestrator still decides
 *  when to spawn what — a static DAG would replace its mergeability/parallelism judgment
 *  with something dumber. Edges document intent and drive the graph view; gates are the
 *  half that is actually enforced. */
export interface WorkflowEdge {
  from: string;
  to: string;
}

/** The ENFORCED half: a merge gate. The backend refuses `gh pr merge` (via the existing
 *  PATH shim) until the named reviewers' verdicts are recorded PASS — which is what makes
 *  multi-reviewer more than theatre, and closes the loomux side of #197. */
export interface MergeGate {
  require: string;
  /** Only meaningful when `require: threshold`. */
  threshold?: number;
  reviewers: string[];
  /** Extra conditions (`ci-green`, …) — passed through; the backend owns their meaning. */
  also: string[];
  /** The small-batch clause (#1174): the largest PR, in changed lines, the gate lets
   *  through. Absent = no limit, and absent is kept apart from any number here — a
   *  `0` this pane invented would be a file the engine refuses. */
  max_diff_lines?: number;
  /** Path-based reviewer routing (#1176). Absent (or empty) = no routing.
   *
   *  Read and emitted here even though this pane offers no editor for it yet, and
   *  that is the whole point: `MergeGate` has no unknown-key bag, so a key this
   *  model did not carry would be a rule the next form edit silently DELETED — and
   *  the thing deleted would be a required reviewer. */
  routing?: RoutingRule[];
}

/** One `gates.merge.routing[]` rule: if the PR changed a file matching any of
 *  `paths`, `reviewers` are required on top of the gate's own list. */
export interface RoutingRule {
  paths: string[];
  reviewers: string[];
}

export interface WorkflowGates {
  merge?: MergeGate;
  extra?: Record<string, YamlValue>;
}

/** Where autonomous work comes from (#382 P1). Every field is OPTIONAL and absent
 *  means "inherit loomux's built-in profile" — the engine resolves a partial block
 *  against `builtin_intake_profile()`, so this model must keep "not declared" and
 *  "declared empty" apart rather than filling defaults in on read (a save would
 *  otherwise write four label lines nobody asked for).
 *
 *  There is deliberately no field here that can weaken the human merge gate — that
 *  lives in the `gh` shim, and the engine's `deny_unknown_fields` makes an invented
 *  one a hard parse error rather than an ignored line. This model preserves unknown
 *  keys (`extra`) so a newer build's file survives a round-trip, and says so out
 *  loud (`unknown-key`) because this build's engine would refuse the whole file. */
export interface WorkflowIntake {
  source?: string;
  labels?: WorkflowIntakeLabels;
  extra?: Record<string, YamlValue>;
}

/** The label vocabulary intake matches on. Declare one, inherit the other four. */
export interface WorkflowIntakeLabels {
  ready?: string;
  investigate?: string;
  owned?: string;
  prototype?: string;
  /** The veto (#778): "held by the human — do not start this". The only label here
   *  whose meaning is opt-OUT, and the boundary full autonomy leaves standing. */
  hold?: string;
  extra?: Record<string, YamlValue>;
}

/** The bisecting merge queue's policy (#581 §11.2). Absent block = the feature is
 *  off; that is why every field is optional rather than defaulted here. */
export interface WorkflowMergeQueue {
  enabled?: boolean;
  max_batch?: number;
  checks_timeout_minutes?: number;
  extra?: Record<string, YamlValue>;
}

/** The review-loop driver's policy (#1778 §5.3). Absent block = the driver is
 *  off; every field is optional for the same reason `WorkflowMergeQueue`'s are. */
export interface WorkflowDriver {
  enabled?: boolean;
  max_review_rounds?: number;
  max_ci_attempts?: number;
  max_rebase_attempts?: number;
  lane_timeout_minutes?: number;
  fix_timeout_minutes?: number;
  drive_timeout_minutes?: number;
  extra?: Record<string, YamlValue>;
}

/** The driver defaults the pane RENDERS when the file omits a field - the
 *  engine's `DriverPolicy::default`, mirrored here because this module is pure
 *  and import-free. A literal at the point of use is a number nothing can
 *  check: `NOTIFY_EXPIRES_DEFAULT_MIN` moves, the manifest pin forces the JSON
 *  to follow, and a `?? 60` left behind renders a stale 60 with nothing red.
 *  So the chrome reads THIS table, and `test/workflowschema.test.ts` pins every
 *  entry against the manifest's declared default - engine → manifest → pane,
 *  with no unpinned step left (#1784 review, premortem 2). */
export const DRIVER_DEFAULTS: Readonly<{
  enabled: boolean;
  max_review_rounds: number;
  max_ci_attempts: number;
  max_rebase_attempts: number;
  lane_timeout_minutes: number;
  fix_timeout_minutes: number;
  drive_timeout_minutes: number;
}> = {
  enabled: false,
  max_review_rounds: 3,
  max_ci_attempts: 3,
  max_rebase_attempts: 1,
  lane_timeout_minutes: 60,
  fix_timeout_minutes: 60,
  drive_timeout_minutes: 240,
};

/** The driver form's enable-toggle write rule (#1869; narrowed by review round 3).
 *
 *  ON writes `{ enabled: true }` — or, when a block already stands (a hand-written
 *  `enabled: false` beside declared counters, or a block with no `enabled:` line at
 *  all), flips just `enabled` and leaves the rest of the human's lines alone: the
 *  merge-queue lesson in reverse (#1020 review, finding 4 — a form must not silently
 *  rewrite what it did not write).
 *
 *  OFF is a **data-loss rule**, so it deletes only what costs nothing to delete:
 *  a block that carries NOTHING BUT `enabled` — no counter, no unknown key, and no
 *  comment in the file's own prose about it (`commentsInSection`) — is removed
 *  whole, because absent and `enabled: false` are the same state to the engine and
 *  deleting is the tidier of the two. Anything more and OFF writes
 *  `enabled: false`, preserving the block: the checkbox reads the `enabled:` line,
 *  and two clicks on it must never delete configuration the human can see. The
 *  comment signal comes from the view, which holds the original text — the model
 *  cannot see comments, and `driverSectionHasComments` is how it asks. */
export function setDriverEnabled(
  w: Workflow,
  on: boolean,
  commentsInSection = false
): void {
  if (!on) {
    const d = w.driver;
    if (!d) return;
    const carriesMore =
      d.max_review_rounds !== undefined ||
      d.max_ci_attempts !== undefined ||
      d.max_rebase_attempts !== undefined ||
      d.lane_timeout_minutes !== undefined ||
      d.fix_timeout_minutes !== undefined ||
      d.drive_timeout_minutes !== undefined ||
      d.extra !== undefined;
    if (carriesMore || commentsInSection) {
      d.enabled = false;
      return;
    }
    delete w.driver;
    return;
  }
  if (w.driver) w.driver.enabled = true;
  else w.driver = { enabled: true };
}

/** Does the `driver:` section of this text carry any comment line — the comment
 *  introducing it, a comment inside the block, or a trailing one on the key line?
 *  `setDriverEnabled`'s OFF rule asks this because the MODEL cannot see comments:
 *  a block that is bare to the model may still be the subject of the file's own
 *  prose, and deleting the section would delete that prose with it. Reuses the
 *  preserving serializer's own splitter (#233) rather than a second scanner — a
 *  `#` inside a block scalar's body is content, and only that splitter knows where
 *  the bodies are. Conservative on a shape the scan refuses to read: `true`, so an
 *  unreadable file is never the one that gets a block deleted. */
export function driverSectionHasComments(text: string): boolean {
  const doc = splitDocument(text);
  if (!doc) return true;
  const entry = doc.entries.find((e) => e.key === "driver");
  if (!entry) return false;
  // A line "carries" a comment when stripping one changes it — a pure `# …` line
  // (strip → empty) AND a significant line with a trailing `# …` (`driver: # off`)
  // both count; the block scalar scan excludes bodies, where a `#` is content.
  const carriesComment = (l: string): boolean => l.trim() !== "" && stripComment(l) !== l;
  const scan = (lines: readonly string[]): boolean => {
    const opaque = opaqueScalarIndices(lines as string[]);
    return lines.some((l, i) => !opaque.has(i) && carriesComment(l));
  };
  return scan(entry.header) || scan(entry.content);
}

/** The driver form's checkbox READ rule (#1869 review round 1) — the pair of
 *  `setDriverEnabled` above. The checkbox shows the driver's enabled state, and the
 *  engine's answer is what the `enabled:` LINE says (`RawDriver.enabled` is
 *  `#[serde(default)] bool`), never the block's presence: a present `driver:` block
 *  without the line is OFF to the engine, which is exactly what the pre-form pane
 *  rendered ("not declared - off (orrerix's default)"). Reading presence instead of
 *  the line would show ON for a driver that will never run — the one case the write
 *  rule's tests cannot see, which is why this half is pinned separately. */
export function isDriverOn(w: Workflow): boolean {
  return w.driver?.enabled === true;
}

/** One named lock resource (#858) — how many agents may hold it at once and for
 *  how long. Two numbers, keyed by a name the repo chose; loomux never learns what
 *  the name means (CLAUDE.md constraint 8 — this is policy, not mechanism). */
export interface WorkflowResource {
  slots?: number;
  max_hold_minutes?: number;
  extra?: Record<string, YamlValue>;
}

/** The `board.wip:` mapping (#1175) — one optional cap per board status. Typed as a
 *  record rather than as seven named fields because the pane's job here is to
 *  round-trip whatever the engine accepts: `WIP_STATUSES` is what says which keys are
 *  known, and a key outside it lands in `extra` with an `unknown-key` finding, exactly
 *  as `deny_unknown_fields` will refuse it on the engine side. */
export type WorkflowWip = Record<string, number>;

/** The `board:` section (#1175). Same declared-or-absent rule as `intake:` and
 *  `merge_queue:`: an omitted key is the engine's own default, never a value this
 *  module writes back in. */
export interface WorkflowBoard {
  wip?: WorkflowWip;
  /** Absent, `true` or `false` — three states, and absent is NOT `false` on the wire
   *  even though the engine reads them alike (the `merge_queue.enabled` argument). */
  enforce?: boolean;
  extra?: Record<string, YamlValue>;
  /** Keys under `wip:` this build does not know — preserved so a file written by a
   *  newer loomux survives a round-trip through an older pane. */
  wipExtra?: Record<string, YamlValue>;
}

export interface Workflow {
  version: number;
  name: string;
  /** The loomux version that CREATED this file (§4's Langflow `last_tested_version`
   *  lesson). Written exactly once, at creation, and never restamped: it records who
   *  authored the workflow, not who last looked at it. A typed field since #880 —
   *  it used to ride the unknown-key bag, which round-tripped it correctly but left
   *  it invisible to everything that asks "what fields does a workflow have?". */
  authored_with?: string;
  blocks: WorkflowBlock[];
  edges: WorkflowEdge[];
  gates: WorkflowGates;
  intake?: WorkflowIntake;
  merge_queue?: WorkflowMergeQueue;
  /** Review-loop driver policy (#1778 §5.3). */
  driver?: WorkflowDriver;
  /** Keyed by a repo-chosen resource name, in the file's own order (the emitter
   *  sorts, matching the engine's `BTreeMap`). */
  resources?: Record<string, WorkflowResource>;
  /** Task-board policy — per-status WIP limits (#1175). */
  board?: WorkflowBoard;
  extra?: Record<string, YamlValue>;
}

// ---------- findings ----------

export type FindingSeverity = "error" | "warning";

export type FindingCode =
  | "yaml-syntax"
  | "not-a-mapping"
  | "version-missing"
  | "version-unsupported"
  | "no-blocks"
  | "block-not-a-mapping"
  | "block-id-missing"
  | "block-id-invalid"
  | "block-id-duplicate"
  | "unknown-kind"
  | "unknown-cli"
  | "prompt-and-profile"
  | "role-hint-unknown"
  | "role-hint-wrong-kind"
  | "role-hint-superseded"
  | "manager-not-unique"
  | "knob-unavailable"
  | "edge-not-a-mapping"
  | "edge-unknown-block"
  | "edge-self"
  | "gate-unknown-require"
  | "gate-no-reviewers"
  | "gate-unknown-reviewer"
  | "gate-not-a-reviewer"
  | "gate-bad-threshold"
  | "gate-bad-max-diff-lines"
  | "gate-bad-routing"
  | "isolated-block"
  | "unreachable-block"
  | "no-entry-block"
  | "unknown-key"
  | "section-not-a-mapping"
  | "section-bad-value"
  | "section-out-of-range"
  | "intake-unknown-source"
  | "intake-bad-label"
  | "resource-name-invalid"
  | "allow-not-permitted"
  | "persona-not-permitted"
  | "remote-invalid-label"
  | "remote-not-permitted"
  | "remote-requires-claude"
  | "allow-sanitized";

/** The policy sections a finding can be ABOUT — the routing key for the three that are
 *  neither a block nor a line (#1020). Same job `blockId` does for a block: the pane's
 *  roster and its findings list land the human on the form that can fix it, and neither
 *  has to match on the message text to work out which one that is. */
/** `board:` (#1175) is deliberately NOT a member yet: this key routes a finding onto the
 *  inspector form that can fix it, and the board section has no form - it is listed as
 *  not-yet-editable in `test/workflowschema.test.ts` like every other field. A section
 *  here with no form to land on would be a click that goes nowhere. Its findings carry a
 *  message naming `board.wip.<status>` instead, which is what the raw-text view needs
 *  anyway; the member and the form arrive together or not at all. */
/** `driver:` (#1778) is a member WITH a form since #1869 (enable-toggle plus the six
 *  counters, same shape as `mergeQueueForm`), so a finding's click lands on the form
 *  that can fix it — the same click-to-the-fix routing the other policy sections get. */
export type FindingSection = "intake" | "merge_queue" | "driver" | "resources";

/** One thing wrong with the workflow. `blockId` lets the pane render the finding INLINE
 *  next to the block it is about (the whole reason the validation pass is worth having is
 *  that it tells you WHERE); `line` does the same for the raw-text view, and `section` for
 *  the policy sections. */
export interface Finding {
  severity: FindingSeverity;
  code: FindingCode;
  message: string;
  blockId?: string;
  /** Which policy section this is about, when it is about one. */
  section?: FindingSection;
  /** 1-based source line, when the finding came from reading the text. */
  line?: number;
}

export function hasErrors(findings: readonly Finding[]): boolean {
  return findings.some((f) => f.severity === "error");
}

/** True only when the text itself couldn't be read as a document at all — a syntax error, or a
 *  shape so wrong the root isn't even a mapping. This is deliberately NARROWER than `hasErrors`:
 *  a `version-unsupported` or `gate-bad-threshold` finding means the WORKFLOW is wrong, not that
 *  the TEXT is unreadable, and the pane's form stays editable through those (see
 *  `workflowview.ts`'s `syntaxBroken`, which this mirrors exactly on purpose — #233 B3. The two
 *  must agree: if the view lets a human keep editing a file, `serializeWorkflowPreserving`
 *  (below) must not treat that same file as too broken to diff against, or the very first edit
 *  silently falls back to a full canonical rewrite for a reason the human was never shown. */
export function isUnreadable(findings: readonly Finding[]): boolean {
  return findings.some((f) => f.code === "yaml-syntax" || f.code === "not-a-mapping");
}

// ---------- YAML subset: reading ----------

interface RawLine {
  /** 0-based index into the source lines. */
  i: number;
  /** Leading-space count. */
  indent: number;
  /** The line with its indent and any trailing comment removed. */
  text: string;
}

/** Strip a `#` comment, ignoring one inside a quoted scalar. A `#` that is not preceded
 *  by whitespace is NOT a comment in YAML (`a#b` is the scalar `a#b`), which matters here
 *  because a model or a branch can legitimately contain one. */
function stripComment(line: string): string {
  let quote: '"' | "'" | null = null;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (quote) {
      if (c === "\\" && quote === '"') i++;
      else if (c === quote) quote = null;
      continue;
    }
    if (c === '"' || c === "'") {
      quote = c;
      continue;
    }
    if (c === "#" && (i === 0 || /\s/.test(line[i - 1]!))) return line.slice(0, i);
  }
  return line;
}

const indentOf = (line: string): number => line.length - line.trimStart().length;

class YamlReader {
  /** Cursor into `raw` — the next line not yet consumed. */
  private i = 0;
  readonly findings: Finding[] = [];
  private readonly raw: string[];

  constructor(raw: string[]) {
    this.raw = raw;
  }

  private err(line: number, message: string): void {
    this.findings.push({ severity: "error", code: "yaml-syntax", message, line: line + 1 });
  }

  /** The next SIGNIFICANT line (blank and comment-only lines skipped), without consuming
   *  it. Callers consume by setting the cursor past the line they took.
   *
   *  TABS. YAML forbids a tab in indentation, and this pane must say so — because the
   *  backend validator (a real parser) will refuse the same file, and a pane that reports
   *  `valid` on a file the spawn then rejects is worse than one that reports nothing.
   *  The line is skipped with a finding rather than aborting the read: the rest of the
   *  file still opens, which is this module's whole contract.
   *
   *  (A tab INSIDE a block scalar is content, not indentation, and stays that way —
   *  `blockScalar` reads `this.raw` directly and never comes through here.) */
  private peek(): RawLine | null {
    for (let j = this.i; j < this.raw.length; j++) {
      const raw = this.raw[j]!;
      // Test the RAW leading whitespace. The previous form — `raw.trimStart().startsWith("\t")`
      // — could never fire, because trimStart() strips the very tab it was looking for
      // (rev-5 F2): the guard was dead, and a fully tab-indented file validated clean.
      if (/^[ ]*\t/.test(raw)) {
        if (!this.tabLines.has(j)) {
          this.tabLines.add(j); // peek() is called repeatedly; the finding is reported once
          this.err(j, "tabs cannot be used for indentation in YAML — use spaces");
        }
        continue;
      }
      const text = stripComment(raw).trimEnd();
      if (!text.trim()) continue;
      return { i: j, indent: indentOf(text), text: text.trim() };
    }
    return null;
  }

  /** Lines already reported as tab-indented, so a re-peek doesn't report them twice. */
  private readonly tabLines = new Set<number>();

  /** Read the whole document as a mapping. */
  document(): YamlValue {
    const p = this.peek();
    if (!p) return {};
    if (p.indent !== 0) {
      this.err(p.i, "the document must start at column 0");
      return {};
    }
    if (p.text.startsWith("-")) {
      this.err(p.i, "a workflow file is a mapping (version:, blocks:, …), not a list");
      return {};
    }
    return this.mapping(0);
  }

  private mapping(indent: number): Record<string, YamlValue> {
    const obj: Record<string, YamlValue> = {};
    for (;;) {
      const p = this.peek();
      if (!p || p.indent < indent) break;
      if (p.indent > indent) {
        // Nothing above claimed this line — it is over-indented. Skip it rather than
        // spin, and say so: an unconsumed line with no error is a silently dropped key.
        this.err(p.i, `unexpected indentation — "${p.text}" is indented further than its siblings`);
        this.i = p.i + 1;
        continue;
      }
      if (p.text.startsWith("-")) {
        // `mapping(0)` is only ever called once, from `document()` — there is no enclosing
        // key left to hand this sequence off to (contrast a nested call, where `indent > 0`
        // and the caller — `afterKey`/`sequence` — is waiting for exactly this handoff).
        // Breaking silently here would drop everything from this line to EOF with no finding
        // at all (#270: a hand-edited orphan `-` line truncates the whole rest of the file).
        // Report it, consume the whole orphan sequence so the rest of the document can still
        // be read, and keep going — the same "report and skip" shape as the tab and
        // over-indented-line guards above.
        if (indent === 0) {
          this.err(
            p.i,
            `unexpected "-" at the top level — a workflow file is a mapping (version:, blocks:, …); this sequence belongs to no key`
          );
          this.sequence(indent);
          continue;
        }
        break; // a sequence at this level ends the mapping, handed off to the enclosing key
      }
      const split = splitKey(p.text);
      if (!split) {
        this.err(p.i, `expected "key: value" but found "${p.text}"`);
        this.i = p.i + 1;
        continue;
      }
      this.i = p.i + 1;
      obj[split.key] = this.afterKey(split.rest, indent, p.i);
    }
    return obj;
  }

  /** The value that follows `key:` on line `at`, whatever form it takes. */
  private afterKey(rest: string, indent: number, at: number): YamlValue {
    if (rest === "") {
      const p = this.peek();
      // A nested block is indented further — EXCEPT a sequence, which YAML allows to sit
      // at the parent key's own indent. Both are common in the wild; accept both.
      if (p && (p.indent > indent || (p.indent === indent && p.text.startsWith("-")))) {
        return p.text.startsWith("-") ? this.sequence(p.indent) : this.mapping(p.indent);
      }
      return null;
    }
    // `|`, `>`, with an optional INDENTATION INDICATOR and/or chomping marker, in either
    // order (`|2`, `|-`, `|2-`, `|-2` are all legal YAML).
    if (/^[|>](?:\d[-+]?|[-+]?\d?)$/.test(rest)) return this.blockScalar(rest, indent);
    return this.flowOrScalar(rest, at);
  }

  private sequence(indent: number): YamlValue[] {
    const items: YamlValue[] = [];
    for (;;) {
      const p = this.peek();
      if (!p || p.indent !== indent) break;
      if (p.text !== "-" && !p.text.startsWith("- ")) break;
      this.i = p.i + 1;
      if (p.text === "-") {
        // The item's content is the block indented under the dash.
        const q = this.peek();
        if (q && q.indent > indent) {
          items.push(q.text.startsWith("-") ? this.sequence(q.indent) : this.mapping(q.indent));
        } else {
          items.push(null);
        }
        continue;
      }
      const rest = p.text.slice(1).trimStart();
      // The column the item's keys live at: where `rest` actually starts on the line.
      // `- id: x` puts them at dash+2, but `-   id: x` is legal too, and getting this
      // wrong silently drops every key after the first.
      const keyIndent = indent + (p.text.length - rest.length);
      const split = rest.startsWith("{") || rest.startsWith("[") ? null : splitKey(rest);
      if (split) {
        const first: Record<string, YamlValue> = {};
        first[split.key] = this.afterKey(split.rest, keyIndent, p.i);
        items.push({ ...first, ...this.mapping(keyIndent) });
      } else {
        items.push(this.flowOrScalar(rest, p.i));
      }
    }
    return items;
  }

  /** A `|` / `>` block scalar: every line indented past the key, dedented by the content's
   *  indent — which the header STATES (`|2`) when it can, and which is otherwise inferred
   *  from the first content line. The explicit form is what we emit, and it is the only one
   *  that survives a prompt whose own first line is indented (rev-5 F3): inferring the
   *  dedent from content that is itself indented eats exactly that indentation.
   *
   *  Comments are NOT stripped here — inside a block scalar a `#` is content, and a prompt
   *  that says "# Review checklist" must survive. Tabs likewise: in here they are text. */
  private blockScalar(header: string, parentIndent: number): string {
    const folded = header.startsWith(">");
    const chomp = header.includes("-") ? "strip" : header.includes("+") ? "keep" : "clip";
    const indicator = /\d/.exec(header);
    const body: string[] = [];
    // -1 = "infer from the first content line". An explicit indicator is RELATIVE to the
    // parent node's indentation, which is what makes it independent of the content.
    let contentIndent = indicator ? parentIndent + Number(indicator[0]) : -1;
    while (this.i < this.raw.length) {
      const raw = this.raw[this.i]!;
      if (!raw.trim()) {
        body.push("");
        this.i++;
        continue;
      }
      const ind = indentOf(raw);
      if (ind <= parentIndent) break;
      if (contentIndent < 0) contentIndent = ind;
      body.push(raw.slice(Math.min(ind, contentIndent)).trimEnd());
      this.i++;
    }
    while (body.length && body[body.length - 1] === "") body.pop();
    if (!body.length) return "";
    const text = folded ? foldLines(body) : body.join("\n");
    return chomp === "strip" ? text : text + "\n";
  }

  private flowOrScalar(text: string, at: number): YamlValue {
    if (text.startsWith("[") || text.startsWith("{")) {
      const flow = new FlowReader(text);
      try {
        const v = flow.parse();
        if (!flow.atEnd()) this.err(at, `trailing text after "${text.slice(0, flow.pos)}"`);
        return v;
      } catch (e) {
        this.err(at, e instanceof Error ? e.message : String(e));
        return null;
      }
    }
    return plainScalar(text);
  }
}

/** Fold a `>` scalar: consecutive non-blank lines join with a space, a blank line is a
 *  paragraph break. (Supported for completeness — `|` is what a prompt actually wants,
 *  because folding a prompt's line breaks changes what the agent reads.) */
function foldLines(lines: string[]): string {
  const out: string[] = [];
  let para: string[] = [];
  const flush = (): void => {
    if (para.length) out.push(para.join(" "));
    para = [];
  };
  for (const l of lines) {
    if (!l.trim()) {
      flush();
      out.push("");
    } else para.push(l.trim());
  }
  flush();
  return out.join("\n");
}

/** Split `key: value` at the first top-level `: ` (or a trailing `:`). Returns null when
 *  the line is not a mapping entry at all. */
function splitKey(text: string): { key: string; rest: string } | null {
  let quote: '"' | "'" | null = null;
  let depth = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text[i]!;
    if (quote) {
      if (c === "\\" && quote === '"') i++;
      else if (c === quote) quote = null;
      continue;
    }
    if (c === '"' || c === "'") quote = c;
    else if (c === "[" || c === "{") depth++;
    else if (c === "]" || c === "}") depth--;
    else if (c === ":" && depth === 0) {
      const next = text[i + 1];
      if (next === undefined || next === " ") {
        const key = text.slice(0, i).trim();
        if (!key) return null;
        return { key: unquote(key), rest: text.slice(i + 1).trim() };
      }
    }
  }
  return null;
}

/** Escape codes a double-quoted scalar can carry. `default: the character itself` covers
 *  `\"` and `\\`, which is the whole point of an escape. */
const ESCAPES: Record<string, string> = { n: "\n", t: "\t", r: "\r" };

function unquote(s: string): string {
  if (s.length >= 2 && s[0] === '"' && s.endsWith('"')) {
    // ONE PASS, left to right (rev-6 F8). Chained `.replace()`s unescape in the wrong order:
    // `\\n` (an escaped backslash followed by the letter n) had its `\n` expanded to a
    // NEWLINE by the first replace, before the later one could collapse `\\` to a single
    // backslash — so `"C:\\new"` read back as `C:` + newline + `ew`. A single pass consumes
    // each backslash with the character it actually escapes, so an escaped backslash can
    // never be re-read as the start of another escape.
    return s.slice(1, -1).replace(/\\(.)/g, (_, c: string) => ESCAPES[c] ?? c);
  }
  if (s.length >= 2 && s[0] === "'" && s.endsWith("'")) return s.slice(1, -1).replace(/''/g, "'");
  return s;
}

function plainScalar(text: string): YamlValue {
  if (text[0] === '"' || text[0] === "'") return unquote(text);
  if (text === "null" || text === "~") return null;
  if (text === "true") return true;
  if (text === "false") return false;
  if (/^-?\d+$/.test(text)) return Number(text);
  if (/^-?\d+\.\d+$/.test(text)) return Number(text);
  return text;
}

/** A one-line flow collection: `[a, b]`, `{ from: x, to: [a, b] }`. */
class FlowReader {
  pos = 0;
  private readonly s: string;

  constructor(s: string) {
    this.s = s;
  }

  atEnd(): boolean {
    this.ws();
    return this.pos >= this.s.length;
  }

  parse(): YamlValue {
    this.ws();
    const c = this.s[this.pos];
    if (c === "[") return this.seq();
    if (c === "{") return this.map();
    return this.scalar();
  }

  private ws(): void {
    while (this.pos < this.s.length && /\s/.test(this.s[this.pos]!)) this.pos++;
  }

  private seq(): YamlValue[] {
    this.pos++; // [
    const out: YamlValue[] = [];
    for (;;) {
      this.ws();
      if (this.s[this.pos] === "]") {
        this.pos++;
        return out;
      }
      if (this.pos >= this.s.length) throw new Error("unterminated [ … ] list");
      out.push(this.parse());
      this.ws();
      if (this.s[this.pos] === ",") this.pos++;
      else if (this.s[this.pos] !== "]") throw new Error(`expected "," or "]" in list`);
    }
  }

  private map(): Record<string, YamlValue> {
    this.pos++; // {
    const out: Record<string, YamlValue> = {};
    for (;;) {
      this.ws();
      if (this.s[this.pos] === "}") {
        this.pos++;
        return out;
      }
      if (this.pos >= this.s.length) throw new Error("unterminated { … } mapping");
      const key = this.scalarText();
      this.ws();
      if (this.s[this.pos] !== ":") throw new Error(`expected ":" after "${key}"`);
      this.pos++;
      out[unquote(key)] = this.parse();
      this.ws();
      if (this.s[this.pos] === ",") this.pos++;
      else if (this.s[this.pos] !== "}") throw new Error(`expected "," or "}" in mapping`);
    }
  }

  private scalar(): YamlValue {
    return plainScalar(this.scalarText());
  }

  /** A scalar token up to the next structural character, quotes respected. */
  private scalarText(): string {
    this.ws();
    const c = this.s[this.pos];
    if (c === '"' || c === "'") {
      const start = this.pos;
      this.pos++;
      while (this.pos < this.s.length) {
        const ch = this.s[this.pos]!;
        if (ch === "\\" && c === '"') this.pos += 2;
        else if (ch === c) {
          this.pos++;
          return this.s.slice(start, this.pos);
        } else this.pos++;
      }
      throw new Error("unterminated quoted string");
    }
    const start = this.pos;
    while (this.pos < this.s.length && !",:[]{}".includes(this.s[this.pos]!)) this.pos++;
    const text = this.s.slice(start, this.pos).trim();
    if (!text) throw new Error("expected a value");
    return text;
  }
}

// ---------- YAML subset: writing (the canonical formatter) ----------
//
// One shape, always, so `git diff` shows what CHANGED and not how it was written: fixed
// key order (the order a human reads a block in — who it is, what it runs as, what it
// runs on, then the persona body last because it is the long one), edges grouped by
// their source and ordered by the blocks they connect, gate lists ordered the same way.
//
// Blocks themselves keep their AUTHORED order. That is the one place a "stable sort"
// would do harm: the roster reads top-to-bottom, and re-sorting it alphabetically on
// every save would churn the diff of the file it is supposed to keep legible.

/** Quote a scalar when leaving it bare would change what it means (or fail to parse).
 *
 *  `,` `[` `]` `{` `}` are in the list for a reason worth stating, because leaving them out
 *  was a silent file-corrupting bug (rev-5 F1): this ONE emitter serves both contexts —
 *  block (`name: …`) and FLOW (`reviewers: [a, b]`, `also: […]`, an unknown key's array or
 *  map). In flow context those five characters are STRUCTURAL, so an unquoted
 *  `Bash(gh pr view --json title,body)` re-reads as two list entries and an unquoted
 *  `fmt{x}` closes the collection early and takes the whole value down with it — and both
 *  happen on an ordinary form edit, because every form edit re-serializes the file.
 *
 *  Rather than keep two emitters and a rule about which context is which (the rule you
 *  forget at exactly one of the six call sites), the ONE emitter quotes for the strictest
 *  context. A quote is always SAFE in block context — it just isn't always necessary — and
 *  "sometimes unnecessary" is a far cheaper failure than "sometimes destroys the value". */
function emitScalar(v: string): string {
  if (v === "") return '""';
  if (
    /^[-?:,[\]{}#&*!|>'"%@`]/.test(v) ||
    /[,[\]{}]/.test(v) || // structural in a flow collection, anywhere in the string
    /:\s/.test(v) ||
    /\s#/.test(v) ||
    v !== v.trim() ||
    v === "true" ||
    v === "false" ||
    v === "null" ||
    v === "~" ||
    /^-?\d+(\.\d+)?$/.test(v) ||
    /[\n\t\r]/.test(v)
  ) {
    // Backslash FIRST, so the escapes introduced below aren't themselves re-escaped — the
    // mirror image of the reader's single pass (see `unquote`), and the two must stay
    // symmetric or a value stops surviving the round-trip it just survived.
    return `"${v
      .replace(/\\/g, "\\\\")
      .replace(/"/g, '\\"')
      .replace(/\n/g, "\\n")
      .replace(/\t/g, "\\t")
      .replace(/\r/g, "\\r")}"`;
  }
  return v;
}

function emitValue(v: YamlValue): string {
  if (v === null) return "null";
  if (typeof v === "boolean" || typeof v === "number") return String(v);
  if (typeof v === "string") return emitScalar(v);
  if (Array.isArray(v)) return `[${v.map(emitValue).join(", ")}]`;
  // The KEY goes through the emitter too (rev-6 F9). A key is a string in a flow mapping and
  // is every bit as capable of holding a `,` or a `}` as a value is — emitting it raw was the
  // value-side bug (F1) with the two halves of the pair swapped, and it survived F1's fix
  // only because nothing had put a structural character in a key yet.
  return `{ ${Object.keys(v)
    .sort()
    .map((k) => `${emitScalar(k)}: ${emitValue(v[k]!)}`)
    .join(", ")} }`;
}

/** A `|` block scalar, indented under its key. A prompt keeps its line breaks — folding
 *  them would change what the agent actually reads. */
function emitBlockScalar(key: string, text: string, indent: string): string[] {
  // A body that ends in a newline is `|` (clip); one that doesn't is `|-` (strip). That
  // is what makes prompt → YAML → prompt exact rather than approximately exact.
  const chomp = text.endsWith("\n") ? "" : "-";
  const body = text.replace(/\n$/, "").split("\n");
  // The INDENTATION INDICATOR (`|2`), and why it isn't optional (rev-5 F3): a plain `|` is
  // read back by dedenting to the FIRST CONTENT LINE's indent, so a prompt whose first line
  // is itself indented — a code snippet, an indented checklist, and it comes straight out of
  // the form's textarea — silently loses that indent on the next read. Same for a prompt
  // that opens with a blank line, where the "first content line" is the second one. Stating
  // the indent explicitly makes the reader's dedent independent of the content, which is the
  // only way this round-trips.
  const first = body[0] ?? "";
  const explicit = first === "" || /^\s/.test(first);
  const header = `|${explicit ? BLOCK_SCALAR_INDENT : ""}${chomp}`;
  const pad = " ".repeat(BLOCK_SCALAR_INDENT);
  return [`${indent}${key}: ${header}`, ...body.map((l) => (l ? `${indent}${pad}${l}` : ""))];
}

/** How far a block scalar's body is indented past its key. Both halves of the round-trip
 *  read it: the emitter pads by it, and the `|2` indicator it writes tells the reader to
 *  dedent by exactly it rather than by guessing from the content. */
const BLOCK_SCALAR_INDENT = 2;

function extraLines(extra: Record<string, YamlValue> | undefined, indent: string): string[] {
  if (!extra) return [];
  // The key goes through the emitter here too, for the same reason as in `emitValue` — an
  // unknown key is as arbitrary as an unknown value, and a key carrying a `: ` would
  // otherwise re-read as a different key with a different value. (`splitKey`/`unquote`
  // already read a quoted key; only the writing side was asymmetric.)
  return Object.keys(extra)
    .sort()
    .map((k) => `${indent}${emitScalar(k)}: ${emitValue(extra[k]!)}`);
}

/** `version:`, `name:` and any top-level unknown keys — the lines every workflow starts
 *  with. Factored out so the comment-preserving serializer (below) can regenerate just this
 *  piece when it — and only it — has changed, without duplicating the exact formatting rules. */
function emitFrontLines(w: Workflow): string[] {
  const out: string[] = [];
  out.push(`version: ${w.version}`);
  if (w.name) out.push(`name: ${emitScalar(w.name)}`);
  // Only when the file HAS one: this key is stamped once, at creation, and a save
  // must never invent it (see `AUTHORED_WITH_KEY`).
  if (w.authored_with !== undefined) out.push(`authored_with: ${emitScalar(w.authored_with)}`);
  out.push(...extraLines(w.extra, ""));
  return out;
}

/** A nested mapping section (`intake:`, `merge_queue:`, one `resources:` entry).
 *  A section with nothing under it emits `key: {}` rather than a bare `key:` — a
 *  bare key is YAML *null*, which reads back as "not declared at all", so the
 *  round-trip would silently delete a section someone deliberately wrote empty.
 *  Exactly the `blocks: []` reasoning (rev-5 F4), one level down. */
function emitMappingSection(key: string, indent: string, body: readonly string[]): string[] {
  if (!body.length) return [`${indent}${key}: {}`];
  return [`${indent}${key}:`, ...body];
}

/** The `intake:` section (#382 P1). Every field is emitted only when DECLARED:
 *  the engine resolves an absent one against its built-in profile, so writing the
 *  defaults out would turn "inherit" into "pin", silently, on the first save. */
function emitIntakeLines(intake: WorkflowIntake, indent = ""): string[] {
  const field = `${indent}  `;
  const body: string[] = [];
  if (intake.source !== undefined) body.push(`${field}source: ${emitScalar(intake.source)}`);
  if (intake.labels) body.push(...emitIntakeLabelLines(intake.labels, field));
  body.push(...extraLines(intake.extra, field));
  return emitMappingSection("intake", indent, body);
}

/** The five label fields, in the order the engine's own struct declares them — which is
 *  therefore the order they are emitted in and the order a form should show them. */
export const INTAKE_LABEL_KEYS = ["ready", "investigate", "owned", "prototype", "hold"] as const;
export type IntakeLabelKey = (typeof INTAKE_LABEL_KEYS)[number];

function emitIntakeLabelLines(labels: WorkflowIntakeLabels, indent: string): string[] {
  const field = `${indent}  `;
  const body: string[] = [];
  for (const key of INTAKE_LABEL_KEYS) {
    const v = labels[key];
    if (v !== undefined) body.push(`${field}${key}: ${emitScalar(v)}`);
  }
  body.push(...extraLines(labels.extra, field));
  return emitMappingSection("labels", indent, body);
}

/** The `merge_queue:` section (#581 §11.2). Same declared-only rule as `intake:`,
 *  and for a sharper reason: an absent block means the queue is OFF, so emitting
 *  `enabled: false` where the file said nothing would be a policy statement the
 *  human never made. */
function emitMergeQueueLines(mq: WorkflowMergeQueue, indent = ""): string[] {
  const field = `${indent}  `;
  const body: string[] = [];
  if (mq.enabled !== undefined) body.push(`${field}enabled: ${mq.enabled}`);
  if (mq.max_batch !== undefined) body.push(`${field}max_batch: ${mq.max_batch}`);
  if (mq.checks_timeout_minutes !== undefined) {
    body.push(`${field}checks_timeout_minutes: ${mq.checks_timeout_minutes}`);
  }
  body.push(...extraLines(mq.extra, field));
  return emitMappingSection("merge_queue", indent, body);
}

/** The `driver:` section (#1778 §5.3). Same declared-only rule as `intake:`
 *  and `merge_queue:`: an absent block means the driver is OFF, so emitting
 *  `enabled: false` where the file said nothing would be a policy statement
 *  the human never made. */
function emitDriverLines(dv: WorkflowDriver, indent = ""): string[] {
  const field = `${indent}  `;
  const body: string[] = [];
  if (dv.enabled !== undefined) body.push(`${field}enabled: ${dv.enabled}`);
  if (dv.max_review_rounds !== undefined) {
    body.push(`${field}max_review_rounds: ${dv.max_review_rounds}`);
  }
  if (dv.max_ci_attempts !== undefined) {
    body.push(`${field}max_ci_attempts: ${dv.max_ci_attempts}`);
  }
  if (dv.max_rebase_attempts !== undefined) {
    body.push(`${field}max_rebase_attempts: ${dv.max_rebase_attempts}`);
  }
  if (dv.lane_timeout_minutes !== undefined) {
    body.push(`${field}lane_timeout_minutes: ${dv.lane_timeout_minutes}`);
  }
  if (dv.fix_timeout_minutes !== undefined) {
    body.push(`${field}fix_timeout_minutes: ${dv.fix_timeout_minutes}`);
  }
  if (dv.drive_timeout_minutes !== undefined) {
    body.push(`${field}drive_timeout_minutes: ${dv.drive_timeout_minutes}`);
  }
  body.push(...extraLines(dv.extra, field));
  return emitMappingSection("driver", indent, body);
}

/** The `resources:` section (#858) — a mapping of repo-chosen names to two numbers.
 *  Names are emitted in SORTED order, matching the engine's `BTreeMap`, because
 *  unlike the roster (whose order is meaning — the first block of a class is the
 *  default one) a resource map has no order to preserve. */
function emitResourcesLines(resources: Record<string, WorkflowResource>, indent = ""): string[] {
  const field = `${indent}  `;
  const body: string[] = [];
  for (const name of Object.keys(resources).sort()) {
    const r = resources[name]!;
    const inner: string[] = [];
    if (r.slots !== undefined) inner.push(`${field}  slots: ${r.slots}`);
    if (r.max_hold_minutes !== undefined) {
      inner.push(`${field}  max_hold_minutes: ${r.max_hold_minutes}`);
    }
    inner.push(...extraLines(r.extra, `${field}  `));
    body.push(...emitMappingSection(emitScalar(name), field, inner));
  }
  return emitMappingSection("resources", indent, body);
}

/** The `board:` section (#1175). Same declared-only rule as `intake:`/`merge_queue:`,
 *  and the same three-state care on `enforce:`: absent and `false` mean the same thing
 *  to the engine, so a save must not convert one into the other behind the human's back.
 *
 *  Caps are emitted in `WIP_STATUSES` order — the board's own order, and the order the
 *  engine's struct declares them in — rather than sorted, so a file reads top-to-bottom
 *  the way the board flows. */
function emitBoardLines(board: WorkflowBoard, indent = ""): string[] {
  const field = `${indent}  `;
  const body: string[] = [];
  if (board.wip) {
    const inner: string[] = [];
    for (const status of WIP_STATUSES) {
      const v = board.wip[status];
      if (v !== undefined) inner.push(`${field}  ${emitScalar(status)}: ${v}`);
    }
    inner.push(...extraLines(board.wipExtra, `${field}  `));
    body.push(...emitMappingSection("wip", field, inner));
  }
  if (board.enforce !== undefined) body.push(`${field}enforce: ${board.enforce}`);
  body.push(...extraLines(board.extra, field));
  return emitMappingSection("board", indent, body);
}

/** One block entry, canonical key order, no leading/trailing blank line. `markerIndent` is
 *  where the `-` sits — 2 (this build's own convention) by default, but the comment-preserving
 *  serializer passes whatever indent the SURROUNDING roster already uses (0 for a same-column
 *  sequence, or whatever else a hand-written file chose), so a regenerated item never mixes a
 *  different marker indent into a sequence that has to share exactly one (#233 non-blocking #2,
 *  and see `splitBlockItems`'s own note on why mixing indents is invalid, not just inconsistent). */
function emitBlockLines(b: WorkflowBlock, markerIndent = 2): string[] {
  const dash = " ".repeat(markerIndent);
  const field = " ".repeat(markerIndent + 2);
  const out: string[] = [];
  out.push(`${dash}- id: ${emitScalar(b.id)}`);
  out.push(`${field}name: ${emitScalar(b.name)}`);
  out.push(`${field}kind: ${emitScalar(b.kind)}`);
  if (b.role_hint !== undefined) out.push(`${field}role_hint: ${emitScalar(b.role_hint)}`);
  out.push(`${field}cli: ${emitScalar(b.cli)}`);
  if (b.model) out.push(`${field}model: ${emitScalar(b.model)}`);
  // #687: with the model they modify, and only when declared — a block that
  // pinned neither serializes byte for byte as it did before.
  if (b.effort !== undefined) out.push(`${field}effort: ${emitScalar(b.effort)}`);
  if (b.context !== undefined) out.push(`${field}context: ${emitScalar(b.context)}`);
  if (b.profile !== undefined) out.push(`${field}profile: ${emitScalar(b.profile)}`);
  // A flow list, like `reviewers:`/`also:` — and every entry goes through
  // `emitScalar`, which quotes for flow context, so `Bash(gh pr view --json
  // title,body)` survives instead of re-reading as two entries (rev-5 F1).
  if (b.allow !== undefined) out.push(`${field}allow: [${b.allow.map(emitScalar).join(", ")}]`);
  // #1457: only when declared — a block that named no remote serializes byte for
  // byte as it did before the key existed.
  if (b.remote !== undefined) out.push(`${field}remote: ${emitScalar(b.remote)}`);
  out.push(...extraLines(b.extra, field));
  if (b.prompt !== undefined) out.push(...emitBlockScalar("prompt", b.prompt, field));
  return out;
}

/** The `edges:` section, or `[]` (nothing pushed) when there are no edges. */
function emitEdgesLines(edges: readonly WorkflowEdge[], order: Map<string, number>): string[] {
  const groups = groupEdges(edges, order);
  if (!groups.length) return [];
  const out: string[] = ["edges:"];
  for (const g of groups) {
    const to = g.to.length === 1 ? emitScalar(g.to[0]!) : `[${g.to.map(emitScalar).join(", ")}]`;
    out.push(`  - { from: ${emitScalar(g.from)}, to: ${to} }`);
  }
  return out;
}

/** The `gates:` section, or `[]` (nothing pushed) when there is nothing to gate. */
function emitGatesLines(w: Workflow, order: Map<string, number>): string[] {
  const gate = w.gates.merge;
  if (!gate && !w.gates.extra) return [];
  const out: string[] = ["gates:"];
  if (gate) {
    out.push("  merge:");
    out.push(`    require: ${emitScalar(gate.require)}`);
    if (gate.threshold !== undefined) out.push(`    threshold: ${gate.threshold}`);
    out.push(`    reviewers: [${sortByBlocks(gate.reviewers, order).map(emitScalar).join(", ")}]`);
    if (gate.also.length) out.push(`    also: [${gate.also.map(emitScalar).join(", ")}]`);
    if (gate.max_diff_lines !== undefined) out.push(`    max_diff_lines: ${gate.max_diff_lines}`);
    // #1176. A block sequence of mappings — the one place this emitter writes
    // one. `emitScalar` already quotes a value starting with `*`, which every
    // `**/…` glob does and which YAML would otherwise read as an ALIAS.
    if (gate.routing?.length) {
      out.push("    routing:");
      for (const rule of gate.routing) {
        out.push(`      - paths: [${rule.paths.map(emitScalar).join(", ")}]`);
        out.push(
          `        reviewers: [${sortByBlocks(rule.reviewers, order).map(emitScalar).join(", ")}]`
        );
      }
    }
  }
  out.push(...extraLines(w.gates.extra, "  "));
  return out;
}

/** Render the workflow in canonical form. `parseWorkflow(serializeWorkflow(w)).workflow`
 *  deep-equals `w`, and serializing twice is a no-op — the two properties the file's
 *  legibility rests on, both pinned in test/workflowmodel.test.ts.
 *
 *  This is the FULL rewrite: fixed key order, no comments, no matter what was there before.
 *  It is what every form/canvas edit used to go through unconditionally (#233's whole
 *  complaint) and is now reserved for the explicit **Format** action and for a model that
 *  has no prior text to diff against (a brand-new file). Everyday edits go through
 *  `serializeWorkflowPreserving`, below, which reuses this file's own emitters for whatever
 *  it can't reuse verbatim from the original text. */
export function serializeWorkflow(w: Workflow): string {
  const order = blockOrder(w);
  const out: string[] = [...emitFrontLines(w)];

  // An EMPTY roster emits `blocks: []`, not a bare `blocks:` (rev-5 F4). A bare key is
  // YAML `null`, so the pane would re-read its own output as a malformed shape and report a
  // syntax-ish error against text it had just written itself — on top of the honest
  // `no-blocks`. Deleting the last block in the form is the ordinary way to get here.
  out.push("", w.blocks.length ? "blocks:" : "blocks: []");
  for (const b of w.blocks) out.push(...emitBlockLines(b));

  const edgeLines = emitEdgesLines(w.edges, order);
  if (edgeLines.length) out.push("", ...edgeLines);

  const gateLines = emitGatesLines(w, order);
  if (gateLines.length) out.push("", ...gateLines);

  // The policy sections, in the engine's own `RawWorkflow` field order — one
  // canonical shape means one order, and taking it from the type that defines the
  // schema is the only choice that can't drift for a reason nobody can name.
  for (const lines of [
    w.intake ? emitIntakeLines(w.intake) : [],
    w.merge_queue ? emitMergeQueueLines(w.merge_queue) : [],
    w.driver ? emitDriverLines(w.driver) : [],
    w.resources ? emitResourcesLines(w.resources) : [],
    w.board ? emitBoardLines(w.board) : [],
  ]) {
    if (lines.length) out.push("", ...lines);
  }

  return out.join("\n") + "\n";
}

/** Block id → its position in the roster. The sort key for everything that REFERENCES a
 *  block (edges, reviewer lists), so those lists read in graph order instead of
 *  alphabetical order — and so an unrelated rename can't reshuffle them. */
function blockOrder(w: Workflow): Map<string, number> {
  return new Map(w.blocks.map((b, i) => [b.id, i]));
}

function sortByBlocks(ids: readonly string[], order: Map<string, number>): string[] {
  // Dangling references (not in the roster) sort last, alphabetically: they are exactly
  // what the validation pass is about to complain about, so they belong where they are
  // easy to see rather than interleaved with the real ones.
  const seen = new Set<string>();
  const uniq = ids.filter((id) => (seen.has(id) ? false : (seen.add(id), true)));
  return uniq.sort((a, b) => {
    const ia = order.get(a),
      ib = order.get(b);
    if (ia !== undefined && ib !== undefined) return ia - ib;
    if (ia !== undefined) return -1;
    if (ib !== undefined) return 1;
    return a.localeCompare(b);
  });
}

/** Collapse the edge list into one entry per source (`{ from: worker, to: [a, b] }`),
 *  deduped and ordered by the roster. The fan-out form is how the schema sketch writes
 *  it and how a human reads it; the model keeps edges flat because every graph question
 *  (reachability, in-degree) is asked of pairs. */
function groupEdges(
  edges: readonly WorkflowEdge[],
  order: Map<string, number>
): { from: string; to: string[] }[] {
  const byFrom = new Map<string, string[]>();
  for (const e of edges) {
    const list = byFrom.get(e.from) ?? [];
    if (!list.includes(e.to)) list.push(e.to);
    byFrom.set(e.from, list);
  }
  return sortByBlocks([...byFrom.keys()], order).map((from) => ({
    from,
    to: sortByBlocks(byFrom.get(from)!, order),
  }));
}

// ---------- comment-preserving serialization (#233) ----------
//
// The bug #233 was filed against: `serializeWorkflow` is a FULL rewrite, and every form or
// canvas edit called it on the whole workflow, every time — so dragging one edge in a file
// with 60 comment lines produced a 60-comment-line diff. The interim mitigation (#231, rev-15)
// was honest about that trade and warned before it happened; this is the real fix.
//
// The approach: reuse the ORIGINAL TEXT'S OWN LINES wherever the new model says nothing
// changed there, and fall back to the canonical emitters (above) only for the piece that
// actually changed. Correctness rests on one guarantee: a segment of original text is only
// ever reused when the NEW block/section is `deepEqual` to what parsing THAT SAME original
// text produced — so splicing it back in can only ever reproduce what was already there.
// There is no attempt to re-attach a comment to a field that changed underneath it; that is
// deliberately out of scope (see the module comment at the top of this file) — the bar is
// "untouched regions keep their comments and formatting", not full-fidelity diffing.

/** Structural equality for the JSON-shaped values this schema is built from (`YamlValue`,
 *  `WorkflowBlock`, `WorkflowGates`, …). Order-independent for object keys (so `extra` bags
 *  built from a `Map`/`Object.keys` in a different order still compare equal), order-sensitive
 *  for arrays (an edge list is a sequence, not a set — see `groupEdges`'s own docblock on why
 *  ordering there is meaningful). This is the one thing that MUST NOT false-positive: comparing
 *  "equal" when something actually changed would splice stale text back over a real edit. */
function deepEqualValue(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
    return a.every((v, i) => deepEqualValue(v, b[i]));
  }
  const ao = a as Record<string, unknown>;
  const bo = b as Record<string, unknown>;
  const ak = Object.keys(ao);
  const bk = Object.keys(bo);
  if (ak.length !== bk.length) return false;
  return ak.every((k) => Object.prototype.hasOwnProperty.call(bo, k) && deepEqualValue(ao[k], bo[k]));
}

/** A `#` at the start of a line or after whitespace, ignoring one inside a quoted scalar —
 *  reusing `stripComment`'s own quote-awareness so a `#` inside a string doesn't fool the
 *  line-is-significant check below. */
const isSignificantLine = (line: string): boolean => stripComment(line).trim() !== "";

/** The header pattern for a `|`/`>` block scalar — the same one `afterKey` (the real reader,
 *  above) tests, kept in one place so the two never drift. */
const BLOCK_SCALAR_HEADER_RE = /^[|>](?:\d[-+]?|[-+]?\d?)$/;

/** Indices into `seg` that fall inside a `|`/`>` block scalar's BODY — content, never trivia,
 *  no matter what character they start with. #233 B2: a prompt's last line can legitimately be
 *  `# a checklist item`, and the naive "does it look like a comment" test used for trivia-
 *  peeling (below) would otherwise steal it onto whatever entry/item comes next — silently, and
 *  only visible once a SIBLING gets edited and that stolen line never comes back.
 *
 *  Scans `seg` from the front exactly once, tracking whether it is currently inside a scalar
 *  body (`scalarIndent`, the indent of the governing `key: |` line — the body ends at the first
 *  non-blank line whose indent drops back to that column or shallower, same rule `blockScalar`
 *  itself uses). Safe to run independently on each already-bounded segment (an entry's
 *  `content`, or one block item's `raw`): a scalar can never span the boundary between two such
 *  segments, because its own governing key is always MORE indented than either boundary the
 *  outer scan looks for (column 0 for a top-level key, `markerIndent` for a block item), so the
 *  boundary is always found before the scalar could bleed across it. */
function opaqueScalarIndices(seg: readonly string[]): Set<number> {
  const opaque = new Set<number>();
  let scalarIndent: number | null = null;
  for (let k = 0; k < seg.length; k++) {
    const line = seg[k]!;
    if (scalarIndent !== null) {
      if (line.trim() !== "" && indentOf(line) <= scalarIndent) {
        scalarIndent = null; // dedented back out — the scalar body ends here, not opaque
      } else {
        opaque.add(k);
        continue;
      }
    }
    if (!isSignificantLine(line)) continue;
    const stripped = stripComment(line);
    const split = splitKey(stripped.trim());
    if (split && BLOCK_SCALAR_HEADER_RE.test(split.rest)) scalarIndent = indentOf(stripped);
  }
  // A scalar that runs to the very END of `seg` with no dedent line to close it (its governing
  // key was the LAST field of the LAST item in this segment) leaves `scalarIndent` open through
  // every trailing blank line — but a block scalar's OWN reader (`blockScalar`, above) already
  // drops its trailing blank body lines during chomping, so there is nothing structural left for
  // those blanks to belong to. Un-mark a purely trailing run of them so the ordinary trivia peel
  // can still separate this item from whatever comes after it, instead of leaving that blank line
  // stuck as "content" and then getting a SECOND, synthetic one stacked in front of it whenever
  // the next item is regenerated (round 2's stray-double-blank-line finding).
  let end = seg.length - 1;
  while (end >= 0 && seg[end]!.trim() === "") opaque.delete(end--);
  return opaque;
}

/** Pop blank/comment lines off the end of `seg`, INTO `pendingTrivia` (in original order), but
 *  never a line the scalar scan above marked opaque — see #233 B2. Mutates both arrays. */
function peelTrailingTrivia(seg: string[], pendingTrivia: string[]): void {
  const opaque = opaqueScalarIndices(seg);
  while (seg.length && !opaque.has(seg.length - 1) && !isSignificantLine(seg[seg.length - 1]!)) {
    pendingTrivia.unshift(seg.pop()!);
  }
}

/** One top-level key's own leading trivia (the comment/blank lines that precede it, read as
 *  "about" that key) plus its full raw text: the key's own line (`header`) and everything
 *  indented under it (`content`), all as ORIGINAL, UNMODIFIED source lines. */
interface TopEntry {
  key: string;
  /** Trivia lines, then the `key: …` line itself. */
  header: string[];
  /** Everything more indented than column 0 that followed the key line, verbatim. */
  content: string[];
}

interface SplitDocument {
  /** Comment/blank lines before the very first top-level key — file-level commentary that
   *  belongs to no single field, so it is always kept rather than tied to `front`. */
  preamble: string[];
  /** One entry per top-level key, in the order the source actually wrote them. */
  entries: TopEntry[];
  /** Comment/blank lines dangling after the last top-level key's content, through EOF. */
  trailer: string[];
}

/** Is `line` (already known significant) a `-` sequence marker at exactly `indent`? Shared by
 *  the "same-indent sequence" check below and by `splitBlockItems`'s own item-boundary test. */
function isDashAt(line: string, indent: number): boolean {
  const t = stripComment(line).trim();
  return indentOf(line) === indent && (t === "-" || t.startsWith("- "));
}

/** Do two driver models differ ONLY in their `enabled` value? The splice path's guard
 *  (#1869 review round 3): the toggle is the only writer of `enabled`, and when
 *  everything else is equal the section's own lines can be reused with just that one
 *  value rewritten — the deepEqual reuse guarantee, narrowed to the one field that
 *  changed. Explicit field copy rather than a destructuring omit, so no unused-binding
 *  lint has an opinion about the field this exists to skip. */
function driverDiffersOnlyInEnabled(a: WorkflowDriver, b: WorkflowDriver): boolean {
  if (a.enabled === b.enabled) return false; // not a value flip — the deepEqual path owns it
  const rest = (d: WorkflowDriver): Omit<WorkflowDriver, "enabled"> => ({
    max_review_rounds: d.max_review_rounds,
    max_ci_attempts: d.max_ci_attempts,
    max_rebase_attempts: d.max_rebase_attempts,
    lane_timeout_minutes: d.lane_timeout_minutes,
    fix_timeout_minutes: d.fix_timeout_minutes,
    drive_timeout_minutes: d.drive_timeout_minutes,
    extra: d.extra,
  });
  return deepEqualValue(rest(a), rest(b));
}

/** Rewrite a `driver:` section's own lines so its `enabled:` line carries `value`,
 *  reusing every other line verbatim — comments and formatting included (#1869
 *  review round 3). The value is REPLACED in place when the line exists (a trailing
 *  comment on it is kept); the line is inserted ahead of the block's first field
 *  when the file never wrote one.
 *
 *  Two RESIDUALS, stated rather than absorbed (#1869 review round 5):
 *
 *  1. The bail path (null) is NOT comment-preserving. The caller's fallback is
 *     canonical regeneration, which drops the section's interior comments — the
 *     same trade every other form edit in the pane has always made for a section
 *     whose content changed. The bail fires on a shape this scan cannot rewrite
 *     in place: no field line to anchor to, or an `enabled:` value spelled
 *     anything but true/false. That residual is pinned by a test (a fixture with
 *     an interior comment and an `enabled: yes` line really does lose the
 *     comment), so it cannot silently go false.
 *
 *  2. A file carrying a DUPLICATE `enabled:` key has this rewrite touch only the
 *     FIRST occurrence, while the reader keeps the LAST. The bound: serde refuses
 *     a duplicate field, so such a file is already unloadable — the rewrite
 *     cannot make a loadable file wrong, only an unloadable one differently
 *     spelled. Not pinned: there is no behavior of a loadable file at stake. */
function spliceEnabledLine(content: readonly string[], value: boolean): string[] | null {
  const wanted = value ? "true" : "false";
  const opaque = opaqueScalarIndices(content as string[]);
  const firstField = content.find((l, i) => !opaque.has(i) && isSignificantLine(l));
  if (firstField === undefined) return null;
  const indent = indentOf(firstField);
  for (let i = 0; i < content.length; i++) {
    const line = content[i]!;
    if (opaque.has(i) || !isSignificantLine(line) || indentOf(line) !== indent) continue;
    const split = splitKey(stripComment(line).trim());
    if (!split || split.key !== "enabled") continue;
    const head = stripComment(line).trimEnd();
    if (!/(true|false)$/i.test(head)) return null; // an unexpected value spelling — bail
    const replaced = head.replace(/(true|false)$/i, wanted) + line.slice(head.length);
    return [...content.slice(0, i), replaced, ...content.slice(i + 1)];
  }
  // No `enabled:` line in the block: insert it as the first field. Leading comment
  // or blank lines stay ABOVE it — they introduce the section, and regeneration
  // would drop them entirely.
  for (let i = 0; i < content.length; i++) {
    if (!opaque.has(i) && isSignificantLine(content[i]!)) {
      return [...content.slice(0, i), `${" ".repeat(indent)}enabled: ${wanted}`, ...content.slice(i)];
    }
  }
  return null;
}

/** Split source text into its top-level keys' raw line ranges, WITHOUT re-parsing their
 *  values — this only needs to know where each key's text begins and ends so the preserving
 *  serializer can splice by that boundary. Returns null for any shape this simple scan isn't
 *  confident about (a document that doesn't open at column 0, for instance) — the caller's
 *  fallback is `serializeWorkflow`, i.e. today's behavior, never a guess that could corrupt
 *  the file. */
function splitDocument(text: string): SplitDocument | null {
  const lines = text.replace(/^﻿/, "").split(/\r?\n/);
  const entries: TopEntry[] = [];
  let pendingTrivia: string[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i]!;
    if (!isSignificantLine(line)) {
      pendingTrivia.push(line);
      i++;
      continue;
    }
    // Tabs are never a safe column to reason about (the real reader flags them as a syntax
    // error rather than a column), and a line that isn't the mapping's `key: value` shape
    // means this scan doesn't understand the document well enough to splice it safely.
    if (/^[ ]*\t/.test(line) || indentOf(line) !== 0) return null;
    const split = splitKey(stripComment(line).trim());
    if (!split) return null;
    // An ORPHAN dash sequence — a `- id: …` line at column 0 that is not itself the same-column
    // continuation of a preceding empty-rest key (that case is handled above, per-entry, via
    // `sameColumnSeq`) — is not a top-level key at all; `splitKey` only returned one here because
    // "- id: a" contains a `: ` too. Reading it as a fresh key is exactly the B1 mistake one
    // level up: the real reader's `mapping()` would have stopped at this line already (filed
    // separately — the reader silently truncates here instead of raising a finding), so trusting
    // this scan's own read of it could front-splice roster content or lose it outright. Bail.
    if (split.key.startsWith("-")) return null;
    const header = [...pendingTrivia, line];
    pendingTrivia = [];
    i++;

    // #233 B1: `blocks:` (etc.) with NOTHING after the colon may be followed by its sequence
    // at the SAME column (0) — `afterKey` (the real reader, above) accepts this, and a scan
    // that didn't would read each `- id: …` line as its own bogus top-level key, splicing
    // roster content into `front` and silently discarding everything from that point on (the
    // real reader, reading the reconstructed text top-down, hits a `-`-prefixed line where it
    // expects a key and stops there — the corruption the reviewer found). So: peek past this
    // key's own trivia for the first significant line, and if it is a same-column dash, this
    // key's content runs until a line that is neither MORE indented than 0 nor another
    // same-column dash — i.e. until an actual new key, not merely the next roster entry.
    let sameColumnSeq = false;
    if (split.rest === "") {
      let j = i;
      while (j < lines.length && !isSignificantLine(lines[j]!)) j++;
      if (j < lines.length && isDashAt(lines[j]!, 0)) sameColumnSeq = true;
    }

    const content: string[] = [];
    while (i < lines.length) {
      const l = lines[i]!;
      if (!isSignificantLine(l) || indentOf(l) !== 0) {
        content.push(l);
        i++;
        continue;
      }
      if (sameColumnSeq && isDashAt(l, 0)) {
        content.push(l);
        i++;
        continue;
      }
      break; // a genuine new top-level key
    }
    // The tail of `content` may be blank/comment lines that read as commentary on the NEXT
    // key, not this one (a section-header comment sitting just above `edges:`, say) — peel
    // them back off so they travel with whatever comes after instead of this entry. Never a
    // line inside a block scalar's body, even if it starts with `#` (#233 B2).
    peelTrailingTrivia(content, pendingTrivia);
    entries.push({ key: split.key, header, content });
  }
  const trailer = pendingTrivia;
  if (!entries.length) return { preamble: [], entries: [], trailer };
  // The document's own preamble is the leading trivia of the very first entry — peeled off
  // into its own bucket so it survives even when that first key's OWN content changes (the
  // comment at the top of the file is about the whole roster, not specifically about
  // `version:`).
  const first = entries[0]!;
  let k = 0;
  while (k < first.header.length && !isSignificantLine(first.header[k]!)) k++;
  const preamble = first.header.slice(0, k);
  entries[0] = { ...first, header: first.header.slice(k) };
  return { preamble, entries, trailer };
}

/** One roster entry's raw source lines (its own leading trivia and everything through its last
 *  field), and the column its `-` sits at. */
interface BlockItems {
  items: string[][];
  /** The indent every item's `-` was written at — 0 (same-column-as-`blocks:`) or some N>0.
   *  Whatever it is, a REGENERATED item (below) is emitted at this SAME indent, never a
   *  hardcoded one — mixing two marker indents in one YAML sequence is invalid, not just
   *  inconsistent (#233 non-blocking #2). */
  indent: number;
}

/** Split a `blocks:` key's content (everything indented under it) into one raw-line segment
 *  per roster entry, each still carrying its own leading trivia. Returns `[]` items for an
 *  empty or flow-style (`blocks: []`) roster, and `null` when the shape isn't the plain block
 *  sequence this scan understands (mixed indentation, a marker column shared with something
 *  that isn't a fresh item, …) — the caller treats `null` exactly like "nothing to reuse" and
 *  regenerates every item, at this build's own two-space indent. */
function splitBlockItems(content: string[]): BlockItems | null {
  const firstSig = content.findIndex(isSignificantLine);
  if (firstSig === -1) return { items: [], indent: 2 };
  const markerIndent = indentOf(content[firstSig]!);
  const isItemStart = (l: string): boolean => isDashAt(l, markerIndent);
  if (!isItemStart(content[firstSig]!)) return null;

  const items: string[][] = [];
  let pending: string[] = [];
  let i = 0;
  while (i < content.length) {
    if (!isSignificantLine(content[i]!)) {
      pending.push(content[i]!);
      i++;
      continue;
    }
    if (!isItemStart(content[i]!)) return null;
    const raw = [...pending, content[i]!];
    pending = [];
    i++;
    while (i < content.length && !(isSignificantLine(content[i]!) && isItemStart(content[i]!))) {
      raw.push(content[i]!);
      i++;
    }
    peelTrailingTrivia(raw, pending); // never steals a scalar body line (#233 B2)
    items.push(raw);
  }
  return { items, indent: markerIndent };
}

/** The top-level keys that are each their own preservable SECTION, in the order they
 *  take when the ORIGINAL document didn't declare them — which is the order
 *  `serializeWorkflow` writes, which is the engine's own `RawWorkflow` field order. A
 *  section the document DID declare keeps the position it already had (see the entry
 *  walk in `serializeWorkflowPreserving`): a file that puts `merge_queue:` above
 *  `blocks:` — as this repo's own does — must not have it relocated by an edit to
 *  something else.
 *
 *  Everything else at the top level (`version:`, `name:`, `authored_with:`, any key
 *  this build doesn't know) is one indivisible "front" piece, and membership here is
 *  not cosmetic: a key listed here is reused, or regenerated, on its own, while a key
 *  that isn't rides the front. Promoting `merge_queue:` out of the unknown-key bag
 *  (where it sat until #880, re-emitted as a flattened flow mapping) into this list is
 *  what lets an edit to it keep the comment lines above it, and what stops an unrelated
 *  `name:` edit from flattening it.
 *
 *  This one list is the source of BOTH the membership test and the emitter table in
 *  `serializeWorkflowPreserving` — a `Record` over exactly this union — so a section
 *  added here without an emitter is a COMPILE error rather than a section that quietly
 *  stops being written to the file at all. */
const SECTION_ORDER = [
  "blocks",
  "edges",
  "gates",
  "intake",
  "merge_queue",
  "driver",
  "resources",
  "board",
] as const;

type SectionKey = (typeof SECTION_ORDER)[number];

const TOP_SECTION_KEYS: ReadonlySet<string> = new Set<string>(SECTION_ORDER);

/** Does this `key: …` line already carry a value on it — `resources: {}`, `blocks: []`, a
 *  hand-written one-line flow mapping — as opposed to being the bare block header (`resources:`)
 *  that block-indented children are allowed to follow? Quote- and flow-aware, because it asks
 *  `splitKey`, the real reader's own splitter, rather than looking for a colon. A line this scan
 *  can't read as a key at all counts as carrying a value: the safe answer is the one that makes
 *  the caller REPLACE it rather than write children under something it doesn't understand. */
function keyLineHasInlineValue(line: string): boolean {
  const split = splitKey(stripComment(line).trim());
  return !split || split.rest !== "";
}

/** The header lines to write for a section whose BODY is being regenerated: the original's own
 *  leading trivia (the comment that introduces the SECTION — see `pushSection`), then a `key:`
 *  line that AGREES with the body about to follow it.
 *
 *  Reusing the original key line verbatim is only safe when the two forms already agree. A key
 *  line carrying an inline value cannot take block children: splicing a regenerated body under
 *  the empty-mapping form this file's own emitter writes (`emitMappingSection`) produced
 *
 *      resources: {}
 *        catfish: {}
 *
 *  which is not YAML at all — so the pane disabled the form over text it had just written itself
 *  (#1090). The inverse is as bad and silent: a section emptied back down to `key: {}` kept its
 *  bare `resources:` header, and a bare key is YAML *null*, i.e. "never declared" — deleting a
 *  section the human deliberately left empty, which is the whole reason `emitMappingSection`
 *  writes `{}` in the first place (rev-5 F4).
 *
 *  So the original key line is reused only when BOTH it and the regenerated one are bare block
 *  headers; otherwise the canonical line wins, and the original's own trailing comment rides
 *  along with it (that comment is about the section, not about the spelling that had to change).
 *  "Both carry an inline value" is NOT a reason to reuse: an inline value is the section's whole
 *  content, so keeping `resources: { build: { slots: 2 } }` over a regenerated `resources: {}`
 *  would silently undo the deletion that emptied it. */
function sectionHeaderLines(entry: TopEntry, keyLine: string): string[] {
  const trivia = entry.header.slice(0, -1);
  const original = entry.header[entry.header.length - 1]!;
  if (!keyLineHasInlineValue(original) && !keyLineHasInlineValue(keyLine)) {
    return [...trivia, original];
  }
  // Everything the comment-stripper left behind, trailing whitespace included, so `resources: {}
  // # pools` re-emits as `resources: # pools` and not as `resources:# pools`.
  const comment = original.slice(stripComment(original).trimEnd().length);
  return [...trivia, keyLine + comment];
}

/** Render the workflow the way a form or canvas edit should: reusing the ORIGINAL text's own
 *  lines — comments, blank-line runs, key order, quoting style, all of it — for every top-level
 *  piece the edit didn't touch, and falling back to the canonical emitters only for the piece
 *  that changed.
 *
 *  "Piece" is deliberately coarse — `front` (version/name/unknown top keys), each block in the
 *  roster BY ID, the whole `edges:` section, the whole `gates:` section — not a per-field diff
 *  within one of them. That is the bar #233 sets (comment-preserving for UNTOUCHED regions;
 *  "edited nodes serialize cleanly" — i.e. canonically — is enough for the parts that changed),
 *  and it is also what keeps this tractable against a hand-rolled parser: matching a whole
 *  block by id and `deepEqualValue` is a much smaller claim than re-attaching a trailing
 *  comment to the one field it happened to sit next to.
 *
 *  Falls back to `serializeWorkflow` (today's full rewrite) whenever `originalText` isn't
 *  READABLE — `isUnreadable`, the same predicate the view's `syntaxBroken` gates the form on
 *  (#233 B3), not the broader `hasErrors` (a `version-unsupported` file is still editable here,
 *  and must not silently lose its comments on the first edit just because *some* finding fired)
 *  — or when this scan doesn't trust its own read of the top-level shape. Always the SAFE
 *  direction, never a guess that could reuse text for content it no longer describes.
 *
 *  The original text's own line ending is kept for the whole output (CRLF in, CRLF out) —
 *  `splitDocument` reads via `split(/\r?\n/)`, which strips every `\r`, so every line this
 *  function handles (reused or freshly generated) is already EOL-free until the final join. */
export function serializeWorkflowPreserving(w: Workflow, originalText: string): string {
  const parsedOriginal = parseWorkflow(originalText);
  if (isUnreadable(parsedOriginal.findings)) return serializeWorkflow(w);
  const doc = splitDocument(originalText);
  if (!doc) return serializeWorkflow(w);

  const orig = parsedOriginal.workflow;
  const order = blockOrder(w);
  const out: string[] = [...doc.preamble];

  // ---- front: version, name, authored_with, unknown top-level keys ----
  const frontEntries = doc.entries.filter((e) => !TOP_SECTION_KEYS.has(e.key));
  const frontUnchanged =
    w.version === orig.version &&
    w.name === orig.name &&
    w.authored_with === orig.authored_with &&
    deepEqualValue(w.extra, orig.extra);
  // Reused, the front pieces stay exactly where the file put them (the walk below emits
  // each at its own position, interleaved with the sections). REGENERATED, they go to the
  // top as one canonical group — which is where `emitFrontLines` has always put them, and
  // the only position that reads right when the file didn't declare them at all.
  const reuseFront = frontUnchanged && frontEntries.length > 0;
  if (!reuseFront) out.push(...emitFrontLines(w));

  // ---- blocks, matched by id (the one thing about a block that never changes — see the
  // module comment at the top of this file) ----
  //
  // A reused `header`/`raw` segment already carries whatever blank line originally separated
  // it from what came before (the scan in `splitDocument`/`splitBlockItems` peels exactly that
  // trivia onto the FOLLOWING entry/item) — so a synthetic `""` is only ever pushed ahead of a
  // FRESHLY regenerated line, never ahead of reused text, or every section gains a blank line
  // it didn't have.
  //
  // NOTE (reorder): a block is matched by id, not by position, so its own comment travels WITH
  // it if the roster gets reordered by hand (in the raw YAML) — a deliberate property, not a
  // bug. What is NOT preserved across a reorder is the blank-line spacing BETWEEN items: each
  // item's leading trivia was captured relative to its ORIGINAL neighbor, so after a reorder it
  // separates a different pair than it used to. The result is still valid YAML and never loses
  // a comment; it can just look unevenly spaced. Fixing that needs re-deriving spacing from the
  // NEW neighbor at every reuse, which is more machinery than the cosmetic cost justifies here,
  // and the pane's own UI has no "reorder" gesture — this only arises from a hand edit.
  const pushBlocks = (blocksEntry: TopEntry | undefined): void => {
    if (!w.blocks.length) {
      // Same header/content split as the non-empty case below (and every other section): the
      // comment introducing the ROSTER ("# BLOCKS — the agents a run may use…") is about the
      // section, not about any one block, so it survives the roster being emptied out too —
      // only the LAST line of the header (the `blocks:`/`blocks: […]` key line itself) is
      // replaced with the canonical empty form.
      if (blocksEntry) out.push(...sectionHeaderLines(blocksEntry, "blocks: []"));
      else out.push("", "blocks: []");
      return;
    }
    const split = blocksEntry ? splitBlockItems(blocksEntry.content) : null;
    const reusable = !!split && split.items.length === orig.blocks.length;
    const targetIndent = split?.indent ?? 2;
    const origById = new Map<string, { block: WorkflowBlock; raw: string[] }>();
    if (reusable) {
      orig.blocks.forEach((b, i) => {
        if (b.id && !origById.has(b.id)) origById.set(b.id, { block: b, raw: split!.items[i]! });
      });
    }
    // The comment introducing the SECTION (not any one block) is reused whenever we have one to
    // reuse, independent of which items below it changed — but the `blocks:` line itself only
    // when it is a bare block header: a roster written `blocks: []` and then given its first
    // entry has to lose the `[]`, or the items land under a line that can't take them (#1090).
    if (blocksEntry) out.push(...sectionHeaderLines(blocksEntry, "blocks:"));
    else out.push("", "blocks:");
    let firstItem = true;
    for (const b of w.blocks) {
      const match = b.id ? origById.get(b.id) : undefined;
      if (match && deepEqualValue(b, match.block)) {
        out.push(...match.raw);
      } else {
        if (!firstItem) out.push("");
        out.push(...emitBlockLines(b, targetIndent));
      }
      firstItem = false;
    }
  };

  /** Every section that is not `blocks:` — one shape, because they all want the same one.
   *
   *  The COMMENT INTRODUCING the section (e.g. "# ADVISORY — the declared happy path") is reused
   *  whenever there is one, independent of whether the content changed: regenerating the whole
   *  section including that comment meant deleting one edge dropped a comment that was never
   *  about that edge (#233 non-blocking #1). Only the CONTENT falls back to canonical, and only
   *  when it changed.
   *
   *  The `key:` line is NOT part of what gets reused unconditionally — it is a function of the
   *  content that follows it, so a regenerated body re-derives it through `sectionHeaderLines`
   *  (the reused one may be an empty map/sequence that block children can't legally follow, or
   *  a bare key that re-reads as undeclared — #1090). That helper is where the rule, and what
   *  happens to a trailing comment on the key line, is spelled out.
   *
   *  `present` is "the model still has something to write here": with an entry that no longer
   *  matches and nothing to write, the section is GONE, and falling through to the else-branch
   *  (which emits nothing for empty `lines`) is what deletes it — the introducing comment
   *  included, since it has no section left to introduce. */
  const pushSection = (
    entry: TopEntry | undefined,
    unchanged: boolean,
    present: boolean,
    lines: string[]
  ): void => {
    if (entry && unchanged) {
      out.push(...entry.header, ...entry.content);
    } else if (entry && present && lines.length) {
      out.push(...sectionHeaderLines(entry, lines[0]!), ...lines.slice(1));
    } else if (lines.length) {
      out.push("", ...lines);
    }
  };

  // A Record over `SectionKey`, not a switch: TypeScript demands an entry for every
  // member, so a section added to SECTION_ORDER with no emitter here fails the build
  // instead of silently vanishing from every file it is written in.
  const pushKey: Record<SectionKey, (entry: TopEntry | undefined) => void> = {
    blocks: (entry) => pushBlocks(entry),
    edges: (entry) =>
      pushSection(
        entry,
        deepEqualValue(w.edges, orig.edges),
        w.edges.length > 0,
        emitEdgesLines(w.edges, order)
      ),
    gates: (entry) =>
      pushSection(
        entry,
        deepEqualValue(w.gates, orig.gates),
        !!w.gates.merge || !!w.gates.extra,
        emitGatesLines(w, order)
      ),
    intake: (entry) =>
      pushSection(
        entry,
        deepEqualValue(w.intake, orig.intake),
        !!w.intake,
        w.intake ? emitIntakeLines(w.intake) : []
      ),
    merge_queue: (entry) =>
      pushSection(
        entry,
        deepEqualValue(w.merge_queue, orig.merge_queue),
        !!w.merge_queue,
        w.merge_queue ? emitMergeQueueLines(w.merge_queue) : []
      ),
    driver: (entry) => {
      // The toggle flips ONE field's value (#1869 review round 3), so the section's
      // own lines are reused with just the `enabled:` line rewritten — the same
      // guarantee the deepEqual path gives untouched sections, narrowed to the one
      // line that changed. This is NOT the re-attachment the module header rules
      // out: no field changed underneath a comment, only a value. Any other edit
      // falls through to `pushSection` and its canonical regeneration, exactly as
      // every other section's edits always have.
      const spliced =
        entry && w.driver && orig.driver && driverDiffersOnlyInEnabled(w.driver, orig.driver)
          ? spliceEnabledLine(entry.content, w.driver.enabled === true)
          : null;
      if (entry && spliced) {
        out.push(...entry.header, ...spliced);
        return;
      }
      pushSection(
        entry,
        deepEqualValue(w.driver, orig.driver),
        !!w.driver,
        w.driver ? emitDriverLines(w.driver) : []
      );
    },
    resources: (entry) =>
      pushSection(
        entry,
        deepEqualValue(w.resources, orig.resources),
        !!w.resources,
        w.resources ? emitResourcesLines(w.resources) : []
      ),
    board: (entry) =>
      pushSection(
        entry,
        deepEqualValue(w.board, orig.board),
        !!w.board,
        w.board ? emitBoardLines(w.board) : []
      ),
  };

  // The document's OWN order is the output's order (#880): walk the entries as the file
  // wrote them, and only sections the file never declared get appended, in canonical order.
  // A fixed emission order would have been fine while every known section happened to be
  // declared in that order — and stopped being fine the moment `merge_queue:` became a
  // section, because this repo's own workflow writes it ABOVE `blocks:`, so any edit would
  // have relocated it (and the six comment lines that introduce it) to the bottom of the file.
  const seen = new Set<SectionKey>();
  for (const e of doc.entries) {
    if (!TOP_SECTION_KEYS.has(e.key)) {
      if (reuseFront) out.push(...e.header, ...e.content);
      continue; // otherwise the canonical front group already went out, above
    }
    const key = e.key as SectionKey; // TOP_SECTION_KEYS is built from SECTION_ORDER
    if (seen.has(key)) continue; // a duplicate top-level key: the reader kept one, so emit one
    seen.add(key);
    pushKey[key](e);
  }
  for (const key of SECTION_ORDER) if (!seen.has(key)) pushKey[key](undefined);

  if (doc.trailer.length) out.push(...doc.trailer);

  const eol = originalText.includes("\r\n") ? "\r\n" : "\n";
  const text = out.join(eol);
  return text.endsWith(eol) ? text : text + eol;
}

// ---------- parse: text → model ----------

export interface ParseResult {
  workflow: Workflow;
  /** Syntax + shape findings. SEMANTIC findings (dangling edges, unknown kinds …) come
   *  from `validateWorkflow` — split because the pane re-validates a model the human is
   *  editing in the form, where there is no text to have a syntax error in. */
  findings: Finding[];
}

const asString = (v: YamlValue): string | null =>
  typeof v === "string" ? v : typeof v === "number" || typeof v === "boolean" ? String(v) : null;

// The keys this build knows, per section. They mirror the engine's `Raw*` structs
// (`crates/loomux-engine/src/workflow.rs`) — the same set `src/workflow-schema.json`
// declares, which `test/workflowschema.test.ts` pins field for field. Hand-written
// rather than read from the manifest on purpose: this module is pure and import-free
// (see the header — its ONE import is a type), and a data file it had to load at
// startup would be a second way for the pane to fail to open a file. The test is the
// link between the two, and it is cheaper than the coupling would be.
const KNOWN_TOP = new Set([
  "version",
  "name",
  "authored_with",
  "blocks",
  "edges",
  "gates",
  "intake",
  "merge_queue",
  "driver",
  "resources",
  "board",
]);
/** The block keys this build knows — the pane's half of the #880 schema
 *  lockstep, EXPORTED so a test can read it as a set.
 *
 *  Exported for one reason, and it is not convenience: the manifest -> pane
 *  direction was already pinned (every declared field is read), but pane ->
 *  manifest was not, so a key added HERE reddened nothing. That asymmetry is
 *  load-bearing for `remote:` (#1457): the whole argument for the key is that a
 *  repo file may not author a destination, and the pane's half of that is that
 *  no destination-shaped key is a field. An 8-name test enumerating
 *  `host`/`port`/… catches only the names it lists; a set equality against the
 *  manifest catches every name nobody thought of.
 *  `test/workflowschema.test.ts` holds both directions. */
export const KNOWN_BLOCK = new Set([
  "id",
  "name",
  "kind",
  "cli",
  "model",
  "prompt",
  "profile",
  "allow",
  "role_hint",
  "effort",
  "context",
  "remote",
]);
/** `gates:` is a MAP keyed by gate name, not a fixed struct: the engine reads it as
 *  `BTreeMap<String, RawGate>`, so a `release:` gate parses fine — loomux simply
 *  enforces none but `merge`. That is why a key here lands in `extra` WITHOUT an
 *  `unknown-key` finding, unlike every other section's leftovers. */
const KNOWN_GATE = new Set(["merge"]);
const KNOWN_INTAKE = new Set(["source", "labels"]);
const KNOWN_INTAKE_LABELS = new Set(INTAKE_LABEL_KEYS);
const KNOWN_MERGE_QUEUE = new Set(["enabled", "max_batch", "checks_timeout_minutes"]);
const KNOWN_DRIVER = new Set([
  "enabled",
  "max_review_rounds",
  "max_ci_attempts",
  "max_rebase_attempts",
  "lane_timeout_minutes",
  "fix_timeout_minutes",
  "drive_timeout_minutes",
]);
const KNOWN_RESOURCE = new Set(["slots", "max_hold_minutes"]);
const KNOWN_BOARD = new Set(["wip", "enforce"]);
const KNOWN_WIP = new Set<string>(WIP_STATUSES);

function collectExtra(
  obj: Record<string, YamlValue>,
  known: Set<string>
): Record<string, YamlValue> | undefined {
  const extra: Record<string, YamlValue> = {};
  for (const k of Object.keys(obj)) if (!known.has(k)) extra[k] = obj[k]!;
  return Object.keys(extra).length ? extra : undefined;
}

/** A nested-mapping section's raw value, or `null` when there is nothing to read.
 *
 *  YAML null — `intake:` with nothing after it — means ABSENT, the same reading the
 *  engine's `Option<RawIntake>` gives it, so it produces no section and no finding.
 *  A scalar or a list where a mapping belongs is a shape finding: the engine would
 *  refuse the whole file over it, and reporting nothing would leave the pane calling
 *  a file valid that cannot load. */
function readSection(
  v: YamlValue | undefined,
  where: string,
  findings: Finding[]
): Record<string, YamlValue> | null {
  if (v === undefined || v === null) return null;
  if (typeof v !== "object" || Array.isArray(v)) {
    findings.push({
      severity: "error",
      code: "section-not-a-mapping",
      message: `${where}: must be a mapping (found ${emitValue(v)}).`,
    });
    return null;
  }
  return v as Record<string, YamlValue>;
}

/** A field whose TYPE is wrong — `max_batch: soon`, `enabled: yes please`. Rejected,
 *  never coerced: the engine's serde would refuse the file, and a pane that quietly
 *  read `soon` as "the default" would be lying about what is going to run. */
function badValue(where: string, want: string, got: YamlValue): Finding {
  return {
    severity: "error",
    code: "section-bad-value",
    message: `${where}: must be ${want} (found ${emitValue(got)}).`,
  };
}

/** Read one declared-or-absent number field. Absent stays absent — the engine
 *  resolves an omitted number against its own default, so writing one in here would
 *  turn "inherit" into "pin" on the next save. */
function readNumberField(
  r: Record<string, YamlValue>,
  key: string,
  where: string,
  findings: Finding[]
): number | undefined {
  const v = r[key];
  if (v === undefined) return undefined;
  if (typeof v === "number") return v;
  findings.push(badValue(`${where}.${key}`, "a number", v));
  return undefined;
}

function readIntake(r: Record<string, YamlValue>, findings: Finding[]): WorkflowIntake {
  const intake: WorkflowIntake = {};
  if (r.source !== undefined) intake.source = asString(r.source) ?? "";
  const labels = readSection(r.labels, "intake.labels", findings);
  if (labels) {
    const out: WorkflowIntakeLabels = {};
    for (const key of INTAKE_LABEL_KEYS) {
      if (labels[key] !== undefined) out[key] = asString(labels[key]!) ?? "";
    }
    const extra = collectExtra(labels, KNOWN_INTAKE_LABELS);
    if (extra) out.extra = extra;
    intake.labels = out;
  }
  const extra = collectExtra(r, KNOWN_INTAKE);
  if (extra) intake.extra = extra;
  return intake;
}

function readMergeQueue(r: Record<string, YamlValue>, findings: Finding[]): WorkflowMergeQueue {
  const mq: WorkflowMergeQueue = {};
  if (r.enabled !== undefined) {
    if (typeof r.enabled === "boolean") mq.enabled = r.enabled;
    else findings.push(badValue("merge_queue.enabled", "true or false", r.enabled));
  }
  const batch = readNumberField(r, "max_batch", "merge_queue", findings);
  if (batch !== undefined) mq.max_batch = batch;
  const timeout = readNumberField(r, "checks_timeout_minutes", "merge_queue", findings);
  if (timeout !== undefined) mq.checks_timeout_minutes = timeout;
  const extra = collectExtra(r, KNOWN_MERGE_QUEUE);
  if (extra) mq.extra = extra;
  return mq;
}

function readDriver(r: Record<string, YamlValue>, findings: Finding[]): WorkflowDriver {
  const dv: WorkflowDriver = {};
  if (r.enabled !== undefined) {
    if (typeof r.enabled === "boolean") dv.enabled = r.enabled;
    else findings.push(badValue("driver.enabled", "true or false", r.enabled));
  }
  const rounds = readNumberField(r, "max_review_rounds", "driver", findings);
  if (rounds !== undefined) dv.max_review_rounds = rounds;
  const ci = readNumberField(r, "max_ci_attempts", "driver", findings);
  if (ci !== undefined) dv.max_ci_attempts = ci;
  const rebase = readNumberField(r, "max_rebase_attempts", "driver", findings);
  if (rebase !== undefined) dv.max_rebase_attempts = rebase;
  const lane = readNumberField(r, "lane_timeout_minutes", "driver", findings);
  if (lane !== undefined) dv.lane_timeout_minutes = lane;
  const fix = readNumberField(r, "fix_timeout_minutes", "driver", findings);
  if (fix !== undefined) dv.fix_timeout_minutes = fix;
  const drive = readNumberField(r, "drive_timeout_minutes", "driver", findings);
  if (drive !== undefined) dv.drive_timeout_minutes = drive;
  const extra = collectExtra(r, KNOWN_DRIVER);
  if (extra) dv.extra = extra;
  return dv;
}

/** `resources:` is a MAP of repo-chosen names, so every key here is a resource, never
 *  an unknown one. A name written with nothing under it (`build:`) reads as a resource
 *  DECLARED WITH DEFAULTS — that is what a human means by it — and re-emits as
 *  `build: {}`, which is the spelling the engine's serde actually accepts. */
function readResources(
  r: Record<string, YamlValue>,
  findings: Finding[]
): Record<string, WorkflowResource> {
  const out: Record<string, WorkflowResource> = {};
  for (const name of Object.keys(r)) {
    const where = `resources.${name}`;
    const body = readSection(r[name], where, findings);
    const res: WorkflowResource = {};
    if (body) {
      const slots = readNumberField(body, "slots", where, findings);
      if (slots !== undefined) res.slots = slots;
      const hold = readNumberField(body, "max_hold_minutes", where, findings);
      if (hold !== undefined) res.max_hold_minutes = hold;
      const extra = collectExtra(body, KNOWN_RESOURCE);
      if (extra) res.extra = extra;
    }
    out[name] = res;
  }
  return out;
}

/** `board:` (#1175) — per-status WIP limits plus one posture bool.
 *
 *  `wip:` is read against `WIP_STATUSES` rather than as an open map: the engine's
 *  `RawWip` is a closed struct, so `in-porgress: 4` is a file that will not load, and a
 *  pane that quietly preserved it as an unremarked key would call that file valid. The
 *  unknown key is still PRESERVED (`wipExtra`) — dropping it is destructive — it is just
 *  also reported, which is the honest pair `collectExtra` establishes everywhere else. */
function readBoard(r: Record<string, YamlValue>, findings: Finding[]): WorkflowBoard {
  const board: WorkflowBoard = {};
  const wipSection = readSection(r.wip, "board.wip", findings);
  if (wipSection) {
    const wip: WorkflowWip = {};
    for (const status of WIP_STATUSES) {
      const n = readNumberField(wipSection, status, "board.wip", findings);
      if (n !== undefined) wip[status] = n;
    }
    board.wip = wip;
    const extra = collectExtra(wipSection, KNOWN_WIP);
    if (extra) board.wipExtra = extra;
  }
  if (r.enforce !== undefined) {
    if (typeof r.enforce === "boolean") board.enforce = r.enforce;
    else findings.push(badValue("board.enforce", "true or false", r.enforce));
  }
  const extra = collectExtra(r, KNOWN_BOARD);
  if (extra) board.extra = extra;
  return board;
}

/** Read a workflow file. NEVER throws and NEVER refuses: a file it cannot fully
 *  understand still yields a workflow (with stub blocks) plus the findings that say why,
 *  because the pane's job is to let the human FIX the file — which it cannot do if the
 *  file won't open. */
export function parseWorkflow(text: string): ParseResult {
  // Strip a BOM. A workflow file written by a Windows editor (or by `Set-Content` without
  // `-Encoding utf8NoBOM`) starts with U+FEFF, and the reader would otherwise take it as part
  // of the first KEY — so `version: 1` arrived as a key named "﻿version", the version
  // read as missing, and the pane reported a file the human could see was right as broken.
  // It is invisible, so nothing about the error message could have led them to the cause.
  const reader = new YamlReader(text.replace(/^﻿/, "").split(/\r?\n/));
  const doc = reader.document();
  const findings = reader.findings;
  const w: Workflow = { version: WORKFLOW_VERSION, name: "", blocks: [], edges: [], gates: {} };

  if (doc === null || typeof doc !== "object" || Array.isArray(doc)) {
    if (text.trim()) {
      findings.push({
        severity: "error",
        code: "not-a-mapping",
        message: "A workflow file is a mapping with version:, blocks: and (optionally) edges: / gates:.",
      });
    }
    return { workflow: w, findings };
  }
  const root = doc as Record<string, YamlValue>;

  if (root.version === undefined) {
    findings.push({
      severity: "error",
      code: "version-missing",
      message: `No version: — this file should declare "version: ${WORKFLOW_VERSION}".`,
    });
  } else if (typeof root.version !== "number") {
    findings.push({
      severity: "error",
      code: "version-unsupported",
      message: `version: must be a number (found "${String(root.version)}").`,
    });
  } else {
    w.version = root.version;
    if (root.version !== WORKFLOW_VERSION) {
      findings.push({
        severity: "error",
        code: "version-unsupported",
        message: `version: ${root.version} is not supported by this build of orrerix (it reads version ${WORKFLOW_VERSION}).`,
      });
    }
  }

  w.name = asString(root.name ?? "") ?? "";
  // Declared-or-absent, never defaulted: an empty `authored_with:` is a file that says
  // it doesn't know, and a missing one is a file that never claimed to.
  if (root.authored_with !== undefined) w.authored_with = asString(root.authored_with) ?? "";
  w.extra = collectExtra(root, KNOWN_TOP);

  // `blocks:` / `edges:` written with nothing after them are YAML null, and null here means
  // EMPTY — an empty roster, no edges. Only a value that is present and is not a list is a
  // shape error (rev-5 F4): reporting "must be a list" against an empty one would have the
  // pane complain about the file it just wrote itself when you delete the last block.
  const blocks = root.blocks;
  if (blocks !== undefined && blocks !== null && !Array.isArray(blocks)) {
    findings.push({
      severity: "error",
      code: "block-not-a-mapping",
      message: "blocks: must be a list of blocks.",
    });
  } else if (Array.isArray(blocks)) {
    blocks.forEach((raw, i) => w.blocks.push(readBlock(raw, i, findings)));
  }

  const edges = root.edges;
  if (edges !== undefined && edges !== null && !Array.isArray(edges)) {
    findings.push({
      severity: "error",
      code: "edge-not-a-mapping",
      message: "edges: must be a list of { from: …, to: … } entries.",
    });
  } else if (Array.isArray(edges)) {
    edges.forEach((raw, i) => w.edges.push(...readEdge(raw, i, findings)));
  }

  const gates = root.gates;
  if (gates !== undefined && (typeof gates !== "object" || gates === null || Array.isArray(gates))) {
    findings.push({
      severity: "error",
      code: "gate-unknown-require",
      message: "gates: must be a mapping (today the only gate is `merge`).",
    });
  } else if (gates && typeof gates === "object" && !Array.isArray(gates)) {
    const g = gates as Record<string, YamlValue>;
    if (g.merge !== undefined) w.gates.merge = readGate(g.merge, findings);
    w.gates.extra = collectExtra(g, KNOWN_GATE);
  }

  // The policy sections (#382 / #581 / #858). Each is optional and each is absent
  // rather than defaulted when the file says nothing — see `WorkflowIntake`.
  const intake = readSection(root.intake, "intake", findings);
  if (intake) w.intake = readIntake(intake, findings);
  const mergeQueue = readSection(root.merge_queue, "merge_queue", findings);
  if (mergeQueue) w.merge_queue = readMergeQueue(mergeQueue, findings);
  const driver = readSection(root.driver, "driver", findings);
  if (driver) w.driver = readDriver(driver, findings);
  const resources = readSection(root.resources, "resources", findings);
  if (resources) w.resources = readResources(resources, findings);
  const board = readSection(root.board, "board", findings);
  if (board) w.board = readBoard(board, findings);

  return { workflow: w, findings };
}

/** One block, ALWAYS — a malformed entry becomes a stub with the findings that explain
 *  it, never a dropped row. A block you cannot see is a block you cannot repair. */
function readBlock(raw: YamlValue, index: number, findings: Finding[]): WorkflowBlock {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    findings.push({
      severity: "error",
      code: "block-not-a-mapping",
      message: `blocks[${index}] is not a block mapping (expected id:, name:, kind:, cli: …).`,
    });
    return { id: "", name: `block ${index + 1}`, kind: "", cli: "", model: "" };
  }
  const r = raw as Record<string, YamlValue>;
  const id = asString(r.id ?? "") ?? "";
  const block: WorkflowBlock = {
    id,
    name: asString(r.name ?? "") ?? id,
    kind: asString(r.kind ?? "") ?? "",
    cli: asString(r.cli ?? "") ?? "",
    model: asString(r.model ?? "") ?? "",
    extra: collectExtra(r, KNOWN_BLOCK),
  };
  if (r.prompt !== undefined) block.prompt = asString(r.prompt) ?? "";
  if (r.profile !== undefined) block.profile = asString(r.profile) ?? "";
  // A list, or a finding — never a coerced scalar. `allow: Bash(git push)` (no
  // brackets) is a file the engine refuses, and pretending it declared nothing would
  // hide a line whose whole purpose is to pre-approve a tool.
  if (r.allow !== undefined) {
    if (Array.isArray(r.allow)) block.allow = r.allow.map((v) => asString(v) ?? "");
    else findings.push(badValue(`blocks[${index}].allow`, "a list of tool patterns", r.allow));
  }
  if (r.role_hint !== undefined) block.role_hint = asString(r.role_hint) ?? "";
  // #687. `undefined` (never declared) and `""` (declared empty) are kept apart
  // the way role_hint keeps them, so a save can't turn one into the other.
  if (r.effort !== undefined) block.effort = asString(r.effort) ?? "";
  if (r.context !== undefined) block.context = asString(r.context) ?? "";
  // #1457. Read as written — the label is validated, never rewritten, so what the
  // pane shows and what the engine refuses are the same string.
  //
  // A NULL is not an empty label, and the two are a real difference here rather
  // than a pedantic one: a bare `remote:` line is YAML null, which the engine
  // reads into `Option<String>` as None — a local block, loaded fine — while
  // `remote: ""` is `Some("")` and is REFUSED (`check_segment` -> Empty). The
  // `?? ""` idiom the neighbouring fields use would collapse the two, and the
  // pane would then paint a file red that the engine loads. So a null is treated
  // as the absent key it means, which is also what the engine's own error would
  // say if you asked it.
  const remote = asString(r.remote);
  if (remote !== null) block.remote = remote;
  return block;
}

/** `{ from: x, to: y }` or `{ from: x, to: [a, b] }` — the fan-out form expands into one
 *  flat edge per target, because that is what every graph question is asked of. */
function readEdge(raw: YamlValue, index: number, findings: Finding[]): WorkflowEdge[] {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    findings.push({
      severity: "error",
      code: "edge-not-a-mapping",
      message: `edges[${index}] is not a { from: …, to: … } mapping.`,
    });
    return [];
  }
  const r = raw as Record<string, YamlValue>;
  const from = asString(r.from ?? "") ?? "";
  const targets = Array.isArray(r.to) ? r.to : r.to === undefined ? [] : [r.to];
  if (!from || !targets.length) {
    findings.push({
      severity: "error",
      code: "edge-not-a-mapping",
      message: `edges[${index}] needs both a from: and a to:.`,
    });
    return [];
  }
  return targets.map((t) => ({ from, to: asString(t) ?? "" }));
}

function readGate(raw: YamlValue, findings: Finding[]): MergeGate {
  const gate: MergeGate = { require: "all-pass", reviewers: [], also: [] };
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    findings.push({
      severity: "error",
      code: "gate-unknown-require",
      message: "gates.merge must be a mapping (require:, reviewers:, …).",
    });
    return gate;
  }
  const r = raw as Record<string, YamlValue>;
  // `threshold: N` with NO `require:` key is a threshold gate — the engine's own rule
  // ("`threshold: N` alone implies a threshold gate; spelling `require: threshold` as well is
  // allowed but redundant", workflow.rs `(Some("threshold") | None, Some(n))`). Defaulting the
  // absent key to "all-pass" here made the pane read such a file as an all-pass gate that
  // happens to carry a number, with three consequences, all silent: the threshold rules below
  // never ran on it, `withGateReviewers` never clamped it, and — worst — the next gate edit
  // re-serialized it as `require: all-pass` + `threshold: N`, a PAIR the engine refuses
  // outright, so the group fell back to the built-in roster over an unrelated edit.
  //
  // ONLY when the key is absent. An empty or non-string `require:` is left exactly as it was:
  // the engine refuses `require: ""` as an unknown value, and quietly reading it as something
  // else is the same lie in the other direction.
  gate.require =
    r.require === undefined
      ? typeof r.threshold === "number"
        ? "threshold"
        : "all-pass"
      : asString(r.require) ?? "all-pass";
  if (typeof r.threshold === "number") gate.threshold = r.threshold;
  else if (r.threshold !== undefined) {
    findings.push({
      severity: "error",
      code: "gate-bad-threshold",
      message: `gates.merge.threshold must be a number (found "${String(r.threshold)}").`,
    });
  }
  const list = (v: YamlValue): string[] =>
    Array.isArray(v) ? v.map((x) => asString(x) ?? "").filter(Boolean) : [];
  gate.reviewers = list(r.reviewers ?? []);
  gate.also = list(r.also ?? []);
  // #1174. Read the same way `threshold` is — a non-number is a finding, never a
  // coerced value — because `MergeGate` has no unknown-key bag: a key this function
  // does not read is a line the next form edit DELETES.
  if (typeof r.max_diff_lines === "number") gate.max_diff_lines = r.max_diff_lines;
  else if (r.max_diff_lines !== undefined) {
    findings.push({
      severity: "error",
      code: "gate-bad-max-diff-lines",
      message: `gates.merge.max_diff_lines must be a number (found "${String(r.max_diff_lines)}").`,
    });
  }
  // #1176. Same rule as `max_diff_lines` above and for a sharper version of the
  // same reason: what a dropped key costs here is a REQUIRED REVIEWER. So a
  // routing block this reader cannot make sense of is a finding plus an
  // entry kept as-far-as-it-was-read, never a silent omission.
  if (Array.isArray(r.routing)) {
    const rules: RoutingRule[] = [];
    r.routing.forEach((raw, i) => {
      if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
        findings.push({
          severity: "error",
          code: "gate-bad-routing",
          message: `gates.merge.routing[${i}] must be a { paths: […], reviewers: […] } mapping.`,
        });
        return;
      }
      const rr = raw as Record<string, YamlValue>;
      for (const key of Object.keys(rr)) {
        if (key !== "paths" && key !== "reviewers") {
          findings.push({
            severity: "error",
            code: "gate-bad-routing",
            message: `gates.merge.routing[${i}]: unknown key "${key}" — a routing rule takes paths: and reviewers: only. The engine refuses the whole file over one, so this pane will not bless it.`,
          });
        }
      }
      rules.push({ paths: list(rr.paths ?? []), reviewers: list(rr.reviewers ?? []) });
    });
    if (rules.length) gate.routing = rules;
  } else if (r.routing !== undefined) {
    findings.push({
      severity: "error",
      code: "gate-bad-routing",
      message: `gates.merge.routing must be a list of rules (found "${String(r.routing)}").`,
    });
  }
  return gate;
}

// ---------- validate: the pre-run pass ----------

/** How a caller answers "what can this block's cli and model actually carry?" —
 *  normally `(cli, model) => knobState(fetched[cli] ?? null, cli, model)`
 *  (selectorknobs.ts). **`null` means "not known yet"**, and is not the same
 *  answer as "cannot": the pane fetches `agent_cli_knobs` per CLI
 *  asynchronously, and a knob check that ran before the reply landed would
 *  invent a finding out of its own ignorance.
 *
 *  Injected rather than imported so this module's "capability is the backend's
 *  to state, never mirrored here" rule holds at the module-graph level too: the
 *  pure model has no runtime dependency on the capability layer at all. */
export type KnobLookup = (cli: string, model: string) => KnobStates | null;

/** The `effort:` / `context:` half of the pre-run pass (#687).
 *
 *  Three ways a knob is undeliverable, all of them a REFUSAL on the real engine
 *  (`validate_knob`, workflow.rs) rather than a silent no-op, so a pane that
 *  reported the file clean would send a human to a launch that quietly falls back
 *  to the built-in roster:
 *
 *    - a value outside the CLI's own vocabulary (`effort: banana`);
 *    - a CLI with no seam for the knob at all (copilot's effort);
 *    - a model the knob has no documented form on — `haiku` + `context: 1m`
 *      composes `--model haiku[1m]`, which is #709's carried finding, caught here
 *      at the surface a human hand-writes the file in.
 *
 *  **It DEFERS rather than guesses.** A `lookup` that returns `null` — no caps
 *  fetched for this block's CLI yet, or none passed at all — produces no
 *  findings, exactly as the real parser skips the CLI half for a block with no
 *  explicit `cli:` and lets `clamped()` re-check with the resolved CLI in hand. */
function knobFindings(b: WorkflowBlock, where: string, lookup: KnobLookup | undefined): Finding[] {
  if (b.effort === undefined && b.context === undefined) return [];
  const states = lookup?.(b.cli.trim(), b.model);
  if (!states) return [];
  const out: Finding[] = [];
  for (const [key, declared, state] of [
    ["effort", b.effort, states.effort],
    ["context", b.context, states.context],
  ] as const) {
    const v = (declared ?? "").trim();
    if (!v) continue; // absent or empty = the CLI's own default, always legal
    if (state.enabled && state.values.includes(v.toLowerCase())) continue;
    out.push({
      severity: "error",
      code: "knob-unavailable",
      message:
        `Block "${where}" declares ${key}: ${v}, which orrerix cannot deliver on ${b.cli} — ` +
        (state.reason || `${key} must be one of ${state.values.join(", ")}.`),
      blockId: b.id,
    });
  }
  return out;
}

// ONE definition for both reviewer lists — the gate's own and every routing
// rule's (#1176). They ask the same question ("could a verdict for this id
// ever be recorded?"), and answering it twice is how the static list ends up
// refusing a manager while a routing rule quietly accepts one. `subject`
// names which list, so the finding points at the line to fix; the backend's
// `gate_reviewer_error` is the same function on the other side. It sits at module
// scope rather than inside `validateWorkflow` because the CANVAS asks it too, before
// it lets a rubber band land on the gate (`gateConnectionError`, #1388) — the pane
// refusing a drop for a different reason than it reports a finding for is the same
// drift, one gesture earlier.
function gateReviewerFinding(
  byId: ReadonlyMap<string, WorkflowBlock>,
  subject: string,
  id: string
): Finding | null {
  const b = byId.get(id);
  if (!b) {
    return {
      severity: "error",
      code: "gate-unknown-reviewer",
      message: `${subject} requires a verdict from "${id}", but no block has that id — the gate could never open.`,
    };
  }
  if (b.kind === "manager") {
    // Reached before the generic kind arm below, which would otherwise
    // describe this as a type error ("that block's kind is manager").
    // An author who named the manager on a gate meant "the human signs
    // off", which is real and which this gate cannot express — so say
    // that. Mirrors the backend's own arm in `parse_workflow` (#1161).
    return {
      severity: "error",
      code: "gate-not-a-reviewer",
      message: `${subject} names "${id}" as a reviewer, but that block is the manager — the human's interface, which records no verdict, so the gate could never open. A gate reads reviewer verdicts; the human's own sign-off is the merge gate orrerix already applies on top of it.`,
      blockId: id,
    };
  }
  if (b.kind !== "reviewer") {
    return {
      severity: "error",
      code: "gate-not-a-reviewer",
      message: `${subject} names "${id}" as a reviewer, but that block's kind is "${b.kind || "(none)"}" — only a reviewer records a verdict.`,
      blockId: id,
    };
  }
  if (b.role_hint?.trim().toLowerCase() === "liaison") {
    // Reviewer-KIND, but a liaison never records a verdict (#891) — so a
    // gate naming one waits on something no code path can produce. Same
    // unsatisfiable-gate finding as the arm above, one kind further in;
    // mirrors the backend's own refusal in `parse_workflow`.
    return {
      severity: "error",
      code: "gate-not-a-reviewer",
      message: `${subject} names "${id}" as a reviewer, but that block is a liaison — it presents the human's questions and never records a verdict, so the gate could never open.`,
      blockId: id,
    };
  }
  return null;
}

/** Everything that is wrong with this workflow, before a single agent is spawned.
 *
 *  This is the pass every surveyed tool skipped (#222 §1a-v): Flowise, Langflow and Dify
 *  all discover a dangling reference at RUN time, and Dify will publish a workflow whose
 *  node isn't even installed. It is cheap, it is pure, and it is the difference between
 *  "your workflow failed after spawning two agents" and "block `rev-perf` doesn't exist —
 *  the merge gate names it". */
export function validateWorkflow(w: Workflow, knobs?: KnobLookup): Finding[] {
  const findings: Finding[] = [];
  const byId = new Map<string, WorkflowBlock>();

  if (!w.blocks.length) {
    findings.push({
      severity: "error",
      code: "no-blocks",
      message: "This workflow declares no blocks — add at least one agent block.",
    });
  }

  const seen = new Set<string>();
  for (const b of w.blocks) {
    const where = b.id || b.name;
    if (!b.id) {
      findings.push({
        severity: "error",
        code: "block-id-missing",
        message: `A block has no id. The id is the block's identity — edges and gates reference it.`,
        blockId: b.id,
      });
    } else if (!isValidBlockId(b.id)) {
      findings.push({
        severity: "error",
        code: "block-id-invalid",
        message: `"${b.id}" is not a valid block id — use lowercase letters, digits, - and _ (e.g. rev-security).`,
        blockId: b.id,
      });
    } else if (seen.has(b.id)) {
      findings.push({
        severity: "error",
        code: "block-id-duplicate",
        message: `Two blocks share the id "${b.id}" — an edge or a gate naming it would be ambiguous.`,
        blockId: b.id,
      });
    }
    if (b.id) {
      seen.add(b.id);
      if (!byId.has(b.id)) byId.set(b.id, b);
    }

    if (!isBlockKind(b.kind)) {
      findings.push({
        severity: "error",
        code: "unknown-kind",
        message: b.kind
          ? `Block "${where}" has kind "${b.kind}", which is not one of ${BLOCK_KINDS.join(", ")}. A workflow can define any persona, but never a new capability class.`
          : `Block "${where}" has no kind — pick one of ${BLOCK_KINDS.join(", ")}.`,
        blockId: b.id,
      });
    }
    if (!isWorkflowCli(b.cli)) {
      findings.push({
        severity: "error",
        code: "unknown-cli",
        message: b.cli
          ? `Block "${where}" runs cli "${b.cli}", which orrerix cannot spawn (supported: ${WORKFLOW_CLIS.join(", ")}).`
          : `Block "${where}" has no cli — pick one of ${WORKFLOW_CLIS.join(", ")}.`,
        blockId: b.id,
      });
    }
    if (b.prompt !== undefined && b.profile !== undefined) {
      findings.push({
        severity: "error",
        code: "prompt-and-profile",
        message: `Block "${where}" declares both a prompt and a profile — pick one. (An inline prompt compiles to the CLI's native inline agent; a profile points at a file the CLI loads by name.)`,
        blockId: b.id,
      });
    }
    // The engine refuses a persona on a loomux-owned block outright, failing the
    // WHOLE file — so a pane that reported this clean would let an author save a
    // workflow that silently launches on the built-in roster instead. Reported
    // whichever key carries it, since `parse_workflow` names both.
    if (b.prompt !== undefined || b.profile !== undefined) {
      const denial = personaDenialReason(b.kind);
      if (denial) {
        const key = b.prompt !== undefined ? "prompt:" : "profile:";
        findings.push({
          severity: "error",
          code: "persona-not-permitted",
          message: `Block "${where}" declares ${key}, which a ${b.kind} block may not — ${denial}.`,
          blockId: b.id,
        });
      }
    }
    if (b.role_hint !== undefined) {
      const required = roleHintRequires(b.role_hint);
      if (!required) {
        findings.push({
          severity: "error",
          code: "role-hint-unknown",
          message: `Block "${where}" has role_hint "${b.role_hint}", which is not one of ${ROLE_HINTS.join(", ")}.`,
          blockId: b.id,
        });
      } else if (required !== b.kind) {
        findings.push({
          severity: "error",
          code: "role-hint-wrong-kind",
          message: `Block "${where}" has role_hint "${b.role_hint}", which requires kind: ${required} (this block is kind: "${b.kind}").`,
          blockId: b.id,
        });
      } else if (b.role_hint.trim().toLowerCase() === "liaison") {
        // A WARNING, never an error: the hint parses on the real engine and the
        // group runs exactly as it did (#1161 decision D4). What the author is
        // owed is where the feature went — a hint on a reviewer cannot express
        // what the manager class is, and someone reading a file written before
        // that class existed has no way to know a better shape is available.
        findings.push({
          severity: "warning",
          code: "role-hint-superseded",
          message: `Block "${where}" has role_hint: liaison, which is superseded by kind: manager — the first-class human-interface class. The hint still parses and this block still runs exactly as it does today; a manager instead gets its own capability class, a durable mailbox from the orchestrator, and a pane no fleet traffic is delivered into. See docs/orchestration.md.`,
          blockId: b.id,
        });
      }
    }
    // #1457. Mirrors `parse_workflow`'s three refusals, in its order, for the
    // reason `personaDenialReason` states: each of them fails the WHOLE file on
    // the engine, so a pane that reported one clean would let an author save a
    // workflow whose launch silently falls back to the built-in roster.
    if (b.remote !== undefined) {
      const denial = remoteDenialReason(b.kind);
      if (!isRemoteLabel(b.remote)) {
        findings.push({
          severity: "error",
          code: "remote-invalid-label",
          message: `Block "${where}" has remote "${b.remote}", which is not a usable label — letters, digits, '-' and '_' only, at most ${REMOTE_LABEL_MAX} characters, and not starting with '-'. A remote label is an abstract name the OPERATOR binds to a host outside this repo, never an address.`,
          blockId: b.id,
        });
      } else if (denial) {
        findings.push({
          severity: "error",
          code: "remote-not-permitted",
          message: `Block "${where}" declares remote:, which a ${b.kind} block may not — ${denial}.`,
          blockId: b.id,
        });
      } else if (b.cli !== "claude") {
        findings.push({
          severity: "error",
          code: "remote-requires-claude",
          message: b.cli
            ? `Block "${where}" declares remote: with cli "${b.cli}" — a remote block must run cli: claude, the only CLI that accepts a session id orrerix minted before the spawn.`
            : `Block "${where}" declares remote: with no cli — a remote block must spell out cli: claude. An omitted cli inherits the group default, which is picked at launch, so orrerix cannot check it here.`,
          blockId: b.id,
        });
      }
    }
    findings.push(...allowFindings(b, where));
    findings.push(...knobFindings(b, where, knobs));
  }

  // At most one manager (#1161) — mirrors the backend's own post-loop check in
  // `parse_workflow`. A roster property, not a block one: the second
  // declaration is no more wrong than the first, so the finding names them all
  // and anchors on none.
  const managers = w.blocks.filter((b) => b.kind === "manager").map((b) => b.id || "(no id)");
  if (managers.length > 1) {
    findings.push({
      severity: "error",
      code: "manager-not-unique",
      message: `${managers.length} blocks declare kind: manager (${managers.join(", ")}) — a workflow may declare at most one. The manager is the human's single interface to the group: two would each hold half a conversation.`,
    });
  }

  for (const e of w.edges) {
    for (const [end, id] of [
      ["from", e.from],
      ["to", e.to],
    ] as const) {
      if (!byId.has(id)) {
        findings.push({
          severity: "error",
          code: "edge-unknown-block",
          message: `The edge ${e.from} → ${e.to} names a block that doesn't exist: "${id}" (${end}:).`,
        });
      }
    }
    if (e.from && e.from === e.to) {
      findings.push({
        severity: "error",
        code: "edge-self",
        message: `Block "${e.from}" has an edge to itself.`,
        blockId: e.from,
      });
    }
  }

  const gate = w.gates.merge;
  if (gate) {
    if (!(GATE_REQUIRES_ACCEPTED as readonly string[]).includes(gate.require)) {
      findings.push({
        severity: "error",
        code: "gate-unknown-require",
        message: `The merge gate requires "${gate.require}", which is not one of ${GATE_REQUIRES.join(", ")}.`,
      });
    }
    if (!gate.reviewers.length) {
      findings.push({
        severity: "error",
        code: "gate-no-reviewers",
        message: "The merge gate names no reviewers — a gate with nothing to wait for gates nothing.",
      });
    }
    // `gateReviewerFinding` (module scope, above) is the ONE definition — the canvas's
    // `gateConnectionError` asks it the same question before it lets a rubber band land
    // on the gate (#1388), and a second copy here is how the two would drift apart.
    const reviewerFinding = (subject: string, id: string): Finding | null =>
      gateReviewerFinding(byId, subject, id);
    for (const id of gate.reviewers) {
      const f = reviewerFinding("The merge gate", id);
      if (f) findings.push(f);
    }
    // #1176's routing rules. Every refusal below is one the ENGINE refuses the
    // whole file over, so a pane that stayed quiet would be blessing a workflow
    // that will not load — and what it would have blessed is a gate that
    // silently stopped requiring a lane.
    (gate.routing ?? []).forEach((rule, i) => {
      const subject = `Routing rule ${i + 1}`;
      if (!rule.paths.length) {
        findings.push({
          severity: "error",
          code: "gate-bad-routing",
          message: `${subject} declares no paths — a rule that matches nothing can never require anybody.`,
        });
      }
      if (!rule.reviewers.length) {
        findings.push({
          severity: "error",
          code: "gate-bad-routing",
          message: `${subject} names no reviewers — a rule that requires nobody is not a rule.`,
        });
      }
      if (rule.paths.length > GATE_ROUTING_PATHS_MAX) {
        findings.push({
          severity: "error",
          code: "gate-bad-routing",
          message: `${subject} declares ${rule.paths.length} paths — at most ${GATE_ROUTING_PATHS_MAX}.`,
        });
      }
      for (const p of rule.paths) {
        if (!isRoutingGlob(p)) {
          findings.push({
            severity: "error",
            code: "gate-bad-routing",
            message: `${subject}: "${p}" is not a usable path glob. Use letters, digits, '.', '_', '-', '/' and '*' — and write a file glob, not a directory: "src/**", never "src/", "/src/**" or a ".." segment, which match nothing at all.`,
          });
        }
      }
      for (const id of rule.reviewers) {
        const f = reviewerFinding(subject, id);
        if (f) findings.push(f);
      }
    });
    if ((gate.routing?.length ?? 0) > GATE_ROUTING_RULES_MAX) {
      findings.push({
        severity: "error",
        code: "gate-bad-routing",
        message: `The merge gate declares ${gate.routing!.length} routing rules — at most ${GATE_ROUTING_RULES_MAX}.`,
      });
    }
    // The pair with no honest reading: a threshold counts passes over a FIXED
    // list, routing makes the list depend on the diff, so together an extra lane
    // could SUPPLY one of the N passes instead of adding one — the gate would get
    // easier to satisfy the more rules you wrote. The engine refuses the file.
    if (gate.routing?.length && gate.require === "threshold") {
      findings.push({
        severity: "error",
        code: "gate-bad-routing",
        message:
          "routing: and require: threshold cannot both be declared — a threshold counts passes over a fixed reviewer list, and a routing rule makes that list depend on the diff. Use require: all-pass with routing:.",
      });
    }
    // #1174. UNCONDITIONAL — unlike `threshold`, which is only meaningful under
    // `require: threshold` and so is only checked there. This clause has no mode to
    // hide behind: a `max_diff_lines: 0` is a file the engine refuses whatever else
    // the gate says, and a pane that stayed quiet about it would be blessing a
    // workflow that will not load.
    if (gate.max_diff_lines !== undefined) {
      const n = gate.max_diff_lines;
      if (!Number.isInteger(n) || n < GATE_MAX_DIFF_LINES_MIN) {
        findings.push({
          severity: "error",
          code: "gate-bad-max-diff-lines",
          message: `max_diff_lines: ${n} — a merge gate's size limit must be a whole number ≥ ${GATE_MAX_DIFF_LINES_MIN}. Omit the key to declare no limit; 0 is refused, not read as "unlimited".`,
        });
      }
    }
    // The other half of "which gate is a threshold gate" (#1388 review N1). With the
    // shorthand normalised in `readGate`, a model that says all-pass AND carries a number got
    // there by spelling both out — or by the gate form's picker being switched to all-pass
    // over a threshold that is still in the file — and the engine refuses that pair outright
    // ("require: all-pass takes no threshold — drop it, or use require: threshold"). A pane
    // that stayed quiet would be blessing a workflow that will not load. Scoped to the two
    // accepted all-pass spellings so an UNKNOWN require still gets one finding, not two.
    if ((gate.require === "all-pass" || gate.require === "all") && gate.threshold !== undefined) {
      findings.push({
        severity: "error",
        code: "gate-bad-threshold",
        message: `The merge gate says require: ${gate.require} and also names threshold: ${gate.threshold} — the engine refuses the pair outright. Drop the threshold, or use require: threshold.`,
      });
    }
    if (gate.require === "threshold") {
      const t = gate.threshold;

      if (t === undefined || !Number.isInteger(t) || t < GATE_THRESHOLD_MIN) {
        findings.push({
          severity: "error",
          code: "gate-bad-threshold",
          message: 'A "threshold" merge gate needs threshold: N with N ≥ 1.',
        });
      } else if (t > gate.reviewers.length) {
        findings.push({
          severity: "error",
          code: "gate-bad-threshold",
          message: `The merge gate needs ${t} passing reviews but names only ${gate.reviewers.length} reviewer(s) — it could never open.`,
        });
      }
    }
  }

  findings.push(...sectionFindings(w));
  findings.push(...unknownKeyFindings(w));
  findings.push(...reachabilityFindings(w, byId));
  return findings;
}

/** The `allow:` half of the pre-run pass (#1020) — two rules, and they fail in opposite
 *  directions, which is why they carry different severities.
 *
 *  A kind that may not declare `allow:` at all is a REFUSAL on the engine
 *  (`parse_workflow`): the file does not load, so it is an error here. A pattern the
 *  engine's `sanitize_allow` would REWRITE is the quieter one and arguably the worse: the
 *  file loads, the agent spawns, and the tool pattern it was pre-approved for is not the
 *  one anybody wrote — `Bash(echo "$X")` reaches the CLI as `Bash(echo X)`, which matches
 *  nothing. Nothing downstream can tell the human that; this can. */
function allowFindings(b: WorkflowBlock, where: string): Finding[] {
  if (!b.allow?.length) return [];
  const denial = allowDenialReason(b.kind);
  if (denial) {
    return [
      {
        severity: "error",
        code: "allow-not-permitted",
        message: `Block "${where}" declares allow:, which a ${b.kind} block may not — ${denial}.`,
        blockId: b.id,
      },
    ];
  }
  const out: Finding[] = [];
  for (const pattern of b.allow) {
    const clean = sanitizeAllowPattern(pattern);
    if (clean === null) {
      out.push({
        severity: "warning",
        code: "allow-sanitized",
        message:
          `Block "${where}" declares the allow: pattern "${pattern}", which has no characters ` +
          `orrerix can pass to the CLI — it is dropped, and pre-approves nothing.`,
        blockId: b.id,
      });
    } else if (clean !== pattern.trim()) {
      out.push({
        severity: "warning",
        code: "allow-sanitized",
        message:
          `Block "${where}" declares the allow: pattern "${pattern}", but orrerix passes only ` +
          `letters, digits and ( ) : * _ - . / , and spaces — the CLI will be given "${clean}".`,
        blockId: b.id,
      });
    }
  }
  return out;
}

/** The policy sections' half of the pre-run pass (#1020): `intake:`, `merge_queue:` and
 *  `resources:`, checked against the same bounds and vocabularies `parse_workflow` uses.
 *
 *  All of these are things the pane can now WRITE, which is exactly why it must also be
 *  able to say them: a form that offers a number the engine refuses turns a config screen
 *  into a way to break your own workflow file, and the pane reporting "valid" over a file
 *  that will not load is the failure mode this whole pass exists to prevent. Each finding
 *  carries its `section`, so clicking it lands on the form that can fix it. */
function sectionFindings(w: Workflow): Finding[] {
  const out: Finding[] = [];
  const err = (section: FindingSection, code: FindingCode, message: string): void => {
    out.push({ severity: "error", code, message, section });
  };

  if (w.intake) {
    if (w.intake.source !== undefined && !isIntakeSource(w.intake.source)) {
      err(
        "intake",
        "intake-unknown-source",
        `intake.source: "${w.intake.source}" is not a source orrerix knows — use one of ${INTAKE_SOURCES.join(", ")} (or leave it out to inherit).`
      );
    }
    for (const key of INTAKE_LABEL_KEYS) {
      const v = w.intake.labels?.[key];
      if (v !== undefined && !isValidIntakeLabel(v)) {
        err(
          "intake",
          "intake-bad-label",
          `intake.labels.${key}: "${v}" is not a usable label — letters, digits, - and _, no leading -, at most ${ID_MAX_CHARS} characters. ` +
            `orrerix rejects it rather than rewriting it, so your repo's own labels keep matching.`
        );
      }
    }
  }

  const mq = w.merge_queue;
  if (mq) {
    if (
      mq.max_batch !== undefined &&
      (!Number.isInteger(mq.max_batch) || mq.max_batch < MERGE_QUEUE_MAX_BATCH_MIN)
    ) {
      err(
        "merge_queue",
        "section-out-of-range",
        `merge_queue.max_batch: ${mq.max_batch} — a batch must carry at least ${MERGE_QUEUE_MAX_BATCH_MIN} PR, and a batch of none could never land anything.`
      );
    }
    const timeout = mq.checks_timeout_minutes;
    if (timeout !== undefined && Number.isInteger(timeout)) {
      if (timeout < MERGE_QUEUE_CHECKS_TIMEOUT_MIN || timeout > MERGE_QUEUE_CHECKS_TIMEOUT_MAX) {
        // CLAMPED by the engine, not refused — so this is a warning, and it says what will
        // actually happen rather than implying the file is broken.
        out.push({
          severity: "warning",
          code: "section-out-of-range",
          message:
            `merge_queue.checks_timeout_minutes: ${timeout} is outside ${MERGE_QUEUE_CHECKS_TIMEOUT_MIN}–${MERGE_QUEUE_CHECKS_TIMEOUT_MAX}, ` +
            `so orrerix will clamp it — the queue will not wait for the time this file names.`,
          section: "merge_queue",
        });
      }
    }
  }

  const dv = w.driver;
  if (dv) {
    // Every driver field is a `u32` on the engine, so serde refuses a value's
    // TYPE before any range or clamp logic runs: a float (`2.5`), a negative
    // (`-1`), anything above 4_294_967_295. A `Number.isInteger`-guarded check
    // goes silent on the first, and an out-of-DECLARED-range check turns the
    // other two into clamp warnings - both bless files the engine refuses at
    // load (#1784 review rounds 1-2). So the type class is tested FIRST, by
    // this one predicate, and the message says refusal, not clamping.
    const outsideU32 = (v: number): boolean => v < 0 || v > 4294967295;
    // The INVARIANT-9 counters are REFUSED by the engine (#1778 2.3) outside
    // their closed range: an error here, because a repo file may tighten
    // INVARIANT 9 but never loosen it. A file this pane calls valid must load.
    const counter = (field: string, v: number | undefined, min: number, max: number): void => {
      if (v === undefined) return;
      if (!Number.isInteger(v) || outsideU32(v)) {
        err(
          "driver",
          "section-bad-value",
          `driver.${field}: ${v} is not a value the engine's u32 field accepts - the engine ` +
            `refuses the type before any range check runs.`
        );
        return;
      }
      if (v < min || v > max) {
        err(
          "driver",
          "section-out-of-range",
          `driver.${field}: ${v} must be an integer in ${min}-${max} - the engine refuses the ` +
            `whole file, because a repo file may tighten INVARIANT 9 but never loosen it.`
        );
      }
    };
    counter(
      "max_review_rounds",
      dv.max_review_rounds,
      DRIVER_MAX_REVIEW_ROUNDS_MIN,
      DRIVER_MAX_REVIEW_ROUNDS_MAX
    );
    counter(
      "max_ci_attempts",
      dv.max_ci_attempts,
      DRIVER_MAX_CI_ATTEMPTS_MIN,
      DRIVER_MAX_CI_ATTEMPTS_MAX
    );
    counter(
      "max_rebase_attempts",
      dv.max_rebase_attempts,
      DRIVER_MAX_REBASE_ATTEMPTS_MIN,
      DRIVER_MAX_REBASE_ATTEMPTS_MAX
    );
    // The backstops are CLAMPED, like `checks_timeout_minutes` - an
    // out-of-range INTEGER inside the u32 type is a warning that says what
    // will actually happen. But the TYPE class comes first: a non-integer
    // (`2.5`) and an integer outside u32 (`-1`, `4294967296`) are refused by
    // the engine outright, and a clamp warning about a file that refuses to
    // load is the same lie in a friendlier tone (#1784 review round 2).
    const backstop = (field: string, v: number | undefined): void => {
      if (v === undefined) return;
      if (!Number.isInteger(v) || outsideU32(v)) {
        err(
          "driver",
          "section-bad-value",
          `driver.${field}: ${v} is not a value the engine's u32 field accepts - the engine ` +
            `refuses the type before any clamp runs.`
        );
        return;
      }
      if (v < DRIVER_TIMEOUT_MIN || v > DRIVER_TIMEOUT_MAX) {
        out.push({
          severity: "warning",
          code: "section-out-of-range",
          message:
            `driver.${field}: ${v} is outside ${DRIVER_TIMEOUT_MIN}-${DRIVER_TIMEOUT_MAX}, ` +
            `so orrerix will clamp it - the driver will not wait for the time this file names.`,
          section: "driver",
        });
      }
    };
    backstop("lane_timeout_minutes", dv.lane_timeout_minutes);
    backstop("fix_timeout_minutes", dv.fix_timeout_minutes);
    backstop("drive_timeout_minutes", dv.drive_timeout_minutes);
  }

  const resources = w.resources;
  if (resources) {
    const names = Object.keys(resources);
    if (names.length > RESOURCES_MAX) {
      err(
        "resources",
        "section-out-of-range",
        `resources: ${names.length} declared — at most ${RESOURCES_MAX} are allowed (every name is listed in the acquire_lock tool description every agent in the group reads).`
      );
    }
    for (const name of names) {
      if (!isValidResourceName(name)) {
        err(
          "resources",
          "resource-name-invalid",
          `resources: "${name}" is not a usable resource name — letters, digits, - and _, at most ${ID_MAX_CHARS} characters. ` +
            `orrerix rejects it rather than rewriting it, so the name an agent's acquire_lock call uses is the name you wrote.`
        );
      }
      const r = resources[name]!;
      const bound = (
        key: "slots" | "max_hold_minutes",
        min: number,
        max: number,
        why: string
      ): void => {
        const v = r[key];
        if (v === undefined) return;
        if (!Number.isInteger(v) || v < min || v > max) {
          err(
            "resources",
            "section-out-of-range",
            `resources.${name}.${key}: ${v} is outside ${min}–${max} — ${why}.`
          );
        }
      };
      bound(
        "slots",
        RESOURCE_SLOTS_MIN,
        RESOURCE_SLOTS_MAX,
        "a resource with no slots could never be acquired, and past the maximum a declaration serializes nothing"
      );
      bound(
        "max_hold_minutes",
        RESOURCE_MAX_HOLD_MINUTES_MIN,
        RESOURCE_MAX_HOLD_MINUTES_MAX,
        "a hold that expires as it is granted serializes nothing, and one on a scarce resource has to be bounded by something a working session outlives"
      );
    }
  }
  return out;
}

/** Every key this build doesn't know, wherever it sits (#880).
 *
 *  The pane PRESERVES unknown keys — a file written by a newer loomux must survive a
 *  round-trip through an older pane rather than be quietly stripped by it (that is
 *  what `extra` is for, and it stays). But preserving alone was a half-truth: this
 *  build's engine is `deny_unknown_fields`, so a single typo (`promt:`) makes
 *  `parse_workflow` refuse the WHOLE file — gates and all — down the loud
 *  `workflow-invalid` path, while the pane cheerfully rendered "valid". Preserving and
 *  warning is the honest pair; dropping is destructive and ignoring is a lie.
 *
 *  `gates:` is exempt, and that is not an oversight: it is a map keyed by gate NAME
 *  (`BTreeMap<String, RawGate>`), so a gate loomux doesn't enforce still parses. See
 *  `KNOWN_GATE`. */
function unknownKeyFindings(w: Workflow): Finding[] {
  const out: Finding[] = [];
  const report = (
    where: string,
    extra: Record<string, YamlValue> | undefined,
    blockId?: string,
    section?: FindingSection
  ): void => {
    for (const key of Object.keys(extra ?? {})) {
      out.push({
        severity: "error",
        code: "unknown-key",
        message:
          `${where} declares "${key}", which is not part of the workflow schema — ` +
          `this build's engine refuses unknown keys, so the file will not load. ` +
          `(The pane keeps the line as written; check the spelling, or the file needs a newer orrerix.)`,
        blockId,
        section,
      });
    }
  };
  report("This workflow", w.extra);
  for (const b of w.blocks) report(`Block "${b.id || b.name}"`, b.extra, b.id || undefined);
  if (w.intake) report("intake:", w.intake.extra);
  if (w.intake?.labels) report("intake.labels:", w.intake.labels.extra);
  if (w.merge_queue) report("merge_queue:", w.merge_queue.extra);
  // #1778. The optional `section` routes the finding onto the driver's
  // read-only summary, the same promise the FindingSection member makes - a
  // mistyped key is the likeliest driver authoring error, and it must not be
  // the one finding the driver surface cannot show. (`board:`'s missing line
  // below is a pre-existing gap, not this section's.)
  if (w.driver) report("driver:", w.driver.extra, undefined, "driver");
  for (const [name, r] of Object.entries(w.resources ?? {})) report(`resources.${name}:`, r.extra);
  return out;
}

/** The two structural warnings — a block nothing points at, and a block nothing can
 *  reach. Both are WARNINGS, not errors: edges are advisory (§2g), so an isolated block
 *  is a workflow the orchestrator can still run — it is just almost certainly a mistake
 *  (a fan-out you forgot to wire, a reviewer that will never be asked). */
function reachabilityFindings(w: Workflow, byId: Map<string, WorkflowBlock>): Finding[] {
  const out: Finding[] = [];
  if (!w.edges.length || w.blocks.length < 2) return out;

  const ids = [...byId.keys()];
  // Nothing here has an ID, so there is no graph to reason about — every edge is dangling
  // and `edge-unknown-block` has already said so. Without this, `entries` came out empty
  // and we announced that "every block is pointed at by another", which is neither true nor
  // useful about a file whose blocks have no identities yet (rev-5 F6).
  if (!ids.length) return out;
  const inDeg = new Map(ids.map((id) => [id, 0]));
  const outAdj = new Map<string, string[]>(ids.map((id) => [id, []]));
  for (const e of w.edges) {
    if (!byId.has(e.from) || !byId.has(e.to) || e.from === e.to) continue;
    inDeg.set(e.to, (inDeg.get(e.to) ?? 0) + 1);
    outAdj.get(e.from)!.push(e.to);
  }

  for (const id of ids) {
    if (inDeg.get(id) === 0 && outAdj.get(id)!.length === 0) {
      out.push({
        severity: "warning",
        code: "isolated-block",
        message: `Block "${id}" has no edges — nothing declares when it runs.`,
        blockId: id,
      });
    }
  }

  // Entries are the blocks nothing points at. A workflow with none is all cycles — the
  // rework loop (worker ⇄ reviewer) is a legitimate cycle, so a cycle is not itself a
  // finding; having NOWHERE TO START is.
  const entries = ids.filter((id) => inDeg.get(id) === 0);
  if (!entries.length) {
    out.push({
      severity: "warning",
      code: "no-entry-block",
      message: "Every block is pointed at by another — the declared path has no starting point.",
    });
    return out;
  }

  const reached = new Set<string>(entries);
  const queue = [...entries];
  while (queue.length) {
    const id = queue.shift()!;
    for (const next of outAdj.get(id)!) {
      if (!reached.has(next)) {
        reached.add(next);
        queue.push(next);
      }
    }
  }
  for (const id of ids) {
    if (!reached.has(id)) {
      out.push({
        severity: "warning",
        code: "unreachable-block",
        message: `Block "${id}" is unreachable — no path leads to it from a starting block.`,
        blockId: id,
      });
    }
  }
  return out;
}

// ---------- the derived graph (read-only) ----------

export interface GraphNode {
  block: WorkflowBlock;
  /** The block's INDEX in the roster — its identity in the picture. Not its id: the blocks
   *  that most need drawing are the broken ones, and two id-less stubs (or a duplicate-id
   *  pair) share an id while being two different rows. Keying the graph by id drew them on
   *  top of each other, so a file with two stubs showed one (rev-5 F5) — in the very view
   *  whose job is to let you SEE the file. The roster already keys by index for exactly
   *  this reason; now the graph agrees with it. */
  index: number;
  /** False when the block's kind isn't a capability class — the view draws it as a stub. */
  known: boolean;
  /** Column in the layered layout: distance from the nearest entry block. */
  layer: number;
}

export interface GraphEdge extends WorkflowEdge {
  /** False when either end names a block that doesn't exist (the view draws it dangling). */
  resolved: boolean;
}

export interface GraphGate {
  /** The gate's name — today, always "merge". */
  name: string;
  require: string;
  threshold?: number;
  reviewers: string[];
}

export interface WorkflowGraph {
  nodes: GraphNode[];
  edges: GraphEdge[];
  gates: GraphGate[];
  /** Block INDICES grouped by layer, left to right (see GraphNode.index). */
  layers: number[][];
}

/** Derive the picture: the blocks, the advisory edges between them, and the enforced
 *  gates hanging off the reviewers they name. READ-ONLY by design (#222 Q6) — the graph
 *  is a view over the file, like GitLab's CI "Visualize" tab, not an editable canvas that
 *  can corrupt it. Layering is longest-path from the entry blocks, with cycles (the
 *  worker ⇄ reviewer rework loop) resolved by leaving the back-edge's target where its
 *  forward path put it. */
export function deriveGraph(w: Workflow): WorkflowGraph {
  const byId = new Map<string, WorkflowBlock>();
  for (const b of w.blocks) if (b.id && !byId.has(b.id)) byId.set(b.id, b);

  const edges: GraphEdge[] = w.edges.map((e) => ({
    ...e,
    resolved: byId.has(e.from) && byId.has(e.to),
  }));

  // Layering is computed over IDS — an edge names ids, so that is what a column can be
  // derived from — and then handed to the NODES, which are rows. The two are different
  // things, and conflating them is what stacked the broken blocks on one another.
  const layer = new Map<string, number>();
  for (const b of w.blocks) if (b.id) layer.set(b.id, 0);

  // Relax forward edges |blocks| times: a node sits one column right of its deepest
  // predecessor. Bounded, so a cycle terminates instead of spinning.
  for (let pass = 0; pass < w.blocks.length; pass++) {
    let moved = false;
    for (const e of edges) {
      if (!e.resolved || e.from === e.to) continue;
      const want = (layer.get(e.from) ?? 0) + 1;
      if (want > (layer.get(e.to) ?? 0)) {
        layer.set(e.to, want);
        moved = true;
      }
    }
    if (!moved) break;
  }

  // An id-less block has no column of its own to compute (nothing can point at it), so it
  // sits in the first one — visible, drawn as the stub it is, next to the finding that says
  // to give it an id.
  const nodes: GraphNode[] = w.blocks.map((b, index) => ({
    block: b,
    index,
    known: isBlockKind(b.kind),
    layer: (b.id && layer.get(b.id)) || 0,
  }));

  const depth = nodes.reduce((m, n) => Math.max(m, n.layer), 0);
  const layers: number[][] = Array.from({ length: depth + 1 }, () => []);
  for (const n of nodes) layers[n.layer]!.push(n.index);

  const gates: GraphGate[] = w.gates.merge
    ? [
        {
          name: "merge",
          require: w.gates.merge.require,
          threshold: w.gates.merge.threshold,
          reviewers: w.gates.merge.reviewers,
        },
      ]
    : [];

  return { nodes, edges, gates, layers };
}

// ---------- editing helpers (used by the form; pure, so they are tested) ----------

/** A fresh block id derived from `base`, unique within `w`. Ids are IMMUTABLE once a
 *  block exists (rule 1 at the top of this file), so this runs exactly once per block —
 *  at creation — and never again. */
export function nextBlockId(w: Workflow, base: string): string {
  const slug =
    base
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .replace(/^([^a-z])/, "b$1") || "block";
  const taken = new Set(w.blocks.map((b) => b.id));
  if (!taken.has(slug)) return slug;
  for (let n = 2; ; n++) {
    const candidate = `${slug}-${n}`;
    if (!taken.has(candidate)) return candidate;
  }
}

// ---------- graph EDIT operations (#222 v2: the canvas edits the file) ----------
//
// The canvas is now bidirectional — you can draw an edge, add a block, delete either — and
// every one of those goes through a function here, in the pure module, rather than through
// the DOM layer poking at the model. That is what makes "draw an edge, serialize, re-read,
// get the same workflow" a unit test instead of a thing you check by hand with a mouse.
//
// They all return a NEW workflow, and none of them is allowed to invent an identity: a block
// gets its id from the human (immutable, human-meaningful — §4), and an edge is a pair of ids
// that already exist.

/** Why a proposed edge can't be drawn, or null when it can. Checked BEFORE the edge is
 *  created rather than reported after — an editable canvas that lets you draw an edge and
 *  then tells you it was invalid has wasted the gesture and left you to undo it. */
export function connectionError(w: Workflow, from: string, to: string): string | null {
  if (!from || !to) return "A block needs an id before an edge can name it.";
  if (from === to) return "A block can't run after itself.";
  if (!w.blocks.some((b) => b.id === from) || !w.blocks.some((b) => b.id === to)) {
    return "That block doesn't exist.";
  }
  if (w.edges.some((e) => e.from === from && e.to === to)) return "That edge already exists.";
  return null;
}

/** Draw an advisory edge. A duplicate or illegal edge is a no-op rather than a throw — the
 *  canvas has already refused the gesture (`connectionError`), and this is the second line of
 *  defence, not the first. */
export function connectBlocks(w: Workflow, from: string, to: string): Workflow {
  if (connectionError(w, from, to)) return w;
  return { ...w, edges: [...w.edges, { from, to }] };
}

/** Erase an edge. Only that edge: the blocks it joined are untouched, which is the whole
 *  difference between deleting a connection and deleting the work. */
export function disconnectBlocks(w: Workflow, from: string, to: string): Workflow {
  return { ...w, edges: w.edges.filter((e) => !(e.from === from && e.to === to)) };
}

/** Why a reviewer block can't be wired to the merge gate, or null when it can (#1388).
 *  The gate half of `connectionError`, and asked at the same moment for the same reason: a
 *  canvas that completes the gesture and only then says the drop was invalid has wasted it.
 *
 *  The one refusal that is NOT re-stated here is "that block's kind can't record a verdict" —
 *  `gateReviewerFinding` already answers that for the validator and for the engine's
 *  `gate_reviewer_error`, so it answers it here too. A gate that refuses a drop for a
 *  different reason than the findings strip gives for the same block is two definitions of
 *  the same rule, which is exactly what #1176 collapsed.
 *
 *  A DUPLICATE is a reason, not a silent no-op, and that mirrors `connectionError`'s "that
 *  edge already exists": the human made a gesture, and a gesture that changes nothing and
 *  says nothing is indistinguishable from one the canvas dropped on the floor — which is the
 *  whole complaint #1387 and #1388 were opened about. `connectToGate` is still a no-op on
 *  it, so the file cannot grow a duplicate seat whatever the view does. */
export function gateConnectionError(w: Workflow, from: string): string | null {
  if (!from) return "A block needs an id before the merge gate can name it.";
  const gate = w.gates.merge;
  if (!gate) {
    return "There is no merge gate yet — turn it on in the gate's settings, then wire reviewers to it.";
  }
  if (gate.reviewers.includes(from)) return `"${from}" already gates the merge.`;
  // First id wins, exactly as `validateWorkflow` resolves a duplicate: the gate would name
  // one of them ambiguously either way, and that is already its own finding.
  const byId = new Map<string, WorkflowBlock>();
  for (const b of w.blocks) if (b.id && !byId.has(b.id)) byId.set(b.id, b);
  const finding = gateReviewerFinding(byId, "The merge gate", from);
  return finding ? finding.message : null;
}

/** Wire a reviewer to the merge gate: append its id to `gates.merge.reviewers`, which is the
 *  shape `parse_workflow` already reads (a list of block ids — no new backend surface).
 *  A refused or duplicate drop is a no-op, like `connectBlocks`.
 *
 *  SEAT ORDER IS NOT THE HUMAN'S, and this is the place to say so because it is the place
 *  someone will come looking. The append is only the cheapest way to add an entry: every
 *  write goes through `emitGatesLines`, which emits `sortByBlocks(gate.reviewers, order)` —
 *  so the file always lists the seats in ROSTER order, whatever order the model's array is
 *  in, on the canvas's `serializeWorkflowPreserving` path as much as on the canonical one.
 *  That is deliberate and pre-dates this: it is the same canonical-form rule that groups
 *  edges by source, so two people who wire the same gate in a different order get the same
 *  file and neither sees a diff from the other's clicking sequence.
 *
 *  The consequence worth writing down: a reorder affordance on this list — drag-to-reorder,
 *  up/down buttons — would appear to work and change nothing in the file. If seat order is
 *  ever meant to MEAN something, `sortByBlocks` is what has to change first. */
export function connectToGate(w: Workflow, from: string): Workflow {
  if (gateConnectionError(w, from)) return w;
  const gate = w.gates.merge!;
  return { ...w, gates: { ...w.gates, merge: { ...gate, reviewers: [...gate.reviewers, from] } } };
}

/** Take a reviewer's seat off the merge gate — the gate-edge mirror of `disconnectBlocks`.
 *  Only the seat: the block, its edges and the rest of the gate's policy are untouched. */
export function disconnectFromGate(w: Workflow, from: string): Workflow {
  const gate = w.gates.merge;
  if (!gate || !gate.reviewers.includes(from)) return w;
  return {
    ...w,
    gates: { ...w.gates, merge: withGateReviewers(gate, gate.reviewers.filter((r) => r !== from)) },
  };
}

/** A gate with a new reviewer list, and its `threshold` kept SATISFIABLE — the one derived
 *  number that a shorter list can invalidate.
 *
 *  `threshold: 3` over three reviewers is a legal file the engine loads; drop one reviewer
 *  and it becomes "3 passes from 2 reviewers", which `parse_workflow` refuses OUTRIGHT —
 *  not the gate, the whole `workflow.yml`, so the group silently falls back to the built-in
 *  roster. A one-click gesture on the canvas must not be able to do that, so the threshold
 *  follows the list down.
 *
 *  It follows it down to `GATE_THRESHOLD_MIN` and no further, which is the difference
 *  between clamping a number and destroying one: removing the LAST reviewer leaves
 *  `threshold: 1` over an empty list — already `gate-no-reviewers`, loudly, whatever we do
 *  with the number — and re-wiring one reviewer makes the gate valid again with the human's
 *  own intent intact. Clamping to 0 would trade one finding for two and lose the number.
 *
 *  The clamp is deliberately silent about the OTHER direction: adding a reviewer never
 *  touches the threshold, because "2 of 3 must pass" is a policy, not a proportion. */
function withGateReviewers(gate: MergeGate, reviewers: string[]): MergeGate {
  const next: MergeGate = { ...gate, reviewers };
  if (
    gate.require === "threshold" &&
    typeof gate.threshold === "number" &&
    gate.threshold > reviewers.length
  ) {
    next.threshold = Math.max(GATE_THRESHOLD_MIN, reviewers.length);
  }
  return next;
}

/** Add a block. The caller supplies the ID — the canvas asks the human for it, because §4's
 *  first commitment is that an id is human-meaningful and immutable, and a canvas that mints
 *  `node_1720794829558` (Dify's actual behaviour) makes every edge in the file unreadable
 *  and every id a lie about what the block is. */
export function addBlock(w: Workflow, block: WorkflowBlock): Workflow {
  return { ...w, blocks: [...w.blocks, block] };
}

/** A new block, filled in with the defaults a reviewer usually wants — the caller overrides
 *  what it asked the human about. Kept here so "what a new block is" has one answer. */
export function newBlock(id: string, name: string, kind: BlockKind = "reviewer"): WorkflowBlock {
  return { id, name: name || id, kind, cli: "claude", model: "" };
}

/** Remove the block at `index`, AND every reference to it — edges at either end, and its
 *  seat on the merge gate. A delete that left the references behind would turn one click
 *  into three validation errors, which is exactly the "dangling reference" class this file
 *  exists to prevent (Dify ships it; we don't).
 *
 *  By INDEX, not by id, and both halves of that matter:
 *   - an id-LESS stub (a block the file got wrong) has no id to delete by, and deleting
 *     "every block whose id is empty" would take its siblings with it;
 *   - a DUPLICATE id survives its own deletion — the other block still answers to it — so
 *     the references are still meaningful and must NOT be stripped. Hence `gone`. */
export function removeBlockAt(w: Workflow, index: number): Workflow {
  const block = w.blocks[index];
  if (!block) return w;
  const blocks = w.blocks.filter((_, i) => i !== index);
  const id = block.id;
  const gone = !!id && !blocks.some((b) => b.id === id);
  const gate = w.gates.merge;
  return {
    ...w,
    blocks,
    edges: gone ? w.edges.filter((e) => e.from !== id && e.to !== id) : w.edges,
    gates: {
      ...w.gates,
      // Through `withGateReviewers` for the same reason `disconnectFromGate` is (#1388):
      // a threshold left above the reviewer count is a file `parse_workflow` refuses
      // WHOLE, and there is no reading on which deleting a block should do that while
      // deleting its gate edge doesn't. One rule, both paths to the same empty seat.
      merge: gate
        ? gone
          ? withGateReviewers(gate, gate.reviewers.filter((r) => r !== id))
          : gate
        : undefined,
    },
  };
}

/** The file a repo with no workflow gets when the human asks for one: today's built-in
 *  pipeline, written out — plus the comments that say what each part is FOR.
 *
 *  Comments, and not just `serializeWorkflow(starterWorkflow())`, because this is the one
 *  moment the file is read by someone who has never seen the schema: it arrives in their
 *  editor, in their diff, in their teammate's `git pull`. A commented scaffold is how every
 *  config-as-code tool worth using introduces itself, and it costs one string.
 *
 *  (They are comments, so they do not survive a canonical re-serialize — the first form edit
 *  rewrites the file without them. That is the honest trade of having ONE canonical shape,
 *  it is stated in the design note, and it is why the scaffold is offered at CREATION rather
 *  than being something the formatter tries to preserve. What the human writes in the YAML
 *  tab and saves is kept verbatim; only an edit made through the form or the canvas
 *  re-serializes.)
 *
 *  `authoredWith` is stamped in the same one moment `starterWorkflow` stamps it. */
export function scaffoldWorkflowText(authoredWith?: string, rel: string = WORKFLOW_FILE): string {
  const stamp = authoredWith ? `authored_with: ${authoredWith}\n` : "";
  // `rel` is the path this text is ABOUT TO BE WRITTEN TO, threaded in rather than
  // hard-coded (#1153 phase 4, rev-lead round 1 B2). The header names the file the
  // reader is looking at: a scaffold written to `.orrerix/workflow.yml` whose first
  // line says `.loomux/workflow.yml` names a path that repo does not have, which is
  // exactly the "reads one file, reports another" defect this phase exists to fix.
  return `# ${rel} — this repo's agent workflow (loomux #222).
# Committed on purpose: everyone who clones the repo gets the same roster.
# Orrerix reads it only when "Advanced orchestrator" is ticked in the launcher.

version: 1
${stamp}name: default

# BLOCKS — the agents a run may use. \`kind\` is a capability class and the list is
# closed (orchestrator | worker | reviewer | planner | manager): a workflow file can
# define any persona, but it can never grant a capability. A planner is read-only; a
# reviewer can review but never push; a worker gets a worktree; a manager (at most one)
# is the human's own interface pane and writes nothing.
blocks:
  - id: planner            # immutable, human-meaningful — edges and gates name THIS
    name: Planner          # display only; safe to rename at any time
    kind: planner
    cli: claude
    model: opus

  - id: worker
    name: Worker
    kind: worker
    cli: claude

  - id: reviewer
    name: Reviewer
    kind: reviewer
    cli: claude
    model: opus
    # A persona is optional: an inline \`prompt:\` (compiled to the CLI's native inline
    # agent) or a \`profile:\` path to a .github/agents/*.md file. Omit both and the
    # block runs orrerix's built-in role instructions.
    #
    # prompt: |
    #   Review ONLY for security defects: injection, authz, secrets, path traversal.

# EDGES — ADVISORY. They declare the intended path; the orchestrator still decides when
# to spawn what. (Its judgment about what can run in parallel is the thing that makes it
# good — a static DAG would replace that with something dumber.)
edges:
  - { from: planner, to: worker }
  - { from: worker, to: reviewer }

# GATES — ENFORCED. Orrerix refuses \`gh pr merge\` until every reviewer named here has
# recorded a PASS verdict. An agent cannot get around it: the refusal lives in the PATH
# shim, not in a prompt. Add a second reviewer to the list and it is a second reviewer
# that must actually pass — which is what makes multi-reviewer more than theatre.
gates:
  merge:
    require: all-pass      # or: threshold, with \`threshold: N\`
    reviewers: [reviewer]
`;
}

/** The optional top-level key recording which loomux WROTE this file — §4's "record the
 *  loomux version that authored it" (the Langflow `last_tested_version` lesson: when a file
 *  misbehaves, the first question is always which build produced it).
 *
 *  It is written EXACTLY ONCE, when the pane creates a new workflow, and never touched
 *  again: on an existing file it round-trips verbatim. That is deliberate — stamping it
 *  on every save would mean every human who opens the pane and changes a model name also
 *  produces a one-line diff nobody asked for, in a file whose whole point is a legible
 *  history. It records who authored the workflow, not who last looked at it.
 *
 *  A KNOWN key since #880 (`Workflow.authored_with`), where it used to ride the
 *  unknown-key bag: that round-tripped it correctly but left it invisible to anything
 *  asking what fields a workflow file has — and once an unknown key became a finding, it
 *  would have had the pane report a key loomux itself writes. The behavior above is
 *  unchanged; only its visibility is. */
export const AUTHORED_WITH_KEY = "authored_with";

/** The workflow loomux runs today, as a file: plan → work → review, with the reviewer's
 *  verdict gating the merge. The starting point a repo with no workflow file
 *  opens on, so the pane's empty state is a working example rather than a blank page.
 *
 *  `authoredWith` is the loomux version doing the creating; omit it and the key is simply
 *  not written (which is what the tests do, and what a caller with no version to hand
 *  should do — an `authored_with: unknown` would be worse than an absent key). */
export function starterWorkflow(authoredWith?: string): Workflow {
  return {
    version: WORKFLOW_VERSION,
    name: "default",
    ...(authoredWith ? { [AUTHORED_WITH_KEY]: authoredWith } : {}),
    blocks: [
      { id: "planner", name: "Planner", kind: "planner", cli: "claude", model: "opus" },
      { id: "worker", name: "Worker", kind: "worker", cli: "claude", model: "" },
      { id: "reviewer", name: "Reviewer", kind: "reviewer", cli: "claude", model: "opus" },
    ],
    edges: [
      { from: "planner", to: "worker" },
      { from: "worker", to: "reviewer" },
    ],
    gates: { merge: { require: "all-pass", reviewers: ["reviewer"], also: [] } },
  };
}

// ---------- the one call the view makes ----------

export interface WorkflowAnalysis {
  workflow: Workflow;
  /** Parse findings and validation findings, in that order — syntax first, because a
   *  file that didn't parse will also fail half the semantic rules and leading with those
   *  would bury the line number that actually explains it. */
  findings: Finding[];
  graph: WorkflowGraph;
}

/** Text in, everything the pane renders out. `knobs` (#687) is the pane's
 *  capability lookup — omitted (or answering `null`) simply skips the knob
 *  checks, so a re-analysis that happens before `agent_cli_knobs` replies reads
 *  exactly as it did before. */
export function analyzeWorkflow(text: string, knobs?: KnobLookup): WorkflowAnalysis {
  const { workflow, findings } = parseWorkflow(text);
  return {
    workflow,
    findings: [...findings, ...validateWorkflow(workflow, knobs)],
    graph: deriveGraph(workflow),
  };
}

/** The canonical formatter, as the pane's ✨ Format button uses it: read the file, write
 *  it back in the one canonical shape. Idempotent by construction. */
export function formatWorkflowText(text: string): string {
  return serializeWorkflow(parseWorkflow(text).workflow);
}

// Workflow-mode status (#316) — DOM-free derivations over `WorkflowStatus`
// (orchestration.ts) for the lifecycle chrome and task-board Approve button
// (Slice C). This module never fetches anything; it turns the backend's
// `orch_workflow_status` payload into the strings/predicates the UI renders,
// so the "Approve cannot succeed, say so up front" rule (#316's design ask 1)
// has one place that decides it instead of each caller re-deriving it.
//
// The gate is a property of the CURRENT SESSION, not any one PR's provenance
// (doc/design/workflows.md, "a gate lives and dies with the toggle that
// authorized it") — so every function here reads the live `WorkflowStatus`,
// never a task's own history.

import type { WorkflowGateStatus, WorkflowStatus } from "./orchestration";

/** One line naming the group's current roster/mode, for the lifecycle
 *  chrome's header row. The built-in roster has no declared name, so it gets
 *  a fixed label rather than an empty string. */
export function workflowModeLabel(status: WorkflowStatus): string {
  if (!status.advanced) return "Standard roster";
  return status.name ? status.name : "Workflow mode";
}

const requireLabel = (require: string): string => {
  const m = /^threshold (\d+)$/.exec(require);
  return m ? `at least ${m[1]} pass` : require;
};

/** "merges to the default branch require: rev-orch + rev-ui + rev-tests ·
 *  all-pass · ci-green" — `null` when no gate is armed, so a caller can omit
 *  the row entirely rather than render an empty sentence. "Default branch",
 *  not "main": loomux is a generic tool and repos may default to
 *  master/trunk (CLAUDE.md hard constraint 8). */
export function gateSummaryLine(status: WorkflowStatus): string | null {
  const gate = status.gate;
  if (!gate) return null;
  const clauses = [gate.reviewers.join(" + "), requireLabel(gate.require), ...gate.also];
  return `merges to the default branch require: ${clauses.join(" · ")}`;
}

/** The loud warning for a gate this session cannot satisfy (#316's
 *  satisfiability guarantee): the gate is still armed (never silently
 *  widened), but named reviewer blocks the roster can't spawn make it
 *  unsatisfiable from here. `null` when there's no gate, or the gate is
 *  satisfiable. */
export function gateSatisfiabilityWarning(status: WorkflowStatus): string | null {
  const gate = status.gate;
  if (!gate || gate.satisfiable) return null;
  const missing = gate.missing_blocks;
  const them = missing.length > 1 ? "them" : "it";
  return `gate names ${missing.join(", ")} — this session can't spawn ${them}; merges will bounce.`;
}

/** The three ways out of a workflow-gated merge an agent can't complete
 *  itself (#316's refusal rule) — reused verbatim by the shim's refusal text
 *  and this module's own tooltips, so the exits never drift between the two
 *  surfaces. */
export function gateExitsMessage(): string {
  return (
    "Run the named reviewer blocks so verdicts exist, toggle workflow mode off, " +
    "or merge via the GitHub UI (unshimmed)."
  );
}

/** Whether clicking Approve on `task` can actually result in a merge. A human
 *  Approve grant is the human merge gate, not the reviewer-consensus one
 *  (#197/#222) — it never opens an armed workflow gate — so this is `false`
 *  whenever a gate is armed and the task carries a PR, regardless of whether
 *  the gate is (today) satisfiable. `reason` is short enough for a button
 *  label; call `gateExitsMessage()` for the longer tooltip.
 *
 *  THE REASON IS THREE-WAY on the PR's base branch (#581), and the branching is
 *  about accuracy, never about permission — `ok` is `false` in all three cases
 *  and no merge is opened by any of them:
 *
 *  - **base unknown** (no `pr_base` recorded, or no resolved `default_branch`
 *    to compare it against) → the conservative default-branch wording. Every
 *    pre-#581 task is in this case, and so is any board whose orchestrator
 *    doesn't record the field.
 *  - **base IS the default branch** → the same wording, now actually earned.
 *  - **base is some other branch** (a sub-PR into an integration branch) → says
 *    so, and names it. The old text ("won't merge — gate needs …") implied the
 *    human's grant is what this PR is waiting on; it isn't. The human gate is
 *    default-branch-only (the shim `exec`s a non-default merge straight
 *    through), so what actually holds a sub-PR is the workflow gate's verdicts.
 *
 *  What does NOT change: the gate applies to EVERY merge of the PR wherever it
 *  lands (shim property 2), so an unsatisfiable gate still outranks all of this,
 *  and `pr_base` is agent-written display metadata that nothing enforces on.
 *
 *  The wrong-in-the-reassuring-direction case — a merge into the default branch
 *  labelled a harmless sub-PR — is what the comparison below is arranged to
 *  avoid, but it is REDUCED, not eliminated (rev-157 NB1). Two known residuals
 *  survive: `default_branch` is resolved from local refs, so a clone that never
 *  fetched a remote default-branch rename compares against the old name; and
 *  `pr_base` is agent-written, so a value in some vocabulary neither side
 *  expects reads as "a different branch". Nothing gates on either — the shim
 *  resolves base and default live at merge time — so the cost of both is a
 *  misleading sentence, never an authorization. */
export function approveWillMerge(
  status: WorkflowStatus,
  task: { pr?: string | null; pr_base?: string | null }
): { ok: boolean; reason?: string } {
  const gate = status.gate;
  if (!gate || !task.pr) return { ok: true };
  if (!gate.satisfiable) {
    return { ok: false, reason: "gate unsatisfiable from this session — merges will bounce" };
  }
  const base = task.pr_base ? baseBranchName(task.pr_base) : undefined;
  const def = status.default_branch?.trim();
  if (base && def && base !== def) {
    return {
      ok: false,
      reason: `sub-PR into ${base} — the orchestrator merges it once the gate verdicts land`,
    };
  }
  return { ok: false, reason: `won't merge — gate needs ${gate.reviewers.join("/")}` };
}

/** A recorded base ref in the vocabulary `default_branch` speaks: trimmed, and
 *  with one leading `origin/` removed (`origin/main` → `main`).
 *
 *  `pr_base` is agent-written and `gh pr view --json baseRefName` reports a bare
 *  branch name, but "the base ref" is equally naturally written `origin/main` —
 *  and against a resolved default of `main` that mismatch produced "sub-PR into
 *  origin/main" for a PR heading straight at the default branch (rev-157 NB1),
 *  the one direction worth spending code on. One leading segment only: an
 *  `origin/origin/main` is a typo, not a vocabulary, and quietly repairing it
 *  would be guessing rather than normalizing.
 *
 *  Case is deliberately NOT folded. Git refs are case-sensitive, so `Main` and
 *  `main` really are two branches, and folding would make a typo read as the
 *  default branch — which is the reassuring direction, the opposite of what the
 *  `origin/` strip is for. */
const baseBranchName = (ref: string): string => {
  const t = ref.trim();
  return t.startsWith("origin/") ? t.slice("origin/".length).trim() : t;
};

export type { WorkflowGateStatus, WorkflowStatus };

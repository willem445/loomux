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
 *  label; call `gateExitsMessage()` for the longer tooltip. */
export function approveWillMerge(
  status: WorkflowStatus,
  task: { pr?: string | null }
): { ok: boolean; reason?: string } {
  const gate = status.gate;
  if (!gate || !task.pr) return { ok: true };
  if (!gate.satisfiable) {
    return { ok: false, reason: "gate unsatisfiable from this session — merges will bounce" };
  }
  return { ok: false, reason: `won't merge — gate needs ${gate.reviewers.join("/")}` };
}

export type { WorkflowGateStatus, WorkflowStatus };

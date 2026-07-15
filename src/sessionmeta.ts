// Pure formatting for the session browser's #1 metadata (name/description/
// goal + repo/branch/PR) — kept DOM-free so the truncation and "what's
// missing stays hidden, never guessed" rules are unit-testable.
// sessions.ts renders these strings; nothing here touches the DOM.

import type { SessionRoleInfo } from "./orchestration";

const MAX_TASK_LEN = 140;

/** The task/goal line for a session item, or null when there's nothing
 *  recorded (a legacy session, or an orchestrator with no assigned task) —
 *  callers hide the line entirely rather than showing an empty one. */
export function taskSummary(role: SessionRoleInfo | undefined): string | null {
  const task = role?.task.trim();
  if (!task) return null;
  return task.length > MAX_TASK_LEN ? `${task.slice(0, MAX_TASK_LEN - 1)}…` : task;
}

/** Last path segment of a repo path, for a compact identity label. Falls
 *  back to the full path if it has no separator (already short). */
function shortRepoName(path: string): string {
  const parts = path.replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/** The repo/branch identity line, or null when neither is known. Shows
 *  whichever pieces are present — branch alone, repo alone, or "repo @
 *  branch" — never a placeholder for the missing half. */
export function repoBranchLine(role: SessionRoleInfo | undefined): string | null {
  const repo = role?.repo?.trim();
  const branch = role?.branch?.trim();
  if (repo && branch) return `${shortRepoName(repo)} @ ${branch}`;
  if (branch) return branch;
  if (repo) return shortRepoName(repo);
  return null;
}

/** The PR chip label, or null when no PR is known yet. A bare number (how
 *  the board stores most PR refs) renders as "#123"; anything already
 *  prefixed or otherwise shaped is shown verbatim. */
export function prLabel(role: SessionRoleInfo | undefined): string | null {
  const pr = role?.pr?.trim();
  if (!pr) return null;
  return /^\d+$/.test(pr) ? `#${pr}` : pr;
}

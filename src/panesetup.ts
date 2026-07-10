// Pure pane-setup core for the welcome screen (#194). DOM-free: the welcome
// form (launcher.ts) collects raw control values into a PaneSetupInput, and this
// module decides — per the chosen KIND — whether the setup is valid and what it
// should spawn. Every "the orchestrator needs a repo", "a worktree needs a
// repo", "a custom agent needs a command", count-clamping, and worktree fan-out
// rule lives here so it is unit-tested without a DOM (test/panesetup.test.ts).
//
// Shell kinds are PLUMBED here (the type carries Git Bash / cmd / PowerShell) but
// only PowerShell actually spawns a distinct shell in Phase 1 — per-kind spawning
// lands in Phase 2 (#194). This module validates and passes the value through;
// the live form disables the not-yet-wired kinds.

export type PaneKind = "agent" | "orchestrator" | "terminal";
export type ShellKind = "powershell" | "gitbash" | "cmd";

const AGENT_MIN = 1;
const AGENT_MAX = 8;

export interface PaneSetupInput {
  kind: PaneKind;
  /** Agent id chosen in the picker ("claude", "custom", …). */
  agentId: string;
  /** True when agentId is the "custom…" entry (command comes from customCommand). */
  isCustom: boolean;
  /** The built-in agent's command line (ignored when isCustom). */
  builtinCommand: string;
  /** The user's custom command line (used when isCustom). */
  customCommand: string;
  /** Requested agent pane count; clamped to [1, 8]. */
  count: number;
  /** Repository / folder; "" = home for a terminal, invalid for an orchestrator. */
  repo: string;
  /** Optional worktree name (agent kind); requires a repo. */
  worktree: string;
  /** Pane name; blank falls back to a sensible default. */
  name: string;
  /** Autopilot ("allow all") toggle (agent kind). */
  autopilot: boolean;
  /** Selected shell kind (terminal kind). */
  shellKind: ShellKind;
}

export interface TerminalPlan {
  kind: "terminal";
  shellKind: ShellKind;
  /** cwd to open the shell in; null = home. */
  cwd: string | null;
  name: string;
}
export interface AgentPlan {
  kind: "agent";
  /** Resolved command line (pre-autopilot-flags — the form appends those). */
  command: string;
  isCustom: boolean;
  /** Clamped pane count. */
  count: number;
  repo: string;
  worktree: string;
  /** Base pane name; multi-pane launches suffix " 1" … " N". */
  baseName: string;
  autopilot: boolean;
}
export interface OrchestratorPlan {
  kind: "orchestrator";
  repo: string;
}
export type PaneSetupPlan = TerminalPlan | AgentPlan | OrchestratorPlan;

/** Which field to focus when validation fails, so the form can surface it. */
export type PaneSetupFocus = "repo" | "custom" | "count";

export type PaneSetupResult =
  | { ok: true; plan: PaneSetupPlan }
  | { ok: false; error: string; focus?: PaneSetupFocus };

const clampCount = (n: number): number =>
  Number.isFinite(n) ? Math.min(AGENT_MAX, Math.max(AGENT_MIN, Math.trunc(n))) : AGENT_MIN;

/** The last path segment of a repo/folder path, for a default pane name. */
export function pathTail(p: string): string {
  const parts = p.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? "";
}

/** The worktree/branch name for the i-th (1-based) agent of a fan-out. A single
 *  agent keeps the base name; a fleet suffixes -1 … -N so every agent gets an
 *  isolated worktree (the existing multi-pane behavior, #194). */
export function worktreeNameFor(base: string, index: number, count: number): string {
  return count > 1 ? `${base}-${index}` : base;
}

/** Validate + shape the chosen pane setup. Pure — no probes, no worktree
 *  creation, no autopilot-flag lookup; those async side effects stay in the form
 *  and run only after this returns `ok`. */
export function planPaneSetup(input: PaneSetupInput): PaneSetupResult {
  const repo = input.repo.trim();

  if (input.kind === "terminal") {
    const cwd = repo || null;
    const name = input.name.trim() || pathTail(repo) || "terminal";
    return { ok: true, plan: { kind: "terminal", shellKind: input.shellKind, cwd, name } };
  }

  if (input.kind === "orchestrator") {
    if (!repo) {
      return {
        ok: false,
        error: "The orchestrator needs a repository — pick one first.",
        focus: "repo",
      };
    }
    return { ok: true, plan: { kind: "orchestrator", repo } };
  }

  // agent
  const command = (input.isCustom ? input.customCommand : input.builtinCommand).trim();
  if (!command) {
    return { ok: false, error: "Enter the command line for the custom agent.", focus: "custom" };
  }
  const worktree = input.worktree.trim();
  if (worktree && !repo) {
    return {
      ok: false,
      error: "A worktree needs a repository — pick one first.",
      focus: "repo",
    };
  }
  const count = clampCount(input.count);
  const baseName = input.name.trim() || command;
  return {
    ok: true,
    plan: {
      kind: "agent",
      command,
      isCustom: input.isCustom,
      count,
      repo,
      worktree,
      baseName,
      autopilot: input.autopilot,
    },
  };
}

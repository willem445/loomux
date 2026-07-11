// Pure pane-setup core for the welcome screen (#194). DOM-free: the welcome
// form (launcher.ts) collects raw control values into a PaneSetupInput, and this
// module decides — per the chosen KIND — whether the setup is valid and what it
// should spawn. Every "the orchestrator needs a repo", "a worktree needs a
// repo", "a custom agent needs a command", count-clamping, and worktree fan-out
// rule lives here so it is unit-tested without a DOM (test/panesetup.test.ts).
//
// Phase 2 (#194) wires all three shell kinds (PowerShell / cmd / Git Bash) to
// real per-kind spawning. PowerShell and cmd are always available on Windows;
// Git Bash depends on a Git-for-Windows install, discovered backend-side. This
// module owns the pure kind→enable/disable mapping and the fallback resolver so
// the form logic is unit-tested without a DOM (test/panesetup.test.ts).
//
// #214 adds a FOURTH kind: `files` — a PTY-less pane whose content is the file
// tree + editor surface, rooted at a directory the user picks. Its only setup
// input is that root, and the only rule this module can decide is "a root was
// given"; whether the directory actually EXISTS is I/O, so the form probes it
// (ftRootIsDir) after this returns ok and surfaces a failure inline.

export type PaneKind = "agent" | "orchestrator" | "terminal" | "files";
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
/** A file-explorer pane (#214): a directory to root the tree at, and a name. No
 *  command, no shell, no PTY — the pane's content IS the file tree + editor. */
export interface FilesPlan {
  kind: "files";
  /** Absolute directory the tree roots at. Non-empty (validated below); its
   *  EXISTENCE is checked by the caller (I/O), not here. */
  root: string;
  name: string;
}
export type PaneSetupPlan = TerminalPlan | AgentPlan | OrchestratorPlan | FilesPlan;

/** Which field to focus when validation fails, so the form can surface it. */
export type PaneSetupFocus = "repo" | "custom" | "count";

export type PaneSetupResult =
  | { ok: true; plan: PaneSetupPlan }
  | { ok: false; error: string; focus?: PaneSetupFocus };

/** A one-shot, re-entrancy-proof latch for the welcome form's async submit
 *  (#194 rev-74 HIGH-1). The form's `submit()` spans `await`s (CLI probe,
 *  worktree creation, group launch) during which the form stays rendered and
 *  enabled; a double-click, Enter auto-repeat, or an impatient second click
 *  would otherwise run `submit()` again and spawn a duplicate group / a second
 *  PTY on the same pane. Pure + stateful so the double-fire semantics are
 *  unit-testable without a DOM:
 *
 *   - `begin()` returns true only for the FIRST caller; every concurrent caller
 *     gets false while a submit is in flight.
 *   - `release()` re-opens the latch after a validation error (the user fixes
 *     the field and retries).
 *   - `finish()` closes it permanently once a submit has actually fired its
 *     result — the form's pane is being converted/retired, so it must never
 *     fire again even if some late event re-enters `submit()`. */
export class SubmitLatch {
  private inFlight = false;
  private done = false;

  /** Try to enter the critical section. True only if no submit is in flight and
   *  none has already finished. */
  begin(): boolean {
    if (this.inFlight || this.done) return false;
    this.inFlight = true;
    return true;
  }

  /** Abandon the in-flight submit (validation failed) — a retry is allowed. */
  release(): void {
    this.inFlight = false;
  }

  /** Mark the submit permanently done — no further submit will be admitted. */
  finish(): void {
    this.inFlight = false;
    this.done = true;
  }

  /** Re-open a FINISHED latch after a downstream launch failed (#194 P4): the
   *  result fired but the caller couldn't act on it (e.g. an orchestrator launch
   *  threw), so the form stays and must accept a retry. Distinct from `release`,
   *  which only covers a validation bounce that never finished. */
  reopen(): void {
    this.inFlight = false;
    this.done = false;
  }

  /** Whether a submit has already fired its result (one-shot spent). */
  get settled(): boolean {
    return this.done;
  }
}

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

// ---------- shell kinds (#194 P2) ----------

/** Backend-discovered availability of the shell kinds whose presence isn't
 *  guaranteed. PowerShell and cmd are always present on Windows; only Git Bash
 *  needs an install, so it's the only field here. */
export interface ShellKindAvailability {
  /** Git Bash `bash.exe` path, or null when Git for Windows isn't installed. */
  gitBashPath: string | null;
}

/** A shell-kind choice for the picker: its label, whether it can be selected,
 *  and — when it can't — the reason to surface (tooltip). */
export interface ShellKindOption {
  key: ShellKind;
  label: string;
  enabled: boolean;
  /** Why the kind is disabled; "" when enabled. */
  reason: string;
}

const GIT_BASH_MISSING = "Git Bash not found — install Git for Windows to enable it.";

/** The shell-kind picker options given what the backend discovered. Order is the
 *  menu order. PowerShell and cmd are always enabled; Git Bash is enabled only
 *  when a `bash.exe` was found, otherwise disabled with a reason (#194 P2). */
export function shellKindOptions(avail: ShellKindAvailability): ShellKindOption[] {
  const gitBash = avail.gitBashPath !== null;
  return [
    { key: "powershell", label: "PowerShell", enabled: true, reason: "" },
    { key: "cmd", label: "Command Prompt", enabled: true, reason: "" },
    {
      key: "gitbash",
      label: "Git Bash",
      enabled: gitBash,
      reason: gitBash ? "" : GIT_BASH_MISSING,
    },
  ];
}

/** Resolve the shell kind a Terminal pane should actually spawn: the requested
 *  kind when it's available, else PowerShell. Mirrors the backend's explicit
 *  fallback so the pane name can't misdescribe what starts, and so a stale
 *  selection (Git Bash uninstalled after it was picked) degrades cleanly. */
export function resolveShellKind(requested: ShellKind, avail: ShellKindAvailability): ShellKind {
  const opt = shellKindOptions(avail).find((o) => o.key === requested);
  return opt && opt.enabled ? requested : "powershell";
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

  // files (#214): the root is mandatory — unlike a terminal, "" can't fall back
  // to home, because a file tree over the whole home directory is never what the
  // user meant, and a rootless files pane has no content at all.
  if (input.kind === "files") {
    if (!repo) {
      return {
        ok: false,
        error: "The file explorer needs a folder — pick one first.",
        focus: "repo",
      };
    }
    const name = input.name.trim() || pathTail(repo) || "files";
    return { ok: true, plan: { kind: "files", root: repo, name } };
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

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
// MANAGER, rooted at a directory the user picks. Its only setup input is that
// root, and the only rule this module can decide is "a root was given"; whether
// the directory actually EXISTS is I/O, so the form probes it (ftRootIsDir)
// after this returns ok and surfaces a failure inline.
//
// #217 adds two more of the same family — `editor` (the #174 file tree + code
// editor, rooted at a folder) and `git` (the #208 git view, over a repo). All
// three are PTY-less CONTENT panes and validate the same way here: the path is
// mandatory, and its REALITY (a directory? a git repo?) is I/O the form probes.
// The one asymmetry is what "real" means per kind — a folder for files/editor, a
// git work tree for git — which is why the probe stays in the form and only the
// "a path was given" rule lives here.

// #222 adds a FOURTH content kind: `workflow` — the pane that makes
// `.loomux/workflow.yml` (the user-defined agent workflow: blocks, edges, gates)
// configurable. Its one input is the REPO the workflow file lives in, so it validates
// exactly like its three siblings: the path is mandatory here, and whether it is a
// readable directory is I/O the form probes.

// #360 Slice D adds a FIFTH content kind: `plugin` — an installed pane plugin (see
// doc/design/pane-plugins.md), hosted in its own isolated WebviewWindow (Slice C).
// It breaks the "one input is a path" pattern its four siblings share: a plugin
// pane's identity is WHICH PLUGIN, not a folder or repo, so its one input is a
// `pluginId` chosen from the installed set (`list_plugins`) rather than a typed
// path. It is still a content kind — no CLI, no shell, no PTY — so it joins
// `isContentKind` the same way; only its OWN validation branch differs from the
// shared "the path is mandatory" rule below.

export type PaneKind =
  | "agent"
  | "orchestrator"
  | "terminal"
  | "files"
  | "editor"
  | "git"
  | "workflow"
  | "plugin";
export type ShellKind = "powershell" | "gitbash" | "cmd";

const AGENT_MIN = 1;
const AGENT_MAX = 8;

/** The PTY-less CONTENT kinds (#214 files, #217 editor + git, #222 workflow, #360 Slice
 *  D plugin): a pane that IS a surface rather than a process. They spawn nothing, pick
 *  no CLI, and take exactly one input each — a folder/repo for the first four, an
 *  installed plugin's id for the fifth — which is why the welcome form can hide every
 *  other field off this one predicate instead of listing the kinds at each site (and
 *  forgetting one when a sixth arrives). */
export function isContentKind(kind: PaneKind): boolean {
  return (
    kind === "files" ||
    kind === "editor" ||
    kind === "git" ||
    kind === "workflow" ||
    kind === "plugin"
  );
}

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
  /** Installed plugin chosen in the picker (plugin kind, #360 Slice D); "" until
   *  one is picked (or when no plugin is installed). Ignored by every other kind. */
  pluginId: string;
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
/** A file-explorer pane (#214): a directory to root the manager at, and a name. No
 *  command, no shell, no PTY — the pane's content IS the file manager. */
export interface FilesPlan {
  kind: "files";
  /** Absolute directory the listing roots at. Non-empty (validated below); its
   *  EXISTENCE is checked by the caller (I/O), not here. */
  root: string;
  name: string;
}
/** A file-EDITOR pane (#217): the #174 tree + code editor as a pane's permanent
 *  content, rooted at a folder. Same shape as FilesPlan and validated the same
 *  way — a different surface over the same one input. */
export interface EditorPlan {
  kind: "editor";
  root: string;
  name: string;
}
/** A GIT pane (#217): the git view (graph, status, diffs, staging, #208 worktree
 *  switching) as a pane's permanent content. `root` need only be SOME directory
 *  inside a work tree — the view resolves the top level itself — but it must be
 *  one, which is I/O the caller probes (gitRepoRoot), not a rule this module can
 *  decide. Named `root` like its two siblings, deliberately: a content pane has ONE
 *  input and every consumer (the pane, the capture, the restore) treats it the same
 *  way, so calling it `repo` here would buy a synonym and cost a special case. */
export interface GitPlan {
  kind: "git";
  root: string;
  name: string;
}
/** A WORKFLOW pane (#222): `.loomux/workflow.yml` — the repo's agent workflow (blocks,
 *  advisory edges, enforced gates) — as an editable surface. `root` is the repo the file
 *  lives in; the pane derives the path from it, so the kind still takes exactly ONE input
 *  like its three siblings. A repo with no workflow file yet is not an error: the pane
 *  opens on an empty state that offers to create one. */
export interface WorkflowPlan {
  kind: "workflow";
  root: string;
  name: string;
}
/** A PLUGIN pane (#360 Slice D): an installed plugin, hosted in its own isolated
 *  WebviewWindow (Slice C). Unlike its four content-kind siblings, its one input
 *  is not a path — `pluginId` names WHICH plugin, chosen from `list_plugins`
 *  (Slice B) — so it carries no `root` at all. */
export interface PluginPlan {
  kind: "plugin";
  pluginId: string;
  name: string;
}
export type PaneSetupPlan =
  | TerminalPlan
  | AgentPlan
  | OrchestratorPlan
  | FilesPlan
  | EditorPlan
  | GitPlan
  | WorkflowPlan
  | PluginPlan;

/** The per-kind halves of the content-pane rule: what to call the missing path in
 *  the error, and what to fall back to when the human names the pane nothing. The
 *  RULE itself (the path is mandatory) is one branch below, not three. Plugin is
 *  NOT here — its one input is a pluginId, not a path, so it gets its own branch
 *  in planPaneSetup rather than forcing "pluginId" through wording meant for a
 *  folder/repo. */
const CONTENT_SETUP: Record<
  "files" | "editor" | "git" | "workflow",
  { missing: string; fallbackName: string }
> = {
  files: { missing: "The file explorer needs a folder — pick one first.", fallbackName: "files" },
  editor: { missing: "The file editor needs a folder — pick one first.", fallbackName: "editor" },
  git: { missing: "The git view needs a repository — pick one first.", fallbackName: "git" },
  workflow: {
    missing: "The workflow pane needs a repository — pick one first.",
    fallbackName: "workflow",
  },
};

/** Which field to focus when validation fails, so the form can surface it. */
export type PaneSetupFocus = "repo" | "custom" | "count" | "plugin";

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

  // PLUGIN (#360 Slice D): the one content kind whose input isn't a path. A pluginId
  // must have been picked from the installed set; there is no "reality" probe left for
  // the form to run afterward (unlike its siblings) because picking FROM list_plugins is
  // itself the proof the plugin exists — the only way it can go stale between here and
  // open is an uninstall racing the submit, which the caller re-checks for real.
  if (input.kind === "plugin") {
    const pluginId = input.pluginId.trim();
    if (!pluginId) {
      return { ok: false, error: "Pick an installed plugin first.", focus: "plugin" };
    }
    const name = input.name.trim() || pluginId;
    return { ok: true, plan: { kind: "plugin", pluginId, name } };
  }

  // The CONTENT kinds (#214 files, #217 editor + git, #222 workflow). ONE rule, because
  // they have one: the path is mandatory. Unlike a terminal, "" can't fall back to home —
  // a file tree over the whole home directory is never what the user meant, a rootless
  // content pane has no content at all, and "home" is not a repo. What differs per kind
  // is only the wording (CONTENT_SETUP), and whether the path is REAL — a directory? a
  // work tree? — which is I/O the form probes, not a rule this module can decide.
  if (isContentKind(input.kind)) {
    const kind = input.kind as "files" | "editor" | "git" | "workflow";
    const setup = CONTENT_SETUP[kind];
    if (!repo) return { ok: false, error: setup.missing, focus: "repo" };
    const name = input.name.trim() || pathTail(repo) || setup.fallbackName;
    return { ok: true, plan: { kind, root: repo, name } };
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

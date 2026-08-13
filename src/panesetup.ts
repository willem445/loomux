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

// #887 (slice S3) adds an EIGHTH kind: `ssh` — a pane whose process is a local
// ssh.exe client connected to a remote host, optionally launching an agent CLI
// there. Its declaration is an SSH PROFILE (sshprofile.ts, slice S1), and the
// argv it spawns is built by the pure builder (sshcommand.ts, slice S2); this
// module is the seam between them, so the whole compose path is unit-tested
// without a DOM. What it validates is what only it can: that a profile was
// chosen at all, and that the destination is still one ssh will read as a HOST
// rather than as an option.

// Explicit `.ts` on both (the convention promote.ts / sessionroute.ts document):
// these are VALUE imports in a module `node --test` loads directly, so they have
// to resolve without a bundler.
import {
  normalizeSshProfile,
  sshDestinationOrNull,
  MAX_KEEPALIVE_SECONDS,
  MAX_SSH_PORT,
  MIN_KEEPALIVE_SECONDS,
  MIN_SSH_PORT,
  type SshProfile,
} from "./sshprofile.ts";
import {
  buildSshArgv,
  claudeSessionArgs,
  sshResumeArgv,
  type SshCommandParams,
} from "./sshcommand.ts";

export type PaneKind =
  | "agent"
  | "orchestrator"
  | "terminal"
  | "files"
  | "editor"
  | "git"
  | "workflow"
  | "ssh";
export type ShellKind = "powershell" | "gitbash" | "cmd";

const AGENT_MIN = 1;
const AGENT_MAX = 8;

/** The PTY-less CONTENT kinds (#214 files, #217 editor + git, #222 workflow): a pane that
 *  IS a surface rather than a process. They spawn nothing, pick no CLI, and take exactly
 *  one input — the folder / repo they are rooted at — which is why the welcome form
 *  can hide every other field off this one predicate instead of listing the kinds at
 *  each site (and forgetting one when a fifth arrives). */
export function isContentKind(kind: PaneKind): boolean {
  return kind === "files" || kind === "editor" || kind === "git" || kind === "workflow";
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
  /** SSH kind (#887 S3): the connection this pane launches, as the form has it
   *  RIGHT NOW — the picked profile with the inline editor's fields already
   *  applied, since those fields ARE the profile's (the form is the profile
   *  editor, and submitting saves what it launches). `null` when the human has
   *  picked nothing yet, or has a half-filled new connection. */
  sshProfile: SshProfile | null;
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
/** An SSH pane (#887 S3): a local `ssh.exe` connected to the profile's host, with
 *  the profile's `defaultCli` (if any) launched on the far end. The plan carries
 *  the whole PROFILE rather than a copy of its fields — every consumer downstream
 *  (the argv composer, the saved store, the pane's own record of which connection
 *  it belongs to) wants the same object, and a flattened copy is how the launched
 *  command and the saved profile come to disagree.
 *
 *  No `cwd`: an SSH pane's LOCAL working directory is deliberately home. The repo
 *  lives on the remote host and no local path corresponds to it, so a local cwd
 *  here would be a directory the human never picked, feeding local-filesystem
 *  chrome (git watch, the folder picker) that cannot mean anything for this pane.
 *  The REMOTE directory is `profile.remoteCwd`, quoted into the remote command by
 *  sshcommand.ts. */
export interface SshPlan {
  kind: "ssh";
  /** The connection to launch — validated here, saved by the form. */
  profile: SshProfile;
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
  | SshPlan;

/** The per-kind halves of the content-pane rule: what to call the missing path in
 *  the error, and what to fall back to when the human names the pane nothing. The
 *  RULE itself (the path is mandatory) is one branch below, not three. */
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
export type PaneSetupFocus = "repo" | "custom" | "count" | "ssh";

export type PaneSetupResult =
  | { ok: true; plan: PaneSetupPlan }
  | { ok: false; error: string; focus?: PaneSetupFocus };

/** A one-shot, re-entrancy-proof latch for an async action that spans `await`s
 *  while its trigger stays on screen. Two users today: the welcome form's
 *  `submit()` (#194 rev-74 HIGH-1, described below) and the SSH reconnect
 *  (#887 S4 — see `withSubmitLatch`), which is the SAME defect one step
 *  removed, so it reuses this rather than growing a second latch beside it.
 *
 *  The form's `submit()` spans `await`s (CLI probe,
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

/** Run `attempt` only when `latch` is free, and hand every concurrent caller
 *  `busy()` instead of a second run — the SINGLE-FLIGHT rule for an action that
 *  must never overlap itself, however many buttons or code paths can trigger it
 *  (#887 S4 / PR #926 review round 2 B1).
 *
 *  Why this exists as a function rather than three `begin()`/`release()` call
 *  sites: the defect it closes was precisely a SECOND trigger that skipped the
 *  first one's gate. The SSH reconnect card has two actions — Reconnect and
 *  Reconnect fresh — and the second one was routed around the card's own pending
 *  state, so a click on each spawned **two ssh clients and two remote agent CLIs**
 *  while the pane could only bind one: the loser's output routes nowhere, its
 *  exit lands unclaimed, and because a kill goes through the pane's `ptyId`
 *  nothing in loomux can stop it — an unaccountable agent left running on someone
 *  else's machine, in a feature whose whole restore policy is argued from not
 *  spending remote credits unattended. Putting the gate at the one function every
 *  reconnect already funnels through makes that structural: a future third
 *  entry point (a menu item, a shortcut) is gated by construction rather than by
 *  remembering.
 *
 *  `release()`, not `finish()`: a reconnect that FAILED must stay retryable — the
 *  card's whole purpose is the retry — and a successful one takes its card with
 *  it. This latch is therefore never permanently spent, which is why the one-shot
 *  half of `SubmitLatch` is deliberately unused here.
 *
 *  Pure and DOM-free, so the concurrency rule is unit-tested rather than argued
 *  about (`test/panesetup.test.ts`) — the buttons that call it are not testable,
 *  but the rule that makes two of them safe is. */
export async function withSubmitLatch<T>(
  latch: SubmitLatch,
  busy: () => T,
  attempt: () => Promise<T>
): Promise<T> {
  if (!latch.begin()) return busy();
  try {
    return await attempt();
  } finally {
    latch.release();
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

// ---------- SSH panes (#887 S3) ----------

export const SSH_NO_PROFILE =
  "Pick an SSH connection — or fill in a new one (a name and a destination) first.";

/** The refusal for a destination ssh would read as an option rather than a host.
 *  Quotes the value back: the human typed it, and a refusal that doesn't say
 *  which of several fields it means is a refusal they have to guess at. */
export function sshDestinationError(destination: string): string {
  return (
    `"${destination}" isn't a destination ssh can use — it must be a host, user@host, or an ` +
    `ssh_config alias, with no leading "-" and no spaces (ssh reads a leading dash as one of ` +
    `its own options, not as a host).`
  );
}

/** Whether loomux mints a session id for a CLI launched on the REMOTE host.
 *
 *  Claude only, and for a structural reason rather than a preference: claude's
 *  session identity is a value loomux puts ON THE COMMAND LINE (`--session-id`),
 *  which survives the trip through ssh untouched. Every other CLI's id is
 *  DISCOVERED by reading a local store afterwards (copilot's session-state dir,
 *  opencode's SQLite) — mechanisms that reach the machine loomux runs on, not
 *  the host the agent is running on. So a remote copilot/opencode pane gets no
 *  recorded id, which is what makes its restore honest rather than a `--resume`
 *  of an id nobody can look up (#887 plan part 4a). One implementation, called
 *  by both the launcher (deciding whether to mint) and the composer below
 *  (deciding whether to emit the flag), so those two can never disagree. */
export function sshMintsSessionId(remoteCli: string | null): boolean {
  return remoteCli === "claude";
}

/** The refusal for a value the human typed that the profile store would DISCARD
 *  — or `""` when nothing was discarded.
 *
 *  Silently dropping a typed value is the defect this exists to prevent (PR #921
 *  review, rev-441). Normalization has to drop an out-of-range port, an
 *  out-of-range keepalive, or an "identity file" that is actually key material —
 *  those are the store's own guards and they are right — but dropping them
 *  QUIETLY means a human types `99999`, hits Create, connects on port 22, and is
 *  never told. Worse for `identityFile`: the pane would connect with no `-i` at
 *  all and the failure would surface as an authentication problem with no
 *  visible cause.
 *
 *  So the launch seam compares what was typed against what survives and refuses
 *  the difference, rather than launching something the human didn't ask for.
 *  This is deliberately NOT a second copy of the bounds: it asks the store's own
 *  normalizer what it kept, so a bound can only ever be changed in one place.
 *
 *  Only reachable for values typed into the form. A profile loaded from disk was
 *  normalized on the way in, so `raw` and `kept` agree and nothing is refused —
 *  which is why this cannot bounce a human's existing saved connections. */
export function sshDiscardedFieldError(raw: SshProfile, kept: SshProfile): string {
  if (raw.port !== null && kept.port === null) {
    return `A port must be a whole number between ${MIN_SSH_PORT} and ${MAX_SSH_PORT} — leave it blank to let your ssh config decide.`;
  }
  if (raw.keepaliveSeconds !== null && kept.keepaliveSeconds === null) {
    return (
      `Keepalive must be a whole number of seconds between ${MIN_KEEPALIVE_SECONDS} and ` +
      `${MAX_KEEPALIVE_SECONDS} — leave it blank to let your ssh config decide.`
    );
  }
  if (raw.identityFile !== null && kept.identityFile === null) {
    return (
      "The identity file must be a PATH to a private key, not the key itself — loomux never " +
      "stores key material, so a pasted key is refused rather than written to sshprofiles.json."
    );
  }
  return "";
}

/** A warning for a remote folder that this launch cannot act on, or `""`.
 *
 *  `remoteCwd` is `cd`'d into as part of the REMOTE COMMAND, so with no remote
 *  CLI there is no command to prefix and the value does nothing — S2's builder
 *  has always worked this way, and the composer follows it rather than adding a
 *  second mechanism.
 *
 *  Warned about rather than either honoured or refused, and both alternatives
 *  are worse. Honouring it would mean synthesizing a login-shell command
 *  (`cd … && exec $SHELL -l`), which is a GUESS about the remote's login shell —
 *  the exact class of guess `remoteShell` exists to refuse (plan part 4a: the
 *  remote's shell is unknowable from here, so the user declares it). Refusing to
 *  SAVE it would throw away a setting that becomes correct the moment the human
 *  picks a CLI. So the value is kept, and the human is told when it won't apply.
 *  Not silent, not lost, not guessed at. */
export function sshRemoteCwdWarning(remoteCli: string | null, remoteCwd: string | null): string {
  if (!remoteCwd || remoteCli) return "";
  return (
    "Remote folder applies only when a remote CLI is set — a plain login shell starts wherever " +
    "the remote host puts it. The folder stays saved on this connection."
  );
}

/** A warning for a profile naming a remote CLI this build doesn't know, or `""`.
 *
 *  Deliberately a warning and not a refusal — S1's own contract: a profile
 *  naming an unknown CLI "is a profile to warn about, not a profile to silently
 *  delete on load". `defaultCli` is only ever a program name to run on the far
 *  host; loomux's catalog decides what it can add flags for (see
 *  `sshMintsSessionId`), not what the remote machine is allowed to have
 *  installed. So the launch proceeds and the human is told what they will and
 *  won't get. */
export function sshRemoteCliWarning(remoteCli: string | null, known: readonly string[]): string {
  if (!remoteCli || known.includes(remoteCli)) return "";
  return (
    `"${remoteCli}" isn't a CLI loomux knows — it will be run on the remote host exactly as ` +
    `written, with no session id and no autopilot flags.`
  );
}

/** Compose a validated SSH plan into the flat parameters sshcommand.ts (S2)
 *  takes. This is the S1→S2 seam: the profile is the user's declaration, the
 *  params are what one launch of it means.
 *
 *  `sessionId` is the loomux-minted claude session id (the launcher mints it
 *  with the webview's Web Crypto), or null. It is emitted only when the remote
 *  CLI is one loomux mints for — a `--session-id` handed to copilot would be an
 *  unknown flag, and the pane would die on its first line of output.
 *
 *  Deliberately does NOT re-check the destination. It is checked exactly once
 *  on the way in, at the launch seam (`planPaneSetup` above, which bounces it
 *  back to the human legibly) and once at the far end as `buildSshArgv`'s own
 *  backstop for callers that never planned. A third copy here would be a guard
 *  no input can reach — the shape of dead code that reads as a live protection
 *  (#907 review NF1's lesson, applied forward). */
export function sshLaunchParams(profile: SshProfile, sessionId: string | null): SshCommandParams {
  const remoteCli = profile.defaultCli;
  const remoteCommand = remoteCli
    ? sessionId && sshMintsSessionId(remoteCli)
      ? [remoteCli, ...claudeSessionArgs(sessionId, "fresh")]
      : [remoteCli]
    : undefined;
  return {
    destination: profile.destination,
    // `?? undefined` throughout: the profile spells "unset" as null, and S2
    // spells it as an absent key — an explicit `port: undefined` would still be
    // an own property, but `undefined` is what its `!== undefined` tests read as
    // absent, so the two vocabularies meet here and nowhere else.
    port: profile.port ?? undefined,
    identityFile: profile.identityFile ?? undefined,
    keepaliveSeconds: profile.keepaliveSeconds ?? undefined,
    extraArgs: profile.extraArgs.length ? profile.extraArgs : undefined,
    remoteShell: profile.remoteShell,
    // Only meaningful with a remote command: the `cd` is a PREFIX to that
    // command, so with no command there is nothing to prefix (S2's builder has
    // the same shape — this makes it explicit rather than emergent). The value is
    // not lost, and the human is not left guessing: it stays on the saved
    // connection, and `sshRemoteCwdWarning` says so while the form is still open.
    remoteCwd: remoteCommand ? profile.remoteCwd ?? undefined : undefined,
    remoteCommand,
  };
}

/** The full argv one SSH launch spawns: `program` is the RESOLVED local ssh.exe
 *  (the launcher probes for it — PATH first, then the System32 OpenSSH install),
 *  which is also the fake-ssh seam S2 documents.
 *
 *  Throws only what `buildSshArgv` throws — a remote-command token cmd.exe
 *  cannot be given safely (a newline, a trailing backslash). The launcher
 *  catches that and surfaces it inline, because it is a fixable data problem
 *  (the remote folder, usually), not a bug. */
export function sshLaunchArgv(program: string, profile: SshProfile, sessionId: string | null): string[] {
  return buildSshArgv(program, sshLaunchParams(profile, sessionId));
}

/** Shown (and refused with) when no OpenSSH client can be found. Names the two
 *  places loomux looked and the feature that installs one, so the human can act
 *  on it instead of just being told no.
 *
 *  Lives here rather than in launcher.ts because it now has TWO surfaces (#887
 *  S4): the launch form and a restored pane's Reconnect card, which is not a
 *  form at all. One message, in the module the other SSH refusals already live
 *  in — a second copy in main.ts is how the two would come to say different
 *  things about the same failure. */
export const SSH_NO_CLIENT =
  "No ssh client found — loomux looked on PATH and in the Windows OpenSSH install " +
  "(System32\\OpenSSH). Install the OpenSSH Client optional feature, or put ssh.exe on PATH.";

/** Shown on a restored SSH pane whose saved connection is no longer in the store
 *  (#887 S4) — deleted since, or lost when a corrupt `sshprofiles.json` was
 *  quarantined. Deliberately offers no fallback: the pane records the CONNECTION,
 *  not a command line (see `PersistedPane.sshProfileId`), so there is nothing left
 *  to reconnect WITH, and inventing a connection out of a stale command line is
 *  how a pane would silently connect somewhere the human removed on purpose. */
export const SSH_PROFILE_GONE =
  "This pane's saved SSH connection no longer exists — it was removed, or the connections " +
  "file was reset. Open a new SSH pane to connect again.";

/** What one Reconnect click launches: the argv to spawn, and the session id the
 *  pane should record for NEXT time (which is the recorded one on a resume, a
 *  freshly minted one on a fresh claude connect, and null when the remote CLI
 *  has no command-line identity at all). */
export interface SshReconnect {
  argv: string[];
  sessionId: string | null;
  /** Whether this reconnect resumes the recorded remote session or starts a new
   *  one. The caller says which in the UI — "reconnected" and "reconnected, but
   *  your previous conversation is not the one on screen" are different facts. */
  mode: "resume" | "fresh";
}

/** Rebuild an SSH pane's launch from its SAVED PROFILE for a reconnect (#887 S4)
 *  — the dormant restore card and the post-disconnect exit card both call this,
 *  so the two can never drift into reconnecting differently.
 *
 *  The profile is re-read from the store at click time and everything is
 *  re-derived from it. That is the point rather than an implementation detail: a
 *  connection the human edited between boots (a new port, a different remote
 *  folder) reconnects with the edit, exactly as S1's `SshProfile.id` contract
 *  promises. Nothing here replays a captured command line.
 *
 *  Resume vs fresh is decided by the profile as it is NOW, not by the record:
 *
 *   - The recorded id is resumed only when this profile still launches a CLI
 *     loomux mints ids for (`sshMintsSessionId`, i.e. claude). A profile switched
 *     to copilot since, or to a plain login shell, must NOT be handed a claude
 *     session id — copilot would reject the unknown flag and a login shell has
 *     nowhere to put it, and in both cases the id names a conversation on the far
 *     host that the new CLI cannot read anyway.
 *   - With no recorded id (a remote copilot/opencode pane, a login shell, or a
 *     claude pane whose id was never captured) it is a plain fresh connect. For a
 *     claude profile that means minting a NEW id — so the reconnected session is
 *     itself resumable next boot, the same way `agentFreshCommand` keeps a local
 *     restore resumable.
 *
 *  `mintSessionId` is injected rather than called from here: minting is Web Crypto
 *  (`crypto.randomUUID`, the webview's — never a getrandom crate; CLAUDE.md
 *  constraint 2 governs the Rust graph), and taking it as a parameter is what
 *  keeps this module pure and its fresh/resume split unit-testable.
 *
 *  Throws only what `buildSshArgv` throws (a remote-command token cmd.exe cannot
 *  be handed safely — a newline, a trailing backslash), which the card surfaces
 *  as its error state, the same way the launch form does. */
export function sshReconnectArgv(
  program: string,
  profile: SshProfile,
  recordedSessionId: string | null,
  mintSessionId: () => string
): SshReconnect {
  const mints = sshMintsSessionId(profile.defaultCli);
  if (mints && recordedSessionId) {
    // `sshLaunchParams` composes the FRESH shape (`--session-id <id>`) and
    // `sshResumeArgv` rewrites that one remote command into its resume shape
    // (`--resume <id>`) — S2's own once-implemented rewrite, rather than a second
    // "compose it in resume form" path here that would need its own coverage.
    return {
      argv: sshResumeArgv(program, sshLaunchParams(profile, recordedSessionId), recordedSessionId),
      sessionId: recordedSessionId,
      mode: "resume",
    };
  }
  const sessionId = mints ? mintSessionId() : null;
  return { argv: sshLaunchArgv(program, profile, sessionId), sessionId, mode: "fresh" };
}

/** One side's view of what a pane is about to be: the options a spawn was handed,
 *  or the state the pane already carries. The two are the SAME SHAPE on purpose —
 *  see `sshOrchestrationRefusal`, which reads every field of both by one rule. */
export interface SshSpawnIdentity {
  /** Whether this side says the pane's process is a local ssh client. */
  ssh?: boolean;
  orchGroup?: string | null;
  orchRole?: string | null;
  orchAgent?: string | null;
}

/** The #887/#888 boundary, enforced rather than merely documented: an SSH pane
 *  can never be a member of an orchestration group.
 *
 *  Returns the refusal to throw, or null when the spawn is legitimate. The
 *  reasons are concrete and none of them degrade gracefully (plan part 4c):
 *  worktrees are local dirs made by local git; the MCP server is loopback-only
 *  and its per-agent config reaches only children loomux spawns itself; the gh
 *  shim reaches only locally-spawned children, so a remote `gh` would face NO
 *  merge gate at all — a security regression, and the reason this is a refusal
 *  instead of a best-effort degradation; and every brief a local orchestrator
 *  writes names local paths.
 *
 *  **Both inputs are read by one rule: the UNION of the two sides, field by
 *  field.** A pane spawn has two sources of truth — the `opts` describing what it
 *  is about to become, and the pane's own existing state — and a guard that read
 *  ssh-ness from both while reading the orchestration identity from only one
 *  would have a hole exactly the width of that asymmetry: an ALREADY-orchestrated
 *  pane relaunched with `ssh` in its options carries its group on the pane and
 *  nothing in the options, so an opts-only read of the identity would wave it
 *  through and spawn an ssh client inside a live group member (PR #921 review,
 *  rev-441). Neither side is authoritative alone, so neither is trusted alone.
 *  Doing the merge HERE rather than at the call sites is the other half of that:
 *  the rule is in the module that is unit-tested, not in two DOM call sites that
 *  can drift apart.
 *
 *  Nothing in the spawn path builds this combination today: the backend's spawn
 *  requests carry a role and a command, never a pane kind or an SSH profile, and
 *  the group surfaces never offer one. This exists so that stays true — a future
 *  edit that wires an SSH profile into a delegate spawn fails loudly at the pane,
 *  before any process starts, instead of quietly producing a group member whose
 *  merge gate isn't enforced. Fail-closed by construction: it refuses on ANY
 *  orchestration marker from EITHER side, so a spawn that carries only half an
 *  identity, on only one side, is refused too. */
export function sshOrchestrationRefusal(
  opts: SshSpawnIdentity,
  pane: SshSpawnIdentity = {}
): string | null {
  const ssh = !!opts.ssh || !!pane.ssh;
  if (!ssh) return null;
  const identity =
    opts.orchGroup ||
    pane.orchGroup ||
    opts.orchRole ||
    pane.orchRole ||
    opts.orchAgent ||
    pane.orchAgent;
  if (!identity) return null;
  return (
    "refusing to give an SSH pane an orchestration identity: SSH panes are solo panes in v1 " +
    "(#887) — worktrees, the loopback MCP server and the gh merge gate all reach only " +
    "locally-spawned children, so a remote group member would run with the merge gate " +
    "unenforced. Remote orchestration is #888."
  );
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

  // SSH (#887 S3). Two rules, and they are the only two this module can decide:
  // a connection was chosen, and its destination is still something ssh will
  // read as a HOST. Everything else about an SSH launch is either the profile's
  // own business (validated by sshprofile.ts on the way to and from disk) or I/O
  // the form does after this returns ok — resolving ssh.exe, saving the profile.
  if (input.kind === "ssh") {
    const raw = input.sshProfile;
    if (!raw) return { ok: false, error: SSH_NO_PROFILE, focus: "ssh" };
    // Through the STORE's own normalizer, not a second set of field rules here.
    // A profile reaching this point may never have touched the disk — the
    // launcher's inline editor builds one out of text typed seconds ago — so
    // this is where it meets the same guards `sshprofiles.json` applies, and
    // it is what makes the profile this pane LAUNCHES identical to the one the
    // form SAVES (an identity file carrying key material, or an out-of-range
    // port, is dropped from both or from neither).
    const profile = normalizeSshProfile(raw);
    if (!profile) {
      // Normalization refuses an entry for exactly three fields, and only one of
      // them can be wrong in a way the human should be told about specifically:
      // a destination that isn't one. A leading `-` is the case that matters —
      // it is an ssh OPTION rather than a host, and `-oProxyCommand=…` runs a
      // command on the LOCAL machine — so it is refused here, legibly and before
      // any spawn, rather than left to the exception `buildSshArgv` throws as
      // its own backstop. (A blank id/name is the other route, and reads as the
      // half-filled form it is.)
      return sshDestinationOrNull(raw.destination) === null
        ? { ok: false, error: sshDestinationError(raw.destination), focus: "ssh" }
        : { ok: false, error: SSH_NO_PROFILE, focus: "ssh" };
    }
    // A value that survived the entry but not the FIELD — an out-of-range port,
    // an "identity file" that is key material. Normalization is right to drop
    // those, but dropping them silently would launch a connection the human
    // didn't describe and never told them so. Refuse the difference instead.
    const discarded = sshDiscardedFieldError(raw, profile);
    if (discarded) return { ok: false, error: discarded, focus: "ssh" };
    const name = input.name.trim() || profile.name || profile.destination;
    return { ok: true, plan: { kind: "ssh", profile, name } };
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

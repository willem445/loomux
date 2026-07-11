// The in-pane welcome / pane-setup surface (#194). Shown on a fresh start, a new
// tab, and a pane split: the user picks what the pane becomes —
//   Agent        — one or N coding-agent CLI panes (a worktree name fans out to
//                  name-1 … name-N so every agent gets an isolated worktree)
//   Orchestrator — an orchestrator pane + N idle workers with guardrails
//                  (max live agents, pinned models, permission mode)
//   Terminal     — a plain shell; the shell-kind picker spawns PowerShell, cmd,
//                  or Git Bash (#194 P2). Git Bash is enabled only when a
//                  Git-for-Windows install is discovered (else disabled, with a
//                  reason surfaced on the option).
//   File explorer— a PTY-less pane hosting the file tree + editor rooted at a
//                  folder (#214). Its only input is that folder; it is validated
//                  for real (does the directory exist?) before the pane is made,
//                  so a typo'd path shows an inline error instead of an empty tree.
//
// This replaces the old modal launcher AND the global "agent mode" toggle: there
// is no global mode anymore, every pane declares its kind here at creation. The
// form is DOM; the kind-selection + validation core is the pure `panesetup.ts`
// (unit-tested). The form owns worktree creation so a failure surfaces inline and
// the user can fix the name and retry instead of losing their input.

import { open } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { gitWorktreeAdd } from "./git";
import type { OrchestratorConfig } from "./orchestration";
import type { PaneKind, PaneSetupInput, ShellKind, ShellKindAvailability } from "./panesetup";
import {
  planPaneSetup,
  worktreeNameFor,
  SubmitLatch,
  shellKindOptions,
  resolveShellKind,
} from "./panesetup";
import { discoverGitBash } from "./pty";
import { ftRootIsDir } from "./fileapi";
import {
  AGENTS,
  addRecentRepo,
  getAutopilot,
  getCustomCommand,
  getDefaultAgent,
  getRecentRepos,
  setAutopilot,
  setCustomCommand,
  setDefaultAgent,
} from "./agents";

export interface AgentLaunchSpec {
  name: string;
  /** Repo, worktree, or plain folder; undefined = home directory. */
  cwd?: string;
  command: string;
  /** Recorded resumable session id (#194 P4): minted here for a session-capable
   *  CLI (Claude) and passed to the pane so its layout snapshot can `--resume`
   *  the exact session on restore. Absent for best-effort CLIs / custom commands. */
  sessionId?: string;
}

/** What a submitted welcome form resolves to — the caller (main.ts) spawns the
 *  chosen kind: a terminal converts the setup pane in place, agent panes fan out
 *  from it, and an orchestrator opens its own project tab. */
export type WelcomeResult =
  | { kind: "terminal"; name: string; cwd?: string; shellKind: ShellKind }
  | { kind: "panes"; specs: AgentLaunchSpec[] }
  | { kind: "orchestrator"; config: OrchestratorConfig }
  /** A file-explorer pane (#214): `root` is a directory this form has already
   *  confirmed exists, so the caller converts the setup pane in place. */
  | { kind: "files"; name: string; root: string };

/** Orchestration roles the setup form configures a CLI + model for. Mirrors the
 *  backend `Role` variants that can be spawned in a group (issue #4/#47). */
type OrchRole = "orchestrator" | "worker" | "reviewer" | "planner";
const ORCH_ROLES: { key: OrchRole; label: string }[] = [
  { key: "orchestrator", label: "Orchestrator" },
  { key: "worker", label: "Worker" },
  { key: "reviewer", label: "Reviewer" },
  { key: "planner", label: "Planner" },
];

interface OrchCli {
  id: string;
  models: string[];
  defaults: Record<OrchRole, string>;
}
const ORCH_CLIS: OrchCli[] = [
  {
    id: "claude",
    models: ["sonnet", "opus", "haiku", "fable"],
    // Reasoning-heavy roles (orchestrator, planner) default to the strong
    // tier; executing roles (worker, reviewer) to the mid tier.
    defaults: { orchestrator: "opus", worker: "sonnet", reviewer: "sonnet", planner: "opus" },
  },
  {
    id: "copilot",
    models: ["auto", "claude-sonnet-4.6", "claude-haiku-4.5", "gpt-5.2", "gpt-5.3-codex"],
    defaults: { orchestrator: "auto", worker: "auto", reviewer: "auto", planner: "auto" },
  },
];

const basename = (p: string): string => p.split(/[\\/]/).filter(Boolean).pop() ?? "";

/** Labeled form field wrapper. `hint` renders subdued after the label. */
function field(label: string, control: HTMLElement, hint?: string): HTMLElement {
  const wrap = document.createElement("div");
  wrap.className = "dlg-field";
  const lab = document.createElement("div");
  lab.className = "dlg-label";
  lab.textContent = label;
  if (hint) {
    const h = document.createElement("span");
    h.className = "opt";
    h.textContent = ` — ${hint}`;
    lab.appendChild(h);
  }
  wrap.append(lab, control);
  return wrap;
}

function select(options: [string, string][], value?: string): HTMLSelectElement {
  const sel = document.createElement("select");
  sel.className = "dlg-select";
  for (const [val, label] of options) {
    const opt = document.createElement("option");
    opt.value = val;
    opt.textContent = label;
    sel.appendChild(opt);
  }
  if (value) sel.value = value;
  return sel;
}

function numberInput(value: number, min: number, max: number): HTMLInputElement {
  const input = document.createElement("input");
  input.className = "dlg-input dlg-num";
  input.type = "number";
  input.min = String(min);
  input.max = String(max);
  input.value = String(value);
  return input;
}

const intVal = (el: HTMLInputElement, fallback: number): number => {
  const n = parseInt(el.value, 10);
  return Number.isFinite(n) ? n : fallback;
};

/** Result of probing an agent CLI on this machine (backend, cached). */
interface CliProbe {
  available: boolean;
  models: string[];
  error: string | null;
}

/** Model dropdown with a "custom…" escape hatch. A plain datalist doesn't
 *  work here: browsers filter its suggestions by the input's current text,
 *  so a pre-filled default hides every other option. */
class ModelPicker {
  readonly root: HTMLElement;
  private sel: HTMLSelectElement;
  private custom: HTMLInputElement;
  private static CUSTOM = "__custom";

  constructor() {
    this.root = document.createElement("div");
    this.root.className = "model-picker";
    this.sel = document.createElement("select");
    this.sel.className = "dlg-select";
    this.custom = document.createElement("input");
    this.custom.className = "dlg-input";
    this.custom.placeholder = "model id…";
    this.custom.spellcheck = false;
    this.custom.hidden = true;
    this.sel.addEventListener("change", () => {
      this.custom.hidden = this.sel.value !== ModelPicker.CUSTOM;
      if (!this.custom.hidden) this.custom.focus();
    });
    this.root.append(this.sel, this.custom);
  }

  /** Rebuild the options, keeping the current choice when still valid. */
  setOptions(models: string[], fallback: string): void {
    const current = this.value || fallback;
    this.sel.replaceChildren(
      ...models.map((m) => {
        const o = document.createElement("option");
        o.value = m;
        o.textContent = m;
        return o;
      })
    );
    const custom = document.createElement("option");
    custom.value = ModelPicker.CUSTOM;
    custom.textContent = "custom…";
    this.sel.appendChild(custom);
    if (models.includes(current)) {
      this.sel.value = current;
      this.custom.hidden = true;
    } else if (current) {
      this.sel.value = ModelPicker.CUSTOM;
      this.custom.value = current;
      this.custom.hidden = false;
    } else {
      this.sel.value = models[0] ?? ModelPicker.CUSTOM;
    }
  }

  get value(): string {
    if (!this.sel.options.length) return "";
    return this.sel.value === ModelPicker.CUSTOM ? this.custom.value.trim() : this.sel.value;
  }
}

export class WelcomeForm {
  /** The form root — mounted INSIDE a setup-state pane (not an overlay). */
  readonly el: HTMLElement;
  /** Called once with the chosen result; the caller spawns the kind. */
  onSubmit: ((result: WelcomeResult) => void) | null = null;

  private kindSel: HTMLSelectElement;
  private agentSel: HTMLSelectElement;
  private agentField: HTMLElement;
  private customField: HTMLElement;
  private customInput: HTMLInputElement;
  private countField: HTMLElement;
  private countInput: HTMLInputElement;
  private shellField: HTMLElement;
  private shellSel: HTMLSelectElement;
  /** Discovered shell-kind availability (#194 P2). Git Bash starts unavailable
   *  and is enabled once backend discovery resolves; PowerShell/cmd are always
   *  available on Windows. */
  private shellAvail: ShellKindAvailability = { gitBashPath: null };
  private repoField: HTMLElement;
  /** The repo field's caption. The same control is the Agent/Orchestrator
   *  "Repository" and the File-explorer "Folder" (#214) — one path input, two
   *  names, so the label follows the kind rather than lying about one of them. */
  private repoLabel: HTMLElement;
  private repoInput: HTMLInputElement;
  private repoList: HTMLDataListElement;
  private worktreeField: HTMLElement;
  private worktreeInput: HTMLInputElement;
  private nameField: HTMLElement;
  private nameInput: HTMLInputElement;
  // Autopilot toggle (agent kind): launch with the CLI's unattended "allow all"
  // flags. Default ON, persisted (#101).
  private autopilotField: HTMLElement;
  private autopilotInput: HTMLInputElement;
  // Orchestrator guardrails.
  private orchFields: HTMLElement;
  private workersInput: HTMLInputElement;
  private maxAgentsInput: HTMLInputElement;
  private idleKillInput: HTMLInputElement;
  private spawnRateInput: HTMLInputElement;
  private watchdogInput: HTMLInputElement;
  private autonomyBudgetInput: HTMLInputElement;
  /** Per-role CLI + model controls (issue #4, mixed agent types). Built once in
   *  the constructor; the group's default CLI (the top Agent field) seeds every
   *  role and can be overridden per role. */
  private roleControls: {
    key: OrchRole;
    cli: HTMLSelectElement;
    model: ModelPicker;
  }[];
  private permsSel: HTMLSelectElement;
  private agentWarn: HTMLElement;
  /** One probe per program per app run; backend caches too. */
  private probes = new Map<string, Promise<CliProbe>>();
  /** Autopilot flags per program, memoized. Empty string = the CLI has no
   *  unattended flag surface, so the toggle is hidden/inert for it (#101). */
  private autopilotFlags = new Map<string, Promise<string>>();

  private errorEl: HTMLElement;
  private submitBtn: HTMLButtonElement;
  /** True once the user hand-edits the pane name; stops auto-fill. */
  private nameDirty = false;
  /** One-shot re-entrancy guard across submit's async gaps (rev-74 HIGH-1): a
   *  double-click / Enter-repeat can't spawn a duplicate group or double-start a
   *  pane. Released on a validation error (retry allowed), finished once the
   *  result fires (the pane is being converted/retired). */
  private latch = new SubmitLatch();

  /** `defaultFolder` seeds the path field: the working directory of the pane this
   *  one is splitting from (or the tab's active pane), so a file explorer opened
   *  beside an agent defaults to THAT agent's worktree rather than to whatever repo
   *  was last used app-wide (#214). Falls back to the most recent repo, as before. */
  constructor(defaultFolder?: string) {
    this.el = document.createElement("div");
    this.el.className = "welcome-form";

    const dlg = document.createElement("div");
    dlg.className = "welcome-card";

    const title = document.createElement("h2");
    title.textContent = "New pane";
    const subtitle = document.createElement("p");
    subtitle.className = "welcome-sub";
    subtitle.textContent = "Pick what this pane becomes.";

    this.kindSel = select([
      ["agent", "Agent — a coding-agent CLI"],
      ["orchestrator", "Orchestrator + workers"],
      ["terminal", "Terminal — a shell"],
      ["files", "File explorer — browse and edit files"],
    ]);
    this.kindSel.addEventListener("change", () => this.applyKind());

    this.agentSel = document.createElement("select");
    this.agentSel.className = "dlg-select";
    for (const a of AGENTS) {
      const opt = document.createElement("option");
      opt.value = a.id;
      opt.textContent = a.label;
      this.agentSel.appendChild(opt);
    }
    this.agentSel.addEventListener("change", () => {
      this.customField.hidden = this.agentSel.value !== "custom" || this.kind === "orchestrator";
      this.applyOrchCli();
      this.applyAutopilot();
      this.updateName();
    });
    this.agentField = field("Agent", this.agentSel);
    this.agentWarn = document.createElement("div");
    this.agentWarn.className = "dlg-error";
    this.agentField.appendChild(this.agentWarn);

    this.customInput = document.createElement("input");
    this.customInput.className = "dlg-input";
    this.customInput.placeholder = "e.g. aider --model sonnet";
    this.customInput.spellcheck = false;
    this.customInput.addEventListener("input", () => this.updateAgentWarning());
    this.customField = field("Command", this.customInput);

    this.countInput = numberInput(1, 1, 8);
    this.countField = field(
      "Panes",
      this.countInput,
      "1 for a single agent; more fans out, suffixing worktrees -1…-N"
    );

    // Terminal shell picker (#194 P2): PowerShell / Command Prompt / Git Bash.
    // Git Bash is disabled until backend discovery finds a Git-for-Windows
    // install (probeGitBash below); PowerShell and cmd are always available.
    this.shellSel = select(shellKindOptions(this.shellAvail).map((o) => [o.key, o.label]));
    this.shellSel.value = "powershell";
    this.shellSel.addEventListener("change", () => this.updateName());
    this.shellField = field("Shell", this.shellSel, "PowerShell, Command Prompt, or Git Bash");
    this.applyShellAvailability();
    // Discover Git Bash off the main path; enable its option when it resolves.
    void this.probeGitBash();

    this.repoInput = document.createElement("input");
    this.repoInput.className = "dlg-input";
    this.repoInput.placeholder = "Repository or folder — empty for home";
    this.repoInput.spellcheck = false;
    // The pane routes its initial (and keyboard-nav) focus here (Pane.focus →
    // this marker) rather than the Kind select, so a welcome pane is ready for a
    // path the moment it opens (rev-74 LOW-4/LOW-6).
    this.repoInput.setAttribute("data-initial-focus", "");
    this.repoList = document.createElement("datalist");
    this.repoList.id = "welcome-recent-repos";
    this.repoInput.setAttribute("list", this.repoList.id);
    this.repoInput.addEventListener("input", () => this.updateName());
    const browse = document.createElement("button");
    browse.className = "dlg-btn";
    browse.type = "button";
    browse.textContent = "Browse…";
    browse.addEventListener("click", () => void this.pickRepo());
    const repoRow = document.createElement("div");
    repoRow.className = "dlg-row";
    repoRow.append(this.repoInput, browse, this.repoList);
    this.repoField = field("Repository", repoRow);
    this.repoLabel = this.repoField.querySelector<HTMLElement>(".dlg-label")!;

    this.worktreeInput = document.createElement("input");
    this.worktreeInput.className = "dlg-input";
    this.worktreeInput.placeholder = "e.g. fix-auth — empty to work in the repo itself";
    this.worktreeInput.spellcheck = false;
    this.worktreeInput.addEventListener("input", () => this.updateName());
    this.worktreeField = field("Worktree", this.worktreeInput, "optional, creates branch + folder");

    this.nameInput = document.createElement("input");
    this.nameInput.className = "dlg-input";
    this.nameInput.spellcheck = false;
    this.nameInput.addEventListener("input", () => (this.nameDirty = true));
    this.nameField = field("Pane name", this.nameInput);

    // Autopilot toggle (#101): launch the agent with the same unattended
    // permission flags a group worker gets (claude's Auto mode + git/gh
    // pre-approval, copilot's --allow-all-tools/--allow-all-paths), so a single
    // pane doesn't start in the CLI's interactive prompt-on-everything mode. The
    // flags come from the backend (`agent_autopilot_flags`) — the same source
    // the orchestration path uses, so the two can't drift. Default ON.
    this.autopilotInput = document.createElement("input");
    this.autopilotInput.type = "checkbox";
    this.autopilotInput.className = "dlg-check";
    const autopilotLabel = document.createElement("label");
    autopilotLabel.className = "dlg-toggle";
    const autopilotText = document.createElement("span");
    autopilotText.textContent = "Autopilot — pre-approve all tools (allow all)";
    autopilotLabel.append(this.autopilotInput, autopilotText);
    this.autopilotField = document.createElement("div");
    this.autopilotField.className = "dlg-field";
    this.autopilotField.appendChild(autopilotLabel);

    // Orchestrator guardrails: enforced by the backend; the form only collects
    // them. Models are pinned per role at group creation; the suggestion list
    // follows the selected agent CLI.
    this.workersInput = numberInput(2, 0, 6);
    this.maxAgentsInput = numberInput(4, 1, 12);
    // Cost guardrails (0 = off): idle-worker auto-kill timeout and a
    // spawns-per-hour backstop against a runaway orchestrator.
    this.idleKillInput = numberInput(0, 0, 1440);
    this.spawnRateInput = numberInput(0, 0, 240);
    // Recovery guardrail: nudge the orchestrator once when a working agent goes
    // silent (no output, no report) for this long. Default on — it's a
    // non-destructive safety net, not a cost driver.
    this.watchdogInput = numberInput(10, 0, 1440);
    // Autonomous-era token budget (#83). Autonomous mode is off by default, so
    // this only bites once the human turns it on from the group panel; setting a
    // cap here just pre-loads it. 0 = no cap. Tokens (not dollars) — the reliable
    // metric on subscription/Max accounts. Applied post-create via the setter
    // (create_orchestration has no budget parameter).
    this.autonomyBudgetInput = document.createElement("input");
    this.autonomyBudgetInput.className = "dlg-input dlg-num";
    this.autonomyBudgetInput.type = "number";
    this.autonomyBudgetInput.min = "0";
    this.autonomyBudgetInput.step = "10000";
    this.autonomyBudgetInput.value = "0";
    // Per-role CLI + model. Each role picks its own agent CLI (claude / copilot /
    // …) and model; changing a role's CLI re-populates its model list from that
    // CLI's suggestions (issue #4).
    this.roleControls = ORCH_ROLES.map(({ key }) => {
      const cli = select(ORCH_CLIS.map((c) => [c.id, c.id]));
      const model = new ModelPicker();
      cli.addEventListener("change", () => {
        this.applyRoleModels(key);
        this.updateAgentWarning();
      });
      return { key, cli, model };
    });
    this.permsSel = select([
      ["auto", "Auto — pre-approve git/gh + agent tools (recommended)"],
      ["edits", "Accept edits only — you approve git/gh yourself"],
    ]);
    const guardRow1 = document.createElement("div");
    guardRow1.className = "dlg-row";
    guardRow1.append(
      field("Initial workers", this.workersInput),
      field("Max live agents", this.maxAgentsInput)
    );
    // One row per role: [role label] CLI select + model picker.
    const roleField = (label: string, cli: HTMLSelectElement, model: ModelPicker): HTMLElement => {
      const pair = document.createElement("div");
      pair.className = "dlg-row";
      pair.append(cli, model.root);
      return field(label, pair);
    };
    const guardRow2 = document.createElement("div");
    guardRow2.className = "dlg-field";
    for (const rc of this.roleControls) {
      const label = ORCH_ROLES.find((r) => r.key === rc.key)!.label;
      guardRow2.append(roleField(`${label} — CLI + model`, rc.cli, rc.model));
    }
    const guardRow3 = document.createElement("div");
    guardRow3.className = "dlg-row";
    guardRow3.append(
      field("Idle-kill (min, 0=off)", this.idleKillInput),
      field("Max spawns/hour (0=∞)", this.spawnRateInput),
      field("Watchdog stall (min, 0=off)", this.watchdogInput)
    );
    this.orchFields = document.createElement("div");
    this.orchFields.className = "dlg-field";
    this.orchFields.append(
      guardRow1,
      guardRow2,
      guardRow3,
      field(
        "Autonomy budget (tokens, 0=no cap)",
        this.autonomyBudgetInput,
        "caps autonomous-era spend once you enable autonomous mode from the group panel"
      ),
      field("Permissions", this.permsSel)
    );

    this.errorEl = document.createElement("div");
    this.errorEl.className = "dlg-error";

    this.submitBtn = document.createElement("button");
    this.submitBtn.className = "dlg-btn primary";
    this.submitBtn.type = "button";
    this.submitBtn.textContent = "Create";
    this.submitBtn.addEventListener("click", () => void this.submit());
    const actions = document.createElement("div");
    actions.className = "dlg-actions";
    actions.append(this.submitBtn);

    dlg.append(
      title,
      subtitle,
      field("Kind", this.kindSel),
      this.agentField,
      this.customField,
      this.countField,
      this.shellField,
      this.repoField,
      this.worktreeField,
      this.autopilotField,
      this.orchFields,
      this.nameField,
      this.errorEl,
      actions
    );
    this.el.appendChild(dlg);

    // Enter submits from any field (number spinners included). No Escape/cancel:
    // the welcome IS the pane's content, closed by closing the pane itself.
    this.el.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        void this.submit();
      }
    });

    // Seed defaults (was the modal's reset()): the form is created fresh per
    // welcome pane, so this runs once at construction.
    this.agentSel.value = getDefaultAgent().id;
    this.customInput.value = getCustomCommand();
    const recent = getRecentRepos();
    this.repoInput.value = defaultFolder?.trim() || recent[0] || "";
    this.repoList.replaceChildren(
      ...recent.map((p) => {
        const opt = document.createElement("option");
        opt.value = p;
        return opt;
      })
    );
    this.autopilotInput.checked = getAutopilot();
    this.applyKind();
  }

  private get kind(): PaneKind {
    return this.kindSel.value as PaneKind;
  }

  /** Show/hide fields for the selected kind. */
  private applyKind(): void {
    const k = this.kind;
    const agent = k === "agent";
    const orch = k === "orchestrator";
    const term = k === "terminal";
    const files = k === "files";
    // A file explorer picks no CLI and spawns nothing: its ONLY input is the folder
    // (plus a name), so every other field is out (#214).
    this.agentField.hidden = term || files; // agent + orchestrator both pick a CLI
    this.customField.hidden = !agent || this.agentSel.value !== "custom";
    this.countField.hidden = !agent;
    this.shellField.hidden = !term;
    this.worktreeField.hidden = !agent; // workers get worktrees on demand
    this.autopilotField.hidden = !agent;
    this.orchFields.hidden = !orch;
    this.nameField.hidden = orch; // orchestrator names its panes from the roles
    // Same control, honest caption: a folder to browse, not a repository to work in.
    this.repoLabel.textContent = files ? "Folder" : "Repository";
    this.repoInput.placeholder = files
      ? "Folder to browse — required"
      : "Repository or folder — empty for home";
    this.applyOrchCli();
    this.applyAutopilot();
    this.updateName();
  }

  /** Show the autopilot toggle only where it applies — agent kind, a non-custom
   *  agent whose CLI actually has unattended flags. Orchestrator mode has its own
   *  permission control; custom commands the user fully owns (appending flags
   *  could collide with ones they typed). */
  private applyAutopilot(): void {
    const applies = this.kind === "agent" && this.agentSel.value !== "custom";
    if (!applies) {
      this.autopilotField.hidden = true;
      return;
    }
    const program = this.currentProgram();
    if (!program) {
      this.autopilotField.hidden = true;
      return;
    }
    void this.autopilotFlagsFor(program).then((flags) => {
      // Bail if the selection moved while the (memoized) lookup resolved.
      if (this.kind !== "agent" || this.agentSel.value === "custom") return;
      if (this.currentProgram() !== program) return;
      this.autopilotField.hidden = !flags;
    });
  }

  /** The unattended launch flags for a program, memoized. Empty when the CLI has
   *  no autopilot surface (backend returns ""), or on any lookup error. */
  private autopilotFlagsFor(program: string): Promise<string> {
    let p = this.autopilotFlags.get(program);
    if (!p) {
      p = invoke<string>("agent_autopilot_flags", { program }).catch((): string => "");
      this.autopilotFlags.set(program, p);
    }
    return p;
  }

  private orchCliFor(id: string): OrchCli {
    return ORCH_CLIS.find((c) => c.id === id) ?? ORCH_CLIS[0];
  }

  /** In orchestrator mode the agent list is restricted to CLIs the backend has
   *  orchestration adapters for, and the model options + defaults follow the
   *  selected CLI: curated list immediately, then merged with whatever the CLI's
   *  own help reports once the probe returns. */
  private applyOrchCli(): void {
    const supported = new Set(ORCH_CLIS.map((c) => c.id));
    const restricted = this.kind === "orchestrator";
    for (const opt of Array.from(this.agentSel.options)) {
      opt.disabled = restricted && !supported.has(opt.value);
    }
    this.updateAgentWarning();
    if (!restricted) return;
    if (!supported.has(this.agentSel.value)) this.agentSel.value = ORCH_CLIS[0].id;
    // The top Agent field is the group *default* CLI: seed every role's CLI from
    // it (the common case is one CLI for the whole group), then populate each
    // role's model list. Per-role selects override it afterward.
    for (const rc of this.roleControls) {
      rc.cli.value = this.agentSel.value;
      this.applyRoleModels(rc.key);
    }
  }

  /** Populate a role's model picker from its selected CLI: curated suggestions
   *  first, merged with the CLI's own reported models once the probe returns. */
  private applyRoleModels(role: OrchRole): void {
    const rc = this.roleControls.find((r) => r.key === role)!;
    const cli = this.orchCliFor(rc.cli.value);
    rc.model.setOptions(cli.models, cli.defaults[role]);
    void this.probe(cli.id).then((p) => {
      if (this.kind !== "orchestrator" || rc.cli.value !== cli.id) return;
      if (p.models.length) {
        // CLI-reported models first, curated suggestions appended.
        const merged = [...p.models, ...cli.models.filter((m) => !p.models.includes(m))];
        rc.model.setOptions(merged, cli.defaults[role]);
      }
    });
  }

  /** Probe an agent program (availability + models), memoized. */
  private probe(program: string): Promise<CliProbe> {
    let p = this.probes.get(program);
    if (!p) {
      p = invoke<CliProbe>("probe_agent_cli", { program }).catch(
        (e): CliProbe => ({ available: false, models: [], error: String(e) })
      );
      this.probes.set(program, p);
    }
    return p;
  }

  /** The program a given launch would execute (first token of the command), or
   *  null for a terminal / file explorer (no CLI to probe — neither runs one). */
  private currentProgram(): string | null {
    if (this.kind === "terminal" || this.kind === "files") return null;
    if (this.kind === "orchestrator") return this.orchCliFor(this.agentSel.value).id;
    const agent = AGENTS.find((a) => a.id === this.agentSel.value) ?? AGENTS[0];
    const command = agent.id === "custom" ? this.customInput.value.trim() : agent.command;
    return command.split(/\s+/)[0]?.toLowerCase() || null;
  }

  /** Distinct agent CLIs an orchestrator launch would spawn across all roles —
   *  each must be on PATH (issue #4: roles can run different CLIs). */
  private orchProgramsToCheck(): string[] {
    const ids = new Set<string>([this.orchCliFor(this.agentSel.value).id]);
    for (const rc of this.roleControls) ids.add(this.orchCliFor(rc.cli.value).id);
    return [...ids];
  }

  /** Inline warning when a selected agent's CLI isn't on PATH. In orchestrator
   *  mode every role's CLI is checked; the first missing one is surfaced.
   *  Terminals have no CLI, so the warning is cleared. */
  private updateAgentWarning(): void {
    if (this.kind === "terminal" || this.kind === "files") {
      this.agentWarn.classList.remove("visible"); // no CLI involved — nothing to warn about
      return;
    }
    if (this.kind === "orchestrator") {
      const ids = this.orchProgramsToCheck();
      void Promise.all(ids.map((id) => this.probe(id).then((p) => ({ id, p })))).then((results) => {
        if (this.kind !== "orchestrator") return; // kind changed under us
        const missing = results.find(({ p }) => !p.available);
        if (!missing) {
          this.agentWarn.classList.remove("visible");
        } else {
          this.agentWarn.textContent = `⚠ ${missing.p.error ?? `'${missing.id}' was not found on PATH`}`;
          this.agentWarn.classList.add("visible");
        }
      });
      return;
    }
    const program = this.currentProgram();
    if (!program) {
      this.agentWarn.classList.remove("visible");
      return;
    }
    void this.probe(program).then((p) => {
      if (this.currentProgram() !== program) return; // selection moved on
      if (p.available) {
        this.agentWarn.classList.remove("visible");
      } else {
        this.agentWarn.textContent = `⚠ ${p.error ?? `'${program}' was not found on PATH`}`;
        this.agentWarn.classList.add("visible");
      }
    });
  }

  /** Auto-fill the pane name until hand-edited: `agent · where` for an agent,
   *  `shell · where` for a terminal, the folder's own name for a file explorer. */
  private updateName(): void {
    if (this.nameDirty) return;
    const where =
      this.worktreeInput.value.trim() || basename(this.repoInput.value.trim()) || "home";
    if (this.kind === "files") {
      // The root's short name IS the useful title here — a "files · " prefix would
      // just eat width in the header for something the pane's icon already says.
      this.nameInput.value = basename(this.repoInput.value.trim()) || "files";
      return;
    }
    if (this.kind === "terminal") {
      const shell = shellKindOptions(this.shellAvail).find((s) => s.key === this.shellSel.value);
      this.nameInput.value = `${(shell?.label ?? "shell").toLowerCase()} · ${where}`;
      return;
    }
    const agent = AGENTS.find((a) => a.id === this.agentSel.value) ?? AGENTS[0];
    this.nameInput.value = `${agent.label.toLowerCase()} · ${where}`;
  }

  /** Reflect the discovered shell availability onto the picker: disable a kind
   *  that isn't installed, surface the reason on its option, and fall the
   *  selection back to PowerShell if the current kind just became unavailable
   *  (#194 P2). */
  private applyShellAvailability(): void {
    const opts = shellKindOptions(this.shellAvail);
    for (const optEl of Array.from(this.shellSel.options)) {
      const o = opts.find((x) => x.key === optEl.value);
      if (!o) continue;
      optEl.disabled = !o.enabled;
      optEl.textContent = o.enabled ? o.label : `${o.label} — not installed`;
      optEl.title = o.reason;
    }
    const current = this.shellSel.value as ShellKind;
    const resolved = resolveShellKind(current, this.shellAvail);
    if (resolved !== current) {
      this.shellSel.value = resolved;
      this.updateName();
    }
  }

  /** Discover Git Bash backend-side and update the picker. Failures leave it
   *  unavailable (disabled with a reason) rather than crashing the form. */
  private async probeGitBash(): Promise<void> {
    let path: string | null = null;
    try {
      path = await discoverGitBash();
    } catch {
      path = null;
    }
    this.shellAvail = { gitBashPath: path };
    this.applyShellAvailability();
  }

  private async pickRepo(): Promise<void> {
    const picked = await open({
      directory: true,
      title: "Choose repository or folder",
      defaultPath: this.repoInput.value.trim() || undefined,
    });
    if (typeof picked === "string") {
      this.repoInput.value = picked;
      this.updateName();
    }
  }

  /** Gather the current control values into the pure planner's input shape. */
  private collectInput(): PaneSetupInput {
    const agent = AGENTS.find((a) => a.id === this.agentSel.value) ?? AGENTS[0];
    return {
      kind: this.kind,
      agentId: agent.id,
      isCustom: agent.id === "custom",
      builtinCommand: agent.command,
      customCommand: this.customInput.value,
      count: intVal(this.countInput, 1),
      repo: this.repoInput.value,
      worktree: this.worktreeInput.value,
      name: this.nameInput.value,
      autopilot: this.autopilotInput.checked,
      shellKind: this.shellSel.value as ShellKind,
    };
  }

  private async submit(): Promise<void> {
    // Re-entrancy guard FIRST — before any await — so a double-click / Enter
    // auto-repeat / impatient second click during the probe/launch gaps can't
    // run a second submit and duplicate the launch (rev-74 HIGH-1). A validation
    // error releases it (retry allowed); a fired result finishes it (one-shot).
    if (!this.latch.begin()) return;
    // Static validation + shaping (pure, tested).
    const res = planPaneSetup(this.collectInput());
    if (!res.ok) {
      this.showError(res.error);
      if (res.focus === "repo") this.repoInput.focus();
      else if (res.focus === "custom") this.customInput.focus();
      else if (res.focus === "count") this.countInput.focus();
      this.latch.release();
      return;
    }
    const plan = res.plan;

    if (plan.kind === "terminal") {
      // Resolve against discovered availability: an unavailable kind (a stale Git
      // Bash selection, or a non-UI caller) falls back to PowerShell so the pane
      // name can't misdescribe what spawned — mirrors the backend fallback.
      const shellKind = resolveShellKind(plan.shellKind, this.shellAvail);
      if (plan.cwd) addRecentRepo(plan.cwd);
      this.setBusy(true, "Starting…");
      this.fire({ kind: "terminal", name: plan.name, cwd: plan.cwd ?? undefined, shellKind });
      return;
    }

    if (plan.kind === "files") {
      // The root must really be there. A terminal or agent in a bad cwd at least
      // fails loudly in its own output; a file explorer would just render an empty
      // tree with no explanation — so probe first and bounce the user back to the
      // field with an inline error, exactly like a missing CLI (#214).
      this.setBusy(true, "Opening…");
      if (!(await ftRootIsDir(plan.root))) {
        this.showError(`Folder not found (or not a directory): ${plan.root}`);
        this.repoInput.focus();
        this.setBusy(false);
        this.latch.release();
        return;
      }
      addRecentRepo(plan.root);
      this.fire({ kind: "files", name: plan.name, root: plan.root });
      return;
    }

    // Fail fast (and legibly) when a selected CLI isn't installed — otherwise the
    // pane just flashes the shell's error and dies. In orchestrator mode every
    // role can run a different CLI, so check each distinct one.
    if (plan.kind === "orchestrator") {
      this.setBusy(true, "Launching…");
      for (const id of this.orchProgramsToCheck()) {
        const p = await this.probe(id);
        if (!p.available) {
          this.showError(p.error ?? `'${id}' was not found on PATH.`);
          this.setBusy(false);
          this.latch.release();
          return;
        }
      }
      addRecentRepo(plan.repo);
      const groupCli = this.orchCliFor(this.agentSel.value);
      setDefaultAgent(groupCli.id);
      const role = (key: OrchRole): { cli: string; model: string } => {
        const rc = this.roleControls.find((r) => r.key === key)!;
        const c = this.orchCliFor(rc.cli.value);
        return { cli: c.id, model: rc.model.value || c.defaults[key] };
      };
      const orch = role("orchestrator");
      const worker = role("worker");
      const reviewer = role("reviewer");
      const planner = role("planner");
      this.fire({
        kind: "orchestrator",
        config: {
          repo: plan.repo,
          agentCli: groupCli.id,
          orchestratorCli: orch.cli,
          workerCli: worker.cli,
          reviewerCli: reviewer.cli,
          plannerCli: planner.cli,
          initialWorkers: intVal(this.workersInput, 2),
          maxAgents: intVal(this.maxAgentsInput, 4),
          workerModel: worker.model,
          reviewerModel: reviewer.model,
          orchestratorModel: orch.model,
          plannerModel: planner.model,
          autoOps: this.permsSel.value === "auto",
          idleKillMinutes: intVal(this.idleKillInput, 0),
          watchdogStallMinutes: intVal(this.watchdogInput, 10),
          maxSpawnsPerHour: intVal(this.spawnRateInput, 0),
          autonomyBudgetTokens: Math.max(0, intVal(this.autonomyBudgetInput, 0)),
        },
      });
      return;
    }

    // agent kind
    this.setBusy(true, "Starting…");
    const program = plan.command.split(/\s+/)[0]?.toLowerCase();
    if (program) {
      const p = await this.probe(program);
      if (!p.available) {
        this.showError(p.error ?? `'${program}' was not found on PATH.`);
        this.setBusy(false);
        this.latch.release();
        return;
      }
    }

    // Autopilot (#101): append the CLI's unattended flags so every launched pane
    // (single, or each of the N) skips the interactive permission prompts.
    // Persisted regardless of whether it applied this time. Skipped for custom
    // commands (the user owns those) and CLIs with no unattended surface (backend
    // returns ""). OFF → command is untouched.
    setAutopilot(plan.autopilot);
    let command = plan.command;
    if (!plan.isCustom && plan.autopilot && program) {
      const flags = await this.autopilotFlagsFor(program);
      if (flags) command = `${command} ${flags}`;
    }

    this.setBusy(true, "Creating worktree…");
    this.hideError();
    try {
      const specs: AgentLaunchSpec[] = [];
      for (let i = 1; i <= plan.count; i++) {
        let cwd = plan.repo || undefined;
        if (plan.worktree) {
          // Fan out to isolated worktrees: fix-auth → fix-auth-1 … fix-auth-N.
          // Each cut is from the repo's default branch, fetched fresh from
          // origin (#204) — same fix the orchestration path gets, and the same
          // trap for a human launcher parked on a feature branch. Cost: one
          // `git fetch --prune origin` per pane, serialized here behind the
          // "Creating worktree…" state (N launches → N fetches). Acceptable for
          // the small fan-out counts this dialog produces; revisit with a
          // resolve-default-once step if it ever grows.
          cwd = await gitWorktreeAdd(plan.repo, worktreeNameFor(plan.worktree, i, plan.count));
        }
        // Session-capable CLIs (Claude) get a pre-assigned session id (#194 P4)
        // so a restored pane can `--resume` the EXACT prior session — the tracked
        // P3 deferral ("the launcher knows the session id"). Minted per agent so a
        // fan-out's panes don't collide on one id. Skipped for custom commands
        // (the user owns those) and best-effort CLIs (no clean resumable id).
        // crypto.randomUUID is the webview's Web Crypto, NOT a getrandom crate —
        // constraint 2 governs src-tauri Rust only, not the frontend.
        let cmd = command;
        let sessionId: string | undefined;
        if (!plan.isCustom && program === "claude") {
          sessionId = crypto.randomUUID();
          cmd = `${command} --session-id ${sessionId}`;
        }
        specs.push({
          name: plan.count > 1 ? `${plan.baseName} ${i}` : plan.baseName,
          cwd,
          command: cmd,
          sessionId,
        });
      }
      setDefaultAgent(plan.isCustom ? "custom" : this.agentSel.value);
      if (plan.isCustom) setCustomCommand(command);
      if (plan.repo) addRecentRepo(plan.repo);
      this.fire({ kind: "panes", specs });
    } catch (err) {
      this.showError(String(err));
      this.setBusy(false);
      this.latch.release();
    }
  }

  /** Deliver the one submit result and permanently close the latch, so no late
   *  re-entry into `submit()` can fire a second time (rev-74 HIGH-1). `onSubmit`
   *  is also nulled as belt-and-suspenders — but retained so a downstream launch
   *  failure can restore it for a retry (reopenAfterLaunchFailure). */
  private lastSubmitCb: ((result: WelcomeResult) => void) | null = null;
  private fire(result: WelcomeResult): void {
    const cb = this.onSubmit;
    this.lastSubmitCb = cb;
    this.onSubmit = null;
    this.latch.finish();
    cb?.(result);
  }

  /** Re-enable this still-mounted form after the caller failed to act on its
   *  result (#194 P1 debt): a downstream launch (e.g. an orchestrator group)
   *  threw, leaving the welcome form stranded with a disabled "Working…" button.
   *  Surface the error, restore the fire()-cleared callback + latch, and re-enable
   *  submit so the human can fix the cause and retry — instead of a dead form.
   *  Only meaningful while the form is still on screen (the orchestrator path,
   *  which doesn't convert its setup pane until the launch succeeds). */
  reopenAfterLaunchFailure(msg: string): void {
    this.onSubmit = this.lastSubmitCb;
    this.latch.reopen();
    this.setBusy(false);
    this.showError(msg);
  }

  private setBusy(busy: boolean, label?: string): void {
    this.submitBtn.disabled = busy;
    this.submitBtn.textContent = busy ? label ?? "Working…" : "Create";
  }

  private showError(msg: string): void {
    this.errorEl.textContent = msg;
    this.errorEl.classList.add("visible");
  }

  private hideError(): void {
    this.errorEl.classList.remove("visible");
  }
}

// "New agent pane" modal. Three modes:
//   single       — one agent pane (original behavior)
//   multi        — N identical agent panes; a worktree name fans out to
//                  name-1 … name-N so every agent gets an isolated worktree
//   orchestrator — an orchestrator pane + N idle workers with guardrails
//                  (max live agents, pinned models, permission mode)
//
// The dialog owns worktree creation so a failure surfaces inline and the
// user can fix the name and retry instead of losing their input.

import { open } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { gitWorktreeAdd } from "./git";
import type { OrchestratorConfig } from "./orchestration";
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
}

export type LaunchResult =
  | { kind: "panes"; specs: AgentLaunchSpec[] }
  | { kind: "orchestrator"; config: OrchestratorConfig };

type Mode = "single" | "multi" | "orchestrator";

/** Agent CLIs the orchestration backend has adapters for, with the model
 *  suggestions each CLI accepts. Free text is allowed — the lists are
 *  datalist suggestions, and the backend sanitizes whatever arrives. */
/** Orchestration roles the launcher configures a CLI + model for. Mirrors the
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

export class AgentLauncher {
  private overlay: HTMLElement;
  private modeSel: HTMLSelectElement;
  private agentSel: HTMLSelectElement;
  private agentField: HTMLElement;
  private customField: HTMLElement;
  private customInput: HTMLInputElement;
  private countField: HTMLElement;
  private countInput: HTMLInputElement;
  private repoInput: HTMLInputElement;
  private repoList: HTMLDataListElement;
  private worktreeField: HTMLElement;
  private worktreeInput: HTMLInputElement;
  private nameField: HTMLElement;
  private nameInput: HTMLInputElement;
  // Autopilot toggle (single + multi modes): launch with the CLI's unattended
  // "allow all" flags. Default ON, persisted (#101).
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
  /** Per-role CLI + model controls (issue #4, mixed agent types). Built once
   *  in the constructor; the group's default CLI (the top Agent field) seeds
   *  every role and can be overridden per role. */
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
  private launchBtn: HTMLButtonElement;
  /** True once the user hand-edits the pane name; stops auto-fill. */
  private nameDirty = false;
  private busy = false;
  private resolver: ((result: LaunchResult | null) => void) | null = null;

  constructor() {
    this.overlay = document.createElement("div");
    this.overlay.className = "launcher-overlay";

    const dlg = document.createElement("div");
    dlg.className = "agent-dialog";

    const title = document.createElement("h2");
    title.textContent = "New agent pane";

    this.modeSel = select([
      ["single", "Single pane"],
      ["multi", "Multiple panes"],
      ["orchestrator", "Orchestrator + workers"],
    ]);
    this.modeSel.addEventListener("change", () => this.applyMode());

    this.agentSel = document.createElement("select");
    this.agentSel.className = "dlg-select";
    for (const a of AGENTS) {
      const opt = document.createElement("option");
      opt.value = a.id;
      opt.textContent = a.label;
      this.agentSel.appendChild(opt);
    }
    this.agentSel.addEventListener("change", () => {
      this.customField.hidden = this.agentSel.value !== "custom" || this.mode === "orchestrator";
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

    this.countInput = numberInput(3, 2, 8);
    this.countField = field("Panes", this.countInput, "each gets its own pane; worktrees are suffixed -1…-N");

    this.repoInput = document.createElement("input");
    this.repoInput.className = "dlg-input";
    this.repoInput.placeholder = "Repository or folder — empty for home";
    this.repoInput.spellcheck = false;
    this.repoList = document.createElement("datalist");
    this.repoList.id = "launcher-recent-repos";
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

    // Orchestrator guardrails: enforced by the backend; the dialog only
    // collects them. Models are pinned per role at group creation; the
    // suggestion list follows the selected agent CLI.
    this.workersInput = numberInput(2, 0, 6);
    this.maxAgentsInput = numberInput(4, 1, 12);
    // Cost guardrails (0 = off): idle-worker auto-kill timeout and a
    // spawns-per-hour backstop against a runaway orchestrator.
    this.idleKillInput = numberInput(0, 0, 1440);
    this.spawnRateInput = numberInput(0, 0, 240);
    // Recovery guardrail: nudge the orchestrator once when a working agent
    // goes silent (no output, no report) for this long. Default on — it's a
    // non-destructive safety net, not a cost driver.
    this.watchdogInput = numberInput(10, 0, 1440);
    // Autonomous-era token budget (#83). Autonomous mode is off by default, so
    // this only bites once the human turns it on from the group panel; setting
    // a cap here just pre-loads it. 0 = no cap. Tokens (not dollars) — the
    // reliable metric on subscription/Max accounts. Applied post-create via the
    // setter (create_orchestration has no budget parameter).
    this.autonomyBudgetInput = document.createElement("input");
    this.autonomyBudgetInput.className = "dlg-input dlg-num";
    this.autonomyBudgetInput.type = "number";
    this.autonomyBudgetInput.min = "0";
    this.autonomyBudgetInput.step = "10000";
    this.autonomyBudgetInput.value = "0";
    // Per-role CLI + model. Each role picks its own agent CLI (claude /
    // copilot / …) and model; changing a role's CLI re-populates its model
    // list from that CLI's suggestions (issue #4).
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

    const cancel = document.createElement("button");
    cancel.className = "dlg-btn";
    cancel.type = "button";
    cancel.textContent = "Cancel";
    cancel.addEventListener("click", () => this.close(null));
    this.launchBtn = document.createElement("button");
    this.launchBtn.className = "dlg-btn primary";
    this.launchBtn.type = "button";
    this.launchBtn.textContent = "Launch";
    this.launchBtn.addEventListener("click", () => void this.launch());
    const actions = document.createElement("div");
    actions.className = "dlg-actions";
    actions.append(cancel, this.launchBtn);

    dlg.append(
      title,
      field("Mode", this.modeSel),
      this.agentField,
      this.customField,
      this.countField,
      field("Repository", repoRow),
      this.worktreeField,
      this.autopilotField,
      this.orchFields,
      this.nameField,
      this.errorEl,
      actions
    );
    this.overlay.appendChild(dlg);

    // Click outside the dialog cancels; keys are handled here so Enter
    // launches and Escape cancels from any field.
    this.overlay.addEventListener("mousedown", (e) => {
      if (e.target === this.overlay && !this.busy) this.close(null);
    });
    this.overlay.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        void this.launch();
      } else if (e.key === "Escape" && !this.busy) {
        e.preventDefault();
        this.close(null);
      }
    });

    document.body.appendChild(this.overlay);
  }

  get isOpen(): boolean {
    return this.resolver !== null;
  }

  private get mode(): Mode {
    return this.modeSel.value as Mode;
  }

  /** Open the dialog; resolves with a launch result, or null on cancel.
   *  A second call while open resolves null immediately. */
  show(): Promise<LaunchResult | null> {
    if (this.resolver) return Promise.resolve(null);
    this.reset();
    this.overlay.classList.add("visible");
    this.repoInput.focus();
    return new Promise((res) => (this.resolver = res));
  }

  private reset(): void {
    this.modeSel.value = "single";
    this.agentSel.value = getDefaultAgent().id;
    this.customInput.value = getCustomCommand();
    const recent = getRecentRepos();
    this.repoInput.value = recent[0] ?? "";
    this.repoList.replaceChildren(
      ...recent.map((p) => {
        const opt = document.createElement("option");
        opt.value = p;
        return opt;
      })
    );
    this.worktreeInput.value = "";
    this.autopilotInput.checked = getAutopilot();
    this.nameDirty = false;
    this.applyMode();
    this.setBusy(false);
    this.hideError();
  }

  /** Show/hide fields for the selected mode. */
  private applyMode(): void {
    const m = this.mode;
    this.customField.hidden = m === "orchestrator" || this.agentSel.value !== "custom";
    this.countField.hidden = m !== "multi";
    this.worktreeField.hidden = m === "orchestrator"; // workers get worktrees on demand
    this.orchFields.hidden = m !== "orchestrator";
    this.nameField.hidden = m === "orchestrator";
    this.applyOrchCli();
    this.applyAutopilot();
    this.updateName();
  }

  /** Show the autopilot toggle only where it applies — single/multi mode, a
   *  non-custom agent whose CLI actually has unattended flags. Orchestrator
   *  mode has its own permission control; custom commands the user fully owns
   *  (appending flags could collide with ones they typed). */
  private applyAutopilot(): void {
    const applies = this.mode !== "orchestrator" && this.agentSel.value !== "custom";
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
      if (this.mode === "orchestrator" || this.agentSel.value === "custom") return;
      if (this.currentProgram() !== program) return;
      this.autopilotField.hidden = !flags;
    });
  }

  /** The unattended launch flags for a program, memoized. Empty when the CLI
   *  has no autopilot surface (backend returns ""), or on any lookup error. */
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

  /** In orchestrator mode the agent list is restricted to CLIs the backend
   *  has orchestration adapters for, and the model options + defaults
   *  follow the selected CLI: curated list immediately, then merged with
   *  whatever the CLI's own help reports once the probe returns. */
  private applyOrchCli(): void {
    const supported = new Set(ORCH_CLIS.map((c) => c.id));
    const restricted = this.mode === "orchestrator";
    for (const opt of Array.from(this.agentSel.options)) {
      opt.disabled = restricted && !supported.has(opt.value);
    }
    this.updateAgentWarning();
    if (!restricted) return;
    if (!supported.has(this.agentSel.value)) this.agentSel.value = ORCH_CLIS[0].id;
    // The top Agent field is the group *default* CLI: seed every role's CLI
    // from it (the common case is one CLI for the whole group), then populate
    // each role's model list. Per-role selects override it afterward.
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
      if (this.mode !== "orchestrator" || rc.cli.value !== cli.id) return;
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

  /** The program a given launch would execute (first token of the command). */
  private currentProgram(): string | null {
    if (this.mode === "orchestrator") return this.orchCliFor(this.agentSel.value).id;
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
   *  mode every role's CLI is checked; the first missing one is surfaced. */
  private updateAgentWarning(): void {
    if (this.mode === "orchestrator") {
      const ids = this.orchProgramsToCheck();
      void Promise.all(ids.map((id) => this.probe(id).then((p) => ({ id, p })))).then((results) => {
        if (this.mode !== "orchestrator") return; // mode changed under us
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

  /** Auto-fill the pane name (`agent · worktree-or-repo`) until hand-edited. */
  private updateName(): void {
    if (this.nameDirty) return;
    const agent = AGENTS.find((a) => a.id === this.agentSel.value) ?? AGENTS[0];
    const where =
      this.worktreeInput.value.trim() || basename(this.repoInput.value.trim()) || "home";
    this.nameInput.value = `${agent.label.toLowerCase()} · ${where}`;
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

  private async launch(): Promise<void> {
    if (this.busy) return;
    const repo = this.repoInput.value.trim();

    // Fail fast (and legibly) when a selected CLI isn't installed — otherwise
    // the pane just flashes the shell's error and dies. In orchestrator mode
    // every role can run a different CLI, so check each distinct one.
    if (this.mode === "orchestrator") {
      for (const id of this.orchProgramsToCheck()) {
        const p = await this.probe(id);
        if (!p.available) {
          this.showError(p.error ?? `'${id}' was not found on PATH.`);
          return;
        }
      }
    } else {
      const program = this.currentProgram();
      if (program) {
        const p = await this.probe(program);
        if (!p.available) {
          this.showError(p.error ?? `'${program}' was not found on PATH.`);
          return;
        }
      }
    }

    if (this.mode === "orchestrator") {
      if (!repo) {
        this.showError("The orchestrator needs a repository — pick one first.");
        this.repoInput.focus();
        return;
      }
      addRecentRepo(repo);
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
      this.close({
        kind: "orchestrator",
        config: {
          repo,
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

    const agent = AGENTS.find((a) => a.id === this.agentSel.value) ?? AGENTS[0];
    let command = agent.id === "custom" ? this.customInput.value.trim() : agent.command;
    if (!command) {
      this.showError("Enter the command line for the custom agent.");
      this.customInput.focus();
      return;
    }
    const worktree = this.worktreeInput.value.trim();
    if (worktree && !repo) {
      this.showError("A worktree needs a repository — pick one first.");
      this.repoInput.focus();
      return;
    }
    const count = this.mode === "multi" ? Math.min(8, Math.max(2, intVal(this.countInput, 3))) : 1;

    // Autopilot (#101): append the CLI's unattended flags so every launched
    // pane (single, or each of the N in multi mode) skips the interactive
    // permission prompts. Persisted regardless of whether it applied this time.
    // Skipped for custom commands (the user owns those) and CLIs with no
    // unattended surface (backend returns ""). OFF → command is untouched.
    setAutopilot(this.autopilotInput.checked);
    const bareCommand = command; // pre-flags, for the pane-name fallback
    if (agent.id !== "custom" && this.autopilotInput.checked) {
      const flags = await this.autopilotFlagsFor(command.split(/\s+/)[0]?.toLowerCase() ?? "");
      if (flags) command = `${command} ${flags}`;
    }

    this.setBusy(true);
    this.hideError();
    try {
      const baseName = this.nameInput.value.trim() || bareCommand;
      const specs: AgentLaunchSpec[] = [];
      for (let i = 1; i <= count; i++) {
        let cwd = repo || undefined;
        if (worktree) {
          // Fan out to isolated worktrees: fix-auth → fix-auth-1 … fix-auth-N.
          const wt = count > 1 ? `${worktree}-${i}` : worktree;
          cwd = await gitWorktreeAdd(repo, wt);
        }
        specs.push({ name: count > 1 ? `${baseName} ${i}` : baseName, cwd, command });
      }
      setDefaultAgent(agent.id);
      if (agent.id === "custom") setCustomCommand(command);
      if (repo) addRecentRepo(repo);
      this.close({ kind: "panes", specs });
    } catch (err) {
      this.showError(String(err));
      this.setBusy(false);
    }
  }

  private setBusy(busy: boolean): void {
    this.busy = busy;
    this.launchBtn.disabled = busy;
    this.launchBtn.textContent = busy ? "Creating worktree…" : "Launch";
  }

  private showError(msg: string): void {
    this.errorEl.textContent = msg;
    this.errorEl.classList.add("visible");
  }

  private hideError(): void {
    this.errorEl.classList.remove("visible");
  }

  private close(result: LaunchResult | null): void {
    this.overlay.classList.remove("visible");
    const res = this.resolver;
    this.resolver = null;
    res?.(result);
  }
}

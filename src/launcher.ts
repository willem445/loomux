// "New agent pane" modal: pick the agent CLI, the repo/folder it works in,
// and optionally a named worktree, then resolve to concrete pane options.
// The dialog owns worktree creation so a failure surfaces inline and the
// user can fix the name and retry instead of losing their input.

import { open } from "@tauri-apps/plugin-dialog";
import { gitWorktreeAdd } from "./git";
import {
  AGENTS,
  addRecentRepo,
  getCustomCommand,
  getDefaultAgent,
  getRecentRepos,
  setCustomCommand,
  setDefaultAgent,
} from "./agents";

export interface AgentLaunchSpec {
  name: string;
  /** Repo, worktree, or plain folder; undefined = home directory. */
  cwd?: string;
  command: string;
}

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

export class AgentLauncher {
  private overlay: HTMLElement;
  private agentSel: HTMLSelectElement;
  private customField: HTMLElement;
  private customInput: HTMLInputElement;
  private repoInput: HTMLInputElement;
  private repoList: HTMLDataListElement;
  private worktreeInput: HTMLInputElement;
  private nameInput: HTMLInputElement;
  private errorEl: HTMLElement;
  private launchBtn: HTMLButtonElement;
  /** True once the user hand-edits the pane name; stops auto-fill. */
  private nameDirty = false;
  private busy = false;
  private resolver: ((spec: AgentLaunchSpec | null) => void) | null = null;

  constructor() {
    this.overlay = document.createElement("div");
    this.overlay.className = "launcher-overlay";

    const dlg = document.createElement("div");
    dlg.className = "agent-dialog";

    const title = document.createElement("h2");
    title.textContent = "New agent pane";

    this.agentSel = document.createElement("select");
    this.agentSel.className = "dlg-select";
    for (const a of AGENTS) {
      const opt = document.createElement("option");
      opt.value = a.id;
      opt.textContent = a.label;
      this.agentSel.appendChild(opt);
    }
    this.agentSel.addEventListener("change", () => {
      this.customField.hidden = this.agentSel.value !== "custom";
      this.updateName();
    });

    this.customInput = document.createElement("input");
    this.customInput.className = "dlg-input";
    this.customInput.placeholder = "e.g. aider --model sonnet";
    this.customInput.spellcheck = false;
    this.customField = field("Command", this.customInput);

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

    this.nameInput = document.createElement("input");
    this.nameInput.className = "dlg-input";
    this.nameInput.spellcheck = false;
    this.nameInput.addEventListener("input", () => (this.nameDirty = true));

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
      field("Agent", this.agentSel),
      this.customField,
      field("Repository", repoRow),
      field("Worktree", this.worktreeInput, "optional, creates branch + folder"),
      field("Pane name", this.nameInput),
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

  /** Open the dialog; resolves with a launch spec, or null on cancel.
   *  A second call while open resolves null immediately. */
  show(): Promise<AgentLaunchSpec | null> {
    if (this.resolver) return Promise.resolve(null);
    this.reset();
    this.overlay.classList.add("visible");
    this.repoInput.focus();
    return new Promise((res) => (this.resolver = res));
  }

  private reset(): void {
    this.agentSel.value = getDefaultAgent().id;
    this.customInput.value = getCustomCommand();
    this.customField.hidden = this.agentSel.value !== "custom";
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
    this.nameDirty = false;
    this.updateName();
    this.setBusy(false);
    this.hideError();
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
    const agent = AGENTS.find((a) => a.id === this.agentSel.value) ?? AGENTS[0];
    const command = agent.id === "custom" ? this.customInput.value.trim() : agent.command;
    if (!command) {
      this.showError("Enter the command line for the custom agent.");
      this.customInput.focus();
      return;
    }
    const repo = this.repoInput.value.trim();
    const worktree = this.worktreeInput.value.trim();
    if (worktree && !repo) {
      this.showError("A worktree needs a repository — pick one first.");
      this.repoInput.focus();
      return;
    }
    this.setBusy(true);
    this.hideError();
    try {
      let cwd = repo || undefined;
      if (worktree) cwd = await gitWorktreeAdd(repo, worktree);
      setDefaultAgent(agent.id);
      if (agent.id === "custom") setCustomCommand(command);
      if (repo) addRecentRepo(repo);
      this.close({ name: this.nameInput.value.trim() || command, cwd, command });
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

  private close(spec: AgentLaunchSpec | null): void {
    this.overlay.classList.remove("visible");
    const res = this.resolver;
    this.resolver = null;
    res?.(spec);
  }
}

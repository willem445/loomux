// Group lifecycle panel for orchestrator panes: the human's at-a-glance view
// of a whole orchestration — how many agents are live, their roles, uptime,
// and running cost — plus the group-level controls that would otherwise mean
// ✕-clicking panes one by one: pause/resume (from #18's cost containment) and
// a destructive, confirmed "End orchestration" that kills every agent and can
// reclaim their worktrees. Read-through-poll like the audit viewer; the only
// writes are the explicit button actions. Same overlay mechanics as the git /
// tasks / audit views (never resizes the PTY).

import {
  endGroup,
  groupPaused,
  groupSummary,
  groupUsage,
  notifyEnabled,
  pauseGroup,
  resumeGroup,
  setNotify,
  type GroupSummary,
  type GroupUsage,
} from "./orchestration";

/** How often the panel re-polls the backend while open (uptime ticks, cost
 *  and roster drift). Matches the audit viewer's follow cadence. */
const POLL_MS = 2000;

function el(tag: string, cls: string, text?: string): HTMLElement {
  const e = document.createElement(tag);
  e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

/** Compact human uptime: "42s", "5m", "2h 5m", "1d 3h". */
function fmtUptime(ms: number | null | undefined): string {
  if (ms == null) return "—";
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ${m % 60}m`;
  return `${Math.floor(h / 24)}d ${h % 24}h`;
}

const fmtCost = (n: number | null): string => (n == null ? "—" : `$${n.toFixed(2)}`);

const ROLE_LABEL: Record<string, string> = {
  orchestrator: "ORCH",
  worker: "W",
  reviewer: "REV",
};

export class GroupView {
  readonly el: HTMLElement;
  private summaryEl: HTMLElement;
  private listEl: HTMLElement;
  private pauseBtn: HTMLButtonElement;
  private notifyBtn: HTMLButtonElement;
  private endBtn: HTMLButtonElement;
  private cleanupChk: HTMLInputElement;
  private toastEl: HTMLElement;
  private toastTimer: number | undefined;

  private summary: GroupSummary | null = null;
  private usage: GroupUsage | null = null;
  private paused = false;
  private notify = false;
  private pollTimer: number | undefined;
  private disposed = false;
  /** True once End is clicked once: the second click within the window
   *  actually tears the group down (two-step confirm for a destructive op). */
  private endArmed = false;
  private endArmTimer: number | undefined;

  constructor(
    private groupId: string,
    opts: { onClose: () => void }
  ) {
    this.el = el("div", "group-view");

    const head = el("div", "group-head");
    head.append(el("span", "group-title", "orchestration"));
    head.append(el("span", "group-group", groupId));
    const refresh = el("button", "pane-btn", "⟳") as HTMLButtonElement;
    refresh.title = "Refresh";
    refresh.addEventListener("click", () => void this.load());
    head.append(refresh);
    const close = el("button", "pane-btn close", "✕") as HTMLButtonElement;
    close.title = "Close (Alt+O)";
    close.addEventListener("click", opts.onClose);
    head.append(close);

    this.summaryEl = el("div", "group-summary");
    this.listEl = el("div", "group-list");

    // Footer: pause/resume + destructive end-orchestration.
    const foot = el("div", "group-actions");
    this.pauseBtn = el("button", "group-btn", "Pause") as HTMLButtonElement;
    this.pauseBtn.addEventListener("click", () => void this.togglePause());

    // Desktop-notification opt-in: OS toasts for report/blocked/attention
    // events in this group (idle-with-prompt, worker reports). Per-group.
    this.notifyBtn = el("button", "group-btn", "🔔 Notify") as HTMLButtonElement;
    this.notifyBtn.addEventListener("click", () => void this.toggleNotify());

    const endWrap = el("div", "group-end-wrap");
    const cleanupLbl = el("label", "group-cleanup") as HTMLLabelElement;
    this.cleanupChk = document.createElement("input");
    this.cleanupChk.type = "checkbox";
    cleanupLbl.append(this.cleanupChk, document.createTextNode(" remove worktrees"));
    cleanupLbl.title =
      "Also delete each agent's git worktree (uncommitted changes are lost; branches are kept).";
    this.endBtn = el("button", "group-btn danger", "End orchestration") as HTMLButtonElement;
    this.endBtn.title = "Kill every agent in this group";
    this.endBtn.addEventListener("click", () => void this.onEndClick());
    endWrap.append(cleanupLbl, this.endBtn);

    foot.append(this.pauseBtn, this.notifyBtn, endWrap);

    this.toastEl = el("div", "git-toast");
    this.toastEl.hidden = true;

    this.el.append(head, this.summaryEl, this.listEl, foot, this.toastEl);
  }

  /** Called by the pane whenever the overlay is (re)opened. */
  show(): void {
    void this.load();
    this.pollTimer = window.setInterval(() => void this.load(), POLL_MS);
  }

  dispose(): void {
    this.disposed = true;
    clearTimeout(this.toastTimer);
    clearTimeout(this.endArmTimer);
    if (this.pollTimer !== undefined) clearInterval(this.pollTimer);
    this.el.remove();
  }

  private toast(msg: string): void {
    this.toastEl.textContent = msg;
    this.toastEl.hidden = false;
    clearTimeout(this.toastTimer);
    this.toastTimer = window.setTimeout(() => (this.toastEl.hidden = true), 5000);
  }

  private async load(): Promise<void> {
    if (this.disposed) return;
    try {
      [this.summary, this.usage, this.paused, this.notify] = await Promise.all([
        groupSummary(this.groupId),
        groupUsage(this.groupId),
        groupPaused(this.groupId),
        notifyEnabled(this.groupId),
      ]);
    } catch (err) {
      this.toast(String(err));
      return;
    }
    this.render();
  }

  private async togglePause(): Promise<void> {
    try {
      if (this.paused) await resumeGroup(this.groupId);
      else await pauseGroup(this.groupId);
    } catch (err) {
      this.toast(String(err));
    }
    await this.load();
  }

  private async toggleNotify(): Promise<void> {
    try {
      await setNotify(this.groupId, !this.notify);
    } catch (err) {
      this.toast(String(err));
    }
    await this.load();
  }

  /** First click arms (turns the button into a confirm); the second within
   *  the window actually ends the group. A destructive, irreversible action
   *  never fires on a single click. */
  private onEndClick(): void {
    if (!this.endArmed) {
      this.endArmed = true;
      this.endBtn.textContent = "Click again to confirm";
      this.endBtn.classList.add("armed");
      this.endArmTimer = window.setTimeout(() => this.disarmEnd(), 4000);
      return;
    }
    this.disarmEnd();
    void this.doEnd();
  }

  private disarmEnd(): void {
    this.endArmed = false;
    clearTimeout(this.endArmTimer);
    this.endBtn.textContent = "End orchestration";
    this.endBtn.classList.remove("armed");
  }

  private async doEnd(): Promise<void> {
    this.endBtn.disabled = true;
    try {
      // The backend kills every agent, optionally reclaims worktrees, audits
      // the teardown, and emits orch-group-ended so the panes close.
      await endGroup(this.groupId, this.cleanupChk.checked);
    } catch (err) {
      this.toast(String(err));
      this.endBtn.disabled = false;
    }
    // On success the pane closes with the group (orch-group-ended), so there
    // is nothing more to render here.
  }

  private render(): void {
    if (this.disposed || !this.summary) return;
    const s = this.summary;

    // Summary line: N agents · role breakdown · uptime · paused badge.
    this.summaryEl.replaceChildren();
    const roleBits = [
      s.roles.orchestrator ? `${s.roles.orchestrator} orch` : "",
      s.roles.worker ? `${s.roles.worker} worker${s.roles.worker > 1 ? "s" : ""}` : "",
      s.roles.reviewer ? `${s.roles.reviewer} reviewer${s.roles.reviewer > 1 ? "s" : ""}` : "",
    ].filter(Boolean);
    const line = el(
      "div",
      "group-line",
      `${s.live_agents} agent${s.live_agents === 1 ? "" : "s"} live` +
        (roleBits.length ? ` · ${roleBits.join(", ")}` : "") +
        ` · up ${fmtUptime(s.uptime_ms)}`
    );
    this.summaryEl.append(line);
    const total = this.usage?.total_cost_usd ?? null;
    const cost = el("div", "group-cost", `group cost ${fmtCost(total)}`);
    if (total == null) cost.title = "No pane is showing a $ figure yet — cost is unknown, not zero.";
    this.summaryEl.append(cost);
    if (s.paused) this.summaryEl.append(el("span", "group-paused-badge", "paused"));

    // Per-agent rows: role chip, name, uptime, state, cost.
    this.listEl.replaceChildren();
    if (s.agents.length === 0) {
      this.listEl.append(el("div", "group-empty", "No live agents in this group."));
    } else {
      const costOf = new Map(this.usage?.agents.map((a) => [a.id, a.cost_usd] as const));
      for (const a of s.agents) {
        const row = el("div", "group-row");
        const chip = el("span", `group-role role-${a.role}`, ROLE_LABEL[a.role] ?? "AGENT");
        const name = el("span", "group-name", a.name);
        name.title = a.id;
        const state = el(
          "span",
          "group-state",
          a.idle_since_ms != null ? `idle ${fmtUptime(Date.now() - a.idle_since_ms)}` : a.task ? "working" : "ready"
        );
        if (a.task) state.title = a.task;
        const up = el("span", "group-uptime", fmtUptime(a.uptime_ms));
        const c = el("span", "group-agent-cost", fmtCost(costOf.get(a.id) ?? null));
        row.append(chip, name, state, up, c);
        this.listEl.append(row);
      }
    }

    // Reflect pause state on the toggle.
    this.paused = s.paused;
    this.pauseBtn.textContent = s.paused ? "Resume" : "Pause";
    this.pauseBtn.classList.toggle("on", s.paused);
    this.pauseBtn.title = s.paused
      ? "Resume delivery so the agents pick work back up"
      : "Stop delivering prompts so the agents finish their turn and idle out";

    // Reflect desktop-notification state on its toggle.
    this.notifyBtn.textContent = this.notify ? "🔔 Notifying" : "🔔 Notify";
    this.notifyBtn.classList.toggle("on", this.notify);
    this.notifyBtn.title = this.notify
      ? "Desktop toasts are on for this group — click to turn off"
      : "Turn on OS toasts for reports and idle-with-prompt panes in this group";
  }
}

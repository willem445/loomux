// Session browser sidebar: lists resumable Claude Code and Copilot CLI
// sessions discovered by the backend; clicking one restores it into a
// new pane.

import { listSessions, type SessionInfo } from "./pty";
import type { SessionRoleInfo } from "./orchestration";
import { taskSummary, repoBranchLine, prLabel } from "./sessionmeta";

const ROLE_CHIPS: Record<string, string> = {
  orchestrator: "ORCH",
  worker: "W",
  reviewer: "REV",
};

function timeAgo(ms: number): string {
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  if (s < 604800) return `${Math.floor(s / 86400)}d ago`;
  return new Date(ms).toLocaleDateString();
}

const shortPath = (p: string): string => p.replace(/^.*[\\/](?=[^\\/]+[\\/][^\\/]+$)/, "…\\");

export class SessionBrowser {
  private listEl: HTMLElement;
  private searchEl: HTMLInputElement;
  private sessions: SessionInfo[] = [];
  private roles = new Map<string, SessionRoleInfo>();

  constructor(
    private el: HTMLElement,
    private onRestore: (session: SessionInfo) => void,
    private loadRoles?: () => Promise<SessionRoleInfo[]>
  ) {
    const head = document.createElement("div");
    head.className = "sessions-head";
    const title = document.createElement("h2");
    title.textContent = "Sessions";
    const refresh = document.createElement("button");
    refresh.className = "bar-btn";
    refresh.textContent = "↻";
    refresh.title = "Refresh";
    refresh.addEventListener("click", () => void this.refresh());
    head.append(title, refresh);

    this.searchEl = document.createElement("input");
    this.searchEl.className = "sessions-search";
    this.searchEl.placeholder = "Filter sessions…";
    this.searchEl.addEventListener("input", () => this.render());

    this.listEl = document.createElement("div");
    this.listEl.className = "sessions-list";

    // Fixed-width inner column so content doesn't squash while the
    // sidebar's width animates open/closed.
    const inner = document.createElement("div");
    inner.className = "sessions-inner";
    inner.append(head, this.searchEl, this.listEl);
    this.el.appendChild(inner);
  }

  get visible(): boolean {
    return !this.el.classList.contains("hidden");
  }

  toggle(): void {
    this.el.classList.toggle("hidden");
    if (this.visible) {
      void this.refresh();
      this.searchEl.focus();
    }
  }

  hide(): void {
    this.el.classList.add("hidden");
  }

  /** Orchestration identity for a session, merging the durable roster with
   *  the transcript-signature fallback detected by the scanner. */
  roleFor(session: SessionInfo): SessionRoleInfo | undefined {
    const recorded = this.roles.get(session.id);
    if (recorded) return recorded;
    if (session.orch_role && session.orch_group) {
      // Transcript-signature fallback (a session predating the durable
      // roster): none of #1's metadata is derivable from the signature
      // alone, so it's honestly absent rather than guessed.
      return {
        session_id: session.id,
        group_id: session.orch_group,
        role: session.orch_role,
        agent_name: "",
        group_live: false,
        task: "",
        branch: null,
        repo: null,
        pr: null,
      };
    }
    return undefined;
  }

  async refresh(): Promise<void> {
    const [sessions, roles] = await Promise.all([
      listSessions(),
      this.loadRoles?.().catch(() => []) ?? Promise.resolve([]),
    ]);
    this.sessions = sessions;
    this.roles = new Map(roles.map((r) => [r.session_id, r]));
    this.render();
  }

  private render(): void {
    const q = this.searchEl.value.trim().toLowerCase();
    const shown = this.sessions.filter(
      (s) =>
        !q ||
        s.title.toLowerCase().includes(q) ||
        s.cwd.toLowerCase().includes(q) ||
        s.source.includes(q)
    );

    this.listEl.replaceChildren();
    if (shown.length === 0) {
      const empty = document.createElement("div");
      empty.className = "sessions-empty";
      empty.textContent = q
        ? "No sessions match."
        : "No Claude Code or Copilot sessions found on this machine.";
      this.listEl.appendChild(empty);
      return;
    }

    for (const s of shown) {
      const item = document.createElement("button");
      item.className = "session-item";
      item.title = `${s.resume_command}\nin ${s.cwd || "(unknown cwd)"}`;

      const top = document.createElement("div");
      top.className = "session-top";
      const badge = document.createElement("span");
      badge.className = `session-badge ${s.source}`;
      badge.textContent = s.source === "claude" ? "CLAUDE" : "COPILOT";
      const title = document.createElement("span");
      title.className = "session-title";
      title.textContent = s.title;
      top.append(badge, title);

      // Orchestration identity: mark recorded orchestrator/worker/reviewer
      // sessions; clicking one restores it INTO its group (MCP + task
      // board) instead of a powerless plain resume.
      const role = this.roleFor(s);
      if (role) {
        const chip = document.createElement("span");
        chip.className = `session-badge orch-role ${role.role}`;
        chip.textContent = ROLE_CHIPS[role.role] ?? role.role.toUpperCase();
        chip.title =
          role.role === "orchestrator"
            ? `Orchestrator of group ${role.group_id}${role.group_live ? " (running)" : " — click to restore the whole orchestration"}`
            : `${role.role} "${role.agent_name}" of group ${role.group_id}${role.group_live ? " — click to rejoin its group" : " (group not running)"}`;
        top.insertBefore(chip, title);
      }

      // PR chip (#1): "when known" per the issue, so it's absent rather than
      // blank until the board records one for this session's task.
      const pr = prLabel(role);
      if (pr) {
        const prChip = document.createElement("span");
        prChip.className = "session-badge session-pr";
        prChip.textContent = pr;
        prChip.title = `Pull request ${pr}`;
        top.appendChild(prChip);
      }

      // Task/goal line (#1): the brief this session's agent was spawned or
      // resumed with — hidden entirely rather than shown empty for a legacy
      // session or the orchestrator (which has no assigned task).
      const goal = taskSummary(role);
      const goalEl = goal ? document.createElement("div") : null;
      if (goalEl) {
        goalEl.className = "session-goal";
        goalEl.textContent = goal;
        goalEl.title = goal!;
      }

      // Repo/branch identity (#1): shown only when at least one is recorded,
      // never a fabricated placeholder for a legacy session or a role (the
      // orchestrator) that never has a branch.
      const identity = repoBranchLine(role);
      const identityEl = identity ? document.createElement("div") : null;
      if (identityEl) {
        identityEl.className = "session-identity";
        identityEl.textContent = identity;
      }

      const meta = document.createElement("div");
      meta.className = "session-meta";
      const cwd = document.createElement("span");
      cwd.className = "cwd";
      cwd.textContent = shortPath(s.cwd || "");
      const when = document.createElement("span");
      when.className = "when";
      when.textContent = timeAgo(s.modified_ms);
      meta.append(cwd, when);

      item.append(top);
      if (goalEl) item.append(goalEl);
      if (identityEl) item.append(identityEl);
      item.append(meta);
      item.addEventListener("click", () => this.onRestore(s));
      this.listEl.appendChild(item);
    }
  }
}

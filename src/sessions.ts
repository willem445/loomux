// Session browser sidebar: lists resumable Claude Code and Copilot CLI
// sessions discovered by the backend; clicking one restores it into a
// new pane.

import { listSessions, type SessionInfo } from "./pty";

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

  constructor(
    private el: HTMLElement,
    private onRestore: (session: SessionInfo) => void
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

    this.el.append(head, this.searchEl, this.listEl);
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

  async refresh(): Promise<void> {
    this.sessions = await listSessions();
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

      const meta = document.createElement("div");
      meta.className = "session-meta";
      const cwd = document.createElement("span");
      cwd.className = "cwd";
      cwd.textContent = shortPath(s.cwd || "");
      const when = document.createElement("span");
      when.className = "when";
      when.textContent = timeAgo(s.modified_ms);
      meta.append(cwd, when);

      item.append(top, meta);
      item.addEventListener("click", () => this.onRestore(s));
      this.listEl.appendChild(item);
    }
  }
}

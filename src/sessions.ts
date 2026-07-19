// Session browser sidebar: lists resumable Claude Code and Copilot CLI
// sessions discovered by the backend; clicking one restores it into a
// new pane.
//
// #380 round 2 correction: earlier rounds (#391/#393/#400/#414) treated this
// panel as a COVERING DOM overlay — the same class as a modal or context menu
// — needing a registered exclude rect (`overlayState.open()`/`close()`) so a
// plugin pane's native webview could clip a hole for it. That model is wrong
// for THIS panel. `#sessions` (`styles.css`) is `flex: none; width: 344px;
// transition: width`, a genuine flex SIBLING of `#grid-area` inside
// `#workspace { display: flex }` (`index.html`) — not `position: absolute`/
// `fixed`. Opening or closing it never paints over another pane's rect at any
// point in its transition; it PUSHES `#grid-area`'s available width via
// ordinary flexbox reflow, which changes every plugin pane's own BOUNDS, not
// what covers them. So this file no longer registers `#sessions` as a
// covering overlay at all — `overlayState.open()`/`close()` never contributed
// a meaningful exclude rect for it (confirmed both by the intersection math
// in `pluginocclusion.ts`, which can't produce a nonempty rect for two
// regions that never overlap, and by live telemetry: `exclude` was 0 in every
// state, every time).
//
// What this panel still needs to do: every plugin pane's own generic
// `ResizeObserver` (`pluginpaneview.ts`'s `"resize"` source) already re-fires
// on every tick of this panel's `width` transition — confirmed empirically —
// so bounds tracking DURING the transition is that mechanism's job, not this
// file's (see `pluginpaneview.ts`'s `repositionGate` doc comment for the
// actual fix to the "stale for several seconds" symptom: an unthrottled burst
// of native IPC calls, one per animation frame, was the real bottleneck, not
// a missing trigger). This file provides exactly ONE thing on top: an
// authoritative SETTLE recompute once the transition has fully committed
// (`transitionend` + `requestAnimationFrame`, below), via `overlayState`'s
// generic `poke()` notify bus — used here purely as "something every
// subscribed plugin pane should recompute against," never paired with an
// `open()`/`close()` registration, since this panel never has a rect worth
// registering.

import { listSessions, type SessionInfo } from "./pty";
import type { SessionRoleInfo } from "./orchestration";
import { taskSummary, repoBranchLine, prLabel } from "./sessionmeta";
import { RefreshGate } from "./refreshgate";
import { overlayState } from "./overlaystate";

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
  /** Single-flight guard (rev-9 review): the boot-time prefetch and a human
   *  opening the sidebar before it resolves must not run two concurrent
   *  `listSessions()` + `loadRoles()` scans — the exact I/O the prefetch
   *  exists to front-load, doubled. Same mechanism IssuesView uses for its
   *  refresh loop; reused rather than a second de-dup scheme. */
  private refreshGate = new RefreshGate();

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

    // #380 round 2: the ONE authoritative recompute this file owes every
    // subscribed plugin pane — once `#sessions`'s own `width` transition has
    // fully committed, not on every tick of it (see this file's header
    // comment for why "during" is `pluginpaneview.ts`'s job, not this file's).
    // `requestAnimationFrame` after `transitionend` ensures the read a
    // subscriber does in response lands after the settled frame has actually
    // painted, not merely been requested. Lives for the component's whole
    // lifetime (constructed once in `main.ts`, never disposed) rather than
    // attached/detached per open/close — there's no ongoing cost to a listener
    // that only ever fires on this element's own `width` transition ending.
    this.el.addEventListener("transitionend", (e) => {
      if (e.target === this.el && e.propertyName === "width") {
        requestAnimationFrame(() => overlayState.poke());
      }
    });
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
    // Single-flight, loss-safe (rev-9 review, mirrors IssuesView.refresh):
    // a call arriving while one is already in flight (the boot prefetch
    // racing a human's click, or a rapid double-toggle) is coalesced into
    // one trailing re-run rather than starting a second concurrent scan —
    // any number of dropped calls still end in exactly one fresh fetch.
    if (!this.refreshGate.begin()) return;
    try {
      const [sessions, roles] = await Promise.all([
        listSessions(),
        this.loadRoles?.().catch(() => []) ?? Promise.resolve([]),
      ]);
      this.sessions = sessions;
      this.roles = new Map(roles.map((r) => [r.session_id, r]));
      this.render();
    } finally {
      if (this.refreshGate.end()) void this.refresh();
    }
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

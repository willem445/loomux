// Session browser sidebar: lists the resumable agent sessions the backend
// discovered — Claude Code's and Copilot CLI's transcripts, OpenCode's store
// (#722) — and clicking one restores it into a new pane. Nothing here
// enumerates those CLIs: the badge, the label and the filter all read the
// row's own `source`, so a scanner added on the backend shows up correctly
// here without a matching edit.

import { listSessions, type SessionInfo } from "./pty";
import type { RecordedOrchestration, SessionRoleInfo } from "./orchestration";
import { orchRows, type OrchRow } from "./orchlist";
import { taskSummary, repoBranchLine, prLabel, sessionBadgeLabel } from "./sessionmeta";
import { RefreshGate } from "./refreshgate";
import { SessionStore } from "./sessionstore";

const ROLE_CHIPS: Record<string, string> = {
  orchestrator: "ORCH",
  worker: "W",
  reviewer: "REV",
};

/** Human "3h ago" / "just now" rendering for a `modified_ms` timestamp. Exported
 *  for the D2 dormant-card enrichment (#440, main.ts), which wants the identical
 *  age phrasing the sidebar itself uses for the same underlying timestamp. */
export function timeAgo(ms: number): string {
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
  /** The app's single session list (#493) — not a private copy. Every other
   *  consumer of the scan (main.ts's group-restore resumability check, the #440
   *  reconciler) goes through this same store, so no two of them can have a scan
   *  in flight at once. */
  private store = new SessionStore(listSessions);
  private roles = new Map<string, SessionRoleInfo>();
  /** Single-flight guard (rev-9 review): the boot-time prefetch and a human
   *  opening the sidebar before it resolves must not run two concurrent
   *  `listSessions()` + `loadRoles()` scans — the exact I/O the prefetch
   *  exists to front-load, doubled. Same mechanism IssuesView uses for its
   *  refresh loop; reused rather than a second de-dup scheme. This stays HERE
   *  and not in `SessionStore` (#493): the store's job is "never two scans at
   *  once", while this gate also covers the `loadRoles()` half of a refresh and
   *  the render, and owes a dropped caller its one trailing re-run. */
  private refreshGate = new RefreshGate();
  /** The Orchestrations section's own DOM and data (#1563). Kept beside the
   *  session list rather than inside it: a group is not a session row, and
   *  the sessions list is emptied and rebuilt (including its empty state)
   *  on every render. */
  private orchEl: HTMLElement;
  private orchestrations: RecordedOrchestration[] = [];

  constructor(
    private el: HTMLElement,
    private onRestore: (session: SessionInfo) => void,
    private loadRoles?: () => Promise<SessionRoleInfo[]>,
    /** Recorded orchestration groups (#1563). Optional so a caller that only
     *  wants the session list  and the tests  need not supply one. */
    private loadOrchestrations?: () => Promise<RecordedOrchestration[]>,
    /** Resume a recorded orchestration: the whole group comes back, exactly
     *  as clicking its ORCH session row does. Only ever called with a row
     *  `orchlist.ts` marked resumable. */
    private onResumeOrchestration?: (groupId: string, sessionId: string) => void
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

    this.orchEl = document.createElement("div");
    this.orchEl.className = "orch-list";

    this.listEl = document.createElement("div");
    this.listEl.className = "sessions-list";

    // Fixed-width inner column so content doesn't squash while the
    // sidebar's width animates open/closed.
    const inner = document.createElement("div");
    inner.className = "sessions-inner";
    inner.append(head, this.searchEl, this.orchEl, this.listEl);
    this.el.appendChild(inner);
  }

  get visible(): boolean {
    return !this.el.classList.contains("hidden");
  }

  /** The last-fetched session list, without triggering a scan (#440). The
   *  session-id reconciler (main.ts) reuses this — and this class's own
   *  single-flight `refresh()` when it needs a fresh read — instead of
   *  running a second `listSessions()` scan of its own (#342: that scan is
   *  real I/O, and the whole point of the boot prefetch this class already
   *  does is to front-load it once). Empty before the first `refresh()`
   *  resolves. */
  get cached(): readonly SessionInfo[] {
    return this.store.cached;
  }

  /** The session list for a consumer that needs the DATA, not freshness (#493):
   *  reuses the boot prefetch's result, or joins it if it's still running, and
   *  only scans when neither can answer. main.ts's group-restore resumability
   *  check calls this — it used to call `listSessions()` directly, which is what
   *  made a restore click issue a second concurrent full scan and then wait on
   *  it. Rejects if the underlying scan failed rather than caching that failure
   *  as an empty success, so the next caller retries; the call site's own
   *  empty-vs-error handling is unchanged (main.ts's pre-existing `seenAny`
   *  guard treats a successful empty list and a rejection ALIKE — see
   *  doc/design/session-index.md). */
  ensureLoaded(): Promise<readonly SessionInfo[]> {
    return this.store.ensureLoaded();
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
      const [, roles, orchestrations] = await Promise.all([
        this.store.refresh(),
        this.loadRoles?.().catch(() => []) ?? Promise.resolve([]),
        // Best-effort, same rule as the roles above: a backend that cannot
        // answer must not take the session list down with it. An empty
        // result renders the section's own empty line, never a stale list.
        this.loadOrchestrations?.().catch(() => []) ?? Promise.resolve([]),
      ]);
      this.roles = new Map(roles.map((r) => [r.session_id, r]));
      this.orchestrations = orchestrations;
      this.render();
    } finally {
      if (this.refreshGate.end()) void this.refresh();
    }
  }

  /** The "Orchestrations" section (#1563), above the session list.
   *
   *  WHY IT IS ABOVE, AND WHY IT LISTS EVERY CLI. This is the only route into
   *  a recorded orchestration that does not go through a CLI's own session
   *  store, and for an opencode group there IS no other route: its sessions
   *  live in `<group>/opencode/opencode.db`, which the sidebar's scan
   *  deliberately excludes (`doc/design/opencode.md`). Listing claude and
   *  copilot groups here too — same shape, same button — makes this the
   *  primary restart surface rather than an opencode special case, so the
   *  docs have one thing to point at.
   *
   *  A ROW WITHOUT A RESUME STILL SAYS WHY. `orchlist.ts` decides that; this
   *  method only renders it. The button exists exactly when `canResume` is
   *  true, and `sessionId` is non-null whenever it is, so there is no path
   *  here that can call the resume with nothing to resume.
   *
   *  Rendered whenever the section is drawn, including with an empty list —
   *  a human whose orchestration "vanished" needs to see the section exist
   *  and say it found nothing, not an absence they have to interpret. */
  private renderOrchestrations(q: string): void {
    this.orchEl.replaceChildren();
    if (!this.loadOrchestrations) return;
    const rows = orchRows(this.orchestrations, q);

    const head = document.createElement("div");
    head.className = "orch-list-head";
    head.textContent = "Orchestrations";
    this.orchEl.appendChild(head);

    if (rows.length === 0) {
      const empty = document.createElement("div");
      empty.className = "orch-empty";
      empty.textContent = q
        ? "No orchestrations match."
        : "No orchestration groups recorded yet.";
      this.orchEl.appendChild(empty);
      return;
    }

    for (const row of rows) {
      this.orchEl.appendChild(this.orchRowEl(row));
    }
  }

  /** One Orchestrations row. A resumable row is a button (the whole row is
   *  the target, matching `.session-item`); every other row is a plain div,
   *  so there is nothing clickable that cannot act. */
  private orchRowEl(row: OrchRow): HTMLElement {
    const item = document.createElement(row.canResume ? "button" : "div");
    item.className = `orch-item ${row.state}`;

    const top = document.createElement("div");
    top.className = "orch-top";
    const badge = document.createElement("span");
    // Same `.session-badge <cli>` shape the session rows use, so the CLI
    // colour table answers one question app-wide (styles.css, #1020 wave 2).
    // A CLI with no rule renders uncoloured, never unlabelled.
    badge.className = `session-badge ${row.cli}`;
    badge.textContent = row.cli;
    const title = document.createElement("span");
    title.className = "orch-title";
    title.textContent = row.title;
    title.title = row.groupId;
    top.append(badge, title);

    const detail = document.createElement("div");
    detail.className = "orch-detail";
    detail.textContent = row.detail;

    item.append(top, detail);

    if (row.canResume && row.sessionId) {
      item.title = `Resume orchestration group ${row.groupId}`;
      const sessionId = row.sessionId;
      item.addEventListener("click", () =>
        this.onResumeOrchestration?.(row.groupId, sessionId)
      );
    } else {
      item.title = row.groupId;
    }
    return item;
  }

  private render(): void {
    const q = this.searchEl.value.trim().toLowerCase();
    this.renderOrchestrations(q);
    const shown = this.store.cached.filter(
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
        : "No agent sessions found on this machine.";
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
      badge.textContent = sessionBadgeLabel(s.source);
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

// Session browser sidebar: lists the resumable agent sessions the backend
// discovered — Claude Code's and Copilot CLI's transcripts, OpenCode's store
// (#722) — and clicking one restores it into a new pane. Nothing here
// enumerates those CLIs: the badge, the label and the filter all read the
// row's own `source`, so a scanner added on the backend shows up correctly
// here without a matching edit.

import { listSessions, type SessionInfo } from "./pty";
import type { RecordedOrchestration, SessionRoleInfo } from "./orchestration";
import { orchRows, type OrchRow } from "./orchlist";
import {
  notesChipLabel,
  paneNameLine,
  prLabel,
  refocusAfterRender,
  repoBranchLine,
  sessionBadgeLabel,
  taskSummary,
  type SessionFocusTarget,
  type SessionRowControl,
} from "./sessionmeta";
import { icon } from "./icons";
import { RefreshGate } from "./refreshgate";
import { SessionStore } from "./sessionstore";
import {
  DEFAULT_SESSION_MODE,
  decodeSessionMode,
  delegateToggleLabel,
  partitionSessions,
  type SessionMode,
} from "./sessionfilter";

/** Where the human's normal ⇄ orchestration choice is remembered (#2116).
 *  `loomux.*` is this app's UI-chrome convention, and `localStorage` is the
 *  right home for it for the reason `agents.ts` gives: it is a per-viewer
 *  reading preference, not durable state anything else reads. */
const MODE_KEY = "loomux.sessions.mode";

/** The notes chip's glyph — the same `file-text` the pane header's Notes button
 *  carries, so the two entry points into one overlay look like one thing. */
const NOTES_ICON = icon("file-text", 12);

/** What the session rows need from orrerix's own sessions log (#2116 slice E2):
 *  four reads, a subscription, and the one gesture — never the store itself.
 *
 *  `SessionLogStore` owns a single multi-tenant file whose whole safety rule is
 *  that a write is published only from a handle that has read it, so a second
 *  thing able to write it is the shape that rule exists to prevent. This class
 *  reads; `main.ts` keeps the one writer and supplies `openNotes`, which is
 *  where the overlay already lives (`notesdialog.ts`). `SessionLogStore`
 *  satisfies the read half structurally, so the adapter is five lines and no
 *  new state. */
export interface SessionNotesHost {
  /** Whether the log has been read back at least once. What separates "this
   *  session has no notes" from "nobody has read the file" — `notesChipLabel`
   *  is where that separation is spent. */
  readonly loaded: boolean;
  /** Read the log if it has not been read. Never throws; resolves `false` when
   *  the read failed, and a later call retries. */
  ensureLoaded(): Promise<boolean>;
  /** The recorded pane name, or `undefined` for an unknown session and for an
   *  unread store alike. A SCALAR read, not `get(id)?.pane_name`: `get` clones
   *  the whole record — a fresh object per note on it — and this is read once
   *  per row on every render (#2319 review round 1). */
  paneName(sessionId: string): string | undefined;
  notesCount(sessionId: string): number;
  /** Fires on every change the store makes. Returns an unsubscribe. */
  onChange(cb: () => void): () => void;
  /** Open the notes overlay against a recorded session — live or dead alike: a
   *  note is the human's record ABOUT a session, and whether it can still be
   *  resumed is the harness's concern, not the note's. Resolves on close. */
  openNotes(session: SessionInfo, title: string): Promise<void>;
}

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
  /** The "show hidden agent sessions" toggle (#1592). Its own element rather
   *  than part of the list, because the list is emptied and rebuilt on every
   *  render and the toggle must survive that with its state intact. */
  private toggleEl: HTMLButtonElement;
  /** Whether delegate (worker/reviewer/...) rows are shown. Default OFF: a
   *  machine that has run a few fleets accumulates hundreds of them against a
   *  handful the human would ever click, and each is respawned by its own
   *  orchestrator rather than restarted by hand (`sessionfilter.ts`).
   *
   *  In-memory and per-window on purpose: this is a reading preference, not a
   *  setting, and every session is still scanned and still one click away.
   *  Nothing here bounds the SCAN — #1592's hang was a backend scaling defect
   *  and is fixed there; this changes only what a human has to read. */
  private showDelegates = false;
  /** Whose sessions the list is showing (#2116) — the human's own panes, or an
   *  orchestration's. Remembered per viewer, unlike `showDelegates`: the human
   *  asked for an explicit control over which world they are looking at, and a
   *  control that resets on every launch is not one.
   *
   *  Decoded totally (`decodeSessionMode`), so a hand-edited or future value in
   *  `localStorage` opens the default view rather than a state no control can
   *  name. */
  private mode: SessionMode = DEFAULT_SESSION_MODE;
  /** The Mine ⇄ Orchestration segmented control. Its own element for the same
   *  reason the delegate toggle is: the list is emptied and rebuilt on every
   *  render and this must survive that with its state intact. */
  private modeEl: HTMLElement;

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
    private onResumeOrchestration?: (groupId: string, sessionId: string) => void,
    /** Orrerix's own sessions log (#2116 slice E2) — the recorded pane name and
     *  the note count on each row, and the way into that session's notes.
     *  Optional so a caller that only wants the session list (and the tests)
     *  need not supply one; without it a row is exactly the pre-#2116 row. */
    private notes?: SessionNotesHost
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

    this.mode = decodeSessionMode(localStorage.getItem(MODE_KEY));
    this.modeEl = document.createElement("div");
    this.modeEl.className = "sessions-mode";
    this.modeEl.setAttribute("role", "tablist");
    this.modeEl.setAttribute("aria-label", "Which sessions to list");
    this.modeEl.append(
      this.modeTab("mine", "Mine", "Sessions you started yourself"),
      this.modeTab("orchestration", "Orchestration", "Sessions an orchestration group minted")
    );

    this.orchEl = document.createElement("div");
    this.orchEl.className = "orch-list";

    this.listEl = document.createElement("div");
    this.listEl.className = "sessions-list";

    this.toggleEl = document.createElement("button");
    this.toggleEl.className = "sessions-delegate-toggle";
    this.toggleEl.addEventListener("click", () => {
      this.showDelegates = !this.showDelegates;
      this.render();
    });

    // The panel's fixed-width inner column belongs to `leftpanel.ts` as of
    // #2122 slice B: this class is handed a BODY inside it and appends its own
    // content there. It used to build `.sessions-inner` itself, which is what
    // made the browser and the panel one thing — they are two now, and only
    // the panel's own open/close moves a column (CLAUDE.md constraint 1).
    //
    // `modeEl` keeps #2116's position in this list (right after the search
    // box): that slice augmented the append this slice replaced, and the mode
    // control has to stay mounted wherever the column is built.
    this.el.append(head, this.searchEl, this.modeEl, this.orchEl, this.listEl, this.toggleEl);

    // A note added or deleted anywhere — this list, a pane's own header — moves
    // the chip on every row that shows that session, and a rename moves the
    // pane-name line. Re-render off the store's own event rather than polling.
    //
    // SKIPPED WHILE THE TAB IS NOT THE VISIBLE ONE. `this.el` is the panel body
    // `leftpanel.ts` hides, and its `onShow` calls `refresh()`, which renders —
    // so a change that lands while the human is looking at the Agents tab (or
    // has the panel shut) is picked up the moment they come back, and a pane
    // rename does not rebuild a list nobody is reading. Never a width change
    // either way: this replaces the children of a list inside the fixed-width
    // `.sessions-inner` column, so it reaches no PTY resize (constraint 1).
    this.notes?.onChange(() => {
      if (!this.el.hidden) this.render();
    });
  }

  /** Put the cursor in the filter box. The panel calls this when the Sessions
   *  tab becomes the visible one — the moment the old `toggle()` did it. */
  focusSearch(): void {
    this.searchEl.focus();
  }

  /** One tab of the mode control. A `<button role="tab">` with `aria-selected`,
   *  the same shape the side dock's tablist uses — the two tabs are one control
   *  the human arrows through, not two independent toggles. */
  private modeTab(mode: SessionMode, label: string, title: string): HTMLButtonElement {
    const b = document.createElement("button");
    b.className = "sessions-mode-tab";
    b.dataset.mode = mode;
    b.type = "button";
    b.setAttribute("role", "tab");
    b.textContent = label;
    b.title = title;
    b.addEventListener("click", () => this.setMode(mode));
    return b;
  }

  /** Switch the view. Nothing here touches the panel's WIDTH: the mode control
   *  sits inside `.sessions-inner`, a fixed-width column — built by
   *  `leftpanel.ts` since #2122 slice B, which is also what makes the Sessions
   *  and Agents TABS free of any width change — so a click here moves no layout
   *  column and reaches no PTY resize (hard constraint 1). */
  private setMode(mode: SessionMode): void {
    if (this.mode === mode) return;
    this.mode = mode;
    localStorage.setItem(MODE_KEY, mode);
    this.render();
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
        // The sessions log (#2116), so the first render already knows whether
        // it can state a count. Without this the store is read lazily — the
        // first thing to open the overlay — and every chip would sit in its
        // "the notes file has not been read" state until then. Best-effort in
        // the same sense as the two above: a rejected read leaves `loaded`
        // false, which the chip states honestly rather than as a zero.
        this.notes?.ensureLoaded().catch(() => false) ?? Promise.resolve(false),
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
   *  WHY IT IS ABOVE, AND WHY IT LISTS EVERY CLI. It is the only route into a
   *  recorded orchestration that reads neither a CLI's session store nor
   *  `tabs.json`: an opencode group's sessions live in
   *  `<group>/opencode/opencode.db`, which the sidebar's scan deliberately
   *  excludes (`doc/design/opencode.md`), and the dormant-group card that
   *  #1563 slice A taught to carry a learned id needs the pane to have been
   *  open when the watcher bound it AND that tab set to survive. This reads
   *  the group's own `agents.json`, so it needs neither. Listing claude and
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
    //
    // The CLASS keys off `cliKey` (the raw wire value) and the TEXT off `cli`
    // (the display label): the label is "unknown CLI" for a damaged group, and
    // interpolating that would put two junk classes — `unknown` and `CLI` — on
    // the element (#1568 review N4). An empty key contributes no class at all,
    // which is the same uncoloured-but-labelled result as an unknown CLI.
    badge.className = row.cliKey ? `session-badge ${row.cliKey}` : "session-badge";
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
    // Read BEFORE anything is replaced (#2319 review round 1): once
    // `replaceChildren` has run, the element the human was standing on is gone
    // and `document.activeElement` is `<body>`, which cannot be told apart from
    // "focus was never in this list".
    const held = this.heldRowFocus();
    const q = this.searchEl.value.trim().toLowerCase();
    for (const tab of this.modeEl.querySelectorAll<HTMLElement>(".sessions-mode-tab")) {
      const on = tab.dataset.mode === this.mode;
      tab.classList.toggle("on", on);
      tab.setAttribute("aria-selected", on ? "true" : "false");
    }
    // The Orchestrations section is the primary restart surface for a recorded
    // GROUP (#1563), so it belongs with the orchestration sessions rather than
    // in a view the human asked to be "my own". `renderOrchestrations` empties
    // the element itself, so `mine` simply clears it — never a stale list left
    // behind by the previous mode.
    if (this.mode === "orchestration") this.renderOrchestrations(q);
    else this.orchEl.replaceChildren();
    const matching = this.store.cached.filter(
      (s) =>
        !q ||
        s.title.toLowerCase().includes(q) ||
        s.cwd.toLowerCase().includes(q) ||
        s.source.includes(q)
    );
    // Role filter AFTER the text filter, so the toggle's count is "how many of
    // the rows you searched for are hidden" rather than a machine-wide total
    // that would not change as you type (#1592).
    const { shown, hidden } = partitionSessions(
      matching,
      (s) => this.roleFor(s),
      this.mode,
      this.showDelegates
    );

    const toggleLabel = delegateToggleLabel(hidden, this.showDelegates, this.mode);
    this.toggleEl.textContent = toggleLabel ?? "";
    this.toggleEl.classList.toggle("hidden", toggleLabel === null);

    this.listEl.replaceChildren();
    if (shown.length === 0) {
      const empty = document.createElement("div");
      empty.className = "sessions-empty";
      // Three genuinely different situations, so three sentences: nothing
      // matched, everything that matched is hidden, or there is nothing here
      // at all. Collapsing the middle one into "no sessions match" would tell
      // a human their search failed while the rows sit behind the toggle
      // right below it.
      // Four genuinely different situations, so four sentences. Each names the
      // MODE, because the commonest reason a list looks empty is now that the
      // rows are in the other one — an unqualified "no sessions match" would
      // tell a human their search failed while the rows sit one click away.
      const noun = this.mode === "orchestration" ? "orchestration sessions" : "sessions of your own";
      empty.textContent =
        hidden > 0
          ? "Only delegate sessions here — use the button below to show them."
          : q
            ? `No ${noun} match.`
            : this.mode === "orchestration"
              ? "No orchestration sessions found on this machine."
              : "No sessions of your own found on this machine.";
      this.listEl.appendChild(empty);
      // Not an early return any more: the human may have been standing on a
      // row that this very render removed, and an empty list is the case where
      // there is most certainly nothing left under them.
      this.applyRowFocus(refocusAfterRender(held, []), new Map());
      return;
    }

    /** The rows this render built, keyed by session id — what the focus handoff
     *  resolves its target against. */
    const built = new Map<string, { item: HTMLElement; chip: HTMLElement | null }>();
    for (const s of shown) {
      // A ROW IS A WRAPPER, NOT A BUTTON (#2116 slice E2). The restore action
      // and the notes chip are two independent actions on one row, so they are
      // two sibling `<button>`s inside a plain div — never a button nested in a
      // button, which is invalid HTML and leaves a keyboard user with a control
      // they cannot reach separately. The row's hover/press feedback moved onto
      // this wrapper for the same reason: hovering either half now lifts the
      // whole row once, rather than the item alone or twice over.
      const row = document.createElement("div");
      row.className = "session-row";
      // What the focus handoff below keys on. The session id and not the
      // position: this list is re-sorted on every refresh, so an index would
      // land the human on whichever row happened to move into their slot.
      row.dataset.sessionId = s.id;

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

      // The name the human gave the pane (#2116). `paneNameLine` decides when
      // one is worth a second line — the fallback IS the title the row already
      // shows, never a placeholder — so a `null` here renders nothing at all.
      const paneName = paneNameLine(this.notes?.paneName(s.id), s.title, s.source);
      const paneNameEl = paneName ? document.createElement("div") : null;
      if (paneNameEl) {
        paneNameEl.className = "session-pane-name";
        paneNameEl.textContent = paneName!;
        paneNameEl.title = `You called this pane “${paneName!}”`;
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
      if (paneNameEl) item.append(paneNameEl);
      if (goalEl) item.append(goalEl);
      if (identityEl) item.append(identityEl);
      item.append(meta);
      item.addEventListener("click", () => this.onRestore(s));
      row.appendChild(item);
      // The overlay is titled with what the human called the pane when there is
      // one worth showing, and with the transcript title otherwise — the same
      // fallback the line itself uses, so the row and the dialog cannot
      // disagree about what this session is called.
      const chip = this.notes ? this.notesChipEl(s, paneName ?? s.title) : null;
      if (chip) row.appendChild(chip);
      this.listEl.appendChild(row);
      built.set(s.id, { item, chip });
    }

    this.applyRowFocus(refocusAfterRender(held, [...built.keys()]), built);
  }

  /** Which row control the keyboard is standing on right now, or `null` when
   *  focus is anywhere else — including a terminal, which is where it usually
   *  is when a store change fires this render.
   *
   *  Read off the DOM because that is the only place the answer lives; the
   *  DECISION it feeds is pure and tested (`refocusAfterRender`). */
  private heldRowFocus(): { sessionId: string; control: SessionRowControl } | null {
    const active = document.activeElement;
    if (!(active instanceof HTMLElement) || !this.listEl.contains(active)) return null;
    const sessionId = active.closest<HTMLElement>(".session-row")?.dataset.sessionId;
    if (!sessionId) return null;
    return {
      sessionId,
      // `closest`, not `classList`: the chip holds a glyph and a count span, so
      // a focus that ever lands on a descendant must still read as the chip
      // rather than silently falling through to the restore button.
      control: active.closest(".session-notes") ? "notes" : "item",
    };
  }

  /** Put focus where `refocusAfterRender` decided, on the freshly built rows.
   *
   *  The pure half decides from ids alone, so the one thing left here is that a
   *  control it names can be ABSENT: a row has no chip when this browser was
   *  built without a notes host. Falling back to that row's restore button, and
   *  then to the search box, means every branch lands somewhere real — the
   *  whole point is that focus never ends up on `<body>`. */
  private applyRowFocus(
    target: SessionFocusTarget,
    rows: Map<string, { item: HTMLElement; chip: HTMLElement | null }>
  ): void {
    if (target.kind === "none") return;
    if (target.kind === "search") {
      this.searchEl.focus();
      return;
    }
    const els = rows.get(target.sessionId);
    const wanted = target.control === "notes" ? els?.chip : els?.item;
    (wanted ?? els?.item ?? this.searchEl).focus();
  }

  /** The notes chip on a session row (#2116 slice E2): the count, and the way
   *  into that session's notes.
   *
   *  RENDERED ON EVERY ROW, including a session with no notes and one whose
   *  count cannot be read. The acceptance criterion asks for "a way to open
   *  that session's notes", and a chip that appeared only once notes existed
   *  would be a way to read them and no way to write the first one.
   *  `notesChipLabel` owns what it SAYS in each of those three states. */
  private notesChipEl(session: SessionInfo, dialogTitle: string): HTMLButtonElement {
    const notes = this.notes!;
    const label = notesChipLabel(notes.notesCount(session.id), notes.loaded);
    const chip = document.createElement("button");
    chip.className = "session-notes";
    chip.type = "button";
    chip.classList.toggle("has-notes", label.hasNotes);
    // The glyph first, then the count: `innerHTML` REPLACES, so appending the
    // count before it would silently drop the number.
    chip.innerHTML = NOTES_ICON;
    if (label.text) {
      const count = document.createElement("span");
      count.className = "session-notes-count";
      count.textContent = label.text;
      chip.appendChild(count);
    }
    chip.title = label.title;
    // The glyph carries no text, so a screen reader would otherwise announce an
    // unlabelled button on every row. Same sentence as the tooltip.
    chip.setAttribute("aria-label", label.title);
    chip.addEventListener("click", () => {
      void notes.openNotes(session, dialogTitle).then(() => {
        // Hand focus back to somewhere that survives a re-render. The row this
        // chip lives on may not exist any more — adding a note fires the store
        // event that rebuilds the list — so the search box is the panel's own
        // home position, and it is where a keyboard user came from. Only while
        // this tab is still the visible one: stealing focus out of a terminal
        // the human went back to would be worse than losing it.
        if (!this.el.hidden) this.searchEl.focus();
      });
    });
    return chip;
  }
}

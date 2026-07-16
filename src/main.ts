import "./styles.css";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { showToast } from "./toast";
import type { Grid } from "./grid";
import { Workspace } from "./workspace";
import { TabManager } from "./tabs";
import { TabBar } from "./tabbar";
import type { Pane, PaneEvents, PaneOptions } from "./pane";
import { SessionBrowser } from "./sessions";
import {
  ensureOutputRouter,
  onPtyExit,
  loadUiTabs,
  saveUiTabs,
  guardAppClose,
  listSessions,
  type PtyExit,
  type SessionInfo,
} from "./pty";
import { modal } from "./modal";
import { SubmitLatch } from "./panesetup";
import {
  dirtyBuffers,
  dirtyBufferLines,
  quitDecision,
  isDoaRevival,
  withDeadline,
  QUIT_FLUSH_TIMEOUT_MS,
  type DirtyBuffer,
  type KeepOpenReason,
} from "./dirtystate";
import { matchShortcut } from "./shortcuts";
import { ftRootIsDir } from "./fileapi";
import { gitRepoRoot } from "./git";
import { voiceController } from "./voicecontrol";
import { initStatusBar } from "./statusbar";
import { initHintBar } from "./hintbar";
import { WelcomeForm, type WelcomeResult, type AgentLaunchSpec } from "./launcher";
import {
  initOrchestration,
  launchOrchestrator,
  orchSessionRoles,
  resumeOrchSession,
  showPaneConnectMenu,
  disconnectPaneChannel,
  cancelPendingConnect,
  soloBind,
  SOLO_GROUP,
  type OrchWiring,
  type OrchTarget,
  type OrchestratorConfig,
  type AttentionItem,
} from "./orchestration";
import { tabAttention, sameAttention, findPaneByPty } from "./tabroute";
import { encodeTabs, decodeTabs, type PersistedTabs, type PersistedLayoutNode, type PersistedPane } from "./tabstore";
import { decideRestore } from "./restoredecision";
import {
  planLayoutRestore,
  planPaneRestore,
  agentResumeCommand,
  agentFreshCommand,
  shouldRespawnFresh,
  type RestoreAction,
  type SessionResumable,
} from "./panerestore";
import { showRestoreSplash } from "./restoresplash";
import { planGroupResume } from "./groupresume";

// Surface unexpected errors as a visible banner instead of a silently
// broken UI — a user-facing "crash" should always come with a message.
function showFatal(msg: string): void {
  let el = document.getElementById("app-error");
  if (!el) {
    el = document.createElement("div");
    el.id = "app-error";
    el.addEventListener("click", () => el!.classList.remove("visible"));
    document.body.appendChild(el);
  }
  el.textContent = msg;
  el.classList.add("visible");
}
window.addEventListener("error", (e) => {
  // The banner only shows e.message, which for a cross-module DOM error hides
  // the throwing frame. Log the underlying Error's stack so the next live
  // occurrence of the intermittent pane-rename NotFoundError (#113) — whose
  // exact reentrant trigger we could not pin from static reading — is captured
  // with its call site instead of just the opaque message.
  console.error("uncaught error:", e.error ?? e.message, "\n", e.error?.stack ?? "(no stack)");
  showFatal(`error: ${e.message}`);
});
window.addEventListener("unhandledrejection", (e) => {
  console.error("unhandled rejection:", e.reason);
  showFatal(`unhandled: ${String(e.reason)}`);
});

const sessionsEl = document.getElementById("sessions")!;
const stackEl = document.getElementById("workspace-stack")!;
const tabBarEl = document.getElementById("tab-bar")!;

// Project tabs (#63): each tab is a Workspace (its own Grid + dock). The old
// module-scope single `grid` is gone; everything acts on the ACTIVE tab's grid.
// True until the boot restore/rebuild finishes: the rebuild opens many panes,
// and we don't want each one to re-render the strip or re-persist mid-flight —
// boot persists ONCE at the end (#194 P4). onGridChanged no-ops while set.
let booting = true;

/** A pane opened / closed / converted inside a tab (grid onChange). Re-render the
 *  tab strip's live agent counter and re-persist the layout — the tab list itself
 *  didn't change, so nothing else would emit (#194 P4). */
function onGridChanged(): void {
  if (booting) return;
  tabs.notifyLayoutChanged();
}

const tabs = new TabManager<Workspace>((id) => {
  const ws = new Workspace(
    id,
    (w) => {
      // Last pane in this tab closed (a human ✕, or a background agent exiting) →
      // keep the tab's grid non-empty by refilling with the welcome / pane-setup
      // surface (#194). This is safe for a hidden/background tab now that the
      // welcome is IN-PANE content, not a floating modal over the active tab — the
      // old MED-1 "silent shell only" rule existed solely to avoid that overlay.
      openWelcomeIn(w);
    },
    () => onGridChanged()
  );
  stackEl.appendChild(ws.el);
  return ws;
});

/** The tab strip, assigned once boot mounts it. Held so the keyboard
 *  Ctrl+Shift+K routes through the same two-step close-confirm the ✕ uses. */
let tabBar: TabBar<Workspace> | null = null;

/** The active tab's grid — the single-grid `grid` of the pre-tabs app. */
const activeGrid = (): Grid => tabs.activeWorkspace.grid;

// Voice push-to-talk (#58, Alt+S): the global capture controller finds its
// insertion target via the active pane (of the active tab).
voiceController.init(() => activeGrid().activePane);

/** Pane events bound to a specific workspace, so a pane always acts on its own
 *  tab's grid — never whichever tab happens to be active when the event fires. */
function eventsFor(ws: Workspace): PaneEvents {
  return {
    onFocus: (pane) => ws.grid.setActive(pane),
    // The pane has already asked its own unsaved-edits question by the time this
    // fires (Pane.requestClose → confirmClose → here), so there is nothing to check:
    // close it. Every human-initiated single-pane close — header ✕, dock chip ✕,
    // Ctrl+Shift+W — arrives through that one path.
    onCloseRequest: (pane) => ws.grid.closePane(pane),
    onSplit: (pane, dir) => openWelcomeIn(ws, dir, pane),
    // The file browser's "Open in file editor pane" (#217): an editor pane beside the
    // browser, in the browser's own tab. Same call the welcome flow makes.
    onOpenEditorPane: (pane, opts) => {
      ws.grid.openContentPane(
        eventsFor(ws),
        { kind: "editor", name: opts.name, root: opts.root, file: opts.file },
        "row",
        pane
      );
    },
    // The file browser's "Open in workflow pane" (#222), on a .yml/.yaml row: the same
    // call, one kind over. `openContentPane` was already generic — it needed nothing.
    onOpenWorkflowPane: (pane, opts) => {
      ws.grid.openContentPane(
        eventsFor(ws),
        { kind: "workflow", name: opts.name, root: opts.root, file: opts.file },
        "row",
        pane
      );
    },
    onMinimize: (pane) => ws.grid.minimize(pane),
    onMaximize: (pane) => ws.grid.toggleMaximize(pane),
    onToggleGroupMinimize: (pane) => {
      const groupId = pane.orchGroupId;
      if (groupId) ws.grid.toggleGroupMinimize(groupId);
    },
    // A content pane re-rooted, or a pane was renamed: the persisted layout is stale
    // but no grid event fired, so nothing else would save it (#214).
    onRecordChanged: () => onGridChanged(),
    // The connect gesture (#271): the pane can't build its own menu (needs the
    // cross-tab armed-connect state + backend wrappers), so it asks its host.
    onPaneContextMenu: (pane, x, y) => void showPaneConnectMenu(pane, x, y),
    // One-click disconnect from the channel chip itself (the "easy close"
    // requirement) — same destination as the pane menu's Disconnect item.
    onDisconnectChannel: (pane) => disconnectPaneChannel(pane),
  };
}

/** Find a pane by pty id across ALL tabs — a PTY exit / focus / rename can
 *  belong to any tab, not just the active one. Scans live panes (never a
 *  maintained side-map, which a pane close would leave stale); the pure core is
 *  `findPaneByPty` (tabroute.ts), unit-tested. */
function findPaneAcrossTabs(ptyId: number): { ws: Workspace; pane: Pane } | null {
  return findPaneByPty(tabs.tabs, (ws) => ws.grid, ptyId);
}

// ---------- project tabs: orchestration routing (#63) ----------

/** Open a new tab the way the user expects (#63): create + activate it, then
 *  present the welcome / pane-setup surface — the SAME starting surface a fresh
 *  loomux pane shows. The welcome pane fills the tab immediately, so it's never
 *  left blank; the user picks the pane's kind from there (#194). */
function openUserTab(): void {
  const ws = tabs.newTab();
  openWelcomeIn(ws);
  persistTabs();
}

/** A short project name for a tab, from a repo/worktree path's last segment. */
function projectName(path: string): string {
  const parts = path.replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || "project";
}

/** Launch an orchestrator into its OWN project tab (created + activated + named
 *  from the repo), then bind the group→tab routing so its workers land here and
 *  focus/attention resolve to this tab (#63). */
async function launchOrchestratorTab(config: OrchestratorConfig): Promise<void> {
  const ws = tabs.newTab();
  tabs.renameTab(ws.id, projectName(config.repo));
  try {
    const { groupId } = await launchOrchestrator(ws.grid, eventsFor(ws), config);
    tabs.bindGroup(groupId, ws.id);
  } catch (err) {
    // The tab was created + activated before the launch could fail; don't leave
    // the human staring at a stranded empty tab (and don't leak one per retry) —
    // tear it down before propagating (#194 P4 MED-5). The caller re-focuses the
    // form's own tab and re-enables it.
    tabs.closeTab(ws.id);
    throw err;
  }
  persistTabs();
}

/** Apply an attention scan across all tabs: badge each pane by its pty (the
 *  pre-tabs behavior, now spanning every tab) AND badge the tab-strip entry of
 *  any tab that owns a needs-attention pty — so a hidden tab's blocked agent
 *  still surfaces (#63). Uses a live pty→tab map built from the actual
 *  panes, so plain (#40) panes badge their tab too, not just bound agents. */
function applyAttention(items: AttentionItem[]): void {
  const byPty = new Map<number, AttentionItem>();
  for (const it of items) if (it.pty_id !== null) byPty.set(it.pty_id, it);
  const ptyToWs = new Map<number, string>();
  for (const ws of tabs.tabs) {
    for (const pane of ws.grid.allPanes()) {
      if (pane.ptyId === null) continue;
      ptyToWs.set(pane.ptyId, ws.id);
      const it = byPty.get(pane.ptyId);
      pane.setAttention(it ? it.reason : null, it?.detail);
    }
  }
  // Dedup against the current set so the 3-second re-emits don't re-render the
  // tab bar when nothing changed.
  const next = tabAttention(items, ptyToWs);
  if (!sameAttention(tabs.tabAttention, next)) tabs.setTabAttention(next);
}

/** The tab layer as the orchestration event router sees it (OrchWiring). */
const orchWiring: OrchWiring = {
  targetForGroup(req): OrchTarget {
    let ws = tabs.workspaceForGroup(req.group_id);
    if (!ws) {
      // First sight of a group with no tab (e.g. a rejoin before its
      // orchestrator restored) — open a background project tab for it.
      ws = tabs.newTab(false);
      tabs.renameTab(ws.id, projectName(req.cwd || req.name));
      tabs.bindGroup(req.group_id, ws.id);
      persistTabs();
    }
    return { grid: ws.grid, paneEvents: eventsFor(ws) };
  },
  findByPty(ptyId): Pane | undefined {
    return findPaneAcrossTabs(ptyId)?.pane;
  },
  allGrids(): Grid[] {
    return tabs.tabs.map((ws) => ws.grid);
  },
  focusPty(ptyId): void {
    const found = findPaneAcrossTabs(ptyId);
    if (!found) return;
    tabs.switchTo(found.ws.id); // switch to the pane's TAB first…
    found.ws.grid.setActive(found.pane); // …then focus the pane.
    found.pane.focus();
  },
  applyAttention,
  refreshTabBar(): void {
    tabs.touch();
  },
};

// ---------- project tabs: persistence (#63) ----------
// The tab set (name / color / order / active tab / owning group) persists to
// durable BACKEND storage via a typed command (loadUiTabs/saveUiTabs → the
// atomic, corrupt-safe tabs.json in AppData; see src-tauri/src/uistate.rs),
// NOT localStorage — so it survives a webview data clear and sits alongside the
// app's other durable state. tabstore.ts owns the schema (encode/decode +
// validation); a bad file is quarantined backend-side and we degrade to a fresh
// tab without losing it. Live PTY buffers are not captured — see
// restoreSessionTabs / the design doc for what does and does not revive, and why.

/** The pre-backend localStorage key, read once for migration then retired. */
const LEGACY_TABS_KEY = "loomux.tabs";

/** The last snapshot actually written, so persistTabs is a no-op when nothing
 *  in the persisted set changed. tabs.onChange also fires for attention-scan
 *  updates (every ~3s) and renames-in-progress, none of which alter the saved
 *  fields — without this dedup we'd rewrite identical bytes to disk on a timer. */
let lastPersisted: string | null = null;

/** Persist the current tab set to the backend when it actually changed.
 *  Fire-and-forget: persistence is best-effort and must never block or crash the
 *  UI (a failed write just means the last change isn't durable until the next). */
function persistTabs(): void {
  const encoded = encodeTabs(tabs.snapshot());
  if (encoded === lastPersisted) return;
  lastPersisted = encoded;
  void saveUiTabs(encoded).catch(() => {
    // The write didn't land — allow the next change to retry the same bytes.
    lastPersisted = null;
  });
}

/** Persist NOW, and wait for the write to land — the app-quit path (#219).
 *
 *  Everywhere else persistence is fire-and-forget, and rightly so: a failed write just
 *  waits for the next change to retry. A quit is the one moment there IS no next change.
 *  So the quit path awaits the write (and skips the identical-bytes dedup, which exists
 *  to spare the disk on a 3-second timer, not to skip the last save of the session).
 *  This is what keeps the #194 restore snapshot honest across a quit. */
async function flushTabs(): Promise<void> {
  const encoded = encodeTabs(tabs.snapshot());
  try {
    await saveUiTabs(encoded);
    lastPersisted = encoded;
  } catch {
    lastPersisted = null; // the write didn't land; let a later change retry these bytes
  }
}

/** Load the persisted tab-set JSON, migrating a pre-backend localStorage blob on
 *  first run after upgrade: read the legacy key ONCE, hand it to the backend,
 *  and clear it so the backend copy is thereafter the single source of truth. */
async function loadPersistedTabs(): Promise<string | null> {
  const fromBackend = await loadUiTabs();
  if (fromBackend !== null) return fromBackend;
  // No backend copy yet. One-time migration from the pre-backend localStorage.
  const legacy = localStorage.getItem(LEGACY_TABS_KEY);
  if (legacy !== null) {
    localStorage.removeItem(LEGACY_TABS_KEY);
    // Adopt the legacy blob as the backend copy immediately, so a crash before
    // the next change doesn't lose it (and we never read localStorage again).
    void saveUiTabs(legacy).catch(() => {});
    return legacy;
  }
  return null;
}

/** Is there prior state worth a restore prompt? Requires at least one tab AND
 *  something to bring back — a captured pane layout, a bound orchestration group,
 *  or simply more than one tab. A lone plain tab with no layout isn't worth
 *  prompting over ("restore" would just re-open a blank tab), so we go fresh —
 *  this is the `hasSnapshot` input to decideRestore (restoredecision.ts). */
function hasRestorableContent(saved: PersistedTabs | null): boolean {
  if (!saved || saved.tabs.length === 0) return false;
  return saved.tabs.some((t) => t.layout != null || t.groupId != null) || saved.tabs.length > 1;
}

/** Rebuild the saved tab set on boot: every tab's name/color/order/group binding
 *  AND its captured pane layout (#194 P4). Terminals re-spawn (right shell + cwd),
 *  agent panes auto-resume their recorded session (no prompt) or fall to a dormant
 *  Start placeholder, and orchestration panes come back DORMANT with a Resume
 *  button — the whole group is revived only by the human via resumeOrchSession, so
 *  nothing here spawns a group (the no-double-spawn contract). Group→tab bindings
 *  survive so a later resume/rejoin still routes into the right tab. */
async function restoreSessionTabs(saved: PersistedTabs, resumable: SessionResumable): Promise<void> {
  // Track the tabs WE create so activeIndex resolves against them, not against
  // tabs.tabs — the pre-splash seed tab sits at index 0 and would offset it (BUG-2).
  const restored: Workspace[] = [];
  for (const t of saved.tabs) {
    const ws = tabs.newTab(false);
    restored.push(ws);
    tabs.renameTab(ws.id, t.name);
    tabs.setColor(ws.id, t.color);
    if (t.groupId) tabs.bindGroup(t.groupId, ws.id);
    if (t.layout) await rebuildLayout(ws, t.layout, resumable);
    if (t.docked?.length) await restoreDocked(ws, t.docked, resumable);
  }
  const activeWs = restored[saved.activeIndex];
  if (activeWs) tabs.switchTo(activeWs.id);
}

/** Replay a persisted layout tree into a tab's grid via panerestore's ordered
 *  open-plan, then apply the saved flex weights so the divider positions come
 *  back exactly (not snapped to 50/50). Each step opens ONE pane; `relativeTo`
 *  indexes an earlier step's pane as the split anchor. `resumable` decides, per
 *  agent, resume-vs-fresh (BUG-1). */
async function rebuildLayout(
  ws: Workspace,
  layout: PersistedLayoutNode,
  resumable: SessionResumable
): Promise<void> {
  const steps = planLayoutRestore(layout, resumable);
  const panes: Pane[] = [];
  for (const step of steps) {
    const anchor = step.relativeTo === null ? undefined : panes[step.relativeTo];
    const pane = await openActionPane(ws, step.action, step.dir, anchor);
    // Symmetry with the welcome/session-restore spawn paths: an exit that raced in
    // before `ptyId` was assigned sits in earlyExits — drain it here (also lets the
    // resume fresh-fallback fire on a sub-tick resume failure) instead of leaking it.
    reapIfExited(ws, pane);
    panes.push(pane);
  }
  // openPane/openDormantPane reset flex to equal shares as they split; put the
  // saved weights back now that the whole tree exists.
  ws.grid.applyLayoutWeights(layout);
}

/** Restore a tab's minimized (docked) panes: open each by its restore action,
 *  then park it back in the dock (#194 P4 MED-6) — otherwise a docked agent
 *  session would be silently lost. Its minimized-ness is preserved; if a docked
 *  pane happens to be the tab's only pane it can't re-minimize (grid never empties
 *  the dock's parent), so it stays visible rather than being dropped. */
async function restoreDocked(
  ws: Workspace,
  docked: PersistedPane[],
  resumable: SessionResumable
): Promise<void> {
  for (const record of docked) {
    const pane = await openActionPane(ws, planPaneRestore(record, resumable));
    reapIfExited(ws, pane); // same early-exit drain as the layout path
    ws.grid.minimize(pane);
  }
}

/** Open the ONE pane a restore action describes, per the adopted hybrid. Shared
 *  by the layout replay (with the step's dir/anchor) and docked restore (default
 *  placement, then minimized by the caller). */
async function openActionPane(
  ws: Workspace,
  a: RestoreAction,
  dir: "row" | "column" = "row",
  anchor?: Pane
): Promise<Pane> {
  const events = eventsFor(ws);
  switch (a.type) {
    case "spawn-terminal":
      return ws.grid.openPane(
        { name: a.name, cwd: a.cwd ?? undefined, shellKind: a.shellKind ?? undefined, background: true },
        events,
        dir,
        anchor
      );
    case "resume-agent": {
      // Resume into the idle TUI — loads context, spends nothing until a prompt,
      // and NEVER carries a replayed prompt (agentResumeCommand only rewrites flags).
      const resume = agentResumeCommand(a.command, a.argv, a.sessionId);
      const pane = await ws.grid.openPane(
        {
          name: a.name,
          cwd: a.cwd ?? undefined,
          command: resume.command,
          argv: resume.argv,
          sessionId: a.sessionId,
          background: true,
        },
        events,
        dir,
        anchor
      );
      // Runtime backstop (BUG-1): if this `--resume` exits on a missing/deleted
      // conversation (or any resume-time CLI failure), respawn fresh in place
      // instead of stranding a dead pane. Remember the fresh opts, keyed by pane;
      // the exit handler consumes it one-shot (see onPtyExit / reapIfExited).
      //
      // The backstop mints a NEW session id rather than reusing the recorded one:
      // if the resume failed because a transcript EXISTS but is corrupt/half-written,
      // `--session-id <recorded>` would hit the same conflict and fail again. A
      // brand-new id always `--session-id`-creates cleanly, and the fresh session is
      // resumable next boot under that new id. (The pre-check path — fresh-agent —
      // does reuse the recorded id; there we KNOW it has no transcript.)
      const freshId = crypto.randomUUID();
      const fresh = agentFreshCommand(a.command, a.argv, freshId);
      resumeFallbacks.set(pane, {
        opts: {
          name: a.name,
          cwd: a.cwd ?? undefined,
          command: fresh.command,
          argv: fresh.argv,
          sessionId: freshId,
        },
        at: Date.now(),
      });
      return pane;
    }
    case "fresh-agent": {
      // The recorded session has no resumable conversation (never prompted, or the
      // transcript is gone) — start a fresh session in place with the same
      // identity, reusing the recorded id so it's resumable again next boot (BUG-1).
      const fresh = agentFreshCommand(a.command, a.argv, a.sessionId);
      return ws.grid.openPane(
        {
          name: a.name,
          cwd: a.cwd ?? undefined,
          command: fresh.command,
          argv: fresh.argv,
          sessionId: a.sessionId,
          background: true,
        },
        events,
        dir,
        anchor
      );
    }
    case "dormant-agent": {
      // A best-effort CLI with no resumable id: a dormant Start placeholder in the
      // recorded cwd. Spawns nothing until the human clicks Start.
      const record: PersistedPane = {
        paneKind: "agent",
        name: a.name,
        cwd: a.cwd,
        command: a.command,
        argv: a.argv,
        shellKind: null,
        sessionId: null,
        role: null,
        file: null,
      };
      let pane: Pane;
      const content = dormantCard(
        "Start",
        a.name,
        "This agent had no resumable session — start it fresh in its folder.",
        () => {
          // startFromDormant tears the card down synchronously, so a second click
          // can't re-fire; notify once it's live so the counter reflects it.
          void pane.startFromDormant({
            name: a.name,
            cwd: a.cwd ?? undefined,
            command: a.command ?? undefined,
            argv: a.argv ?? undefined,
          }).then(() => onGridChanged());
        }
      );
      pane = ws.grid.openDormantPane(events, record, content, dir, anchor);
      return pane;
    }
    case "open-files":
    case "open-editor":
    case "open-workflow": {
      // A file explorer / file editor / workflow pane comes straight back — no process,
      // no session, no credits. The one thing that can have changed under it is the
      // folder: deleted, renamed, or on a drive that isn't mounted this boot. A pane
      // rooted at a vanished directory would render an empty tree and a mystery, so fail
      // SOFT to the welcome form in that slot with a message — the human re-points it in
      // two clicks, and the rest of the layout restores around it (#214, #217, #222).
      //
      // The WORKFLOW pane probes the same way (is the root a readable directory?) and
      // deliberately does NOT probe the workflow FILE: a repo whose `.loomux/workflow.yml`
      // has been deleted is not a broken pane, it is a pane with nothing in it yet — and
      // it opens on the empty state that offers to create one.
      const kind =
        a.type === "open-files" ? "files" : a.type === "open-editor" ? "editor" : "workflow";
      const what =
        kind === "files" ? "File explorer" : kind === "editor" ? "File editor" : "Workflow pane";
      const root = a.root;
      if (!root || !(await ftRootIsDir(root))) {
        showToast(
          `${what} "${a.name}": ${root ? `folder is gone — ${root}` : "no folder was recorded"}. Pick one to reopen it.`,
          "info"
        );
        return openWelcomeIn(ws, dir, anchor);
      }
      return ws.grid.openContentPane(
        events,
        {
          kind,
          name: a.name,
          root,
          // The editor reopens the file it was showing (a path — never a buffer; see
          // panerestore). A file deleted since just fails to open with a toast, in a
          // pane that is otherwise back exactly as it was. The workflow pane's file rides
          // the same field, and an ABSENT one means the default `.loomux/workflow.yml`.
          file: a.type === "open-files" ? undefined : a.file ?? undefined,
          background: true,
        },
        dir,
        anchor
      );
    }
    case "open-git": {
      // Same fail-soft, stricter probe (#217): the folder can still be there and no
      // longer be a git work tree — a removed worktree, a deleted .git, a repo restored
      // from a backup as plain files. Ask git rather than the filesystem, so the pane
      // never opens on something that can only tell you it isn't a repository.
      //
      // But TELL THE TWO FAILURES APART. `gitRepoRoot` returning null is git's own
      // answer: not a repo — fail soft to the welcome form. `gitRepoRoot` THROWING is a
      // tooling failure (git not on PATH this boot, an unreadable path, a network share
      // that hasn't woken up) — a fact about the environment, not about the repo. Fail
      // softing on that would replace every git pane with a welcome form AND drop the
      // recorded repo from the next layout save, losing it permanently over a transient
      // hiccup. So the pane opens anyway: the view itself reports "git was not found on
      // PATH" / the error, and ↻ recovers it once the environment does.
      const root = a.root;
      if (root) {
        let notARepo = false;
        try {
          notARepo = (await gitRepoRoot(root)) === null;
        } catch {
          notARepo = false; // couldn't ASK — that is not an answer; keep the pane
        }
        if (!notARepo) {
          return ws.grid.openContentPane(
            events,
            { kind: "git", name: a.name, root, background: true },
            dir,
            anchor
          );
        }
      }
      showToast(
        `Git pane "${a.name}": ${root ? `not a git repository any more — ${root}` : "no repository was recorded"}. Pick one to reopen it.`,
        "info"
      );
      return openWelcomeIn(ws, dir, anchor);
    }
    case "dormant-group": {
      // The one credit/process-storm-sensitive case: keep the WHOLE group dormant.
      // The Resume button revives it via resumeOrchSession — the only path that
      // spawns it — so this placeholder itself spawns nothing (no double-spawn).
      const record: PersistedPane = {
        paneKind: "orch",
        name: a.name,
        cwd: null,
        command: null,
        argv: null,
        shellKind: null,
        // Carry the captured member identity so a group resume restores exactly
        // the panes that were live at close (#194.5) and re-capture is exact.
        sessionId: a.sessionId,
        role: a.role,
        file: null,
      };
      const content = dormantCard(
        "Resume group",
        a.name,
        "Orchestration group — dormant. Resume brings the whole group back; no agents run until you do.",
        (btn) => {
          // In-flight guard (#194 P4 MED-3): resumeDormantGroup awaits, and the
          // card stays until it succeeds — a second click while it's running could
          // double-create the group (two orchestrator PTYs), the exact double-spawn
          // the contract forbids. Disable on first click; re-enable only on failure
          // (success disposes the card).
          if (btn.disabled) return;
          btn.disabled = true;
          void resumeDormantGroup(ws).finally(() => {
            btn.disabled = false;
          });
        }
      );
      return ws.grid.openDormantPane(events, record, content, dir, anchor);
    }
  }
}

/** The small card a dormant restore placeholder renders: a title, a one-line
 *  explanation, and the single action (Start / Resume group). The click handler
 *  receives the button so it can guard against a double-fire (MED-3). */
function dormantCard(
  action: string,
  title: string,
  body: string,
  onClick: (btn: HTMLButtonElement) => void
): HTMLElement {
  const wrap = document.createElement("div");
  wrap.className = "dormant-card";
  const h = document.createElement("div");
  h.className = "dormant-title";
  h.textContent = title;
  const p = document.createElement("div");
  p.className = "dormant-body";
  p.textContent = body;
  const btn = document.createElement("button");
  btn.className = "dormant-btn";
  btn.type = "button";
  btn.textContent = action;
  btn.addEventListener("click", () => onClick(btn));
  wrap.append(h, p, btn);
  return wrap;
}

/** Groups with a resume in flight (#194 P4). A restored group tab renders one
 *  Resume card per persisted orch pane, so two DIFFERENT buttons of the same
 *  group can race — each button's own guard can't see the other. The backend
 *  already refuses a double-create (safe either way), but this per-group latch
 *  suppresses the redundant error toast the loser would otherwise raise. */
const resumingGroups = new Set<string>();

/** Revive the dormant orchestration group bound to `ws` (the Resume button on a
 *  dormant-group placeholder). ONE click restores exactly the panes that were LIVE
 *  at close — no more (demo round 4). The member set is the tab's CAPTURED dormant
 *  ORCH placeholders (one per orch pane open at close), NOT the backend's full
 *  historical roster (which lists every worker the group ever had — resuming that
 *  over-restores). The orchestrator relaunches the control plane (MCP identity,
 *  task board) via resumeOrchSession, then every captured worker/reviewer/planner
 *  with a resumable session REJOINS — the backend re-registers each into the
 *  now-live group (so the orchestrator can message it) and its pane arrives in this
 *  tab via the group→tab routing. Sequential, orchestrator first (a delegate can't
 *  rejoin a group that isn't live yet). The per-group latch covers the whole set,
 *  so it's one atomic restore — no double-spawn of any member. The dormant ORCH
 *  placeholders are cleared afterward, replaced by the resumed panes.
 *
 *  WHAT DOESN'T re-attach: a captured delegate whose session was never prompted has
 *  no transcript, so `--resume` would fail and strand a dead pane, and the frontend
 *  can't spawn a fresh GROUP-registered worker (only the orchestrator does). Such
 *  members are reported and skipped; the orchestrator can respawn them on demand.
 *  Members of the group that were NOT open at close stay dead — they remain
 *  resumable later from the session browser (out of scope here, by design). */
async function resumeDormantGroup(ws: Workspace): Promise<void> {
  const groupId = tabs.groupForWorkspace(ws.id);
  if (!groupId) {
    sessions.toggle(); // no binding to resume from — let the human pick a session
    return;
  }
  // Another card of this same group is already resuming — the whole group comes
  // back at once, so ignore the duplicate rather than re-run the multi-pane resume.
  if (resumingGroups.has(groupId)) return;
  resumingGroups.add(groupId);
  try {
    // The member set is the CAPTURED orch panes — the tab's dormant ORCH
    // placeholders, one per orch pane that was live at close, each carrying its own
    // session id + role. This is the fix for the over-restore regression: the set
    // comes from what was captured, NEVER expanded by session_roles().
    const orchRecords = ws.grid
      .allPanes()
      .filter((p) => p.isDormant && p.dormantKind === "orch")
      .map((p) => p.restoreRecord)
      .filter((r): r is PersistedPane => r !== null);
    const captured = orchRecords
      .filter((r) => r.sessionId !== null)
      .map((r) => ({ sessionId: r.sessionId as string, role: r.role ?? "worker" }));
    // Captured members with no resumable id (e.g. a copilot delegate — copilot
    // mints its own session id after boot, so there's nothing to --resume). They
    // can't be brought back, but they WERE live at close, so they're counted in the
    // skip toast below rather than silently dropped from the tally.
    const idlessCount = orchRecords.length - captured.length;

    if (captured.length === 0) {
      // No captured orch session ids (a group captured before per-pane session
      // capture, or a copilot-only group with no resumable ids) — let the human
      // resume it from the session browser instead of guessing at the roster.
      showToast(
        "This restored group has no captured agent sessions — resume it from the session browser.",
        "info"
      );
      sessions.toggle();
      return;
    }

    let resumableIds = new Set<string>();
    try {
      resumableIds = new Set((await listSessions()).map((s) => s.id));
    } catch {
      /* empty → assume resumable below */
    }
    const seenAny = resumableIds.size > 0;
    const plan = planGroupResume(captured, (sid) => (seenAny ? resumableIds.has(sid) : true));

    if (!plan.orchestrator) {
      // A stale orchestrator (transcript gone) is gated the same way delegates are:
      // fall back to the browser rather than relaunch into a dead orchestrator pane.
      showToast(
        plan.orchestratorUnresumable
          ? "This group's orchestrator session has no saved conversation to resume — open the session browser."
          : "No captured orchestrator session for this group — open the session browser.",
        "info"
      );
      sessions.toggle();
      return;
    }

    const preexisting = ws.grid.allPanes();
    // 1. Orchestrator first — relaunches the group and makes it live so delegates
    //    can rejoin. A failure here aborts the whole restore (nothing to rejoin into).
    try {
      const restored = await resumeOrchSession(ws.grid, eventsFor(ws), plan.orchestrator.sessionId, {
        group: groupId,
        role: "orchestrator",
      });
      if (restored) tabs.bindGroup(restored.groupId, ws.id);
    } catch (err) {
      // Recoverable (retry the button) — a toast, not the app-crash banner (MED-3).
      showToast(`Couldn't resume group: ${String(err)}`, "error");
      return;
    }
    // 2. Rejoin each resumable delegate INTO the now-live group. Sequential (not
    //    concurrent) so the group settles live before each rejoin and we don't
    //    fan out a spawn burst; a single member's failure doesn't sink the rest.
    for (const member of plan.rejoin) {
      try {
        await resumeOrchSession(ws.grid, eventsFor(ws), member.sessionId, {
          group: groupId,
          role: member.role,
        });
      } catch (err) {
        showToast(`Couldn't rejoin a ${member.role}: ${String(err)}`, "info");
      }
    }
    // 3. Report members we can't bring back — a captured delegate with no
    //    transcript (would be a dead pane) OR one with no resumable id at all (a
    //    copilot delegate). Both were live at close; count them together so the
    //    tally reflects every captured member left behind, not a silent subset.
    const notRestored = plan.skipped.length + idlessCount;
    if (notRestored > 0) {
      showToast(
        `${notRestored} idle agent${notRestored === 1 ? "" : "s"} had no saved conversation and ${notRestored === 1 ? "was" : "were"} not restored — the orchestrator can respawn ${notRestored === 1 ? "it" : "them"}.`,
        "info"
      );
    }
    // 4. Drop the dormant ORCH placeholders that predated the resume (a mixed tab's
    //    dormant AGENT placeholders and live panes stay). The orchestrator resume
    //    already added a real pane, so this can't empty the grid.
    for (const p of preexisting) {
      if (p.isDormant && p.dormantKind === "orch") ws.grid.closePane(p, false);
    }
    persistTabs();
  } finally {
    resumingGroups.delete(groupId);
  }
}

// PTYs whose exit event arrived before their pane finished starting.
const earlyExits = new Map<number, PtyExit>();

// Fresh-session fallback for resumed agent panes (#194 BUG-1): a `--resume` that
// exits on a missing/deleted conversation should respawn FRESH in place, not
// strand a dead pane. Keyed by pane, with the spawn time so we only treat an
// IMMEDIATE failure as a resume failure — a resume that succeeded and was worked
// in for a while before exiting must NOT be clobbered. Consumed one-shot.
const resumeFallbacks = new Map<Pane, { opts: PaneOptions; at: number }>();

/** How soon after a `--resume` spawn a failure exit still counts as "the resume
 *  itself failed" (the CLI rejects a missing conversation at startup, within a
 *  second). A later exit is the human's own session ending — leave it alone. */
const RESUME_FAIL_WINDOW_MS = 15_000;

/** If `pane` is a resumed agent whose `--resume` failed at startup, respawn it
 *  fresh in place and report handled. One-shot: the fallback is removed whether
 *  or not it fires, so a later exit falls through to the normal keep-open/close
 *  path. Time-gated so a long-lived resumed session that later exits non-zero is
 *  never mistaken for a resume failure and clobbered. */
function tryResumeFallback(pane: Pane, exit: PtyExit): boolean {
  const fb = resumeFallbacks.get(pane);
  if (!fb) return false;
  resumeFallbacks.delete(pane); // one-shot regardless of outcome
  if (!shouldRespawnFresh(exit)) return false;
  if (Date.now() - fb.at > RESUME_FAIL_WINDOW_MS) return false; // a real session ended, not a resume failure
  showToast(`Recorded session not resumable — started a fresh ${pane.name} session.`, "info");
  void pane.respawnFresh(fb.opts).then(() => onGridChanged());
  return true;
}

// ---------- welcome / pane-setup surface (#194) ----------
// There is no global "agent mode" anymore: every pane opens as the welcome /
// pane-setup surface, where the user declares its kind (Agent / Orchestrator /
// Terminal). The setup pane has no PTY until the user submits — so nothing can
// resize a ConPTY before then (constraint 1).

/** Open a welcome / pane-setup pane in `ws`, wiring its submit to spawn the
 *  chosen kind. Returns the setup pane (already placed; PTY-less until submit).
 *
 *  The form's folder field is seeded from the pane we're splitting FROM (or the
 *  tab's active pane): its shell cwd, agent worktree, or files root. That's the
 *  "current pane cwd context" a new pane almost always wants — most sharply for a
 *  file explorer (#214), which should open on the project you're looking at, not
 *  the last repo you happened to launch app-wide. Falls back to the recent-repo
 *  default when there's no context (an empty tab, a welcome pane). */
function openWelcomeIn(ws: Workspace, dir: "row" | "column" = "row", relativeTo?: Pane): Pane {
  const context = relativeTo ?? ws.grid.activePane;
  const form = new WelcomeForm(context?.workdir ?? undefined);
  const pane = ws.grid.openWelcomePane(eventsFor(ws), form.el, dir, relativeTo);
  form.onSubmit = (result) => void handleWelcomeSubmit(ws, pane, form, result);
  return pane;
}

/** Act on a welcome submission: convert the setup pane into the chosen kind.
 *  Terminal → a shell in place; Agent → the first pane in place, the rest fanned
 *  out beside it; Orchestrator → its own project tab (the setup pane retires). */
async function handleWelcomeSubmit(
  ws: Workspace,
  pane: Pane,
  form: WelcomeForm,
  result: WelcomeResult
): Promise<void> {
  if (result.kind === "terminal") {
    // Phase 2 (#194): the chosen shell kind is threaded to the PTY so a Terminal
    // pane spawns PowerShell / cmd / Git Bash as picked.
    await pane.startFromWelcome({
      name: result.name,
      cwd: result.cwd,
      shellKind: result.shellKind,
    });
    reapIfExited(ws, pane);
    // The setup pane converted in place — no grid open/close fired, so notify
    // explicitly (re-renders the agent counter AND persists) (#194 P4 HIGH-1).
    onGridChanged();
    return;
  }

  if (
    result.kind === "files" ||
    result.kind === "editor" ||
    result.kind === "git" ||
    result.kind === "workflow"
  ) {
    // Convert the setup pane into a CONTENT pane in place (#214 files, #217 editor /
    // git, #222 workflow). Synchronous — there is no process to start, so no await, no
    // PTY, nothing to reap. The root was confirmed for real by the form before it fired
    // this: a readable directory for files/editor/workflow, a git work tree for git. The
    // workflow pane takes no `file` here — the welcome flow means the repo's default
    // `.loomux/workflow.yml`.
    pane.startContent({ kind: result.kind, name: result.name, root: result.root });
    // Converted in place — no grid open/close fired, so notify explicitly (this is
    // what re-renders the tab strip and re-persists the layout), same as terminal.
    onGridChanged();
    return;
  }

  if (result.kind === "orchestrator") {
    try {
      await launchOrchestratorTab(result.config);
    } catch (err) {
      // The group launch failed AFTER the form fired its result — without this the
      // welcome form would sit stranded with a disabled "Working…" button (#194 P1
      // review debt). launchOrchestratorTab already tore down its stranded tab
      // (MED-5); switch back to the form's own tab, surface the error, and re-enable
      // the still-mounted form so the human can fix the cause and retry.
      if (tabs.get(ws.id)) tabs.switchTo(ws.id);
      showToast(`Couldn't start orchestrator: ${String(err)}`, "error");
      form.reopenAfterLaunchFailure(String(err));
      return;
    }
    // The setup pane has served its purpose. A split slot just closes; a
    // dedicated welcome tab (fresh start / Ctrl+T) closes entirely so we don't
    // strand a blank tab beside the new orchestrator tab. (The sole-pane /
    // sole-tab case can't happen here — launchOrchestratorTab just added a tab.)
    if (ws.grid.paneCount > 1) ws.grid.closePane(pane);
    else if (tabs.count > 1) tabs.closeTab(ws.id);
    return;
  }

  // Agent panes: the setup pane becomes the first agent; any extras fan out
  // beside it, alternating split direction so a fleet lays out as a matrix
  // instead of ever-thinner slivers. Each spec carries a session id (Claude) so
  // the pane records it for restore (#194 P4).
  const [first, ...rest] = result.specs;
  await pane.startFromWelcome({
    name: first.name,
    cwd: first.cwd,
    command: first.command,
    sessionId: first.sessionId,
    channelAgent: channelAgentFor(first),
  });
  await bindSoloIfNeeded(pane, first);
  reapIfExited(ws, pane);
  // The first agent converted the setup pane in place — notify so the counter
  // reflects it immediately, not only after the fan-out (#194 P4 HIGH-1). The
  // fan-out panes below use grid.openPane, which now notifies after each PTY.
  onGridChanged();
  let prev: Pane = pane;
  let d: "row" | "column" = "column";
  for (const spec of rest) {
    const p = await ws.grid.openPane(
      {
        name: spec.name,
        cwd: spec.cwd,
        command: spec.command,
        sessionId: spec.sessionId,
        channelAgent: channelAgentFor(spec),
      },
      eventsFor(ws),
      d,
      prev
    );
    await bindSoloIfNeeded(p, spec);
    reapIfExited(ws, p);
    prev = p;
    d = d === "row" ? "column" : "row";
  }
}

/** Build a freshly-launched agent pane's `channelAgent` carrier from its
 *  launch spec, or `undefined` if the launcher didn't mint one (a CLI with no
 *  MCP config seam — codex/gemini/opencode/custom — stays lazy, adopted only
 *  on first Connect). See `AgentLaunchSpec.channelAgent`. */
function channelAgentFor(spec: AgentLaunchSpec) {
  return spec.channelAgent
    ? { group: SOLO_GROUP, agentId: spec.channelAgent.agentId, role: "solo", canSend: spec.channelAgent.canSend }
    : undefined;
}

/** Bind the just-spawned pane's pty to the `AgentEntry` `orch_solo_prepare`
 *  minted for it (#271 W3 addendum, part A2) — the launcher's counterpart to
 *  the orchestration group's `bind_agent` round trip. Best-effort: a failed
 *  bind just leaves the pane without a live channel identity, same as any
 *  other mint failure. */
async function bindSoloIfNeeded(pane: Pane, spec: AgentLaunchSpec): Promise<void> {
  if (!spec.channelAgent || pane.ptyId === null) return;
  try {
    await soloBind(spec.channelAgent.agentId, pane.ptyId);
  } catch {
    /* best-effort — the pane just won't be channel-connectable until adopted */
  }
}

/** Open a welcome pane in the active tab — the entry point the toolbar/shortcuts
 *  use for a "new pane". */
const openPane = (dir: "row" | "column" = "row", relativeTo?: Pane): void => {
  openWelcomeIn(tabs.activeWorkspace, dir, relativeTo);
};

/** Dispose or keep a just-dead pane per `keepOpenOnExit`, with one override
 *  (#280): a DOA orchestration-delegate revival — a worker/reviewer/planner
 *  pane that crashed having produced no output at all — is closed with a
 *  brief toast instead of left open with nothing to read. The generic
 *  "output" rule exists to protect a real crash's output; there is none here. */
function closeOrKeep(ws: Workspace, pane: Pane, exit: PtyExit, keep: KeepOpenReason | null): void {
  if (
    isDoaRevival({
      orchRole: pane.orchRole,
      keep,
      receivedOutput: pane.hasReceivedOutput,
      hasUnsavedWork: pane.hasUnsavedWork(),
    })
  ) {
    // The auto-close skips notifyExited, so the in-pane [loomux] diagnostic
    // (#281) never gets written here — the toast is the only pointer the
    // human gets, so it has to say WHERE the actual evidence lives (the
    // orchestrator's own pane got the same exit notice; the audit log is
    // durable) rather than just announcing that something was closed.
    showToast(
      `${pane.name} exited before producing any output — closed (see the orchestrator's pane or the audit log for why)`,
      "info"
    );
    ws.grid.closePane(pane, false);
    return;
  }
  if (keep) {
    pane.notifyExited(exit.exit_code, keep);
    onGridChanged(); // a kept-open pane is now dead → drop it from the live count
  } else ws.grid.closePane(pane, false);
}

function reapIfExited(ws: Workspace, pane: Pane): void {
  if (pane.ptyId === null) return;
  const exit = earlyExits.get(pane.ptyId);
  if (!exit) return;
  earlyExits.delete(pane.ptyId);
  if (tryResumeFallback(pane, exit)) return; // resume failed → fresh respawn in place
  closeOrKeep(ws, pane, exit, pane.keepOpenOnExit(exit));
}

const sessions = new SessionBrowser(
  sessionsEl,
  (s: SessionInfo) => {
    void restoreSession(s);
  },
  orchSessionRoles
);

// Prefetch the session list in the background at boot (live-test feedback:
// the first click into the sidebar felt slow because nothing had been
// fetched yet — scanning ~/.claude/projects + ~/.copilot/session-state and
// resolving each orchestration session's roster/board metadata is real I/O,
// none of it started until that first click). `refresh()` populates and
// renders into the (still-hidden) sidebar DOM regardless of visibility, so
// by the time the human opens it the list is already there; `toggle()`
// still re-refreshes on open for freshness, but with the fetch already warm
// that's no longer the FIRST load. Best-effort — a failure here just means
// the sidebar's own refresh path (open, or the ↻ button) covers it instead,
// same as it always has.
void sessions.refresh().catch(() => {
  /* best-effort warm-up; never block or fail boot on it */
});

async function restoreSession(s: SessionInfo): Promise<void> {
  // Recorded orchestration sessions restore into their group — MCP identity,
  // badges, and task board included — instead of a powerless plain `--resume`.
  const orchRole = s.source === "claude" ? sessions.roleFor(s) : undefined;
  if (orchRole) {
    // Route a restored group into the tab that OWNS it, if one exists — a
    // persisted tab (its shell restored on boot) whose group binding survived,
    // or a tab already hosting that group this session. This is the real
    // persistence↔restore integration (#63): the group re-inhabits its own tab
    // through the resume machinery, not whatever tab happens to be active. Only
    // when no tab owns the group does it land in the active tab.
    const owning = tabs.workspaceForGroup(orchRole.group_id);
    const ws = owning ?? tabs.activeWorkspace;
    if (owning && owning.id !== tabs.activeTabId) tabs.switchTo(owning.id);
    try {
      const restored = await resumeOrchSession(ws.grid, eventsFor(ws), s.id, {
        group: orchRole.group_id,
        role: orchRole.role,
      });
      // Bind the restored group to this tab so its rejoined workers spawn here
      // and focus/attention resolve here (#63); idempotent when the tab already
      // owned it. Pane lookups scan live panes, so there's no per-pty binding.
      if (restored) {
        tabs.bindGroup(restored.groupId, ws.id);
        persistTabs();
      }
    } catch (err) {
      showFatal(String(err));
    }
    return;
  }
  // Plain (non-orchestration) sessions restore into the active tab.
  const ws = tabs.activeWorkspace;
  const name =
    (s.source === "claude" ? "claude · " : "copilot · ") +
    (s.title.length > 34 ? s.title.slice(0, 34) + "…" : s.title);
  const pane = await ws.grid.openPane(
    { name, cwd: s.cwd || undefined, command: s.resume_command },
    eventsFor(ws),
    ws.grid.paneCount >= 2 ? "column" : "row"
  );
  reapIfExited(ws, pane);
}

// When a process exits on its own, retire its pane — unless the pane has a reason to
// survive it: a command pane dying with an error (its output must stay readable), or an
// unsaved Alt+F buffer (#219 — an automatic teardown must never destroy work nobody
// agreed to lose). The pane says WHICH reason in its exit banner.
void onPtyExit((exit) => {
  const found = findPaneAcrossTabs(exit.id);
  if (!found) {
    earlyExits.set(exit.id, exit);
    // A pane that never finishes starting would leak its entry forever.
    window.setTimeout(() => earlyExits.delete(exit.id), 5 * 60_000);
    return;
  }
  const { ws, pane } = found;
  if (tryResumeFallback(pane, exit)) return; // resume failed → fresh respawn in place
  closeOrKeep(ws, pane, exit, pane.keepOpenOnExit(exit));
});

// ---------- app quit: the last place unsaved work can be lost (#219) ----------

/** Every unsaved editor buffer in the app, across ALL tabs — visible, hidden, and
 *  docked — and both hosts: an editor PANE's buffer and the Alt+F OVERLAY's inside a
 *  terminal/agent pane. The overlay in a background tab is exactly the one a human
 *  forgets, which is why the sweep is total rather than "the active tab". The pure
 *  filter (dirtystate.dirtyBuffers) decides which reports count as unsaved. */
function unsavedBuffers(): DirtyBuffer[] {
  return dirtyBuffers(tabs.tabs.flatMap((ws) => ws.bufferReports()));
}

/** Persist on the way out — with a DEADLINE.
 *
 *  The final save is awaited (see flushTabs) because a quit is the one moment there is no
 *  next change to retry on. But an await with no deadline is an unquittable app: the
 *  guard fails open on a throw, and a promise that HANGS never throws. So the write is
 *  raced, and on expiry the close proceeds regardless — a possibly-stale snapshot is a
 *  small, recoverable loss (the fire-and-forget write is at most one edit behind), while a
 *  ✕ that does nothing is not recoverable at all. */
async function flushSessionForQuit(): Promise<void> {
  const outcome = await withDeadline(flushTabs(), QUIT_FLUSH_TIMEOUT_MS);
  if (outcome === "timeout") {
    // No toast: the window is about to die and nobody would read it. The breadcrumb is
    // for the next boot's crash/obs report, where "the last save never landed" is the
    // one clue that explains a layout that looks a step behind.
    console.warn(`loomux: final session save did not land within ${QUIT_FLUSH_TIMEOUT_MS}ms — quitting anyway`);
  }
}

/** One-shot latch over the quit confirm (#194 P1's SubmitLatch, the same pattern the
 *  welcome form uses for its async submit — and the same one `Pane.requestClose` uses).
 *
 *  The guard is ASYNC: while the confirm is on screen, a second ✕ (or Alt+F4, or an
 *  impatient double-click on a window button that appears not to have registered) fires
 *  onCloseRequested again and would stack a SECOND identical dialog — whose answer then
 *  races the first one's. The in-flight ask already owns the decision, so a re-entrant
 *  request is simply refused: keep the window, let the dialog that is up decide. */
const quitLatch = new SubmitLatch();

/** Gate the app's close. Nothing unsaved → quit silently (the common case must not grow
 *  a dialog). Something unsaved → ONE consolidated confirm listing every buffer, then
 *  quit or stay.
 *
 *  Deliberately one ask, not a save prompt per file: a human quitting with six dirty
 *  files wants to know that six files are dirty and decide once — a chain of six modals
 *  is how you train someone to hammer Enter through them. "Quit anyway" discards; Cancel
 *  leaves the app exactly as it was, every buffer intact, so they can go save. */
function guardQuit(): void {
  void guardAppClose(async () => {
    // A confirm is already up (see quitLatch): this close request is a duplicate, and the
    // dialog on screen is the one that decides. Refuse it rather than stack a second.
    if (!quitLatch.begin()) return false;
    try {
      const dirty = unsavedBuffers();
      if (quitDecision(dirty) === "close") {
        await flushSessionForQuit();
        quitLatch.finish(); // quitting: admit nothing further
        return true;
      }
      const files = dirtyBufferLines(dirty);
      const quit = await modal<boolean>((resolve) => ({
        title:
          files.length === 1 ? "1 file has unsaved edits" : `${files.length} files have unsaved edits`,
        body: "Quitting loomux now discards them. Cancel, save what you want to keep, then quit again.",
        bodyLines: files,
        buttons: [
          { label: "Cancel", value: false },
          { label: "Quit anyway", value: true, kind: "danger" },
        ],
        onKey: (k) => (k === "Escape" ? resolve(false) : undefined),
      }));
      if (!quit) {
        quitLatch.release(); // they stayed — a later ✕ must ask again
        return false;
      }
      await flushSessionForQuit();
      quitLatch.finish();
      return true;
    } catch (err) {
      // Fail open, and re-open the latch with it: a guard that throws must neither block
      // the close nor wedge the next one shut (guardAppClose lets this through).
      quitLatch.release();
      throw err;
    }
  });
}
guardQuit();

// Global shortcuts (terminals decline these in their key handlers).
document.addEventListener(
  "keydown",
  (e) => {
    const action = matchShortcut(e);
    if (!action) return;
    e.preventDefault();
    e.stopPropagation();
    switch (action) {
      case "split-right":
        openPane("row");
        break;
      case "split-down":
        openPane("column");
        break;
      case "close-pane": {
        // Through the pane's own close request, like the header ✕ and the dock chip:
        // one entry point for every human-initiated single-pane close (rev-100).
        activeGrid().activePane?.requestClose();
        break;
      }
      case "new-tab":
        void openUserTab();
        break;
      case "close-tab":
        // Route through the strip's two-step confirm (destructive if the tab
        // owns a group), same as clicking its ✕ (LOW-1).
        if (tabs.activeTabId) tabBar?.requestClose(tabs.activeTabId);
        break;
      case "next-tab":
        tabs.nextTab();
        break;
      case "prev-tab":
        tabs.prevTab();
        break;
      case "toggle-sessions":
        sessions.toggle();
        break;
      case "toggle-git":
        activeGrid().activePane?.toggleGitView();
        break;
      case "toggle-issues":
        activeGrid().activePane?.toggleIssuesView();
        break;
      case "toggle-files":
        activeGrid().activePane?.toggleFileEditView();
        break;
      case "open-editor":
        void activeGrid().activePane?.openInEditor();
        break;
      case "toggle-tasks":
        activeGrid().activePane?.toggleTasksView();
        break;
      case "toggle-audit":
        activeGrid().activePane?.toggleAuditView();
        break;
      case "toggle-group":
        activeGrid().activePane?.toggleGroupView();
        break;
      case "focus-compose":
        activeGrid().activePane?.focusCompose();
        break;
      case "voice-ptt":
        voiceController.toggleFromHotkey();
        break;
      case "maximize-pane": {
        const g = activeGrid();
        if (g.activePane) g.toggleMaximize(g.activePane);
        break;
      }
      case "minimize-pane": {
        const g = activeGrid();
        if (g.activePane) g.minimize(g.activePane);
        break;
      }
      case "rename-pane":
        activeGrid().activePane?.startRename();
        break;
      case "focus-left":
        activeGrid().moveFocus("left");
        break;
      case "focus-right":
        activeGrid().moveFocus("right");
        break;
      case "focus-up":
        activeGrid().moveFocus("up");
        break;
      case "focus-down":
        activeGrid().moveFocus("down");
        break;
    }
  },
  { capture: true }
);

// Top bar buttons.
document.getElementById("btn-sessions")!.addEventListener("click", () => sessions.toggle());
document.getElementById("btn-split-right")!.addEventListener("click", () => openPane("row"));
document.getElementById("btn-split-down")!.addEventListener("click", () => openPane("column"));

// Keep the browser from hijacking terminal-relevant defaults (Ctrl+F etc.
// stays inside the shell; F5/F7 reach TUI apps instead of the webview).
window.addEventListener("contextmenu", (e) => {
  if ((e.target as HTMLElement).closest(".pane-term")) e.preventDefault();
});

// WebView2 can come up without keyboard focus; make sure the active
// terminal reclaims it whenever the window is (re)focused.
window.addEventListener("focus", () => activeGrid().activePane?.focus());

// Esc cancels an in-progress connect gesture (#271) from anywhere — deliberately
// NOT preventDefault/stopPropagation: cancelPendingConnect() is a no-op when
// nothing is armed, so this must never compete with contextmenu.ts's own Esc (menu
// dismissal), a rename input's Esc, or an overlay's Esc for the same keystroke.
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") cancelPendingConnect();
});

// Stamp the running app version into the brand badge (single source of
// truth: tauri.conf.json). Non-fatal — the badge just stays blank if the
// backend can't answer.
void (async () => {
  try {
    const el = document.getElementById("app-version");
    if (el) el.textContent = `v${await getVersion()}`;
  } catch {
    /* version is cosmetic; ignore */
  }
})();

// Crash observability (issue #53): if the previous run died without a clean
// exit, the backend armed a notice naming the newest crash log. Drain it once
// and surface it as an info toast so the user knows there's something to read.
void (async () => {
  try {
    const notice = await invoke<string | null>("take_startup_notice");
    if (notice) showToast(notice, "info");
  } catch {
    /* observability is best-effort; never block startup on it */
  }
})();

// Start streaming CPU/mem/GPU/VRAM into the bottom status bar.
initStatusBar();

// Let the shortcut hint bar scroll horizontally on a vertical wheel when it
// overflows a narrow window.
initHintBar();

// Orchestration is tab-aware (#63): spawns land in their group's tab (created on
// first sight), focus switches tab then pane, group-end closes the owning tab's
// panes, and attention badges hidden tabs' strip entries. The router
// (orchWiring) is implemented over the TabManager above. Wired before any
// orchestrator can launch (below), so no spawn event races an unready router.
initOrchestration(orchWiring);

// Boot the tab layer. Restoring the tab set is now async (it reads the durable
// backend store), so the whole seed → mount → fill sequence is one async flow.
// Preview thumbnails serialize live on hover (see TabBar) from the in-memory
// buffer — no layout, no PTY resize (#63 no-resize invariant).
void (async () => {
  // Seed exactly one tab BEFORE anything can touch the active workspace (#194 P4
  // BUG-2). The restore splash is awaited below, and during that await the
  // window-focus handler (and voice init, etc.) resolve through
  // `tabs.activeWorkspace`, which THROWS when the manager is empty ("no active
  // workspace"). Seeding first guarantees there's always an active tab; the
  // restore path discards this seed once it has built the saved tabs, and the
  // fresh/decline path just keeps it as the blank welcome tab.
  const seed = tabs.newTab();

  // Decode the persisted session and decide restore vs fresh (#194 P4). The
  // decision is pure (decideRestore); the splash only appears when the remembered
  // preference is still "ask" AND there's something worth restoring.
  const saved = decodeTabs(await loadPersistedTabs());
  if (saved) tabs.setRestorePreference(saved.restorePref ?? "ask");
  const hasSnapshot = hasRestorableContent(saved);

  let outcome = decideRestore(saved?.restorePref ?? "ask", hasSnapshot);
  // Whether to overwrite the saved session at boot end. A NON-COMMITTAL fresh
  // (Esc / decline without "remember") must leave the saved tabs.json untouched
  // so the next boot can still offer it — otherwise one habitual Escape silently
  // and permanently destroys the session (#194 P4 MED-4).
  let committed = true;
  if (outcome === "prompt") {
    const choice = await showRestoreSplash();
    outcome = choice.restore ? "restore" : "fresh";
    // Remember the choice per the decision matrix; leaving it unremembered keeps
    // the preference "ask" so the splash returns next launch.
    if (choice.remember) tabs.setRestorePreference(outcome);
    if (outcome === "fresh" && !choice.remember) committed = false;
  }

  // The PTY output router must be live before restore spawns any pane.
  await ensureOutputRouter();

  if (outcome === "restore" && saved) {
    // Which recorded agent sessions still have a resumable conversation on disk:
    // listSessions() lists exactly the sessions that HAVE a transcript, so an id
    // absent here (a never-prompted / deleted session) restores FRESH instead of a
    // doomed --resume (BUG-1). Best-effort — on failure, assume resumable and let
    // the runtime backstop catch a resume that fails anyway.
    let resumableIds = new Set<string>();
    try {
      resumableIds = new Set((await listSessions()).map((s) => s.id));
    } catch {
      /* keep the empty set's caller-friendly default below */
    }
    const seenAny = resumableIds.size > 0;
    // If the list came back empty (or errored), don't force every agent fresh —
    // fall back to "assume resumable" and lean on the runtime backstop.
    const resumable: SessionResumable = (sid) => (seenAny ? resumableIds.has(sid) : true);
    await restoreSessionTabs(saved, resumable);
    // Drop the pre-splash seed now that the saved tabs (and their active tab) exist.
    if (tabs.count > 1) tabs.closeTab(seed.id);
  }
  // else: the seed tab IS the fresh/decline welcome tab — keep it.

  // Empty-tab fill (#194): any tab still empty after restore — a restored tab whose
  // layout was null (old file / group-only), a group-bound tab whose orchestrator
  // hasn't resumed, or the kept seed (fresh/decline) — opens the welcome surface.
  // In-pane content (no PTY until submit), so filling a background tab is safe.
  // Still under the `booting` guard so it doesn't persist (which would clobber the
  // saved session in the non-committal case).
  for (const ws of tabs.tabs) {
    if (ws.grid.paneCount === 0) openWelcomeIn(ws);
  }

  // Boot rebuild done: from here every pane open/close re-renders + re-persists.
  booting = false;
  // Subscribe persistTabs AFTER restore so rebuilding the saved set doesn't
  // redundantly write it straight back.
  tabs.onChange(persistTabs);
  // The "+" button opens a real starting surface, same as the shortcut.
  tabBar = new TabBar(tabBarEl, tabs, () => void openUserTab());

  // Persist the freshly rebuilt session once (records the layout + the remembered
  // restore preference); the onChange subscription covers every change after. A
  // non-committal decline skips this so the saved session survives to next boot.
  if (committed) persistTabs();
})();

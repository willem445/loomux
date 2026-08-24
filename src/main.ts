import "./styles.css";
import { invoke, hostVersion } from "./transport.ts";
import { showToast } from "./toast";
import type { Grid } from "./grid";
import type { SplitPolicy } from "./splitfloor";
import { Workspace } from "./workspace";
import { TabManager } from "./tabs";
import { TabBar } from "./tabbar";
import type { Pane, PaneEvents, PaneOptions } from "./pane";
import { SessionBrowser, timeAgo } from "./sessions";
import {
  ensureOutputRouter,
  onPtyExit,
  loadUiTabs,
  saveUiTabs,
  loadSettings,
  saveSettings,
  guardAppClose,
  recordCopilotLaunchPosture,
  recordClaudeLaunchPosture,
  discoverSsh,
  loadSshProfiles,
  type PtyExit,
  type SessionInfo,
} from "./pty";
import { decodeSettings, encodeSettings, setSettings, DEFAULT_SETTINGS } from "./settings";
import { modal, confirmModal } from "./modal";
import {
  SubmitLatch,
  withSubmitLatch,
  sshReconnectArgv,
  SSH_NO_CLIENT,
  SSH_PROFILE_GONE,
} from "./panesetup";
import { decodeSshProfiles } from "./sshprofile";
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
import { reclaimFocusOnWindowFocus } from "./nativedialog.ts";
import { SideDock } from "./sidedock";
import { followsPaneChange, isActiveTabChange } from "./sidedockmodel";
import { admitRoot, ftRootIsDir } from "./fileapi";
import { gitRepoRoot } from "./git";
import { voiceController } from "./voicecontrol";
import { initStatusBar } from "./statusbar";
import { startModelDetection } from "./modelprobe";
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
  soloPrepare,
  soloBind,
  confirmSoloCopilotAutopilot,
  SOLO_GROUP,
  type OrchWiring,
  type OrchTarget,
  type OrchestratorConfig,
  type AttentionItem,
} from "./orchestration";
import { tabAttention, sameAttention, findPaneByPty, orchestratorLaunchTarget } from "./tabroute";
import { AttentionGate } from "./attentiongate";
import {
  encodeTabs,
  decodeTabs,
  type PersistedTabs,
  type PersistedLayoutNode,
  type PersistedPane,
  type PersistedEmbed,
} from "./tabstore";
import { decideRestore } from "./restoredecision";
import { resumeFailureKind, offersStartFresh, resumeFailureReason } from "./resumeerror";
import {
  planLayoutRestore,
  planPaneRestore,
  agentResumeCommand,
  agentFreshCommand,
  stripSoloMcpFlags,
  appendSoloMcpArgs,
  shouldRespawnFresh,
  findResumedPaneIndex,
  hasForkSession,
  shouldWatchCopilotOnRestore,
  normalizeAgentProgram,
  type RestoreAction,
  type SessionResumable,
} from "./panerestore";
import { showRestoreSplash } from "./restoresplash";
import {
  planSessionAdoption,
  dormantResumeCandidate,
  type ReconcilePane,
  type SessionRecord,
} from "./sessionreconcile";
import { sessionRestoreRoute } from "./sessionroute";
import { planGroupResume, partitionByGroup } from "./groupresume";
import {
  IDLE_RESTORE_CARD_STATE,
  errorRestoreCardState,
  nextRestoreCardState,
  type RestoreCardState,
} from "./restorecard";

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
const workspaceEl = document.getElementById("workspace")!;
const stackEl = document.getElementById("workspace-stack")!;
const tabBarEl = document.getElementById("tab-bar")!;

/** The right-side dock (#1020 item 6): git / files / editor over the right edge
 *  of the workspace, following the active pane's folder. Assigned once the tab
 *  manager below exists — it is declared here, ahead of the Workspace factory
 *  that notifies it, so the factory's closure has a name to reach and nothing
 *  depends on module-init ordering. */
let sideDock: SideDock | null = null;

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
    () => onGridChanged(),
    // The active pane moved inside THIS tab — which is only the dock's business
    // when this tab is the foreground one. Every workspace has a grid, so every
    // workspace gets this callback; a background tab reshuffling its own active
    // pane (an agent finishing, a delegate spawning, a group resuming) must not
    // drive a follow.
    //
    // It is not enough that the dock reads the active pane itself rather than
    // the pane this fired for. That gets the right pane and still re-reads its
    // LIVE cwd at a moment the human did not cause — adopting a `cd` they typed
    // earlier and had every reason to think was ignored, whenever some other
    // tab's agent happened to be busy (#1097 rev-776, the second door onto the
    // same defect as the `tabs.onChange` one below).
    (w) => {
      if (!followsPaneChange(w.id, tabs.activeTabId)) return;
      sideDock?.followActivePane();
    }
  );
  stackEl.appendChild(ws.el);
  return ws;
});

/** The tab strip, assigned once boot mounts it. Held so the keyboard
 *  Ctrl+Shift+K routes through the same two-step close-confirm the ✕ uses. */
let tabBar: TabBar<Workspace> | null = null;

/** The active tab's grid — the single-grid `grid` of the pre-tabs app. */
const activeGrid = (): Grid => tabs.activeWorkspace.grid;

/** The dock button, held so the dock can report that the row has no room for
 *  it (#1150). Its resting title is read off the markup rather than duplicated
 *  here, so the two cannot drift. */
const dockBtn = document.getElementById("btn-sidedock") as HTMLButtonElement;
const dockBtnTitle = dockBtn.title;

sideDock = new SideDock(workspaceEl, document.getElementById("grid-area")!, {
  // `activeWorkspace` THROWS before the first tab exists, and this is read at
  // construction — which happens before boot seeds one.
  activeCwd: () => (tabs.count === 0 ? null : tabs.activeWorkspace.grid.activePane?.workdir ?? null),
  // The dock's grip drag resizes the grid since #1150, so it is bracketed like
  // any other divider (#432). Only the ACTIVE tab's panes are held: a background
  // tab's are `display: none` and report zero width, which `shouldResizePty`
  // already refuses, so holding them would be bookkeeping with no effect.
  // Captured once, released once — same list both times, whatever happens to
  // the tab set mid-drag.
  holdPaneResizes: () => {
    const held = tabs.count === 0 ? [] : tabs.activeWorkspace.grid.panes();
    held.forEach((p) => p.beginResizeHold());
    return () => held.forEach((p) => p.endResizeHold());
  },
  // A dock the row cannot seat is hidden rather than cropped (#1150), so the
  // control has to say why — otherwise it is a button that does nothing, which
  // is the other way for this to look broken.
  setToggleAvailability: (available, reason) => {
    dockBtn.disabled = !available;
    dockBtn.title = reason ?? dockBtnTitle;
  },
});

// Switching PROJECT TABS changes the active pane without any grid's `setActive`
// firing: `applyActive` focuses the incoming tab's already-active pane, and
// `setActive` early-returns on the pane it is already on. So the dock needs a
// second trigger, or it keeps showing the previous tab's repo.
//
// `tabs.onChange` is the only subscription available, and it is a tab-SET
// listener, not an active-tab one — it also fires on rename, colour, reorder,
// close, an attention flip in any background tab, and orch-channel traffic.
// Following it unfiltered was a real defect (#1097 rev-767 B1): the dock re-reads
// the active pane's LIVE cwd, so a `cd` the human typed minutes ago and that was
// correctly ignored at the time would be adopted later, at whatever unrelated
// moment some other tab's attention chip happened to flip — silently rebuilding
// the file explorer out from under them and closing a clean editor file.
//
// So the id is compared, and only a genuine active-tab change gets through.
let lastActiveTabId: string | null = tabs.activeTabId;
tabs.onChange(() => {
  const next = tabs.activeTabId;
  if (!isActiveTabChange(lastActiveTabId, next)) return;
  lastActiveTabId = next;
  sideDock?.followActivePane();
});

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
    // A pane header's ◫/⬓ — a human split gesture, so the pane being split is
    // the one that pays for the new one (#885 `halve`).
    onSplit: (pane, dir) => openWelcomeIn(ws, dir, pane, "halve"),
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

const attentionGate = new AttentionGate();

/** A token that changes whenever the pane population does: the tab list, in
 *  order, with each tab's pane count (docked panes included). Allocation-free
 *  apart from the string — it must be cheaper than the pass it guards, so it
 *  counts panes rather than walking them. */
function paneTopology(): string {
  let token = "";
  for (const ws of tabs.tabs) token += `${ws.id}:${ws.grid.allPaneCount};`;
  return token;
}

/** Apply an attention scan across all tabs: badge each pane by its pty (the
 *  pre-tabs behavior, now spanning every tab) AND badge the tab-strip entry of
 *  any tab that owns a needs-attention pty — so a hidden tab's blocked agent
 *  still surfaces (#63). Uses a live pty→tab map built from the actual
 *  panes, so plain (#40) panes badge their tab too, not just bound agents. */
function applyAttention(items: AttentionItem[]): void {
  // The 3 s tick re-emits the whole set whether or not anything moved, so the
  // pass below — every pane of every tab, plus the tab-attention recompute —
  // only runs when the payload or the pane population actually changed
  // (#743 S5; see attentiongate.ts for why the payload alone is not enough).
  if (!attentionGate.shouldApply(items, paneTopology())) return;
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
  bindGroupForPane(pane, groupId): void {
    // #407: the promoted pane never moved, so its group belongs to the tab it is
    // already in. Silent no-op if the pane isn't in any grid (it was closed
    // mid-promotion) — there is no tab to bind, and the group is durable either way.
    const ws = tabs.tabs.find((t) => t.grid.allPanes().includes(pane));
    if (!ws) return;
    tabs.bindGroup(groupId, ws.id);
    persistTabs();
  },
  forgetGroup(groupId): void {
    tabs.forgetGroup(groupId);
    persistTabs();
  },
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
async function restoreSessionTabs(saved: PersistedTabs, resumable?: SessionResumable): Promise<void> {
  // Track the tabs WE create so activeIndex resolves against them, not against
  // tabs.tabs — the pre-splash seed tab sits at index 0 and would offset it (BUG-2).
  const restored: Workspace[] = [];
  for (const t of saved.tabs) {
    const ws = tabs.newTab(false);
    restored.push(ws);
    tabs.renameTab(ws.id, t.name);
    tabs.setColor(ws.id, t.color);
    // EVERY group this tab owned (#485), not just the first: a tab can hold two
    // orchestration groups, and binding only one left the second group's
    // rejoined panes routing into a freshly minted background tab instead of
    // back here. `groupIds` decodes to `[groupId]` for a pre-#485 snapshot, so
    // a single-group tab binds exactly what it always did.
    for (const g of t.groupIds ?? (t.groupId ? [t.groupId] : [])) tabs.bindGroup(g, ws.id);
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
  resumable?: SessionResumable
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
  resumable?: SessionResumable
): Promise<void> {
  for (const record of docked) {
    const pane = await openActionPane(ws, planPaneRestore(record, resumable));
    reapIfExited(ws, pane); // same early-exit drain as the layout path
    ws.grid.minimize(pane);
  }
}

/** Re-mint a restored agent's solo channel identity (#439) before its command
 *  boots. A recorded resume/fresh/dormant command can carry the MCP flags
 *  `soloPrepare` appended at ITS pane's last launch — they point at a
 *  `configs/solo-N.json` that agent's exit already deleted (and cleared the
 *  token for), so replaying them hard-errors claude and authenticates nothing
 *  on copilot. `stripSoloMcpFlags` (pure, panerestore.ts) removes them and
 *  reports which CLI they belonged to; when it reports none (a custom
 *  command, or a channel-tools-off launch — nothing was ever minted) this is
 *  a no-op and the command comes back unchanged.
 *
 *  When a solo identity WAS recorded, this re-mints a fresh one via
 *  `soloPrepare` — same best-effort contract as a live launch
 *  (`bindSoloIfNeeded`/launcher.ts:1337): a failed mint just leaves the
 *  stripped command with no MCP flags at all, so the pane still boots —
 *  delivery-only, adoptable later via Connect — rather than replaying a
 *  guaranteed-dead path. The returned `bind` thunk must be invoked with the
 *  pane's `ptyId` once its pty is up (a no-op when nothing was minted); the
 *  returned `channelAgent` is the carrier `PaneOptions.channelAgent` expects
 *  — pass it through `openPane`/`startFromDormant` (both apply it via
 *  `Pane.start`) or, for `respawnFresh` (which does NOT apply it — #439
 *  finding), via `Pane.setChannelAgent` explicitly. */
async function remintSoloIdentity(
  name: string,
  cwd: string | undefined,
  command: string | undefined,
  argv: string[] | undefined
): Promise<{
  command?: string;
  argv?: string[];
  channelAgent?: { group: string; agentId: string; role: string; canSend: boolean };
  bind: (ptyId: number) => void;
}> {
  const stripped = stripSoloMcpFlags(command, argv);
  if (!stripped.cli) return { command: stripped.command, argv: stripped.argv, bind: () => {} };
  try {
    const prepared = await soloPrepare(stripped.cli, cwd ?? "", name);
    const withArgs = appendSoloMcpArgs(stripped.command, stripped.argv, prepared.mcp_args);
    return {
      command: withArgs.command,
      argv: withArgs.argv,
      channelAgent: { group: SOLO_GROUP, agentId: prepared.agent_id, role: "solo", canSend: !prepared.delivery_only },
      bind: (ptyId: number) => {
        void soloBind(prepared.agent_id, ptyId).catch(() => {
          /* best-effort — same as launch (launcher.ts:1337) */
        });
      },
    };
  } catch {
    // Best-effort: the mint failed, so the pane boots with the stale flags
    // simply gone — delivery-only until the human adopts it via Connect.
    return { command: stripped.command, argv: stripped.argv, bind: () => {} };
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
    case "spawn-terminal": {
      // #1042: the trusted webview re-declaring its own persisted state, before
      // the pane exists. A spawn cwd is deliberately NOT an admit path on its
      // own (`spawn_pty` widening the filesystem surface as a side effect would
      // be an admit the roster cannot see) — so this declaration, and the
      // launcher's matching one for a fresh pane, are what keep the restored
      // pane's folder chip and git watch alive: they name this directory, or
      // something the shell `cd`s into below it, and the descendant rule covers
      // the rest without the pane's OSC-7 stream ever declaring anything.
      if (a.cwd) await admitRoot(a.cwd);
      return ws.grid.openPane(
        { name: a.name, cwd: a.cwd ?? undefined, shellKind: a.shellKind ?? undefined, background: true },
        events,
        dir,
        anchor
      );
    }
    case "resume-agent": {
      // #1042: as `spawn-terminal` above — the recorded cwd is this webview's
      // own persisted state, and it is what the resumed agent pane's chip and
      // git watch will name back.
      if (a.cwd) await admitRoot(a.cwd);
      // Resume into the idle TUI — loads context, spends nothing until a prompt,
      // and NEVER carries a replayed prompt (agentResumeCommand only rewrites flags).
      const resume = agentResumeCommand(a.command, a.argv, a.sessionId);
      // #439: the recorded command may carry a solo channel identity's MCP flags
      // pointing at a config file this pane's own last exit already deleted (and
      // cleared the token for) — re-mint a fresh identity before replaying it.
      const remint = await remintSoloIdentity(a.name, a.cwd ?? undefined, resume.command, resume.argv);
      const pane = await ws.grid.openPane(
        {
          name: a.name,
          cwd: a.cwd ?? undefined,
          command: remint.command,
          argv: remint.argv,
          sessionId: a.sessionId,
          channelAgent: remint.channelAgent,
          background: true,
        },
        events,
        dir,
        anchor
      );
      if (pane.ptyId !== null) remint.bind(pane.ptyId);
      // #456: a restored kickoff is trusted no differently than a fresh one
      // (#364's own precedent for the group path) — checked against the
      // FINAL command (post `remintSoloIdentity` — the MCP re-mint above
      // only ever touches `--mcp-config`/`--additional-mcp-config`, never
      // `--autopilot`, but the command that actually opened the pane is the
      // correct thing to check regardless).
      if (shouldWatchCopilotOnRestore(remint.command ?? null, remint.argv ?? null) && pane.ptyId !== null) {
        void confirmSoloCopilotAutopilot(pane.ptyId, "copilot").catch(() => {
          /* best-effort — see confirmSoloCopilotAutopilot's own doc comment */
        });
      }
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
      // #439: same re-mint as resume-agent — a recorded solo identity's config was
      // deleted at this pane's last exit, so its MCP flags must never be replayed.
      const remint = await remintSoloIdentity(a.name, a.cwd ?? undefined, fresh.command, fresh.argv);
      const pane = await ws.grid.openPane(
        {
          name: a.name,
          cwd: a.cwd ?? undefined,
          command: remint.command,
          argv: remint.argv,
          sessionId: a.sessionId,
          channelAgent: remint.channelAgent,
          background: true,
        },
        events,
        dir,
        anchor
      );
      if (pane.ptyId !== null) remint.bind(pane.ptyId);
      // #456: see the identical guard in "resume-agent" above.
      if (shouldWatchCopilotOnRestore(remint.command ?? null, remint.argv ?? null) && pane.ptyId !== null) {
        void confirmSoloCopilotAutopilot(pane.ptyId, "copilot").catch(() => {
          /* best-effort — see confirmSoloCopilotAutopilot's own doc comment */
        });
      }
      return pane;
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
        groupId: null, // an agent pane belongs to no orchestration group (#485)
        file: null,
        sshProfileId: null, // …nor to an SSH connection (#887 S4)
        embeds: [],
      };
      let pane: Pane;
      // #479 B: this card's whole reason for existing IS "no resumable
      // session" — mount it directly in the error visual (red accent,
      // warning icon) rather than the same neutral gray a plain "Resume
      // group" card uses, so the two are no longer visually indistinguishable
      // (the human's own complaint). The diagnostic wording is unchanged;
      // only the styling and the failure-handling below are new.
      const content = dormantCard({
        action: "Start",
        pendingLabel: "Starting…",
        errorTitle: "Not available to resume",
        title: a.name,
        body: "This agent had no resumable session — start it fresh in its folder.",
        initial: errorRestoreCardState("This agent had no resumable session — start it fresh in its folder."),
        onClick: async () => {
          // startFromDormant flips `pane.isDormant` false synchronously (so a
          // second click / the #440 D2 prefetch can't re-fire against this
          // same card) but keeps the placeholder ELEMENT mounted until the
          // spawn itself settles (#479 review finding 2) — notify once it's
          // live so the counter reflects it. This is copilot's own restore
          // path (it never gets a recorded session id — see launcher.ts:1321
          // — so it always lands here, never resume/fresh-agent), and its
          // recorded command can carry the SAME dead solo-identity MCP flags
          // (#439) — re-mint before replaying, just like resume/fresh-agent.
          // Wrapped in try/catch (#479 A): a failed start must land the card
          // on its error state, never an unhandled rejection behind a spinner
          // stuck forever — and dormantCard's own render() now falls back to
          // a toast if this card's element is ever detached by the time this
          // settles (finding 2's structural backstop, not just this ordering
          // fix).
          try {
            const remint = await remintSoloIdentity(
              a.name,
              a.cwd ?? undefined,
              a.command ?? undefined,
              a.argv ?? undefined
            );
            await pane.startFromDormant({
              name: a.name,
              cwd: a.cwd ?? undefined,
              command: remint.command,
              argv: remint.argv,
              channelAgent: remint.channelAgent,
            });
            if (pane.ptyId !== null) remint.bind(pane.ptyId);
            // #456: today's most-reachable copilot restore path — copilot
            // never carries a tracked session id on this build, so it always
            // restores dormant (see panerestore.ts's `decide()`). Same guard
            // as "resume-agent"/"fresh-agent" above.
            if (shouldWatchCopilotOnRestore(remint.command ?? null, remint.argv ?? null) && pane.ptyId !== null) {
              void confirmSoloCopilotAutopilot(pane.ptyId, "copilot").catch(() => {
                /* best-effort — see confirmSoloCopilotAutopilot's own doc comment */
              });
            }
            onGridChanged();
            return { ok: true };
          } catch (err) {
            return { ok: false, message: String(err) };
          }
        },
      });
      pane = ws.grid.openDormantPane(events, record, content, dir, anchor);
      // #440 D2: this card has NO recorded session id (that's exactly why it's
      // dormant) — but the pane's cwd might still have a matching transcript the
      // human can name (a custom-launched agent that reconciliation hasn't run
      // against, or ran against and found ambiguous). Resolved AFTER the card is
      // already rendered/returned, off the existing background prefetch — never
      // blocking this pane's open on a `listSessions()` scan (#342). A folder
      // with no match keeps today's plain-Start-only wording.
      //
      // A --fork-session line is excluded outright (review round 2, B3): the
      // button would attach candidate.id to a line that mints a DIFFERENT id
      // on its very next resume, recording something wrong-but-authoritative-
      // looking instead of honestly nothing. Such a card stays plain-Start-
      // only — same exclusion `reconcileCandidates` applies automatically.
      //
      // #457: routed through the same `normalizeAgentProgram` `Pane.agentCli`/
      // `programFromRestore` now use, instead of a fourth independent
      // first-token derivation — a path-qualified or `.exe`-suffixed
      // recorded command is recognized here too now.
      const first = a.command?.trim().split(/\s+/)[0];
      const cli = first ? normalizeAgentProgram(first) : undefined;
      if (a.cwd && (cli === "claude" || cli === "copilot") && !hasForkSession(a.command, a.argv)) {
        void sessionsPrefetch.then(() => {
          if (pane.isDisposed || !pane.isDormant) return; // Start already clicked, or pane closed
          const candidate = dormantResumeCandidate({ cli, cwd: a.cwd }, toRecords(sessions.cached));
          if (!candidate) return;
          const age = timeAgo(candidate.modifiedMs);
          const cliLabel = cli === "claude" ? "CLAUDE" : "COPILOT";
          addDormantCardAction(
            content,
            "Resume last session",
            `${cliLabel} · "${candidate.title}" · ${age}\nin ${a.cwd}`,
            () => {
              const rewrite =
                a.command || (a.argv && a.argv.length)
                  ? agentResumeCommand(a.command, a.argv, candidate.id)
                  : { command: candidate.resumeCommand };
              void pane
                .startFromDormant({
                  name: a.name,
                  cwd: a.cwd ?? undefined,
                  ...rewrite,
                  sessionId: candidate.id,
                })
                .then(() => onGridChanged());
            }
          );
        });
      }
      return pane;
    }
    case "dormant-ssh": {
      // An SSH pane comes back as a card, never as a connection (#887 S4): the
      // far end is an agent CLI on someone else's machine (remote credits, no
      // human present) and a host that is down or behind a VPN would otherwise
      // put a TCP connect on the boot path. One click, then it connects.
      const record: PersistedPane = {
        paneKind: "ssh",
        name: a.name,
        cwd: null,
        command: null,
        argv: null,
        shellKind: null,
        sessionId: a.sessionId,
        // The #887/#888 boundary, restated where the placeholder is built: an
        // SSH pane is never an orchestration member, so this record carries no
        // role, no group, and no docked orchestration views. The restore ACTION
        // has no field to carry one either (panerestore.ts) — this is the second
        // half of the same guarantee, at the only other place a record is made.
        role: null,
        groupId: null,
        file: null,
        sshProfileId: a.profileId,
        embeds: [],
      };
      let pane: Pane;
      /** One reconnect attempt. `useRecordedSession: false` forces the fresh
       *  path — the "Reconnect fresh" escape (review NB3), for a remote
       *  conversation that is gone and so can never be resumed. */
      const attempt = (useRecordedSession: boolean): Promise<RestoreCardResult> =>
        reconnectSshPane(
          pane,
          a.profileId,
          useRecordedSession ? a.sessionId : null,
          async (argv, sessionId, profileId, defaultCli) => {
            try {
              await pane.startFromDormant({
                name: a.name,
                argv,
                sessionId: sessionId ?? undefined,
                ssh: { profileId, defaultCli },
              });
            } catch (err) {
              // Review NB4, the dormant half: `startFromDormant` has already
              // torn the placeholder down by the time this lands, so put a
              // floating card back — the pane keeps a persistent reason and a
              // retry instead of turning into an empty pane with a toast.
              // (`opts.ssh` is applied at the top of `Pane.start`, before the
              // spawn that threw, so the pane is still recognizably an SSH one.)
              mountSshReconnectCard(ws, pane, String(err));
              throw err;
            }
            onGridChanged();
            // Review NB2: drain a spawn that died before the frontend finished
            // wiring this pane — otherwise its exit sits unclaimed in
            // `earlyExits` and the pane looks alive over a dead PTY.
            reapIfExited(ws, pane);
          }
        );
      const content = dormantCard({
        action: "Reconnect",
        pendingLabel: "Connecting…",
        errorTitle: "Couldn't reconnect",
        title: a.name,
        body: "SSH connection — dormant. Reconnect opens it again, resuming the remote session when one was recorded.",
        // A profile-less record can never reconnect, and it can say so at MOUNT
        // time rather than making the human click to find out — the same
        // already-know-it-has-nothing case the dormant-agent card uses `initial`
        // for.
        initial: a.profileId ? undefined : errorRestoreCardState(SSH_PROFILE_GONE),
        onClick: () => attempt(true),
        secondary: sshFreshEscape(a.profileId, a.sessionId, () => attempt(false)),
      });
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
      // deliberately does NOT probe the workflow FILE: a repo whose workflow file
      // has been deleted is not a broken pane, it is a pane with nothing in it yet — and
      // it opens on the empty state that offers to create one.
      const kind =
        a.type === "open-files" ? "files" : a.type === "open-editor" ? "editor" : "workflow";
      const what =
        kind === "files" ? "File explorer" : kind === "editor" ? "File editor" : "Workflow pane";
      const root = a.root;
      // #1042: the trusted webview re-declaring its OWN persisted state — the
      // same authority that admitted this root when the human first pointed the
      // pane at it. Before the probe, because `ftRootIsDir` is an `ft_list_dir`
      // call and slice C root-scopes it; a declaration after the probe would be
      // a declaration for a pane that had already been failed soft.
      //
      // A remote client replaying a tabs file cannot do this: `admit_root` is
      // off the wire roster, so the declaration is refused and the pane lands on
      // the SAME welcome form this arm already opens for a deleted folder. And
      // because the backend registry is never persisted, the replay itself can
      // never make a root declared server-side.
      if (root) await admitRoot(root);
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
          // the same field, and an ABSENT one means the default workflow path.
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
        // #1042: as above — re-declare the recorded root before asking git,
        // which slice C root-scopes. Note this arm's throw-vs-null distinction
        // keeps working either way: a refused declaration does not make
        // `gitRepoRoot` throw, and slice C's refusal arrives as a throw (a
        // tooling-shaped failure), which this arm deliberately treats as "keep
        // the pane" rather than as git's own "not a repo".
        await admitRoot(root);
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
        // …including WHICH group it belongs to (#485): the click below resumes
        // this placeholder's own group, not whatever group the tab is bound to.
        groupId: a.groupId,
        file: null,
        // An orchestration pane is never an SSH pane — the #887/#888 boundary
        // refuses that combination before any process starts.
        sshProfileId: null,
        // The docked-view preferences (#361) ride along too, so re-capturing
        // a still-dormant tab (Resume never clicked) reproduces them byte
        // for byte, and resumeDormantGroup can reapply them once the pane
        // they belong to is actually resumed.
        embeds: a.embeds,
      };
      const content = dormantCard({
        action: "Resume group",
        pendingLabel: "Resuming…",
        errorTitle: "Couldn't resume this group",
        title: a.name,
        body: "Orchestration group — dormant. Resume brings the whole group back; no agents run until you do.",
        // In-flight guard (#194 P4 MED-3): resumeDormantGroup awaits, and the
        // card's own state machine holds "pending" until it settles — a second
        // click while it's running is a no-op (nextRestoreCardState), not a
        // double-create of the group (two orchestrator PTYs), the exact
        // double-spawn the contract forbids. #479: unlike the old bare
        // disable/re-enable, a genuine failure now lands on the card's error
        // state (red, the diagnostic message from resumeDormantGroup) instead
        // of silently reverting to the same neutral "Resume group" card with
        // only a toast to show for it.
        onClick: () => resumeDormantGroup(ws, record),
      });
      return ws.grid.openDormantPane(events, record, content, dir, anchor);
    }
  }
}

/** What a dormant card's action resolves to: success (the caller almost
 *  always tears the whole card down right after, replacing it with a live
 *  pane) or a failure with the diagnostic to show. Every call site below
 *  catches its OWN known failure points into this shape, and `dormantCard`'s
 *  own click handler ALSO has a rejection handler on `onClick` itself (#479
 *  review finding 1) as a structural backstop — so a restore failure lands on
 *  the card's error state whether or not a given call site remembered to
 *  catch it locally, not only when it did. This type/contract covers every
 *  `dormantCard`'s primary action; the separate #440 D2 "Resume last
 *  session" secondary button (`addDormantCardAction`) is NOT wired through
 *  it — that action predates this state machine and still falls back to the
 *  pre-#479 behavior (an uncaught rejection reaching the global error
 *  banner) on failure, same as before this PR, not silently worse and not
 *  newly better either. */
type RestoreCardResult = { ok: true } | { ok: false; message: string };

/** Options for `dormantCard` (#479 rework): the same small card a dormant
 *  restore placeholder renders — title, one-line explanation, single action —
 *  now driven by `restorecard.ts`'s pure state machine instead of a bare
 *  disable/re-enable, so BOTH halves of #479 are one component:
 *   (A) the click is acknowledged immediately (pending: spinner, disabled,
 *       "click did nothing" is no longer a real possibility to worry about);
 *   (B) a failure lands on a persistent, visually distinct error card — red
 *       accent, warning icon, a heading that says so — carrying the
 *       diagnostic `message` (never traded away for a cleaner-looking card,
 *       #440) and a retry of the same action, never a spinner that just
 *       quietly reverts to looking like nothing happened.
 *  `initial` lets a card that already KNOWS at mount time it has nothing to
 *  resume (the dormant-agent "no resumable session" card) start directly in
 *  the error visual — no click needed to discover that. */
function dormantCard(opts: {
  action: string;
  title: string;
  body: string;
  onClick: () => Promise<RestoreCardResult>;
  initial?: RestoreCardState;
  /** Heading shown while in the error state; defaults to `title`. */
  errorTitle?: string;
  /** Button label while pending; defaults to `action`. */
  pendingLabel?: string;
  /** A second action driven by THIS SAME state machine (#887 S4 / PR #926
   *  review round 2 B1). Distinct from `addDormantCardAction` below, which
   *  appends a button that runs OUTSIDE it: that is right for #440's
   *  asynchronously-discovered "Resume last session" (it races a local agent
   *  start — visible, killable, cheap), and wrong for the SSH card, where the
   *  two actions race a REMOTE one. Sharing the machine means a click on either
   *  puts BOTH into pending: no button stays live looking clickable while a
   *  connection is being made, and the fresh path gets the same "Connecting…"
   *  acknowledgement the primary has always had. */
  secondary?: { action: string; tooltip: string; onClick: () => Promise<RestoreCardResult> };
}): HTMLElement {
  let state = opts.initial ?? IDLE_RESTORE_CARD_STATE;

  const wrap = document.createElement("div");
  wrap.className = "dormant-card";
  const icon = document.createElement("div");
  icon.className = "dormant-icon";
  icon.textContent = "⚠"; // warning triangle — CSS shows it only in the error tone
  icon.setAttribute("aria-hidden", "true");
  const h = document.createElement("div");
  h.className = "dormant-title";
  const p = document.createElement("div");
  p.className = "dormant-body";
  const btn = document.createElement("button");
  btn.className = "dormant-btn";
  btn.type = "button";
  const spinner = document.createElement("span");
  spinner.className = "dormant-spinner";
  spinner.setAttribute("aria-hidden", "true");
  const label = document.createElement("span");
  label.className = "dormant-btn-label";
  btn.append(spinner, label);
  // The optional second action (see `opts.secondary`). Quiet tone: it sits
  // beside a primary that IS the recommended click, so it must not compete with
  // it the way `dormant-btn-secondary`'s green accent would.
  const secondBtn = opts.secondary ? document.createElement("button") : null;
  if (secondBtn && opts.secondary) {
    secondBtn.className = "dormant-btn dormant-btn-quiet";
    secondBtn.type = "button";
    secondBtn.textContent = opts.secondary.action;
    secondBtn.title = opts.secondary.tooltip;
  }

  const render = (): void => {
    // #479 review finding 2: a failure can arrive after the caller has
    // already torn this card's element out of the document (the dormant-
    // agent Start card removes its placeholder before `onClick`'s own
    // work finishes — pane.ts's `startFromDormant` awaits the spawn before
    // tearing down as of this same fix, but a future teardown-then-fail
    // ordering elsewhere is exactly the class of bug that closes over, not
    // just the two named instances). Rendering an error into a detached
    // element is invisible — worse than the pre-PR uncaught rejection,
    // which at least reached the global banner. A toast is the fallback
    // surface so a caught failure is never silently swallowed by its own
    // element being gone.
    if (state.status === "error" && !wrap.isConnected) {
      showToast(`${opts.errorTitle ?? opts.title}: ${state.message ?? ""}`, "error");
      return;
    }
    wrap.classList.toggle("dormant-card-error", state.status === "error");
    h.textContent = state.status === "error" ? (opts.errorTitle ?? opts.title) : opts.title;
    // The diagnostic detail (#440) IS the error message once one exists;
    // outside "error" this is just the card's normal one-line explanation.
    p.textContent = state.status === "error" && state.message ? state.message : opts.body;
    btn.disabled = state.status === "pending";
    // Round 2 B1: the SECOND action is gated by the same state, in both
    // directions. A button left live during another action's spawn is exactly
    // how two ssh clients got started against one pane.
    if (secondBtn) secondBtn.disabled = state.status === "pending";
    label.textContent =
      state.status === "pending" ? (opts.pendingLabel ?? opts.action) : opts.action;
  };
  render();

  /** One click, whichever button it came from: the pending transition, the
   *  render, and the settle/fail handling are identical — only which action
   *  runs differs. */
  const runAction = (action: () => Promise<RestoreCardResult>): void => {
    const next = nextRestoreCardState(state, { type: "click" });
    if (next === state) return; // already pending — ignore the re-entrant click (#194 P4 MED-3)
    state = next;
    render();
    // #479 review finding 1: `.then` with no rejection handler left the
    // card stuck at "pending" forever on any throw outside onClick's own
    // internal catches — the DoD's named anti-goal ("never a stuck
    // spinner"). The second callback below is the structural fix: every
    // path out of `onClick` — resolve ok, resolve not-ok, or reject —
    // reaches a `nextRestoreCardState` transition and a `render()`, so
    // "pending" can never be a terminal state by omission. Wrapping the
    // CALL itself in `Promise.resolve().then(...)` (not just chaining off
    // its result) means even a synchronous throw from a future non-async
    // `onClick` becomes a rejection this handles too, not an uncaught
    // exception escaping the click handler.
    void Promise.resolve()
      .then(action)
      .then(
        (result) => {
          state = result.ok
            ? nextRestoreCardState(state, { type: "settle" })
            : nextRestoreCardState(state, { type: "fail", message: result.message });
          render();
        },
        (err) => {
          state = nextRestoreCardState(state, { type: "fail", message: String(err) });
          render();
        }
      );
  };

  btn.addEventListener("click", () => runAction(() => opts.onClick()));
  if (secondBtn && opts.secondary) {
    const secondary = opts.secondary;
    secondBtn.addEventListener("click", () => runAction(() => secondary.onClick()));
  }

  wrap.append(icon, h, p, btn);
  if (secondBtn) wrap.appendChild(secondBtn);
  return wrap;
}

/** Append a second action button to an ALREADY-RENDERED dormant card (#440
 *  D2) — the resume-candidate lookup this feeds resolves asynchronously,
 *  off the background session prefetch, strictly after `dormantCard` above
 *  has already built and returned the card (never blocking the pane's own
 *  open on it — #342). Styled distinctly (`dormant-btn-secondary`) so the
 *  smarter "we found your last session" option reads as the recommended
 *  one next to plain Start, without demoting Start to a smaller button
 *  (each still spawns nothing until clicked). `tooltip` carries the
 *  identifying detail (CLI · title · age · folder) the button label itself
 *  has no room for. */
function addDormantCardAction(
  card: HTMLElement,
  label: string,
  tooltip: string,
  onClick: (btn: HTMLButtonElement) => void
): void {
  const btn = document.createElement("button");
  btn.className = "dormant-btn dormant-btn-secondary";
  btn.type = "button";
  btn.textContent = label;
  btn.title = tooltip;
  btn.addEventListener("click", () => onClick(btn));
  card.appendChild(btn);
}

/** Shown to a second reconnect click that arrives while one is already in
 *  flight. Phrased as a state of the world rather than a refusal: the human's
 *  click DID something (the connection they asked for is being made), it just
 *  isn't a second connection. */
const SSH_RECONNECT_IN_FLIGHT =
  "Already reconnecting — one attempt is in flight. Wait for it to finish (or fail) before starting another.";

/** One reconnect at a time, PER PANE (PR #926 review round 2 B1).
 *
 *  Per pane rather than global because two different SSH panes reconnecting at
 *  once is fine and normal; two attempts against the SAME pane is the defect —
 *  the pane can bind only one pty, so the other ssh client and the remote agent
 *  CLI behind it are orphaned: unrouted output, an unclaimed exit, and no way to
 *  kill it, since a kill goes through the pane's own `ptyId`.
 *
 *  A WeakMap rather than a field on `Pane`: this is main.ts's restore wiring, not
 *  a property of a terminal pane, and a WeakMap keyed on the pane cannot outlive
 *  it or leak one. Created on first use, so a pane that never reconnects never
 *  gets one. */
const sshReconnectLatches = new WeakMap<Pane, SubmitLatch>();
function sshReconnectLatch(pane: Pane): SubmitLatch {
  let latch = sshReconnectLatches.get(pane);
  if (!latch) {
    latch = new SubmitLatch();
    sshReconnectLatches.set(pane, latch);
  }
  return latch;
}

/** Reconnect one SSH pane — the shared body of BOTH reconnect affordances (#887
 *  S4): the dormant card a restored pane mounts, and the card that floats over a
 *  pane whose connection dropped mid-session. One implementation, because the
 *  two must not be able to reconnect differently; the caller supplies only how
 *  the pane itself is (re)started, which is the one thing that genuinely differs
 *  (a dormant placeholder has no terminal yet; a disconnected pane has one, full
 *  of the output explaining why it is here).
 *
 *  Everything is resolved AT CLICK TIME, deliberately:
 *   - the local ssh client is re-probed, so a machine that has gained (or lost)
 *     one since boot is answered honestly rather than from a stale capture;
 *   - the PROFILE is re-read from `sshprofiles.json`, so an edited connection
 *     reconnects with the edit and a deleted one reconnects with nothing. That
 *     is the contract `SshProfile.id` states: a pane records the connection, not
 *     its contents.
 *
 *  Every failure returns a card-renderable message rather than throwing, so a
 *  reconnect that cannot happen says which of the three reasons it is — no
 *  client, no such connection, or a value the argv builder refuses — instead of
 *  landing on a spinner or a bare stack string. */
async function reconnectSshPane(
  pane: Pane,
  profileId: string | null,
  recordedSessionId: string | null,
  launch: (
    argv: string[],
    sessionId: string | null,
    profileId: string,
    defaultCli: string | null
  ) => Promise<void>
): Promise<RestoreCardResult> {
  return withSubmitLatch(
    sshReconnectLatch(pane),
    () => ({ ok: false as const, message: SSH_RECONNECT_IN_FLIGHT }),
    () => reconnectSshPaneOnce(profileId, recordedSessionId, launch)
  );
}

/** One reconnect attempt, already known to be the only one in flight. Split from
 *  the gate above so the gate is impossible to skip: every caller reaches this
 *  through `reconnectSshPane`, and nothing else calls it. */
async function reconnectSshPaneOnce(
  profileId: string | null,
  recordedSessionId: string | null,
  launch: (
    argv: string[],
    sessionId: string | null,
    profileId: string,
    defaultCli: string | null
  ) => Promise<void>
): Promise<RestoreCardResult> {
  if (!profileId) return { ok: false, message: SSH_PROFILE_GONE };
  const program = await discoverSsh().catch(() => null);
  if (!program) return { ok: false, message: SSH_NO_CLIENT };
  let profile;
  try {
    profile = decodeSshProfiles(await loadSshProfiles())?.profiles.find((p) => p.id === profileId);
  } catch (err) {
    // A store we could not READ is not a store that says the connection is gone
    // — it is a different failure and it names itself, so the human can tell a
    // deleted profile from an unreadable file.
    return { ok: false, message: `Could not read your saved SSH connections: ${String(err)}` };
  }
  if (!profile) return { ok: false, message: SSH_PROFILE_GONE };
  let plan;
  try {
    // Web Crypto — the webview's, never a getrandom crate (constraint 2 governs
    // the Rust graph); the same mint the launch form makes for a fresh connect.
    plan = sshReconnectArgv(program, profile, recordedSessionId, () => crypto.randomUUID());
  } catch (err) {
    // The one refusal the builder raises: a remote-command token cmd.exe cannot
    // be handed safely (a newline, a trailing backslash). A fixable data problem
    // in the saved connection, so it is shown as one.
    return { ok: false, message: String(err instanceof Error ? err.message : err) };
  }
  try {
    // The profile is in hand right here and nowhere downstream — the pane only records a
    // profile ID, and resolving it back costs an async store read the header mark cannot
    // wait for. So the far-end CLI travels with the argv (#992 review B1).
    await launch(plan.argv, plan.sessionId, profile.id, profile.defaultCli);
  } catch (err) {
    return { ok: false, message: String(err) };
  }
  // A reconnect that HAD a session to resume and started a new one anyway is the
  // one outcome the human would otherwise misread: the pane comes back looking
  // exactly like a resume, on a conversation the far host has never seen. It
  // happens when the connection has been edited to a CLI whose session identity
  // loomux cannot carry (`sshMintsSessionId`), so say so rather than let them
  // discover it by asking the agent about work it has no memory of. A fresh
  // connect with nothing recorded needs no such notice — nothing was lost.
  if (recordedSessionId && plan.mode === "fresh") {
    showToast(
      `${profile.name}: reconnected with a NEW remote session — this connection's CLI ` +
        `has no session id orrerix can resume, so the earlier conversation is not the one on screen.`,
      "info"
    );
  }
  return { ok: true };
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
async function resumeDormantGroup(
  ws: Workspace,
  clicked: PersistedPane
): Promise<RestoreCardResult> {
  // WHICH GROUP THIS CLICK RESUMES (#485): the clicked placeholder's own
  // recorded group. Reading it off the TAB is what let a two-group tab resume
  // group A, drop B's orchestrator without a word, and rejoin B's delegates
  // into A. The tab binding is only the fallback for a pre-#485 snapshot,
  // where no placeholder recorded a group at all.
  const groupId = clicked.groupId ?? tabs.groupForWorkspace(ws.id);
  if (!groupId) {
    sessions.toggle(); // no binding to resume from — let the human pick a session
    return { ok: true };
  }
  // Another card of this same group is already resuming — the whole group comes
  // back at once, so ignore the duplicate rather than re-run the multi-pane resume.
  if (resumingGroups.has(groupId)) return { ok: true };
  resumingGroups.add(groupId);
  try {
    // The member set is the CAPTURED orch panes — the tab's dormant ORCH
    // placeholders, one per orch pane that was live at close, each carrying its own
    // session id + role. This is the fix for the over-restore regression: the set
    // comes from what was captured, NEVER expanded by session_roles().
    //
    // …and only the placeholders belonging to THIS group (#485). A second
    // group's placeholders in the same tab are not members of this plan, are
    // not rejoined by it, and (step 4) are not cleared by it — they keep their
    // own Resume card, which resumes their own group.
    const myPanes = partitionByGroup(
      ws.grid
        .allPanes()
        .filter((p) => p.isDormant && p.dormantKind === "orch" && p.restoreRecord !== null)
        .map((p) => ({ pane: p, groupId: p.restoreRecord!.groupId })),
      clicked.groupId
    ).mine;
    const orchRecords = myPanes.map((m) => m.pane.restoreRecord as PersistedPane);
    const captured = orchRecords
      .filter((r) => r.sessionId !== null)
      .map((r) => ({
        sessionId: r.sessionId as string,
        role: r.role ?? "worker",
        // Each member's OWN group travels into the plan (#485), so the plan
        // itself — not this wiring — is what refuses a foreign member.
        groupId: r.groupId,
      }));
    // The docked-view preferences (#361) aren't part of the resume plan
    // itself (planGroupResume only orders/gates on session id + role) —
    // they're captured UI preferences, reapplied below once each member's
    // pane actually comes back, by matching sessionId the same way the plan
    // itself does.
    const embedsBySession = new Map<string, PersistedEmbed[]>();
    for (const r of orchRecords) {
      if (r.sessionId && r.embeds.length) embedsBySession.set(r.sessionId, r.embeds);
    }
    // resumeOrchSession only returns the group id — "the pane itself is located
    // later by scanning live panes" (orchestration.ts) — so this does the same,
    // matching on the session id we just asked it to resume. The dormant-
    // exclusion DECISION (a stale placeholder for this same member carries the
    // identical captured session id and must never shadow the real match) is
    // the pure, tested `findResumedPaneIndex` (panerestore.ts); only the live
    // pane traversal itself is wiring, hand-validated like the rest of this
    // function's grid access.
    const findResumedPane = (sessionId: string): Pane | undefined => {
      const panes = ws.grid.allPanes();
      const idx = findResumedPaneIndex(
        panes.map((p) => ({ isDormant: p.isDormant, sessionId: p.capture()?.sessionId ?? null })),
        sessionId
      );
      return idx === -1 ? undefined : panes[idx];
    };
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
      return { ok: true };
    }

    let resumableIds = new Set<string>();
    try {
      // #493: the SHARED session list, not a scan of this path's own. This line
      // used to call `listSessions()` directly, and the breadcrumb log caught
      // what that cost: a restore click ~4s into boot issued a second, fully
      // concurrent scan of the same 826 files while the sidebar's boot prefetch
      // was still running (12.9s + 16.7s, contending), and the group restore
      // then waited on it — the second, unfixed half of #479's restore lag.
      // `ensureLoaded()` reuses the prefetch's rows, or joins the prefetch if
      // it's still in flight, and only scans if neither can answer. Freshness
      // isn't what this check needs: it asks whether ids CAPTURED AT CLOSE still
      // have transcripts, and a transcript the newest read already saw hasn't
      // stopped existing since.
      resumableIds = new Set((await sessions.ensureLoaded()).map((s) => s.id));
    } catch {
      /* empty → assume resumable below */
    }
    const seenAny = resumableIds.size > 0;
    const plan = planGroupResume(
      captured,
      (sid) => (seenAny ? resumableIds.has(sid) : true),
      clicked.groupId
    );

    if (plan.ambiguous) {
      // Two orchestrators in one tab and no recorded group to tell them apart
      // — a snapshot written before #485. Refusing is the whole point: the old
      // behavior kept one, dropped the other silently, and rejoined the
      // dropped group's delegates into the survivor. The session browser knows
      // each session's real group, so send the human there.
      const message =
        "This tab's saved layout holds more than one orchestrator and doesn't record which group each belongs to (saved by an older build) — resume each group from the session browser.";
      showToast(message, "error");
      sessions.toggle();
      return { ok: false, message };
    }
    if (!plan.orchestrator) {
      // A stale orchestrator (transcript gone) is gated the same way delegates are:
      // fall back to the browser rather than relaunch into a dead orchestrator pane.
      // #479 B: this IS a "no resumable session" outcome for this card's own
      // action — the error state is warranted, not just a toast + redirect.
      const message = plan.orchestratorUnresumable
        ? "This group's orchestrator session has no saved conversation to resume — open the session browser."
        : "No captured orchestrator session for this group — open the session browser.";
      showToast(message, "info");
      sessions.toggle();
      return { ok: false, message };
    }

    const preexisting = ws.grid.allPanes();
    // 1. Orchestrator first — relaunches the group and makes it live so delegates
    //    can rejoin. A failure here aborts the whole restore (nothing to rejoin into).
    try {
      const restored = await resumeOrchSession(ws.grid, eventsFor(ws), plan.orchestrator.sessionId, {
        group: groupId,
        role: "orchestrator",
      });
      if (restored) {
        tabs.bindGroup(restored.groupId, ws.id);
        const embeds = embedsBySession.get(plan.orchestrator.sessionId);
        if (embeds) findResumedPane(plan.orchestrator.sessionId)?.restoreEmbeds(embeds);
      }
    } catch (err) {
      // Recoverable (retry the button) — a toast, not the app-crash banner (MED-3).
      // #479: the card itself now also lands on its error state carrying this
      // same message, so the failure is still visible after the toast fades.
      const message = String(err);
      showToast(`Couldn't resume group: ${message}`, "error");
      return { ok: false, message };
    }
    // 2. Rejoin each resumable delegate INTO the now-live group. Sequential (not
    //    concurrent) so the group settles live before each rejoin and we don't
    //    fan out a spawn burst; a single member's failure doesn't sink the rest.
    //
    //    #412 review N5: this is a BATCH restore, not the single-session-browser
    //    click `restoreSession` handles — a confirm dialog per failed member (one
    //    click could pop several in a row) would be worse UX than the session
    //    browser's single-target "Start fresh?", so this stays a toast rather than
    //    reusing that dialog. It DOES reuse resumeerror.ts's classification so the
    //    toast is specific about what happened and points at the fix (the session
    //    browser) instead of a raw error string — a `not-found`/`workspace-missing`
    //    member is dropped for good this pass (its whole point is that resuming it
    //    would have failed anyway); it's still resumable/start-freshable later from
    //    the session browser, same as any other unresolvable session.
    for (const member of plan.rejoin) {
      try {
        await resumeOrchSession(ws.grid, eventsFor(ws), member.sessionId, {
          group: groupId,
          role: member.role,
        });
        const embeds = embedsBySession.get(member.sessionId);
        if (embeds) findResumedPane(member.sessionId)?.restoreEmbeds(embeds);
      } catch (err) {
        const message = String(err);
        const kind = resumeFailureKind(message);
        // The two GROUP refusals (#485) get the same treatment the
        // start-freshable kinds already got: the classified sentence from
        // resumeerror.ts rather than the raw `resume-<tag>: …` string, which
        // leaks the wire tag into a human's toast. They are NOT start-freshable
        // (a fresh session would land in the same unverified/wrong group), so
        // they say what to do instead of offering that. Everything else still
        // shows the raw message — for an unclassified failure the diagnostic
        // detail IS the value (#440), and inventing prose for it would lose
        // that.
        const groupRefusal = kind === "group-mismatch" || kind === "group-unknown";
        const reason = offersStartFresh(kind)
          ? `${resumeFailureReason(kind)} Resumable from the session browser.`
          : groupRefusal
            ? resumeFailureReason(kind)
            : message;
        // A group refusal is a correctness stop, not an "FYI" — it is the one
        // failure in this loop that means something was PREVENTED rather than
        // merely unavailable, so it doesn't share the informational styling.
        showToast(`Couldn't rejoin a ${member.role}: ${reason}`, groupRefusal ? "error" : "info");
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
    //
    //    ONLY THIS GROUP'S placeholders (#485) — literally the panes the member
    //    set was read off (`myPanes`), so the two can't drift apart. A second
    //    group sharing the tab keeps its own dormant card: clearing it here is
    //    how the old sweep made the drop invisible — the tab came back looking
    //    fully restored with one group missing from it.
    for (const { pane } of myPanes) {
      if (preexisting.includes(pane)) ws.grid.closePane(pane, false);
    }
    persistTabs();
    return { ok: true };
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
//
// A WeakMap, for the same reason `sshReconnectLatches` above is one: "consumed
// one-shot" is true of the path that FIRES, and the entry is only removed by
// `tryResumeFallback`, which the exit reaper reaches only via
// `findPaneAcrossTabs` — i.e. only while the pane is still in a live grid. A
// restored agent pane the human simply CLOSES therefore exits after it has left
// every grid, the reaper parks the exit in `earlyExits` and nothing ever
// consumes the fallback: under a strong Map that pane, its terminal and its
// whole scrollback stayed reachable for the life of the process (#1301). The
// map is only ever get/set/deleted by pane, never iterated, so weak keys cost
// nothing here.
const resumeFallbacks = new WeakMap<Pane, { opts: PaneOptions; at: number }>();

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
  // #439: fb.opts.command/argv were built (case "resume-agent", above) from the
  // ORIGINAL recorded command, so they can carry the same dead solo-identity MCP
  // flags — re-mint before this fresh respawn replays them. Done here, lazily, at
  // the moment the fallback actually fires (not pre-minted when the entry was
  // queued): the common case never needs a fallback at all, and pre-minting one
  // "just in case" would leak an orphan solo-N config for every successful resume.
  // respawnFresh does NOT apply opts.channelAgent (unlike Pane.start), so the
  // channel identity is set explicitly via setChannelAgent once it's spawned.
  void remintSoloIdentity(fb.opts.name ?? pane.name, fb.opts.cwd, fb.opts.command, fb.opts.argv).then((remint) =>
    pane
      .respawnFresh({ ...fb.opts, command: remint.command, argv: remint.argv })
      .then(() => {
        pane.setChannelAgent(remint.channelAgent ?? null);
        if (pane.ptyId !== null) remint.bind(pane.ptyId);
        onGridChanged();
      })
  );
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
function openWelcomeIn(
  ws: Workspace,
  dir: "row" | "column" = "row",
  relativeTo?: Pane,
  // `share` by default because this function also serves restore fail-softs (a
  // pane whose folder/repo is gone comes back as a welcome form) — programmatic
  // placement. The human split gestures pass `halve` explicitly (#885).
  policy: SplitPolicy = "share"
): Pane {
  const context = relativeTo ?? ws.grid.activePane;
  const form = new WelcomeForm(context?.workdir ?? undefined);
  const pane = ws.grid.openWelcomePane(eventsFor(ws), form.el, dir, relativeTo, policy);
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

  if (result.kind === "ssh") {
    // #887 S3: an SSH pane converts the setup pane in place exactly like a
    // terminal, and spawns through the ordinary argv path — the local ssh.exe IS
    // the pane's child process, so pty.rs needs no SSH anything (plan part 4a).
    //
    // No `cwd` and no `env`, both deliberate: the pane's LOCAL directory stays
    // home (the repo is on the far end, and no local path stands for it), and the
    // gh-shim/`LOOMUX_GROUP_DIR` env is an ORCHESTRATION pane's, which an SSH
    // pane can never be (the #888 boundary, enforced in `Pane.start`).
    await pane.startFromWelcome({
      name: result.name,
      argv: result.argv,
      sessionId: result.sessionId,
      // `defaultCli` rides along so the pane's header mark can name the agent running on
      // the far end rather than the ssh client carrying it (#992 review B1).
      ssh: { profileId: result.profileId, defaultCli: result.defaultCli },
    });
    reapIfExited(ws, pane);
    // Converted in place — no grid open/close fired, so notify explicitly, same
    // as the terminal arm above.
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
    // workflow pane takes no `file` here — the welcome flow means the repo's
    // default workflow path.
    pane.startContent({ kind: result.kind, name: result.name, root: result.root });
    // Converted in place — no grid open/close fired, so notify explicitly (this is
    // what re-renders the tab strip and re-persists the layout), same as terminal.
    onGridChanged();
    return;
  }

  if (result.kind === "orchestrator") {
    // #478: decided from `paneCount` read synchronously HERE, before the
    // `create_orchestration` await below — deliberate, not an oversight (the
    // pre-#478 code re-read live state AFTER its own await instead). The
    // cost: if the human splits THIS tab again while the launch is in
    // flight, `paneCount` is already stale and the own-tab arm's
    // `tabs.closeTab(ws.id)` would destroy that just-made second split
    // instead of leaving it — a millisecond-scale, human-initiated race,
    // called out in review and left as-is (not asked to be fixed here).
    if (orchestratorLaunchTarget(ws.grid.paneCount) === "split") {
      // This setup pane arrived via a genuine split into an
      // already-populated tab — honour that spatial gesture and convert it
      // in place (openAgentPane → existingPane.startFromWelcome), instead of
      // falling into launchOrchestratorTab below, which always mints its own
      // dedicated project tab and would silently relocate the result. Not
      // renamed to the launched repo's project name and not (re)bound away
      // from whatever this tab already represents — a split target's tab may
      // hold panes for an entirely different project, so only the new
      // orchestrator group is bound to it, same precedent `restoreSession`
      // already establishes for binding a group into a tab that wasn't
      // minted for it (`owning ?? tabs.activeWorkspace` fallback, below) —
      // NOT `orchWiring.targetForGroup`, which mints a fresh tab per unseen
      // group and never shares one.
      try {
        const { groupId } = await launchOrchestrator(ws.grid, eventsFor(ws), result.config, pane);
        tabs.bindGroup(groupId, ws.id);
        // The in-place conversion (existingPane.startFromWelcome) fired no
        // grid open/close, so nothing else would notify — same "notify
        // explicitly" requirement startFromWelcome's other callers document
        // (terminal/content/agent above): re-renders the tab strip's live
        // agent counter and re-persists the layout (persistTabs is a
        // tabs.onChange listener, main.ts boot wiring).
        onGridChanged();
      } catch (err) {
        // Same "don't strand a disabled form" contract as the own-tab catch
        // below — but there is no stray tab to tear down here since nothing
        // but this tab's own setup pane was ever touched.
        showToast(`Couldn't start orchestrator: ${String(err)}`, "error");
        form.reopenAfterLaunchFailure(String(err));
      }
      return;
    }
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
    // The setup pane has served its purpose, and (orchestratorLaunchTarget
    // above having taken the "split" branch when it wasn't) it was this tab's
    // ONLY pane — a dedicated welcome tab (fresh start / Ctrl+T) closes
    // entirely so we don't strand a blank tab beside the new orchestrator
    // tab. (The sole-pane / sole-tab case can't happen here —
    // launchOrchestratorTab just added a tab, so tabs.count is at least 2.)
    if (tabs.count > 1) tabs.closeTab(ws.id);
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
  watchCopilotAutopilotIfNeeded(pane, first);
  recordCopilotPostureIfNeeded(first);
  recordClaudePostureIfNeeded(first);
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
      prev,
      // Pinned to `share` (#885), not left to the default: this fan-out is THE
      // reason the even-matrix policy is kept. It places a whole fleet in one
      // pass, each pane beside the last — halving every time would deal out a
      // 1/2, 1/4, 1/8, 1/16 sliver staircase instead of the matrix the
      // alternating direction above is building.
      "share"
    );
    await bindSoloIfNeeded(p, spec);
    watchCopilotAutopilotIfNeeded(p, spec);
    recordCopilotPostureIfNeeded(spec);
    recordClaudePostureIfNeeded(spec);
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

/** Start the solo-pane copilot autopilot consent watcher (#364) for a
 *  just-spawned pane, fire-and-forget. Deliberately independent of
 *  `bindSoloIfNeeded`/`channelAgent` above — the "Enable autopilot mode"
 *  dialog must be answered whenever `--autopilot` is actually on the command
 *  line, regardless of whether channel tools happen to be enabled. Best-effort:
 *  a failed call just leaves the pane's own first submit to hit an unanswered
 *  dialog, no worse than before this fix existed. */
function watchCopilotAutopilotIfNeeded(pane: Pane, spec: AgentLaunchSpec): void {
  if (!spec.watchCopilotAutopilot || pane.ptyId === null) return;
  void confirmSoloCopilotAutopilot(pane.ptyId, "copilot").catch(() => {
    /* best-effort — see doc comment above */
  });
}

/** Record a just-spawned copilot solo pane's Autopilot toggle state (#456),
 *  fire-and-forget, so a LATER Sessions-tab resume of a session from this cwd
 *  can rebuild the same flags instead of dropping them silently — see
 *  `AgentLaunchSpec.copilotAutopilotPosture`'s doc comment and
 *  `src-tauri/src/sessions.rs`'s module doc for the full mechanism and its
 *  ambiguity rule. Skipped when there's no cwd to key on (a home-dir launch)
 *  — nothing reliable to match a later restore against. Independent of
 *  `watchCopilotAutopilotIfNeeded` above: this records intent regardless of
 *  whether the toggle was on, that one only ever watches when it was. */
function recordCopilotPostureIfNeeded(spec: AgentLaunchSpec): void {
  if (spec.copilotAutopilotPosture === undefined || !spec.cwd) return;
  void recordCopilotLaunchPosture(spec.cwd, spec.copilotAutopilotPosture).catch(() => {
    /* best-effort — a lost record just means a later restore falls back to no flags */
  });
}

/** Claude's counterpart to `recordCopilotPostureIfNeeded` (#457) — keyed by
 *  the `sessionId` this launch just minted (`AgentLaunchSpec.claudeAutopilotPosture`'s
 *  doc comment) instead of a cwd. Skipped when there's no id — a custom
 *  command or a non-claude CLI never mints one, so there's nothing to key a
 *  later restore on. Same best-effort, record-both-states contract as the
 *  copilot function above. */
function recordClaudePostureIfNeeded(spec: AgentLaunchSpec): void {
  if (spec.claudeAutopilotPosture === undefined || !spec.sessionId) return;
  void recordClaudeLaunchPosture(spec.sessionId, spec.claudeAutopilotPosture).catch(() => {
    /* best-effort — a lost record just means a later restore falls back to no flags */
  });
}

/** Open a welcome pane in the active tab — the entry point the toolbar/shortcuts
 *  use for a "new pane". `halve` (#885): Ctrl+Shift+E/O and the two top-bar
 *  split buttons are human gestures, so the active pane pays for the new one
 *  out of its own space and no other pane on screen moves. */
const openPane = (dir: "row" | "column" = "row", relativeTo?: Pane): void => {
  openWelcomeIn(tabs.activeWorkspace, dir, relativeTo, "halve");
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
    // The auto-close skips notifyExited, so the in-pane [orrerix] diagnostic
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
    offerSshReconnect(ws, pane, keep);
  } else ws.grid.closePane(pane, false);
}

/** Float a Reconnect card over an SSH pane whose connection just dropped (#887
 *  S4). A no-op for every other pane, and — deliberately — for an ssh pane kept
 *  open for the OTHER reason: `keep === "unsaved"` means the process ended
 *  cleanly (or loomux killed it) and the pane is surviving only to protect a
 *  dirty Alt+F buffer. That is not a disconnection, and offering to reconnect a
 *  session the human deliberately ended would be noise sitting on top of the
 *  buffer they actually need to deal with. `"output"` is the disconnection case
 *  (`keepOpenOnExit`'s ssh arm: unexpected, non-zero — a dropped link, a refused
 *  auth), and it still wins the label when a dirty buffer is present too.
 *
 *  Human-driven, never automatic (plan part 4b): a surprise reconnect re-enters
 *  a remote TUI in a state nobody looked at, and the pane underneath is where
 *  the evidence of what happened is. The card floats over that terminal rather
 *  than replacing it, so the human reads the reason and then decides. */
function offerSshReconnect(ws: Workspace, pane: Pane, keep: KeepOpenReason): void {
  if (!pane.isSshPane || keep !== "output") return;
  mountSshReconnectCard(ws, pane);
}

/** Build and mount the floating Reconnect card. Split from the gate above so a
 *  reconnect that FAILS AT SPAWN can put one back (PR #926 review NB4): both
 *  relaunch paths tear their card down before spawning, so without this a failed
 *  reconnect would leave the pane with no card at all and its only diagnosis in a
 *  transient toast — on the one feature whose expected case is a connection that
 *  fails. `initialError` is that failure, mounted straight into the card's error
 *  state so the reason is persistent and the retry is one click.
 *
 *  What this does NOT recover is the scrollback: `respawnFresh` resets the
 *  terminal before it spawns, and that ordering is deliberate and shared (#720 —
 *  a reset deferred until after a successful spawn would paint the dead session's
 *  tail over the new one's first bytes, because `reset()` clears synchronously
 *  while `term.write` parses asynchronously). Stated rather than quietly traded:
 *  see the design note. */
function mountSshReconnectCard(ws: Workspace, pane: Pane, initialError?: string): void {
  const profileId = pane.sshProfile;
  const recordedSessionId = pane.sessionId;
  // Assigned by the mount below; the click that reads it cannot happen before
  // the card is on screen.
  let dismiss: () => void = () => {};
  /** One reconnect attempt, in place. `sessionId` null forces the fresh path —
   *  that is the whole of the "Reconnect fresh" escape below. */
  const attempt = (useRecordedSession: boolean): Promise<RestoreCardResult> =>
    reconnectSshPane(pane, profileId, useRecordedSession ? recordedSessionId : null, async (argv, sessionId, profileId, defaultCli) => {
      try {
        // In place, in the same pane: `respawnFresh` wipes the dead session's
        // output, clears this very card, and starts the new ssh client on the
        // terminal that is already mounted.
        await pane.respawnFresh({
          name: pane.name,
          argv,
          sessionId: sessionId ?? undefined,
          ssh: { profileId, defaultCli },
        });
      } catch (err) {
        // NB4: the card this click came from is already detached. Put a fresh
        // one back, carrying the reason, so the failure has a persistent home
        // and a retry. (The detached card's own toast fallback also fires — a
        // duplicate notice on a rare path, which beats no persistent surface.)
        mountSshReconnectCard(ws, pane, String(err));
        throw err;
      }
      onGridChanged();
      // NB2: an ssh client that died before the frontend finished wiring the
      // pane parks its exit in `earlyExits`, where nothing else would ever drain
      // it — leaving a pane that looks alive over a dead PTY. The fresh launch
      // path (`handleWelcomeSubmit`) has always reaped here; both reconnect
      // paths now do too.
      reapIfExited(ws, pane);
    });
  const card = dormantCard({
    action: "Reconnect",
    pendingLabel: "Connecting…",
    errorTitle: "Couldn't reconnect",
    title: pane.name,
    body: "The SSH connection closed. Reconnect opens it again, resuming the remote session when one was recorded.",
    initial: initialError ? errorRestoreCardState(initialError) : undefined,
    onClick: () => attempt(true),
    secondary: sshFreshEscape(profileId, recordedSessionId, () => attempt(false)),
  });
  // A pane the human would rather just read (or close) shouldn't have to keep a
  // card over it. Dismiss removes the OFFER, not the pane — the scrollback,
  // which is the thing worth keeping, is untouched either way, and closing the
  // pane stays Ctrl+Shift+W exactly as the exit banner says.
  // Deliberately NOT routed through the card's state machine, unlike the fresh
  // escape (round 2 B1): Dismiss starts nothing, so an extra click cannot cost a
  // second connection — the entire reason that gate exists. Clicking it while a
  // reconnect is in flight just drops the offer; the attempt itself continues,
  // and a failure re-mounts a card carrying the reason, which is the outcome
  // worth seeing whether or not the offer was dismissed first.
  const dismissBtn = quietCardButton("Dismiss", "Hide this offer — the pane and its output stay");
  dismissBtn.addEventListener("click", () => dismiss());
  card.appendChild(dismissBtn);
  dismiss = pane.showReconnectCard(card);
}

/** A secondary card button that is NOT a recommendation. `addDormantCardAction`
 *  is the #440 "we found your last session" button and carries its green
 *  recommended-action accent; Dismiss and "Reconnect fresh" both sit beside a
 *  primary Reconnect that IS the recommended click, so they get a quiet tone
 *  instead of competing with it. */
function quietCardButton(label: string, tooltip: string): HTMLButtonElement {
  const btn = document.createElement("button");
  btn.className = "dormant-btn dormant-btn-quiet";
  btn.type = "button";
  btn.textContent = label;
  btn.title = tooltip;
  return btn;
}

/** The "Reconnect fresh" escape, as the card's SECOND state-machine-driven
 *  action (PR #926 review NB3; reworked in round 2) — or `undefined` when there
 *  is nothing for it to do.
 *
 *  A remote conversation can be gone while the id naming it is still recorded
 *  here: deleted on the far host, a cleared `~/.claude`, a rebuilt box.
 *  `claude --resume <id>` then exits non-zero every time, so plain Reconnect
 *  loops — the pane dies, the card comes back, the human clicks it again. Locally
 *  this exact failure earned its own machinery (#194 BUG-1's one-shot
 *  fresh-respawn backstop), which cannot serve here: it triggers on a resume the
 *  frontend itself launched and can rewrite, whereas the failing `--resume` is a
 *  token inside a remote command string on a host loomux cannot inspect. A human
 *  choosing "fresh" is the honest equivalent, and it stays a CHOICE — an
 *  automatic downgrade to a new session would silently abandon a conversation
 *  that might just be behind a host still booting.
 *
 *  Offered only when BOTH halves of a fresh reconnect exist:
 *   - a recorded session, or the primary Reconnect is already a fresh connect and
 *     a second button doing the same thing is noise;
 *   - a saved connection (round 2 NB1). A card whose record has no `profileId`
 *     mounts already saying the connection is gone; giving it an action whose
 *     only possible outcome is that same refusal — as a toast this time — offers
 *     the human a button that cannot work.
 *
 *  Returned as the card's `secondary` rather than appended afterwards, so the
 *  card's own pending state gates it: see `dormantCard`'s `secondary` and round 2
 *  B1 for what an ungated second action cost. */
function sshFreshEscape(
  profileId: string | null,
  recordedSessionId: string | null,
  start: () => Promise<RestoreCardResult>
): { action: string; tooltip: string; onClick: () => Promise<RestoreCardResult> } | undefined {
  if (!recordedSessionId || !profileId) return undefined;
  return {
    action: "Reconnect fresh",
    tooltip:
      "Start a NEW remote session instead of resuming the recorded one — use this when the remote " +
      "conversation is gone (deleted on the far host), which makes every resume fail",
    onClick: start,
  };
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
const sessionsPrefetch: Promise<void> = sessions.refresh().catch(() => {
  /* best-effort warm-up; never block or fail boot on it */
});

// ---------- #440 D1 option B: post-start session-id reconciliation ----------
//
// adoptableSessionId (panerestore.ts, applied in Pane.start/respawnFresh)
// only catches a custom command line that already NAMES its session. A bare
// `claude` custom line mints its own id with nothing on the line to read —
// the only way to learn THAT id is to watch listSessions() for a match once
// the pane has had a chance to produce a transcript, and adopt it. That's
// this reconciler; sessionreconcile.ts's planSessionAdoption does the actual
// (deliberately refusal-biased — see its module comment) matching.
//
// #342-safe: NOTHING here runs on the boot path. The boot restore sequence
// (`restoreSessionTabs`, above) never awaits this — it isn't even called
// from there. The only triggers are (a) once, chained off this file's own
// sidebar prefetch a few lines up, which already runs in the background and
// was never on the boot-blocking path either, and (b) a periodic check
// thereafter. Both are gated on `reconcileCandidates()` being non-empty, so
// an ordinary boot with every id already recorded costs nothing beyond that
// cheap in-memory filter — no `listSessions()` re-scan, no persistTabs().

/** Null-id, non-dormant agent panes that have actually been PROMPTED — the
 *  earliest point a transcript could exist for them to match against (#194
 *  BUG-1's same "never prompted → no transcript" fact, cited in the design
 *  note). Gated on `firstInputAt`, NOT `hasReceivedOutput` (review round 2,
 *  B2): a claude/copilot TUI produces output — its banner — within about a
 *  second of spawn, long before any transcript exists, so gating on output
 *  left a pane adoption-eligible for its entire idle-before-first-prompt
 *  lifetime with provably no transcript of its own in the scan — a window
 *  wide enough for an unrelated same-CLI/same-cwd session to be a sole,
 *  UNCONTESTED false match. `firstInputAt` narrows that to "has this pane
 *  actually been typed into," which is also the earliest a transcript search
 *  is worth running at all.
 *
 *  A `--fork-session` line is excluded outright (B3, same review round):
 *  such a line invalidates whatever id it's given on its very next resume,
 *  so it must stay unrecorded/dormant-eligible rather than risk acquiring an
 *  id that looks authoritative but silently discards itself later — see
 *  `Pane.hasForkSession`'s comment and the design note. Panes with no
 *  recognized CLI or no cwd yet can't be matched and are excluded before the
 *  caller even checks non-emptiness. */
function reconcileCandidates(): Pane[] {
  return tabs.tabs
    .flatMap((ws) => ws.grid.allPanes())
    .filter(
      (p) =>
        p.isAgentPane &&
        !p.isDormant &&
        p.sessionId === null &&
        p.agentCli !== null &&
        p.workdir !== null &&
        p.firstInputAt !== null &&
        p.ptyId !== null && // input can queue before the PTY attaches; the key below needs it
        !p.hasForkSession
    );
}

/** Session ids that must NOT be adopted onto a different pane: every live
 *  pane's own recorded id, plus every dormant placeholder's captured id (a
 *  session with no live pane is still "spoken for" — the D2 card, not this
 *  reconciler, is how a human reclaims it). */
function claimedSessionIds(): Set<string> {
  const ids = new Set<string>();
  for (const ws of tabs.tabs) {
    for (const p of ws.grid.allPanes()) {
      const id = p.isDormant ? p.restoreRecord?.sessionId : p.sessionId;
      if (id) ids.add(id);
    }
  }
  return ids;
}

function toRecords(list: readonly SessionInfo[]): SessionRecord[] {
  return list.map((s) => ({
    id: s.id,
    cli: s.source,
    cwd: s.cwd,
    modifiedMs: s.modified_ms,
    title: s.title,
    resumeCommand: s.resume_command,
  }));
}

let reconcileInFlight = false;
let lastReconcileAt = 0;
/** Floor between reconcile passes once one has run (#440 plan step 5: "at
 *  least 60s between scans") — a fresh `listSessions()` scan is real I/O
 *  (seconds on a long history, same cost the boot-path comment above
 *  documents), so a null-id pane sitting there mid-conversation shouldn't
 *  re-trigger it every tick of the periodic timer below. */
const RECONCILE_MIN_INTERVAL_MS = 60_000;

async function reconcileSessionIds(): Promise<void> {
  // Single-flight: the periodic timer and the one-shot post-prefetch call
  // below can otherwise overlap.
  if (reconcileInFlight) return;
  const candidates = reconcileCandidates();
  if (!candidates.length) return; // the common case, every boot after the first
  const now = Date.now();
  if (lastReconcileAt !== 0 && now - lastReconcileAt < RECONCILE_MIN_INTERVAL_MS) return;
  reconcileInFlight = true;
  lastReconcileAt = now;
  try {
    // sessions.refresh() is itself single-flight (RefreshGate) and IS the
    // scan #342 keeps off the boot path — safe here because this only ever
    // runs after boot (see the module comment above).
    await sessions.refresh();
    const records = toRecords(sessions.cached);
    const keyed = new Map<string, Pane>();
    const planPanes: ReconcilePane[] = [];
    for (const p of candidates) {
      // ptyId is guaranteed non-null here: `reconcileCandidates` filters on it
      // explicitly (input can queue and set firstInputAt slightly before the
      // PTY finishes attaching, so this can't be inferred from firstInputAt
      // alone — see that function's filter).
      const key = String(p.ptyId);
      keyed.set(key, p);
      planPanes.push({ key, cli: p.agentCli!, cwd: p.workdir!, eligibleSinceMs: p.firstInputAt! });
    }
    const adoptions = planSessionAdoption(planPanes, records, claimedSessionIds());
    if (!adoptions.length) return;
    for (const a of adoptions) keyed.get(a.key)?.adoptSessionId(a.sessionId);
    persistTabs();
  } finally {
    reconcileInFlight = false;
  }
}

// One-shot: as soon as the sidebar's own prefetch above resolves, take a
// first pass (near-instant on most boots — most panes are freshly spawned
// and haven't produced output yet, so `reconcileCandidates()` is empty and
// this returns immediately without a second scan). The periodic timer below
// covers every pane that only starts qualifying later.
void sessionsPrefetch.then(() => reconcileSessionIds());

// Recurring: cheap (an in-memory filter) on every tick unless a null-id
// agent pane with output actually exists, in which case it's still throttled
// to RECONCILE_MIN_INTERVAL_MS by the guard above. Interval < the floor so a
// pane that starts qualifying mid-window is picked up promptly rather than
// waiting a full extra minute.
setInterval(() => void reconcileSessionIds(), 20_000);

async function restoreSession(s: SessionInfo): Promise<void> {
  // Recorded orchestration sessions restore into their group — MCP identity,
  // badges, and task board included — instead of a powerless plain `--resume`.
  // The route is decided by RECORDED MEMBERSHIP alone (`sessionroute.ts`, #781):
  // this used to gate on `s.source === "claude"`, which silently demoted every
  // copilot orchestration session to a bare `--resume` the chip beside it
  // promised it would not be.
  const route = sessionRestoreRoute(s, sessions.roleFor(s));
  if (route.kind === "orchestration") {
    // Route a restored group into the tab that OWNS it, if one exists — a
    // persisted tab (its shell restored on boot) whose group binding survived,
    // or a tab already hosting that group this session. This is the real
    // persistence↔restore integration (#63): the group re-inhabits its own tab
    // through the resume machinery, not whatever tab happens to be active. Only
    // when no tab owns the group does it land in the active tab.
    const owning = tabs.workspaceForGroup(route.groupId);
    const ws = owning ?? tabs.activeWorkspace;
    if (owning && owning.id !== tabs.activeTabId) tabs.switchTo(owning.id);
    const hint = { group: route.groupId, role: route.role };
    // #412: the resume/rejoin machinery never opens a pane speculatively — a
    // failure throws BEFORE anything is spawned (see `resolve_worker_resume_cwd`
    // in the backend), so there is never a degraded plain pane to clean up
    // here; there is only "opened" or "didn't, with a reason". A tagged
    // not-found/workspace-missing failure gets an actionable choice instead of
    // just a fatal banner: retry as a FRESH session, reusing the same
    // recorded group/role/block/task brief the resume would have rejoined.
    const attempt = (startFresh: boolean) =>
      resumeOrchSession(ws.grid, eventsFor(ws), s.id, hint, startFresh);
    try {
      const restored = await attempt(false);
      // Bind the restored group to this tab so its rejoined workers spawn here
      // and focus/attention resolve here (#63); idempotent when the tab already
      // owned it. Pane lookups scan live panes, so there's no per-pty binding.
      if (restored) {
        tabs.bindGroup(restored.groupId, ws.id);
        persistTabs();
      }
    } catch (err) {
      const message = String(err);
      const kind = resumeFailureKind(message);
      if (!offersStartFresh(kind)) {
        showFatal(message);
        return;
      }
      // #412 rev-17 NB4: an orchestrator has no task brief to "keep the same" —
      // start-fresh re-boots its control plane on the group's EXISTING board/
      // roster/gate (never a re-read of the repo's workflow file — see
      // create_orchestration_group's Launch::Resume contract), a worker/
      // reviewer's start-fresh reuses its recorded task in a NEW worktree.
      const whatItDoes =
        route.role === "orchestrator"
          ? "Start a fresh orchestrator session instead? It reattaches to this group's existing board and roster."
          : "Start a fresh session instead, with the same task, in a new worktree?";
      const startFresh = await confirmModal(
        "Session not resumable",
        `${resumeFailureReason(kind)} ${whatItDoes}`,
        "Start fresh"
      );
      if (!startFresh) return;
      try {
        const restored = await attempt(true);
        if (restored) {
          tabs.bindGroup(restored.groupId, ws.id);
          persistTabs();
        }
      } catch (err2) {
        showFatal(String(err2));
      }
    }
    return;
  }
  // Plain (non-orchestration) sessions restore into the active tab. A row with
  // no recorded membership genuinely IS a plain session — it carries no chip
  // claiming otherwise — so this is the honest route, not a degraded one.
  const ws = tabs.activeWorkspace;
  const name = route.paneName;
  const pane = await ws.grid.openPane(
    // #440 D1c: pass the id we're already holding. Without this, a session
    // restored by hand from the Sessions sidebar came back DORMANT on the
    // NEXT boot anyway — the exact self-perpetuating trap the issue reports
    // (nothing here ever recorded the id, even though it drove this very
    // resume) — even though step 2's adoptableSessionId fallback would also
    // catch it via `--resume` on resume_command: certainty over inference.
    { name, cwd: s.cwd || undefined, command: s.resume_command, sessionId: s.id },
    eventsFor(ws),
    ws.grid.paneCount >= 2 ? "column" : "row"
  );
  // #456: only when `s.resume_command` actually carries `--autopilot` (the
  // backend appends it only for an unambiguous loomux-recorded ON posture —
  // see sessions.rs) is there any dialog for this watcher to answer. Gating
  // on the string avoids spinning up `confirmSoloCopilotAutopilot`'s up-to-
  // 10-minute poll for the common case (no record, or an explicit OFF/
  // ambiguous record) where it could only ever time out doing nothing.
  if (s.source === "copilot" && s.resume_command.includes("--autopilot") && pane.ptyId !== null) {
    void confirmSoloCopilotAutopilot(pane.ptyId, "copilot").catch(() => {
      /* best-effort — see confirmSoloCopilotAutopilot's own doc comment */
    });
  }
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
  // #407: loomux killed this process itself, mid-promotion, and is about to spawn
  // its replacement in the same terminal. `kill_pty` marks the exit expected,
  // which is exactly the shape `closeOrKeep` reads as "retire the pane" — so
  // without this the reaper would close the pane and take the conversation the
  // promotion exists to preserve with it.
  //
  // The flag stays up across the bind that follows the respawn, so this also
  // swallows an exit of the NEW pty in those few milliseconds (a `--resume` that
  // dies instantly) — no banner, and the pane still counts as live until the next
  // exit. Deliberate, and the safer direction of the two: the alternative is
  // `tryResumeFallback` respawning that pane FRESH, which is precisely the silent
  // discard of the promoted conversation the whole flow refuses to do. A promotion
  // that lands in that window reports through its own recovery note instead.
  if (pane.isRelaunching) return;
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
  const paneReports = tabs.tabs.flatMap((ws) => ws.bufferReports());
  // The side dock's editor (#1020 item 6) is the one buffer holder that is NOT
  // inside a pane, so walking tabs→panes cannot reach it. A quit that misses a
  // holder silently destroys it, which is the whole of #219 — so it is
  // concatenated here rather than left to be discovered.
  const dockReport = sideDock?.bufferReport();
  return dirtyBuffers(dockReport ? [...paneReports, dockReport] : paneReports);
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
    console.warn(`orrerix: final session save did not land within ${QUIT_FLUSH_TIMEOUT_MS}ms — quitting anyway`);
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
        body: "Quitting orrerix now discards them. Cancel, save what you want to keep, then quit again.",
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
      case "autosize-panes":
        activeGrid().autosize();
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
      case "move-tab-right":
        tabs.moveActiveTab(1);
        break;
      case "move-tab-left":
        tabs.moveActiveTab(-1);
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
      case "toggle-decisions":
        activeGrid().activePane?.toggleDecisionsView();
        break;
      case "toggle-audit":
        activeGrid().activePane?.toggleAuditView();
        break;
      case "toggle-timeline":
        activeGrid().activePane?.toggleTimelineView();
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
// Autosize (#936): even out every pane in the active tab. Same one-line call as
// the Ctrl+Shift+A path — the button is an affordance for it, not a second
// implementation of it.
document.getElementById("btn-autosize")!.addEventListener("click", () => activeGrid().autosize());
// The side dock (#1020 item 6). A button and no keyboard chord, deliberately: every
// remaining free chord has to clear the agent-cli-reference check first (a Ctrl+Shift+
// binding is withheld from every terminal pane, so taking one steals it from whatever
// CLI is running with no escape hatch), and that check is a doc read this change did not
// do. A dock nobody can toggle from the keyboard is a missing convenience; a dock that
// eats an agent's binding is a defect.
document.getElementById("btn-sidedock")!.addEventListener("click", () => sideDock?.toggle());

// Keep the browser from hijacking terminal-relevant defaults (Ctrl+F etc.
// stays inside the shell; F5/F7 reach TUI apps instead of the webview).
window.addEventListener("contextmenu", (e) => {
  if ((e.target as HTMLElement).closest(".pane-term")) e.preventDefault();
});

// WebView2 can come up without keyboard focus; make sure the active
// terminal reclaims it whenever the window is (re)focused — EXCEPT while a
// native dialog is outstanding (#1564). Answering a focus event by grabbing
// focus back is the app's only code that initiates a focus change from inside
// one, and a folder picker initializing on a foreign thread is calling
// `SetFocus` at that same instant; the tug-of-war re-enters WebView2's focus
// machinery from inside its own focus callback, which is where the #1564
// minidump faulted. The decision lives in nativedialog.ts so it is testable;
// this line is the DOM wiring.
window.addEventListener("focus", () =>
  reclaimFocusOnWindowFocus(() => activeGrid().activePane?.focus())
);

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
    if (el) el.textContent = `v${await hostVersion()}`;
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

// Take the backend's automatic model detection as it lands (#1020). The sweep
// starts in Tauri's `setup`, so it is already running by now — subscribing
// early is what decides whether an open picker learns its models by push or has
// to pull for them. Neither loses the answer; the push is just sooner.
startModelDetection();

// Let the shortcut hint bar scroll horizontally on a vertical wheel when it
// overflows a narrow window.
initHintBar();

// Orchestration is tab-aware (#63): spawns land in their group's tab (created on
// first sight), focus switches tab then pane, group-end closes the owning tab's
// panes, and attention badges hidden tabs' strip entries. The router
// (orchWiring) is implemented over the TabManager above. Wired before any
// orchestrator can launch (below), so no spawn event races an unready router.
initOrchestration(orchWiring);

// Load durable app settings (#370) — independent of the tab layer below, so
// it's its own boot IIFE rather than threaded through the restore flow. Any
// terminal keydown before this resolves reads settings.ts's DEFAULT_SETTINGS
// (module-level default), which is the safe direction — no real pane can spawn
// and take a keystroke before the seed tab below even exists, so in practice
// this always wins the race, but there's no hard ordering dependency either
// way. A first-run/quarantined-corrupt load (raw === null) seeds a fresh,
// discoverable settings.json with the defaults — config-file-only (no
// Settings UI exists in loomux yet) means "find the file and see what's in
// it" is the whole discovery story, so it should exist to be found.
void (async () => {
  try {
    const raw = await loadSettings();
    const decoded = decodeSettings(raw) ?? DEFAULT_SETTINGS;
    setSettings(decoded);
    // raw === null covers BOTH first run and a quarantined-corrupt file
    // (load_settings returns None for either, uistate.rs) — seed a fresh,
    // discoverable file in both cases. A THROWN load (the catch below) is
    // deliberately NOT treated the same way: that's a transient IPC failure,
    // not "nothing is there," and writing defaults over it could silently
    // reset a user's real settings.json to the default the next time this
    // races a hiccup.
    if (raw === null) void saveSettings(encodeSettings(decoded));
  } catch {
    /* best-effort — keep running on DEFAULT_SETTINGS (settings.ts's own
       module-level default); never write anything on this path. */
  }
})();

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
    // Restore no longer waits on a session-resumability precheck (#342). It used
    // to: listSessions() lists exactly the recorded sessions that still have a
    // transcript, so an id absent from it (never prompted / deleted) could
    // restore FRESH instead of attempting a doomed `--resume` (BUG-1). But that
    // list comes from a full scan of EVERY historical Claude/Copilot session file
    // on disk — on a machine with a long orchestration history (many past
    // projects, each accumulating session transcripts) that scan can itself take
    // seconds, and it gated opening even the first restored pane. `sessions.ts`'s
    // own sidebar prefetch (below, unawaited) already runs the identical scan in
    // the background, so nothing stops needing it — restoring just stops
    // BLOCKING on it. Every pane restores optimistically assuming its recorded
    // session id is resumable (`resumable` omitted → planPaneRestore's own
    // documented default), and the existing runtime backstop (tryResumeFallback,
    // BUG-1) already handles the one case that assumption gets wrong: a
    // `--resume` against a missing transcript exits immediately, and that pane
    // alone respawns fresh in place — instead of every OTHER pane's restore
    // waiting on a full-history scan to rule that out up front.
    await restoreSessionTabs(saved);
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

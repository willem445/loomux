// Group lifecycle panel for orchestrator panes: the human's at-a-glance view
// of a whole orchestration — how many agents are live, their roles, uptime,
// and running cost — plus the group-level controls that would otherwise mean
// ✕-clicking panes one by one: pause/resume (from #18's cost containment) and
// a destructive, confirmed "End orchestration" that kills every agent and can
// reclaim their worktrees, plus a live **max agents** stepper that adjusts the
// group's live-agent guardrail on the fly (backend persists + audits + tells the
// orchestrator to re-plan; lowering it never kills anyone). Read-through-poll
// like the audit viewer; the only writes are the explicit control actions. Same
// overlay mechanics as the git / tasks / audit views (never resizes the PTY).

import {
  autonomyState,
  endGroup,
  groupPaused,
  groupSummary,
  groupUsage,
  groupWatches,
  notifyEnabled,
  pauseGroup,
  resumeGroup,
  grantRelease,
  setAdvancedOrchestrator,
  setAutoMerge,
  setAutoRelease,
  setAutonomous,
  setAutonomyBudget,
  setDangerousMode,
  setIdleActivityFloor,
  setIdleTickMinutes,
  setMaxAgents,
  setNotify,
  setSpawnExpanded,
  spawnExpanded,
  workflowPreview,
  workflowStatus,
  type AutonomyState,
  type GroupSummary,
  type GroupUsage,
  type GroupWatch,
  type WorkflowStatus,
} from "./orchestration";
import { watchLine } from "./watchline";
import {
  approvalControl,
  autoMergeFromApproval,
  autoReleaseControl,
  dangerousControl,
  budgetMeter,
  formatTokens,
  isValidReleaseTag,
  normalizeComment,
  tickStatusLabel,
} from "./autonomy";
import { gateSatisfiabilityWarning, gateSummaryLine, workflowModeLabel } from "./workflowstatus";
import { compactionStatusLabel, compactionStatusTitle, contextUsageLabel } from "./compactionstatus";
import { roleLabel } from "./orchbadge";
import { getDefaultAgent } from "./agents";
import { confirmModal } from "./modal";

/** Hard bounds on the live-agent cap, mirroring the launcher's input range and
 *  the backend's `MAX_AGENTS_CEILING`. The backend re-validates; these only
 *  gate the stepper so an out-of-range click never round-trips. */
const MIN_MAX_AGENTS = 1;
const MAX_MAX_AGENTS = 12;

/** How often the panel re-polls the backend while open (uptime ticks, cost
 *  and roster drift). Matches the audit viewer's follow cadence. */
const POLL_MS = 2000;

/** Backend default idle-tick activity floor (bytes). Shown as the floor input's
 *  placeholder, and used to render the input blank when it's at the default
 *  (mirrors DEFAULT_IDLE_ACTIVITY_FLOOR_BYTES in the backend). */
const DEFAULT_ACTIVITY_FLOOR = 2048;

/** Roster height kept visible at the panel's minimum height — about two rows,
 *  so at the collapse floor the list is present (and scrolls) rather than gone,
 *  while the fixed chrome and footer stay fully rendered (#83 rev-58). */
const MIN_ROSTER_SLIVER = 48;

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

/** Compact token count: "845", "12K", "1.20M". Tokens are the reliable metric
 *  (subscription/Max accounts show $0.00 in the CLI regardless of usage). */
function fmtTokens(n: number): string {
  if (n < 1000) return `${n}`;
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}K`;
  return `${(n / 1_000_000).toFixed(2)}M`;
}

/** Format a dollar total with its basis label. Estimated/mixed totals get a
 *  `~` (they include price-table estimates); a purely CLI-`reported` total does
 *  not. `mixed` means the total blends estimated and reported figures. */
function costWithBasis(
  n: number,
  basis: "estimated" | "reported" | "mixed" | null
): string {
  const approx = basis === "reported" ? "" : "~";
  const label = basis ? ` ${basis === "estimated" ? "est" : basis}` : "";
  return `${approx}${fmtCost(n)}${label}`;
}

// Role chip text comes from orchbadge.ts — the same table the PANE badge reads, so
// a pane and its roster row can never label the same agent differently. This panel
// kept its own copy for a while and it silently missed `planner` (#47): every
// planner in the list showed a generic "AGENT" chip.

export class GroupView {
  readonly el: HTMLElement;
  private summaryEl: HTMLElement;
  private maxDecBtn: HTMLButtonElement;
  private maxIncBtn: HTMLButtonElement;
  private maxInput: HTMLInputElement;
  private maxNoteEl: HTMLElement;
  private maxErrEl: HTMLElement;
  private listEl: HTMLElement;
  // Autonomous-mode section (#83).
  private autoBtn: HTMLButtonElement;
  private approvalChk: HTMLInputElement;
  private autoReleaseChk: HTMLInputElement;
  private dangerousChk: HTMLInputElement;
  private budgetInput: HTMLInputElement;
  private budgetErrEl: HTMLElement;
  private meterEl: HTMLElement;
  private meterBar: HTMLElement;
  private meterFill: HTMLElement;
  private meterLabel: HTMLElement;
  private tickMinInput: HTMLInputElement;
  private tickMinErrEl: HTMLElement;
  private floorInput: HTMLInputElement;
  private tickStatusEl: HTMLElement;
  private suspendEl: HTMLElement;
  // Release-grant control (#83): collapsed power action.
  private releaseToggle: HTMLButtonElement;
  private releaseBody: HTMLElement;
  private releaseTagInput: HTMLInputElement;
  private releaseCommentInput: HTMLInputElement;
  private releaseBtn: HTMLButtonElement;
  private releaseErrEl: HTMLElement;
  private releaseOpen = false;
  /** True once Authorize is clicked with a valid tag: the second click within
   *  the window actually issues the release grant (publish is irreversible). */
  private releaseArmed = false;
  private releaseArmTimer: number | undefined;
  private pauseBtn: HTMLButtonElement;
  private notifyBtn: HTMLButtonElement;
  // Workflow-mode chrome (#316): active workflow + armed gate, and the live
  // advanced-orchestrator toggle.
  private workflowRow: HTMLElement;
  private workflowLineEl: HTMLElement;
  private workflowWarnEl: HTMLElement;
  private workflowToggleBtn: HTMLButtonElement;
  private workflow: WorkflowStatus | null = null;
  private workflowBusy = false;
  /** #260: toggles whether newly spawned worker/reviewer/planner panes open
   *  docked to the minimize tray (the default) or expanded into the split
   *  tree (the pre-#260 behavior) — backend-persisted per group. */
  private dockBtn: HTMLButtonElement;
  private foldBtn: HTMLButtonElement | null = null;
  private endBtn: HTMLButtonElement;
  private cleanupChk: HTMLInputElement;
  private toastEl: HTMLElement;
  private toastTimer: number | undefined;

  private summary: GroupSummary | null = null;
  private usage: GroupUsage | null = null;
  /** Live CI watches across the group's agents (#248), refreshed on the same
   *  poll cadence as summary/usage below — no separate timer. */
  private watches: GroupWatch[] = [];
  private paused = false;
  private notify = false;
  /** #260: true once the group opted OUT of the minimize-on-spawn default
   *  (i.e. wants panes to keep opening expanded, like before #260). */
  private spawnExpandedFlag = false;
  private autonomy: AutonomyState | null = null;
  private pollTimer: number | undefined;
  private disposed = false;
  /** True once End is clicked once: the second click within the window
   *  actually tears the group down (two-step confirm for a destructive op). */
  private endArmed = false;
  private endArmTimer: number | undefined;

  /** Notified after each render (content height may have changed — e.g. the
   *  suspended banner appeared), so the host can re-apply the overlay height
   *  clamp and keep every control on-screen. */
  private onResize?: () => void;
  private getRepo?: () => string | null;
  private embedBtn: HTMLButtonElement;

  constructor(
    private groupId: string,
    opts: {
      onClose: () => void;
      onToggleMinimize?: () => void;
      onResize?: () => void;
      /** The group's repo path (its orchestrator pane's cwd), for the
       *  workflow-toggle confirm's roster preview. `undefined`/`null` just
       *  degrades that preview to a generic description — the toggle itself
       *  doesn't need it (the backend resolves the repo from the group). */
      getRepo?: () => string | null;
      onToggleEmbed?: () => void;
    }
  ) {
    this.onResize = opts.onResize;
    this.getRepo = opts.getRepo;
    this.el = el("div", "group-view");

    const head = el("div", "group-head");
    head.append(el("span", "group-title", "orchestration"));
    head.append(el("span", "group-group", groupId));
    const refresh = el("button", "pane-btn", "⟳") as HTMLButtonElement;
    refresh.title = "Refresh";
    refresh.addEventListener("click", () => void this.load());
    head.append(refresh);
    // Embed toggle (#361): switch between the floating overlay and the
    // pane's embed-panel slot.
    this.embedBtn = el("button", "pane-btn embed", "⬒") as HTMLButtonElement;
    this.embedBtn.addEventListener("click", () => opts.onToggleEmbed?.());
    head.append(this.embedBtn);
    this.setPanelActive(false);
    const close = el("button", "pane-btn close", "✕") as HTMLButtonElement;
    close.title = "Close (Alt+O)";
    close.addEventListener("click", opts.onClose);
    head.append(close);

    this.summaryEl = el("div", "group-summary");

    // Max live-agent cap: adjustable on the fly. Stepper + direct input, wired
    // to the guardrail command; the backend persists, audits, and tells the
    // orchestrator to re-plan. Lowering below the live count kills no one — new
    // spawns just wait for attrition (see the note line + control tooltip).
    const maxRow = el("div", "group-maxrow");
    const maxCtl = el("div", "group-max");
    maxCtl.title =
      "Max live workers + reviewers + planners (the orchestrator is exempt). Lowering it below " +
      "the current live count never kills anyone — new spawns are blocked until agents finish.";
    maxCtl.append(el("span", "group-max-label", "Max live agents"));
    this.maxDecBtn = el("button", "group-max-step", "−") as HTMLButtonElement;
    this.maxDecBtn.title = "Lower the cap";
    this.maxDecBtn.addEventListener("click", () => void this.nudgeMax(-1));
    this.maxInput = document.createElement("input");
    this.maxInput.className = "group-max-input";
    this.maxInput.type = "number";
    this.maxInput.min = String(MIN_MAX_AGENTS);
    this.maxInput.max = String(MAX_MAX_AGENTS);
    this.maxInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") void this.applyMax(parseInt(this.maxInput.value, 10));
    });
    this.maxInput.addEventListener("blur", () => void this.applyMax(parseInt(this.maxInput.value, 10)));
    this.maxIncBtn = el("button", "group-max-step", "+") as HTMLButtonElement;
    this.maxIncBtn.title = "Raise the cap";
    this.maxIncBtn.addEventListener("click", () => void this.nudgeMax(1));
    maxCtl.append(this.maxDecBtn, this.maxInput, this.maxIncBtn);
    this.maxNoteEl = el("span", "group-max-note");
    this.maxErrEl = el("span", "group-max-err");
    maxRow.append(maxCtl, this.maxNoteEl, this.maxErrEl);

    // Workflow-mode chrome (#316): whether this group is on the built-in
    // roster or a repo-declared custom workflow, and the merge gate armed for
    // THIS session — named here, next to the roster/cap controls, so it's
    // visible before an Approve or a merge ever bounces off it (see the task
    // board's own gate-aware Approve label, tasksview.ts). The toggle itself
    // lives in the button row below, next to Pause/Notify.
    this.workflowRow = el("div", "group-workflow-row");
    this.workflowLineEl = el("span", "group-workflow-line");
    this.workflowWarnEl = el("div", "group-workflow-warn");
    this.workflowWarnEl.hidden = true;
    this.workflowRow.append(this.workflowLineEl, this.workflowWarnEl);

    this.listEl = el("div", "group-list");

    // Autonomous-mode section (#83): two dense rows matching the max-agents
    // row's density (finding 1). Row A = label + live toggle; the "spends money
    // unattended" caveat is folded into the section tooltip, not its own line.
    // Row B = merge gate + token budget + an inline meter. The suspended banner
    // is a third row shown only while the budget enforcer has it paused. Every
    // state stays visible — compressed, never hidden (it's the consent surface).
    const autoRow = el("div", "group-autorow");
    autoRow.title =
      "Autonomous ticks poll labeled issues and re-check PRs while you're away — " +
      "they spend tokens without you present.";

    const autoHead = el("div", "group-auto-head");
    autoHead.append(el("span", "group-auto-title", "Autonomous mode"));
    this.autoBtn = el("button", "group-btn sm", "🤖 Off") as HTMLButtonElement;
    this.autoBtn.addEventListener("click", () => void this.toggleAutonomous());
    autoHead.append(this.autoBtn);

    // Row B: merge gate + budget + inline meter, wrapping if the panel is narrow.
    const ctlRow = el("div", "group-auto-controls");

    // Merge gate: the checkbox is the human's framing (ON = require approval,
    // today's default) and maps to the inverse backend auto_merge flag.
    const approvalLbl = el("label", "group-auto-check") as HTMLLabelElement;
    this.approvalChk = document.createElement("input");
    this.approvalChk.type = "checkbox";
    // Consent surface must never show the unsafe direction: start checked
    // (approval required) so pre-load / a failed autonomyState read renders
    // auto-merge as OFF, matching the backend default.
    this.approvalChk.checked = true;
    this.approvalChk.addEventListener("change", () => void this.toggleApproval());
    approvalLbl.append(
      this.approvalChk,
      document.createTextNode(" Require human approval before merge")
    );
    approvalLbl.title =
      "On (default): the human merges every PR. Off: the orchestrator may merge an " +
      "adequately-tested PR (reviewer-approved + green CI) itself while autonomous.";

    // Auto-release: a POSITIVE checkbox (checked = orchestrator may publish
    // releases/tags itself). Sibling of the merge gate, same dependency — only
    // usable while autonomous (the backend rejects enabling it otherwise).
    const releaseLbl = el("label", "group-auto-check") as HTMLLabelElement;
    this.autoReleaseChk = document.createElement("input");
    this.autoReleaseChk.type = "checkbox";
    this.autoReleaseChk.checked = false; // safe default: releases need approval
    this.autoReleaseChk.disabled = true; // until a status read shows autonomous on
    this.autoReleaseChk.addEventListener("change", () => void this.toggleAutoRelease());
    releaseLbl.append(this.autoReleaseChk, document.createTextNode(" Auto-release"));
    releaseLbl.title =
      "Off (default): publishing a release/tag needs an explicit human grant. On: the " +
      "orchestrator may run `gh release` / push a v* tag itself while autonomous.";

    // Dangerous mode: a DANGER-styled toggle for supervised, NOT-autonomous work.
    // Lets agents merge & release without per-item approval while you watch.
    // Mutually exclusive with autonomous (only usable while autonomous is OFF).
    const dangerLbl = el("label", "group-auto-check danger") as HTMLLabelElement;
    this.dangerousChk = document.createElement("input");
    this.dangerousChk.type = "checkbox";
    this.dangerousChk.checked = false;
    this.dangerousChk.addEventListener("change", () => void this.toggleDangerous());
    dangerLbl.append(this.dangerousChk, document.createTextNode(" ⚠ Dangerous mode"));
    dangerLbl.title =
      "SUPERVISED: while you're here (and NOT in autonomous mode), let agents merge to the " +
      "default branch and publish releases/tags themselves — no per-item approval. Every " +
      "action is audited. Enabling Autonomous clears this (they're mutually exclusive).";

    // Token budget: 0 / empty = no cap. When autonomous is on this drives the
    // inline meter beside it.
    const budgetWrap = el("div", "group-auto-budget");
    budgetWrap.append(el("span", "group-auto-blabel", "Budget"));
    this.budgetInput = document.createElement("input");
    this.budgetInput.className = "group-auto-binput";
    this.budgetInput.type = "number";
    this.budgetInput.min = "0";
    this.budgetInput.step = "10000";
    this.budgetInput.placeholder = "no cap";
    this.budgetInput.title =
      "Autonomous-era token spend cap (0 or empty = no cap). Metered from the moment " +
      "you enable autonomous mode; crossing it suspends ticking until you re-enable.";
    this.budgetInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") void this.applyBudget();
    });
    this.budgetInput.addEventListener("blur", () => void this.applyBudget());
    this.budgetErrEl = el("span", "group-auto-berr");
    budgetWrap.append(this.budgetInput, this.budgetErrEl);

    // Inline meter (shown only while autonomous is on): a slim bar + a compact
    // "X / Y · Z%" (or "X · no cap") read of spend-since-enable vs the budget.
    this.meterEl = el("div", "group-auto-meter");
    this.meterEl.hidden = true;
    this.meterBar = el("div", "group-auto-bar");
    this.meterFill = el("div", "group-auto-fill");
    this.meterBar.append(this.meterFill);
    this.meterLabel = el("span", "group-auto-mlabel");
    this.meterEl.append(this.meterBar, this.meterLabel);

    ctlRow.append(approvalLbl, releaseLbl, dangerLbl, budgetWrap, this.meterEl);

    // Row C (slim): idle-tick cadence knob + a power-user activity-floor knob +
    // the live tick-status line. The knobs configure the tick even while off
    // (set-then-enable); the status text only appears once autonomous is on.
    const tickRow = el("div", "group-auto-tick");
    const tickWrap = el("label", "group-auto-tickwrap") as HTMLLabelElement;
    tickWrap.title =
      "How long the orchestrator's pane must be output-quiet before loomux delivers one " +
      "idle tick (poll labeled issues, re-check PRs). Default 5 min; min 1.";
    tickWrap.append(el("span", "group-auto-blabel", "Idle tick"));
    this.tickMinInput = document.createElement("input");
    this.tickMinInput.className = "group-auto-binput sm";
    this.tickMinInput.type = "number";
    this.tickMinInput.min = "1";
    this.tickMinInput.max = "1440";
    this.tickMinInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") void this.applyTickMinutes();
    });
    this.tickMinInput.addEventListener("blur", () => void this.applyTickMinutes());
    tickWrap.append(this.tickMinInput, el("span", "group-auto-unit", "min"));
    this.tickMinErrEl = el("span", "group-auto-berr");

    // Activity floor (power-user): output below this many bytes per interval is
    // treated as idle, so CLI repaints/spinners don't reset the quiet clock.
    const floorWrap = el("label", "group-auto-tickwrap") as HTMLLabelElement;
    floorWrap.title =
      "Advanced: bytes of pane output per interval below which the orchestrator counts as " +
      "idle. Makes the quiet clock tolerant of repaint/spinner noise. Default 2048; 0 resets it.";
    floorWrap.append(el("span", "group-auto-blabel", "· floor"));
    this.floorInput = document.createElement("input");
    this.floorInput.className = "group-auto-binput sm";
    this.floorInput.type = "number";
    this.floorInput.min = "0";
    this.floorInput.step = "512";
    this.floorInput.placeholder = String(DEFAULT_ACTIVITY_FLOOR);
    this.floorInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") void this.applyFloor();
    });
    this.floorInput.addEventListener("blur", () => void this.applyFloor());
    floorWrap.append(this.floorInput, el("span", "group-auto-unit", "B"));

    this.tickStatusEl = el("span", "group-auto-status");
    tickRow.append(tickWrap, this.tickMinErrEl, floorWrap, this.tickStatusEl);

    // Budget-exhausted banner (autonomy auto-suspended): distinct row + the
    // re-enable affordance is the toggle above (re-enabling re-anchors).
    this.suspendEl = el("div", "group-auto-suspend");
    this.suspendEl.hidden = true;

    autoRow.append(autoHead, ctlRow, tickRow, this.suspendEl);

    // Release-grant control (#83): a collapsed power action — releases have no
    // board task, so this is the human path to authorize one. Kept collapsed by
    // default; the copy is blunt that it publishes.
    const releaseRow = el("div", "group-releaserow");
    this.releaseToggle = el("button", "group-release-toggle", "▸ Authorize a release…") as HTMLButtonElement;
    this.releaseToggle.title =
      "Authorize a one-time release/tag publish (GH release + npm). Releases are never " +
      "auto-approved by autonomous mode — this explicit grant is the only path.";
    this.releaseToggle.addEventListener("click", () => this.toggleRelease());

    this.releaseBody = el("div", "group-release-body");
    this.releaseBody.hidden = true;
    this.releaseBody.append(
      el(
        "div",
        "group-release-copy",
        "Authorizes ONE publish of this tag (GH release + npm) — single-use, expires in ~30 min. " +
          "Releases are never auto-approved by autonomous mode."
      )
    );
    const releaseInputs = el("div", "group-release-inputs");
    this.releaseTagInput = document.createElement("input");
    this.releaseTagInput.className = "group-release-tag";
    this.releaseTagInput.placeholder = "tag — e.g. v1.2.3";
    this.releaseTagInput.spellcheck = false;
    this.releaseTagInput.addEventListener("input", () => this.disarmRelease());
    this.releaseTagInput.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Enter") this.onReleaseClick();
    });
    this.releaseCommentInput = document.createElement("input");
    this.releaseCommentInput.className = "group-release-comment";
    this.releaseCommentInput.placeholder = "optional instructions for the agent";
    this.releaseCommentInput.spellcheck = false;
    this.releaseCommentInput.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Enter") this.onReleaseClick();
    });
    this.releaseBtn = el("button", "group-btn sm", "Authorize") as HTMLButtonElement;
    this.releaseBtn.addEventListener("click", () => this.onReleaseClick());
    releaseInputs.append(this.releaseTagInput, this.releaseCommentInput, this.releaseBtn);
    this.releaseErrEl = el("div", "group-release-err");
    this.releaseBody.append(releaseInputs, this.releaseErrEl);
    releaseRow.append(this.releaseToggle, this.releaseBody);

    // Footer: pause/resume + destructive end-orchestration.
    const foot = el("div", "group-actions");
    this.pauseBtn = el("button", "group-btn", "Pause") as HTMLButtonElement;
    this.pauseBtn.addEventListener("click", () => void this.togglePause());

    // Desktop-notification opt-in: OS toasts for report/blocked/attention
    // events in this group (idle-with-prompt, worker reports). Per-group.
    this.notifyBtn = el("button", "group-btn", "🔔 Notify") as HTMLButtonElement;
    this.notifyBtn.addEventListener("click", () => void this.toggleNotify());

    // LIVE advanced-orchestrator toggle (#316): flips the workflow row above.
    // A human action, confirmed via modal (the consent moment) before it
    // actually arms/clears the gate and swaps the roster for future spawns.
    this.workflowToggleBtn = el("button", "group-btn", "Workflow: off") as HTMLButtonElement;
    this.workflowToggleBtn.addEventListener("click", () => void this.toggleWorkflow());

    // Auto-dock toggle (#260): whether newly spawned delegate panes open
    // minimized to the tray (default) or expanded into the split tree.
    // No icon glyph — 🗕 (U+1F5D5 SCREEN) was tried first, but it's an
    // obscure Supplementary-Plane pictograph outside the widely-supported
    // "RGI" emoji set; Windows' Segoe UI Emoji doesn't cover it and the
    // fallback glyph reads as a stray underscore before the label (live-test
    // report). Plain text instead, matching the Fold-panes button just below
    // (also iconless) rather than gambling on another emoji's font coverage.
    this.dockBtn = el("button", "group-btn", "Auto-dock") as HTMLButtonElement;
    this.dockBtn.addEventListener("click", () => void this.toggleSpawnExpanded());

    // Fold-group toggle (#46), mirroring the orchestrator header button:
    // minimize every worker/reviewer pane to the dock at once, or restore them.
    if (opts.onToggleMinimize) {
      this.foldBtn = el("button", "group-btn", "Fold panes") as HTMLButtonElement;
      this.foldBtn.title =
        "Minimize all worker/reviewer panes to the dock (or restore them if already minimized)";
      this.foldBtn.addEventListener("click", () => opts.onToggleMinimize!());
    }

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

    foot.append(this.pauseBtn, this.notifyBtn, this.workflowToggleBtn, this.dockBtn);
    if (this.foldBtn) foot.append(this.foldBtn);
    foot.append(endWrap);

    this.toastEl = el("div", "git-toast");
    this.toastEl.hidden = true;

    this.el.append(
      head,
      this.summaryEl,
      maxRow,
      this.workflowRow,
      autoRow,
      releaseRow,
      this.listEl,
      foot,
      this.toastEl
    );
  }

  /** Called by the pane whenever the view is (re)opened, in either mode. */
  show(): void {
    void this.load();
    this.pollTimer = window.setInterval(() => void this.load(), POLL_MS);
  }

  /** Reflect whether the pane currently has this view in its embed-panel
   *  slot (#361) — pure display state on the header's toggle button. */
  setPanelActive(active: boolean): void {
    this.embedBtn.classList.toggle("active", active);
    this.embedBtn.textContent = active ? "⬓" : "⬒";
    this.embedBtn.title = active
      ? "Un-embed — back to a floating overlay"
      : "Embed beside the terminal (resizes this pane)";
  }

  dispose(): void {
    this.disposed = true;
    clearTimeout(this.toastTimer);
    clearTimeout(this.endArmTimer);
    clearTimeout(this.releaseArmTimer);
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
      [
        this.summary,
        this.usage,
        this.paused,
        this.notify,
        this.spawnExpandedFlag,
        this.autonomy,
        this.watches,
        this.workflow,
      ] = await Promise.all([
        groupSummary(this.groupId),
        groupUsage(this.groupId),
        groupPaused(this.groupId),
        notifyEnabled(this.groupId),
        spawnExpanded(this.groupId),
        autonomyState(this.groupId),
        groupWatches(this.groupId),
        workflowStatus(this.groupId),
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

  /** LIVE advanced-orchestrator toggle (#316). A confirm modal is the consent
   *  moment (the human sees the resolved roster/gate — or the toggle-off
   *  restore note — before it takes effect); the actual arm/clear + roster
   *  swap happens backend-side in `setAdvancedOrchestrator`. Refusals (e.g. a
   *  broken workflow.yml) surface as a toast, same as every other control
   *  here. */
  private async toggleWorkflow(): Promise<void> {
    if (this.workflowBusy) return;
    const turningOn = !(this.workflow?.advanced ?? false);
    const body = turningOn
      ? await this.previewWorkflowOnBody()
      : "Clears the armed merge gate and returns future spawns to the built-in roster on your " +
        "default CLI (per-role CLI overrides from launch aren't preserved). Agents already " +
        "running keep the block they were spawned under.";
    const ok = await confirmModal(
      turningOn ? "Turn on workflow mode?" : "Turn off workflow mode?",
      body,
      turningOn ? "Turn on" : "Turn off"
    );
    if (!ok) return;
    this.workflowBusy = true;
    try {
      await setAdvancedOrchestrator(this.groupId, turningOn);
    } catch (err) {
      this.toast(String(err));
    }
    this.workflowBusy = false;
    await this.load();
  }

  /** What turning workflow mode ON would resolve to, for the confirm modal —
   *  a best-effort preview (`workflowPreview`), not the authoritative read
   *  (the toggle's own backend call re-resolves for real; this can only
   *  degrade to a generic description, never block the toggle). Uses the
   *  launcher's own last-picked default CLI as the preview's `agentCli`: the
   *  group's actual default isn't separately retained today (same reason
   *  toggle-off can't restore per-role CLI overrides — see the confirm body
   *  above), so this is the same stand-in the launcher itself falls back to. */
  private async previewWorkflowOnBody(): Promise<string> {
    const repo = this.getRepo?.() ?? null;
    if (!repo) {
      return (
        "Switches future spawns to this repo's declared workflow (.loomux/workflow.yml) and " +
        "arms any merge gate it declares. Agents already running keep their current block."
      );
    }
    const preview = await workflowPreview(repo, getDefaultAgent().id).catch(() => null);
    if (!preview || !preview.present) {
      return `No .loomux/workflow.yml found at ${repo} — turning workflow mode on will be refused.`;
    }
    if (!preview.valid) {
      return (
        `${preview.path} is present but invalid — turning workflow mode on will be refused:\n` +
        preview.errors.join("\n")
      );
    }
    const blocks = preview.blocks.map((b) => `${b.id} (${b.kind}, ${b.cli})`).join(", ");
    const gate = preview.gates.length ? ` Declares gate(s): ${preview.gates.join(", ")}.` : "";
    return (
      `"${preview.name || preview.path}" — ${preview.blocks.length} block` +
      `${preview.blocks.length === 1 ? "" : "s"}: ${blocks}.${gate} Agents already running keep ` +
      "their current block; future spawns use this roster."
    );
  }

  /** Step the cap by ±1 from the current backend value. */
  private nudgeMax(delta: number): void {
    const cur = this.summary?.max_agents;
    if (cur == null) return;
    void this.applyMax(cur + delta);
  }

  /** Commit a new cap. The backend bounds-checks, persists, and audits each
   *  click immediately, then debounces a single re-plan notice to the
   *  orchestrator so rapid stepping is one prompt, not many (#79); on rejection
   *  we surface the reason inline and the poll restores the input to the real
   *  value. */
  private async applyMax(n: number): Promise<void> {
    this.maxErrEl.textContent = "";
    if (!Number.isFinite(n)) {
      await this.load(); // restore the input from a non-numeric entry
      return;
    }
    // Skip a no-op (the backend treats it as one too, but this avoids a
    // needless round-trip on every input blur).
    if (n === this.summary?.max_agents) return;
    try {
      await setMaxAgents(this.groupId, n);
    } catch (err) {
      this.maxErrEl.textContent = String(err);
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

  /** Flip the #260 minimize-on-spawn default for this group. */
  private async toggleSpawnExpanded(): Promise<void> {
    try {
      await setSpawnExpanded(this.groupId, !this.spawnExpandedFlag);
    } catch (err) {
      this.toast(String(err));
    }
    await this.load();
  }

  /** Flip autonomous idle-ticking. Enabling (including re-enabling after a
   *  budget suspension) re-anchors the budget meter backend-side. */
  private async toggleAutonomous(): Promise<void> {
    const on = this.autonomy?.autonomous ?? false;
    try {
      await setAutonomous(this.groupId, !on);
    } catch (err) {
      this.toast(String(err));
    }
    await this.load();
  }

  /** Commit the merge gate from the "require approval" checkbox — the human
   *  framing is the inverse of the backend `auto_merge` flag. */
  private async toggleApproval(): Promise<void> {
    try {
      await setAutoMerge(this.groupId, autoMergeFromApproval(this.approvalChk.checked));
    } catch (err) {
      this.toast(String(err));
    }
    await this.load();
  }

  /** Commit the auto-release gate (positive checkbox = auto_release ON). A rejected
   *  write (e.g. autonomous off) toasts and the poll re-syncs the real state. */
  private async toggleAutoRelease(): Promise<void> {
    try {
      await setAutoRelease(this.groupId, this.autoReleaseChk.checked);
    } catch (err) {
      this.toast(String(err));
    }
    await this.load();
  }

  /** Commit supervised dangerous mode (positive checkbox = dangerous_mode ON).
   *  Rejected while autonomous is on (mutually exclusive); toast + re-sync. */
  private async toggleDangerous(): Promise<void> {
    try {
      await setDangerousMode(this.groupId, this.dangerousChk.checked);
    } catch (err) {
      this.toast(String(err));
    }
    await this.load();
  }

  /** Commit the token budget (empty/non-numeric = no cap = 0). The backend
   *  clamps/persists and returns the applied value; the poll then re-syncs the
   *  input, so a rejected write restores the real value. */
  private async applyBudget(): Promise<void> {
    this.budgetErrEl.textContent = "";
    const raw = this.budgetInput.value.trim();
    const n = raw === "" ? 0 : parseInt(raw, 10);
    const tokens = Number.isFinite(n) && n > 0 ? n : 0;
    if (tokens === (this.autonomy?.budget_tokens ?? 0)) return; // no-op
    try {
      await setAutonomyBudget(this.groupId, tokens);
    } catch (err) {
      this.budgetErrEl.textContent = String(err);
    }
    await this.load();
  }

  /** Commit the idle-tick window (empty/non-numeric → 0, which the backend maps
   *  to its default). The backend clamps/persists and returns the applied value;
   *  the poll then re-syncs the input from the response. */
  private async applyTickMinutes(): Promise<void> {
    this.tickMinErrEl.textContent = "";
    const raw = this.tickMinInput.value.trim();
    const n = raw === "" ? 0 : parseInt(raw, 10);
    const minutes = Number.isFinite(n) && n > 0 ? n : 0;
    if (minutes === (this.autonomy?.idle_tick_minutes ?? 0)) return; // no-op
    try {
      await setIdleTickMinutes(this.groupId, minutes);
    } catch (err) {
      this.tickMinErrEl.textContent = String(err);
    }
    await this.load();
  }

  /** Commit the activity floor (empty/non-numeric → 0 = reset to the backend
   *  default). Backend clamps/persists and returns the applied value. */
  private async applyFloor(): Promise<void> {
    const raw = this.floorInput.value.trim();
    const n = raw === "" ? 0 : parseInt(raw, 10);
    const bytes = Number.isFinite(n) && n > 0 ? n : 0;
    // A blank input means "default"; skip the round-trip when already at default.
    const cur = this.autonomy?.idle_activity_floor_bytes ?? DEFAULT_ACTIVITY_FLOOR;
    if (bytes === cur || (bytes === 0 && cur === DEFAULT_ACTIVITY_FLOOR)) return;
    try {
      await setIdleActivityFloor(this.groupId, bytes);
    } catch (err) {
      this.toast(String(err));
    }
    await this.load();
  }

  /** Expand/collapse the release-grant control. */
  private toggleRelease(): void {
    this.releaseOpen = !this.releaseOpen;
    this.releaseBody.hidden = !this.releaseOpen;
    this.releaseToggle.textContent = this.releaseOpen
      ? "▾ Authorize a release"
      : "▸ Authorize a release…";
    if (!this.releaseOpen) this.disarmRelease();
    else this.releaseTagInput.focus();
    // Height changed — let the host re-clamp so no control clips (#83 rev-58).
    this.onResize?.();
  }

  /** First click validates the tag and arms; the second within the window
   *  actually issues the grant (a release publish is irreversible). */
  private onReleaseClick(): void {
    this.releaseErrEl.textContent = "";
    const tag = this.releaseTagInput.value.trim();
    if (!isValidReleaseTag(tag)) {
      this.releaseErrEl.textContent = "enter a tag with no spaces — e.g. v1.2.3";
      this.releaseTagInput.focus();
      return;
    }
    if (!this.releaseArmed) {
      this.releaseArmed = true;
      this.releaseBtn.textContent = `Publish ${tag}?`;
      this.releaseBtn.classList.add("armed");
      this.releaseArmTimer = window.setTimeout(() => this.disarmRelease(), 4000);
      return;
    }
    this.disarmRelease();
    void this.doGrantRelease(tag, this.releaseCommentInput.value);
  }

  private disarmRelease(): void {
    this.releaseArmed = false;
    clearTimeout(this.releaseArmTimer);
    this.releaseBtn.textContent = "Authorize";
    this.releaseBtn.classList.remove("armed");
  }

  /** Issue the one-time release grant. On success, collapse and clear so the
   *  control can't be re-fired by a stray click; a failure surfaces inline. */
  private async doGrantRelease(tag: string, comment: string): Promise<void> {
    this.releaseBtn.disabled = true;
    try {
      await grantRelease(this.groupId, tag, normalizeComment(comment));
      this.toast(`release authorized: ${tag} (one-time, ~30 min)`);
      this.releaseTagInput.value = "";
      this.releaseCommentInput.value = "";
      this.releaseOpen = false;
      this.releaseBody.hidden = true;
      this.releaseToggle.textContent = "▸ Authorize a release…";
      this.onResize?.();
    } catch (err) {
      this.releaseErrEl.textContent = String(err);
    } finally {
      this.releaseBtn.disabled = false;
    }
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
      s.roles.planner ? `${s.roles.planner} planner${s.roles.planner > 1 ? "s" : ""}` : "",
    ].filter(Boolean);
    const line = el(
      "div",
      "group-line",
      `${s.live_agents} agent${s.live_agents === 1 ? "" : "s"} live` +
        (roleBits.length ? ` · ${roleBits.join(", ")}` : "") +
        ` · up ${fmtUptime(s.uptime_ms)}`
    );
    this.summaryEl.append(line);

    // Cost line: tokens are the honest metric (exact, and non-zero even on
    // Max plans where the CLI reports $0.00); dollars are a labelled estimate.
    // Lifetime includes killed/recycled agents; live is the current burn.
    const u = this.usage;
    const lifetimeCost = u?.lifetime_cost_usd ?? null;
    const parts: string[] = [`${fmtTokens(u?.lifetime_tokens ?? 0)} tok`];
    if (lifetimeCost != null) parts.unshift(costWithBasis(lifetimeCost, u?.lifetime_cost_basis ?? null));
    const cost = el("div", "group-cost", `group lifetime cost — ${parts.join(" · ")}`);
    cost.title =
      "Tokens come from each agent's session transcript and are exact. Dollars are estimated from a dated model price table — subscription/Max accounts show $0.00 in the CLI regardless of usage, so tokens are the reliable metric. 'reported' = the CLI's own figure; 'mixed' = a blend of both. Lifetime includes killed/recycled agents.";
    this.summaryEl.append(cost);

    // Live burn (current agents only), shown when it differs from lifetime.
    const liveCost = u?.live_cost_usd ?? null;
    const liveTok = u?.live_tokens ?? 0;
    const liveParts: string[] = [`${fmtTokens(liveTok)} tok`];
    if (liveCost != null) liveParts.unshift(costWithBasis(liveCost, u?.live_cost_basis ?? null));
    const live = el("div", "group-cost-live", `live — ${liveParts.join(" · ")}`);
    this.summaryEl.append(live);

    if (s.paused) this.summaryEl.append(el("span", "group-paused-badge", "paused"));

    this.renderMax(s);

    // Per-agent rows: role chip, name, uptime, state, cost.
    this.listEl.replaceChildren();
    if (s.agents.length === 0) {
      this.listEl.append(el("div", "group-empty", "No live agents in this group."));
    } else {
      const usageOf = new Map(this.usage?.agents.map((a) => [a.id, a] as const));
      for (const a of s.agents) {
        const wrap = el("div", "group-agent");
        const row = el("div", "group-row");
        const chip = el("span", `group-role role-${a.role}`, roleLabel(a.role));
        const name = el("span", "group-name", a.name);
        name.title = a.id;
        // A workflow group's agents are BLOCKS (#222). Three reviewers all badged
        // "REV" is exactly the ambiguity declaring them separately was meant to
        // remove, so name the block beside the chip. For the built-in roster a
        // block id IS its role name, so a default group's rows gain nothing and
        // look exactly as they did.
        const block =
          a.block && a.block !== a.role ? el("span", "group-block", a.block) : null;
        if (block) block.title = `workflow block ${a.block}`;
        const state = el(
          "span",
          "group-state",
          a.idle_since_ms != null ? `idle ${fmtUptime(Date.now() - a.idle_since_ms)}` : a.task ? "working" : "ready"
        );
        if (a.task) state.title = a.task;
        const up = el("span", "group-uptime", fmtUptime(a.uptime_ms));

        // Tokens first (always trustworthy), then the dollar figure with a
        // reported/estimated marker so a $0.00 Max-plan figure isn't mistaken
        // for "no usage".
        const usage = usageOf.get(a.id);
        const tok = usage ? `${fmtTokens(usage.tokens.total)} tok` : "";
        const c = el("span", "group-agent-cost");
        if (usage && usage.cost_usd != null) {
          const mark = usage.estimated ? "~" : "";
          const label = usage.estimated ? "est" : "reported";
          c.textContent = `${mark}${fmtCost(usage.cost_usd)} ${label}${tok ? ` · ${tok}` : ""}`;
        } else {
          c.textContent = tok || "—";
        }
        if (usage) {
          c.title = `source: ${usage.source}${usage.model ? ` · ${usage.model}` : ""} · ${usage.tokens.total} tokens (in ${usage.tokens.input}, out ${usage.tokens.output}, cache +${usage.tokens.cache_creation}/${usage.tokens.cache_read})`;
        }
        // Compact-nudge (PR #329 round 6): current context-window usage,
        // shown whenever a reading exists — the whole point of this UI is
        // live demo feedback, not just alerting once something's wrong.
        const ctxLabel = contextUsageLabel(a.context);
        const ctx = ctxLabel ? el("span", "group-context", ctxLabel) : null;

        row.append(chip, name, ...(block ? [block] : []), state, up, c, ...(ctx ? [ctx] : []));
        wrap.append(row);

        // "⏳ waiting on …" indicator (#248): a correctly-WAITING agent parked
        // on a CI watch is otherwise indistinguishable from a hung one — see
        // the matching watchdog-notice annotation backend-side. One line,
        // never a layout change; overlay text only.
        const mine = this.watches.filter((w) => w.agent === a.id);
        if (mine.length > 0) {
          const line = el("div", "group-watch-line", watchLine(mine, Date.now()));
          const notes = mine.map((w) => w.note).filter(Boolean);
          if (notes.length > 0) line.title = notes.join(" · ");
          wrap.append(line);
        }

        // Compact-nudge status line (PR #329 round 6): only rendered while
        // there's something worth a human's attention (an arm, an in-flight
        // reinjection, or a recent lost outcome) — `"none"` omits the row
        // entirely, same "no layout change for the common case" shape as the
        // watch-line above.
        const compactionLabel = compactionStatusLabel(a.compaction);
        if (compactionLabel) {
          const line = el("div", "group-compaction-line", compactionLabel);
          const title = compactionStatusTitle(a.compaction);
          if (title) line.title = title;
          wrap.append(line);
        }

        this.listEl.append(wrap);
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

    // Reflect the #260 minimize-on-spawn setting on its toggle (positive
    // sense: "on" means new panes auto-dock, i.e. spawnExpandedFlag is false).
    const autoDock = !this.spawnExpandedFlag;
    this.dockBtn.textContent = autoDock ? "Auto-dock" : "Auto-dock: off";
    this.dockBtn.classList.toggle("on", autoDock);
    this.dockBtn.title = autoDock
      ? "New worker/reviewer/planner panes open minimized to the dock — click to have them open expanded instead"
      : "New panes open expanded (pre-#260 behavior) — click to auto-dock them again";

    this.renderAutonomy();
    this.renderWorkflow();

    // Content height may have changed (roster size, suspended banner) — let the
    // host re-clamp the overlay so no control is pushed under overflow:hidden.
    this.onResize?.();
  }

  /** Workflow-mode chrome (#316): name + roster size + armed gate in one
   *  line (`workflowModeLabel`/`gateSummaryLine`, workflowstatus.ts — never
   *  re-derived here), a loud warning when the gate names reviewers this
   *  session can't spawn, and the toggle button's on/off state. `null` only
   *  before the first successful `load()`. */
  private renderWorkflow(): void {
    const w = this.workflow;
    this.workflowRow.hidden = !w;
    if (!w) return;

    const bits = [workflowModeLabel(w)];
    if (w.advanced) bits.push(`${w.blocks.length} block${w.blocks.length === 1 ? "" : "s"}`);
    const gateLine = gateSummaryLine(w);
    if (gateLine) bits.push(gateLine);
    this.workflowLineEl.textContent = bits.join(" · ");
    this.workflowLineEl.title = w.advanced
      ? "This group is running a repo-declared custom workflow (.loomux/workflow.yml)."
      : "This group is running the built-in roster (orchestrator/worker/reviewer/planner).";

    const warn = gateSatisfiabilityWarning(w);
    this.workflowWarnEl.hidden = warn === null;
    this.workflowWarnEl.textContent = warn ?? "";

    this.workflowToggleBtn.disabled = this.workflowBusy;
    this.workflowToggleBtn.textContent = w.advanced ? "Workflow: on" : "Workflow: off";
    this.workflowToggleBtn.classList.toggle("on", w.advanced);
    this.workflowToggleBtn.title = w.advanced
      ? "Turn off workflow mode — clears the merge gate and returns future spawns to the built-in roster"
      : "Turn on workflow mode — arms this repo's declared merge gate and swaps future spawns to its roster";
  }

  /** The minimum overlay-content height at which every fixed control row renders
   *  and a sliver of roster remains — so `.group-view`'s `overflow:hidden` never
   *  clips a control (footer End/Pause, the suspended banner, #83 rev-58).
   *  MEASURED, not guessed: sums the live heights of every child except the
   *  scrollable roster (and the absolutely-positioned toast). The autonomous
   *  row's height already includes the suspended banner when it's showing, so
   *  the floor grows to fit it. Returns 0 before the panel is laid out (heights
   *  unknown); the caller floors it against a baseline minimum. */
  minChromeHeight(): number {
    let fixed = 0;
    for (const child of Array.from(this.el.children) as HTMLElement[]) {
      if (child === this.listEl || child === this.toastEl) continue;
      fixed += child.offsetHeight;
    }
    if (fixed === 0) return 0; // not laid out yet
    return fixed + MIN_ROSTER_SLIVER;
  }

  /** Sync the autonomous-mode controls, budget meter, and suspended banner to
   *  the last `orch_autonomy` read (+ audit-derived suspension). */
  private renderAutonomy(): void {
    const a = this.autonomy;
    if (!a) return;

    // Toggle button reflects the live marker (dense label; the section title
    // spells out "Autonomous mode", so the button just carries on/off).
    this.autoBtn.textContent = a.autonomous ? "🤖 On" : "🤖 Off";
    this.autoBtn.classList.toggle("on", a.autonomous);
    this.autoBtn.title = a.autonomous
      ? "Idle-ticking is live — the orchestrator polls labeled issues and re-checks PRs while you're away. Click to stop."
      : "Enable idle-ticking: loomux pokes the orchestrator to run its intake/monitoring cadence when the group goes quiet.";

    // Merge gate: reflect the backend flag AND the #83 dependency — auto-merge
    // exists only in autonomous mode, so with autonomous off the control is locked
    // to "approval required" (the enforced human gate) with an explanatory tooltip.
    const approval = approvalControl(a.autonomous, a.auto_merge);
    this.approvalChk.checked = approval.checked;
    this.approvalChk.disabled = approval.disabled;
    this.approvalChk.title = approval.tooltip;

    // Auto-release: same dependency as the merge gate (only under autonomous).
    const release = autoReleaseControl(a.autonomous, a.auto_release);
    this.autoReleaseChk.checked = release.checked;
    this.autoReleaseChk.disabled = release.disabled;
    if (release.tooltip) this.autoReleaseChk.title = release.tooltip;

    // Dangerous mode: the INVERSE gating — usable only while autonomous is OFF.
    // Enabling autonomous force-clears it backend-side; this render reflects that
    // truthfully from the live status, and greys the toggle while autonomous is on.
    const danger = dangerousControl(a.autonomous, a.dangerous_mode);
    this.dangerousChk.checked = danger.checked;
    this.dangerousChk.disabled = danger.disabled;
    if (danger.tooltip) this.dangerousChk.title = danger.tooltip;
    // DANGER affordance: highlight only when actually engaged.
    (this.dangerousChk.closest(".group-auto-check") as HTMLElement | null)
      ?.classList.toggle("on", danger.checked);

    // Budget input: don't clobber while the human is editing it.
    if (document.activeElement !== this.budgetInput) {
      this.budgetInput.value = a.budget_tokens > 0 ? String(a.budget_tokens) : "";
    }

    // Inline meter: only while autonomous (spend is null when off). Off ⇒ hidden.
    // The slim bar shows only with a cap; capless still reads spend so the money
    // surface stays visible.
    if (a.autonomous && a.spend_since_enable_tokens != null) {
      this.meterEl.hidden = false;
      const m = budgetMeter(a.spend_since_enable_tokens, a.budget_tokens);
      if (m.hasCap) {
        this.meterBar.hidden = false;
        this.meterFill.style.width = `${m.percent}%`;
        this.meterFill.classList.toggle("warn", m.percent >= 80 && !m.exhausted);
        this.meterFill.classList.toggle("over", m.exhausted);
        this.meterLabel.textContent =
          `${formatTokens(m.spend)} / ${formatTokens(m.budget)} · ${m.percent}%` +
          (m.exhausted ? " · reached" : "");
      } else {
        this.meterBar.hidden = true;
        this.meterLabel.textContent = `${formatTokens(m.spend)} spent · no cap`;
      }
    } else {
      this.meterEl.hidden = true;
    }

    // Idle-tick knobs (don't clobber while the human is editing). Minutes always
    // shows the applied value; the floor shows blank at the default so its
    // placeholder (2048) reads as the current setting.
    if (document.activeElement !== this.tickMinInput) {
      this.tickMinInput.value = a.idle_tick_minutes > 0 ? String(a.idle_tick_minutes) : "";
    }
    if (document.activeElement !== this.floorInput) {
      this.floorInput.value =
        a.idle_activity_floor_bytes > 0 && a.idle_activity_floor_bytes !== DEFAULT_ACTIVITY_FLOOR
          ? String(a.idle_activity_floor_bytes)
          : "";
    }

    // Live tick-status line: only while autonomous. The label enforces the
    // null-countdown discipline (no lying timer on non-time-gated statuses).
    const statusText = a.autonomous ? tickStatusLabel(a.tick_status, a.eligible_in_secs) : "";
    this.tickStatusEl.textContent = statusText;
    this.tickStatusEl.hidden = statusText === "";

    // Suspended banner: distinct from a plain-off state. `suspended` comes
    // straight from orch_autonomy (true only while off, and only when the budget
    // enforcer flipped it). The re-enable affordance is the toggle above
    // (re-enabling re-anchors the meter).
    if (a.suspended) {
      this.suspendEl.hidden = false;
      this.suspendEl.textContent =
        "⏸ Suspended: token budget exhausted. Re-enable autonomous mode to resume (the budget re-anchors at the current spend).";
    } else {
      this.suspendEl.hidden = true;
    }
  }

  /** Sync the max-agents stepper to the backend value and reflect whether the
   *  current cap is below the live count (spawns blocked until attrition). */
  private renderMax(s: GroupSummary): void {
    const max = s.max_agents;
    const known = max != null;
    this.maxDecBtn.disabled = !known || max <= MIN_MAX_AGENTS;
    this.maxIncBtn.disabled = !known || max >= MAX_MAX_AGENTS;
    this.maxInput.disabled = !known;
    // Don't clobber the value while the human is editing it; the blur/Enter
    // commit (or its failure) refreshes it.
    if (document.activeElement !== this.maxInput) {
      this.maxInput.value = known ? String(max) : "";
    }
    // Copy that reassures: lowering the cap never kills a live agent.
    if (known && max < s.live_delegates) {
      this.maxNoteEl.textContent = `cap below ${s.live_delegates} live — no one is killed; new spawns wait for attrition`;
      this.maxNoteEl.classList.add("warn");
    } else {
      this.maxNoteEl.textContent = "workers + reviewers + planners cap; the orchestrator is exempt";
      this.maxNoteEl.classList.remove("warn");
    }
  }
}

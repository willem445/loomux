// Task-board overlay for orchestrator panes: the human's live window into
// the group's work queue (tasks.json, maintained by the orchestrator via
// MCP tools and edited here). Supports status changes, inline title edits,
// notes, reordering, add, and delete. Every human edit is audited
// backend-side and (except reorders) surfaced to the orchestrator as a
// typed notice.

import { invoke, listen, type UnlistenFn } from "./transport.ts";
import { swapIfConnected } from "./domutil";
import {
  approvableSelection,
  artifactLinkDraft,
  artifactLinksAtCap,
  BACKLOG_SPRINT,
  blockedTaskMap,
  blockingAncestor,
  boardMarker,
  boardUsesDeps,
  boardUsesHierarchy,
  boardUsesLinks,
  boardUsesSprints,
  canApprove,
  canProceed,
  childCounts,
  clearableCount,
  clearedIds,
  containerIds,
  currentSprint,
  filterActive,
  focusMiss,
  settledIds,
  depCandidates,
  depState,
  DONE_STATUS,
  doneCount,
  grantableCount,
  hasMissingParent,
  indentLevel,
  isAwaitingHuman,
  isReady,
  KINDS,
  kindFilterChoices,
  kindPickerChoices,
  DEFAULT_LINK_TYPE,
  levelRuleText,
  LINK_TYPES,
  linkDisplayText,
  linkDraftIsPristine,
  linkOpenPlan,
  linkTypeIcon,
  MAX_ARTIFACT_LINKS,
  nextPicker,
  NO_FILTER,
  parentPickerChoices,
  pickerIsOpen,
  QUEUED_STATUS,
  reorderWithSubtree,
  REQUEST_CHANGES_STATUS,
  retainExisting,
  retainExistingKeys,
  siblingPosition,
  sprintAdvance,
  sprintFilterChoices,
  sprintPickerChoices,
  sprintProgress,
  STATUSES,
  subtreeAllDone,
  taskActivityState,
  UNLABELLED_KIND,
  unmetDeps,
  visibleRows,
  // #1349: the stale-snapshot guard. `composeLinkArrayWrite` is what pairs a
  // row's arrays with the fingerprint they were read under, which is why this
  // view no longer calls `withDep`/`withArtifactLink` and friends directly.
  composeLinkArrayWrite,
  isStaleLinkEtag,
  retriesAfterStale,
  type LinkArrayEdit,
  type BoardFilter,
  type BoardMarker,
  type BoardRow,
  type PickerField,
  type PickerTarget,
  type LinkDraft,
  type TaskArtifactLink,
} from "./taskboard";
import {
  approveTask,
  approveTasks,
  groupSummary,
  questionsList,
  workflowStatus,
  type WorkflowStatus,
} from "./orchestration";
import { writeClipboard } from "./clipboard";
import { isPending, type OrchQuestion } from "./decisions";
import { normalizeComment } from "./autonomy";
import { CoalescingRefresh } from "./refreshgate";
import { WakeGate } from "./wakegate";
import { approveWillMerge, gateExitsMessage } from "./workflowstatus";
import { wipChips } from "./wipchips";
import { BoardPrefsStore } from "./boardprefs.ts";
import { loadBoardPrefs, saveBoardPrefs } from "./pty.ts";

export interface OrchTaskNote {
  ts_ms: number;
  author: string;
  text: string;
}

export interface OrchTask {
  id: string;
  title: string;
  status: string;
  issue?: string | null;
  pr?: string | null;
  /** The branch `pr` targets (#581), as the orchestrator recorded it. Absent
   *  on every pre-#581 task and on any board that doesn't record it — the
   *  Approve relabel treats that as "base unknown" and stays conservative.
   *  Display metadata: nothing here gates a merge. */
  pr_base?: string | null;
  assignee?: string | null;
  session?: string | null;
  notes: OrchTaskNote[];
  /** Ids of tasks on this board that must be `done` first (#582). Optional
   *  because the backend omits an empty vec entirely
   *  (`skip_serializing_if`) — every pre-#582 board arrives with no key at
   *  all, so the board must never assume the array is there. */
  deps?: string[];
  /** Non-blocking "see also" ids (#582). Rendered read-only here: the human
   *  board's `orch_upsert_task` deliberately takes `deps` but not `related`,
   *  which the orchestrator maintains through its own tools. */
  related?: string[];
  /** The task this one sits inside (#958) — containment, not ordering (that
   *  is still `deps`). Optional on the wire like the link arrays: the backend
   *  skips it when absent, so every pre-#958 board arrives with no key. The
   *  board derives the whole tree from this field; nothing gates on it. */
  parent?: string | null;
  /** Agile level — one of `KINDS` (#958), enforced since #1156: a levelled row
   *  must sit directly inside the level above it, so this field decides what
   *  the nest and level pickers may offer. Absent = no level, which is exempt
   *  from the ladder and legal anywhere. */
  kind?: string | null;
  /** Numbered work batch this row belongs to (#1272) — an ordering/grouping
   *  construct with no dates attached. Optional on the wire like `parent`/
   *  `kind`: the backend omits the key when absent, so a board that runs no
   *  sprints arrives with no key at all and ABSENT must read as backlog,
   *  never as sprint 0. Always >= 1 when present. Nothing gates on it — it
   *  ranks what the orchestrator should pick up next, and that is all. */
  sprint?: number | null;
  /** Grounding artifacts governing this row (#1273) — requirements, specs,
   *  design notes, test cases, docs. Optional and omitted when empty, like
   *  the #582 link arrays.
   *
   *  Distinct from `deps`/`related` in what they POINT AT: those name task
   *  ids on this board, these name things outside it (issue/PR refs, repo
   *  paths, URLs), which is why the backend validates the two differently
   *  and never existence-checks these. Context, never structure: they affect
   *  no readiness and no ordering. */
  links?: TaskArtifactLink[];
  /** Worktree path where a demo of this row lives (#1091 slice B) — recorded
   *  by the orchestrator on a `prototype`/`human-testing` row so the NEEDS-YOU
   *  panel can tell the human where to go run it. Optional on the wire like
   *  `parent`/`kind`: the backend omits the key entirely when absent, so a
   *  pre-#1091 board arrives without it. Absent means "no path recorded", never
   *  "there is no demo" — nothing here guesses one from an assignee's cwd.
   *  Display metadata: nothing gates on it. */
  demo_path?: string | null;
  /** When the human cleared this row out of their working board view (#1152).
   *  Optional on the wire like `parent`/`kind`/`demo_path`: the backend omits
   *  the key entirely when absent, so a pre-#1152 board arrives without it and
   *  absent means "not cleared". An ARCHIVE marker, never a delete — the row,
   *  its notes and its links stay on the board, and `isCleared` only honours
   *  the stamp while the row is still `done`. */
  cleared_ms?: number | null;
  updated_ms: number;
  /** Fingerprint of this row's `deps`/`related`/`links` as of the read that
   *  produced it (#1349) — DERIVED per read by the backend, never stored on the
   *  board. Every write this view composes from one of those arrays sends it
   *  back as `expect_link_etag`, so an agent's concurrent edit refuses the write
   *  instead of being silently replaced by a list painted before it existed.
   *  Optional only because the wire type is; see `HasLinkArrays`. */
  link_etag?: string | null;
}

/** The nest picker's "take it back to the top level" option (#958). A sentinel
 *  rather than `""`, because the empty value is already the picker's own
 *  "nothing chosen yet" placeholder; it is translated to the empty string —
 *  which is what `orch_upsert_task` reads as "clear the container" — at the
 *  moment of the write, and never travels to the backend as an id. Ids are
 *  `<prefix>-<n>` over a closed set of prefixes (#1156), so no row can ever
 *  collide with this value. */
const TOP_LEVEL_CHOICE = "__top_level__";

/** The kind picker's "clear the label" option (#958 slice K) — same sentinel
 *  shape as `TOP_LEVEL_CHOICE` above and for the same reason: `""` is already
 *  the picker's own placeholder ("nothing chosen yet"), so the clear needs its
 *  own value. Translated to `""` — which `orch_upsert_task` reads as "clear
 *  the label" — only at the moment of the write, and never sent as a literal
 *  kind. `KINDS` entries are plain words (`epic`, `feature`, …), so no real
 *  kind can ever collide with this value. */
const CLEAR_KIND_CHOICE = "__clear_kind__";

function el(tag: string, cls: string, text?: string): HTMLElement {
  const e = document.createElement(tag);
  e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

const fmtTime = (ms: number): string =>
  new Date(ms).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });

export class TasksView {
  readonly el: HTMLElement;
  private listEl: HTMLElement;
  private addInput: HTMLInputElement;
  /** Archive every `done` row out of the working view (#1152) — non-destructive,
   *  so no confirm step. */
  private clearDoneBtn: HTMLButtonElement;
  /** Show/hide the archive, and bring all of it back (#1152). */
  private showClearedBtn: HTMLButtonElement;
  private restoreClearedBtn: HTMLButtonElement;
  /** The DESTRUCTIVE batch: delete every `done` row, archive included (#120). */
  private deleteDoneBtn: HTMLButtonElement;
  private deleteDoneTimer: number | undefined;
  private deleteSelectedBtn: HTMLButtonElement;
  private deleteSelectedTimer: number | undefined;
  private approveSelectedBtn: HTMLButtonElement;
  /** The board's WIP chips (#1175) — hidden when the repo declares no caps. */
  private wipEl: HTMLElement;
  /** The header sprint lens (#1272) — `sprint 2 · 3/7 done`, doubling as the
   *  one-click filter for the current sprint, with the advance affordance
   *  beside it. Hidden entirely on a board that runs no sprints. */
  private sprintEl: HTMLElement;
  private sprintLensBtn: HTMLButtonElement;
  private sprintAdvanceBtn: HTMLButtonElement;
  /** The tree-view control strip (#1270): collapse-all/expand-all, search, the
   *  kind/status chips, the needs-you toggle, and the "showing N of M" hint. */
  private filterEl: HTMLElement;
  private collapseAllBtn: HTMLButtonElement;
  private expandAllBtn: HTMLButtonElement;
  private searchInput: HTMLInputElement;
  private attentionBtn: HTMLButtonElement;
  private clearFilterBtn: HTMLButtonElement;
  private filterChipsEl: HTMLElement;
  private filterCountEl: HTMLElement;
  private toastEl: HTMLElement;
  private toastTimer: number | undefined;
  private tasks: OrchTask[] = [];
  /** Pending + settled questions (#1091 slice G) — read alongside `tasks` so
   *  the board can derive the decision-blocked marker. Best-effort like
   *  `liveAgentIds`/`workflow` below: a failed read just means no marker
   *  until the next successful refresh, never a broken board. */
  private questions: OrchQuestion[] = [];
  /** Ids of the group's currently-live agents (#339 refinement) — an
   *  assignee not in this set reads as history, not active work, however
   *  recently its task was touched. Refreshed alongside `tasks`; best-effort
   *  (an empty set just means everything reads as idle until the next
   *  successful refresh, never a broken board). */
  private liveAgentIds = new Set<string>();
  /** The group's live workflow-mode status (#316), for the gate-aware Approve
   *  label below — refetched alongside `tasks` on every board refresh. `null`
   *  before the first successful read, or if that read fails; either way
   *  `approveWillMerge` treats it as "no gate known" (Approve reads plain)
   *  rather than guessing a warning it can't back up. */
  private workflow: WorkflowStatus | null = null;
  /** Task ids with their notes section expanded (survives re-renders). */
  private expanded = new Set<string>();
  /** Task ids with their GROUNDING section expanded (#1273) — its own set,
   *  not a second meaning for `expanded` above. The two sections answer
   *  different questions ("what was said about this" / "what governs this")
   *  and a human reading one is routinely not done with the other, so folding
   *  them into one toggle would close the notes to open the links. Like
   *  `expanded` it is frontend-only and survives re-renders; an id whose row
   *  has since gone simply never matches. */
  private expandedLinks = new Set<string>();
  /** The half-typed grounding link per row (#1273 N1), so a re-render never
   *  eats what the human is in the middle of writing.
   *
   *  It has to live HERE rather than in the input elements, because the
   *  elements do not survive the event this exists for. `mutate` resyncs the
   *  board after a REFUSED write, and `refreshNow` only defers a render while
   *  `isEditing()` is true — which reads `document.activeElement`, so it holds
   *  for Enter (focus still in the field) and NOT for a click on Add link
   *  (focus on the button). Clearing on success alone would therefore still
   *  lose the text on exactly the route most people use, and the whole point
   *  of keeping validation backend-side is that the refusal TEACHES — an error
   *  naming `deps`/`related` is worth nothing if the target it is about has
   *  already been wiped out of the box. Pruned to live rows on every refresh,
   *  like `selected`/`collapsed`. */
  private linkDrafts = new Map<string, LinkDraft>();
  /** The row whose target field should take focus after the next render — the
   *  same one-shot hook as `pickingFocus`, so adding several links to a row in
   *  a row does not need a click between each. */
  private linkFocus: string | null = null;
  /** Task ids the human has ticked for batch delete. Frontend-only, so it's
   *  pruned to live rows on every refresh (see retainExisting). */
  private selected = new Set<string>();
  /** Containers the human has collapsed (#958). Frontend-OWNED — a view
   *  preference that never becomes board data (the board is the orchestrator's
   *  queue, not this window's UI state) — but, since #1270, durable: it is
   *  persisted per group in `boardprefs.json`, a sibling blob no agent reads,
   *  NOT on the task. See doc/design/board-tree-view.md for why the drift
   *  objection #1152 raised against a sidecar does not carry over.
   *
   *  That is what separates it from `expanded` and `selected` above, which
   *  remain per-session. Pruned to live rows on every refresh, like
   *  `selected` — a stale id here is inert (it names no container), and the
   *  pruning is what keeps the saved set from accumulating them. */
  private collapsed = new Set<string>();
  /** The armed view filter (#1270). Frontend state like `collapsed` above —
   *  and, since #1270, persisted alongside it. */
  private filter: BoardFilter = { ...NO_FILTER };
  /** The durable board-view store (#1270). Owns the read-before-publish
   *  ordering, because `boardprefs.json` is ONE file for every group: a save
   *  built from a store that was never read would publish an empty map as the
   *  whole truth and silently destroy every other group's record. That
   *  invariant is a race between two async calls, so it lives in a tested
   *  module rather than in this file — see `BoardPrefsStore`. */
  private readonly prefsStore = new BoardPrefsStore({
    load: loadBoardPrefs,
    save: saveBoardPrefs,
  });
  /** The human has changed the collapse set or the filter in this window. The
   *  boot load must not overwrite a click that beat it: the disk copy is what
   *  they left LAST session, and a live gesture always wins. */
  private prefsTouched = false;
  /** The stored view for this group has been read and adopted. Distinct from
   *  `prefsTouched`: if the boot read FAILED this stays false, and `show()`
   *  re-attempts adoption on a later open (#1270 review N5) — a transient IPC
   *  rejection at boot should not cost the human their folds for the rest of the
   *  session, and it must not let a later gesture publish this view's defaults
   *  over a record nobody has looked at. */
  private prefsAdopted = false;
  private prefsSaveTimer: number | undefined;
  /** Re-render debounce for the search box only. Every other control is one
   *  discrete click; typing is a burst, and a 400-row board re-rendered per
   *  keystroke is the one place this view can feel slow. */
  private searchTimer: number | undefined;
  /** Whether the archived (cleared) rows are on screen right now (#1152).
   *  Frontend-only and deliberately NOT persisted — unlike `collapsed` and
   *  `filter`, which #1270 made durable. WHICH rows are archived is board data
   *  (`cleared_ms`, written by the human's own command); whether this window is
   *  currently looking at them is a momentary act of inspection, not a shape
   *  the human arranged and would want back. Reopening the board on a working
   *  view is the right default, and 👁 is one click. */
  private showCleared = false;
  /** The task whose picker is open, if any (#582, #958) — one at a time across
   *  EVERY picker, and kept here rather than in the DOM so a background
   *  refresh re-renders it instead of silently closing it mid-choice. `field`
   *  says which one: a dependency (ordering), a container (nesting), or an
   *  Agile level (#958 slice K). */
  private picking: PickerTarget | null = null;
  /** The picker was just opened by a click, so it should take focus on this
   *  render. Cleared once consumed: a later refresh must re-render the open
   *  picker without stealing focus back from wherever the human has moved. */
  private pickingFocus = false;
  /** A refresh arrived while the human was mid-edit; run it on blur. */
  private pendingRefresh = false;
  /** Single-flight + trailing-edge merge for this view's refresh (#743 S5).
   *  `orch-tasks-changed` fires on EVERY `write_tasks`, and agents write in
   *  bursts (a plan posting ten tasks, a batch status sweep), so an ungated
   *  handler cost one full refetch — three backend commands and a whole-board
   *  re-render — per write, per board. A burst of N now costs the run
   *  already in flight plus exactly one trailing run, which reads the final
   *  state, so nothing is lost by coalescing.
   *
   *  "Per board" meant per board that had EVER been opened until #1318 — the
   *  coalescer bounds a burst's cost, `wakeGate` below is what decides whether
   *  this board pays it at all. */
  private readonly refresher = new CoalescingRefresh(() => this.refreshNow());
  /** Whether this board is on screen (#1318). Its stream is agent-driven and
   *  its render is super-linear in the board, so a closed board that keeps
   *  refetching costs every pane in the session on every `write_tasks` by every
   *  agent. `show()` refreshes unconditionally, so a wake dropped here is
   *  re-earned the moment anyone looks — see `wakegate.ts` for why the pane's
   *  live `isVisible` read, not the latch alone, is what releases it. */
  private readonly wakeGate = new WakeGate(() => this.opts.isVisible());
  /** The open request-changes modal, if any (kept to one at a time). */
  private dialogEl: HTMLElement | null = null;
  private unlistenTasks: UnlistenFn | null = null;
  /** #1091 slice G: the board's marker chip needs `orch-questions-changed`
   *  too — a question being asked, answered, or withdrawn changes whether a
   *  row is decision-blocked, independently of any `orch-tasks-changed`. */
  private unlistenQuestions: UnlistenFn | null = null;
  private disposed = false;

  private embedBtn: HTMLButtonElement;
  private closeBtn: HTMLButtonElement;

  constructor(
    private groupId: string,
    private opts: {
      onClose: () => void;
      onEmbedMenu: (anchor: HTMLElement) => void;
      /** Drain any focus request parked for the BOARD (#1091 slice C), called
       *  once per render. Non-null only when something asked for a specific
       *  row while the board was closed or unbuilt — a NEEDS-YOU card citing
       *  `t-N` is the first caller. `undefined` when the host wires no focus. */
      takeFocus?: () => string | null;
      /** The board-to-panel direction of the focus hook (#1091 slice G): ask
       *  the pane to open the NEEDS-YOU panel at `id` — a `q-N` for a
       *  decision-blocked row, this row's own `t-N` for a demo-gated one (see
       *  `boardMarker` in `taskboard.ts`). Returns whether the pane could
       *  route it, mirroring `decisionsview.ts`'s `onFocusTask` the other way. */
      onFocusDecision: (id: string) => boolean;
      /** The pane's live read of whether this view is on screen, in either
       *  hosting mode (#1318) — `wakeGate`'s bounded release. Required, not
       *  optional: an event-driven view that forgets it is the defect #1318
       *  fixed, and a default would let the next one forget it silently. */
      isVisible: () => boolean;
    }
  ) {
    this.el = el("div", "tasks-view");

    const head = el("div", "tasks-head");
    head.append(el("span", "tasks-title", "task board"));
    head.append(el("span", "tasks-group", groupId));

    // The board's WIP chips (#1175). Hidden — and empty — for every repo that
    // declares no `board.wip` block, which is most of them: the header keeps
    // exactly the shape it had.
    this.wipEl = el("span", "tasks-wip");
    this.wipEl.hidden = true;
    head.append(this.wipEl);

    // The sprint lens (#1272). Header CHROME, like the WIP chips beside it —
    // it is inside the board overlay and nothing about it is a layout sibling
    // of #grid-area, so hard constraint 1 is untouched.
    //
    // Hidden — and empty — on every board that runs no sprints, which is most
    // of them: the header keeps exactly the shape it had. `boardUsesSprints`
    // is the same pay-for-what-you-use gate the collapse gutter and the kind
    // chips already apply.
    this.sprintEl = el("span", "tasks-sprint");
    this.sprintEl.hidden = true;
    // The lens itself is the label: clicking it arms the sprint filter on the
    // CURRENT sprint number and clicking again clears it. It writes a concrete
    // number rather than a "current" sentinel, deliberately — see
    // `renderSprintLens`.
    this.sprintLensBtn = el("button", "pane-btn sprint-lens", "") as HTMLButtonElement;
    this.sprintLensBtn.addEventListener("click", () => this.toggleSprintLens());
    this.sprintAdvanceBtn = el("button", "pane-btn sprint-advance", "⏭") as HTMLButtonElement;
    this.sprintAdvanceBtn.addEventListener("click", () => this.onAdvanceSprint());
    this.sprintEl.append(this.sprintLensBtn, this.sprintAdvanceBtn);
    head.append(this.sprintEl);

    // Archive every done row out of the working view in one action (#1152).
    // NOTHING is deleted — the rows keep their notes, links and place in
    // tasks.json, the action is audited, and the 👁 toggle beside this brings
    // them back — so unlike its destructive neighbour below there is no
    // two-click confirm to stand between the human and a reversible action.
    this.clearDoneBtn = el("button", "pane-btn clear-done", "") as HTMLButtonElement;
    this.clearDoneBtn.hidden = true;
    this.clearDoneBtn.addEventListener("click", () => this.onClearDone());
    head.append(this.clearDoneBtn);

    // Show/hide the archive. A per-window view preference, so it lives in
    // `showCleared` and is not persisted — see that field for why it stayed
    // per-session when #1270 made collapse and the filters durable. Hidden
    // entirely while there is nothing archived to look at.
    this.showClearedBtn = el("button", "pane-btn show-cleared", "") as HTMLButtonElement;
    this.showClearedBtn.hidden = true;
    this.showClearedBtn.addEventListener("click", () => {
      this.showCleared = !this.showCleared;
      this.render();
    });
    head.append(this.showClearedBtn);

    // Bulk un-archive, the counterpart to one clear click. Only shown while the
    // archive is on screen: a bulk restore the human cannot see the effect of
    // would be the same kind of blind batch the clear button avoids by being
    // reversible in the first place.
    this.restoreClearedBtn = el("button", "pane-btn restore-cleared", "") as HTMLButtonElement;
    this.restoreClearedBtn.hidden = true;
    this.restoreClearedBtn.addEventListener("click", () => this.onRestoreAll());
    head.append(this.restoreClearedBtn);

    // Batch-DELETE all done tasks in one action. Hidden until there is
    // something to delete (updated in render). Two-click confirm — a mis-click
    // must not wipe the board — mirroring the per-row delete. The backend does
    // this as one operation so the orchestrator gets a single board-change
    // notice for the whole batch, not one per task (#120).
    this.deleteDoneBtn = el("button", "pane-btn delete-done", "") as HTMLButtonElement;
    this.deleteDoneBtn.hidden = true;
    this.deleteDoneBtn.addEventListener("click", () => this.onDeleteDone());
    head.append(this.deleteDoneBtn);

    // Multi-select delete: tick task rows, then clear them in one action. Like
    // "delete all done" it's a single backend call (one coalesced notice #120)
    // with a two-click confirm; hidden until at least one row is selected.
    this.deleteSelectedBtn = el("button", "pane-btn delete-selected", "") as HTMLButtonElement;
    this.deleteSelectedBtn.hidden = true;
    this.deleteSelectedBtn.addEventListener("click", () => this.onDeleteSelected());
    head.append(this.deleteSelectedBtn);

    // Multi-select approve (#507): tick several merge-gate rows and authorize
    // them in one action. Each PR still gets its own one-time grant — what the
    // batch saves is the orchestrator receiving N separate prompts. No
    // two-click confirm here (unlike delete): the modal below IS the confirm,
    // exactly as it is for a single Approve, because a grant is an
    // authorization the human should read before issuing.
    this.approveSelectedBtn = el("button", "pane-btn approve-selected", "") as HTMLButtonElement;
    this.approveSelectedBtn.hidden = true;
    this.approveSelectedBtn.addEventListener("click", () => this.onApproveSelected());
    head.append(this.approveSelectedBtn);

    // Embed side-picker: switch between the floating overlay and any of the
    // pane's (up to three) embed slots (#361) — a discrete, user-initiated
    // layout change, like a split (see doc/design/embedded-panels.md).
    // setPanelActive() below keeps the icon/tooltip in sync with whether the
    // pane currently has this docked, regardless of which side.
    this.embedBtn = el("button", "pane-btn embed", "⬒") as HTMLButtonElement;
    this.embedBtn.addEventListener("click", () => opts.onEmbedMenu(this.embedBtn));
    head.append(this.embedBtn);

    this.closeBtn = el("button", "pane-btn close", "✕") as HTMLButtonElement;
    this.closeBtn.title = "Close (Alt+T)";
    this.closeBtn.addEventListener("click", opts.onClose);
    head.append(this.closeBtn);
    // Now that both buttons `setPanelActive` touches exist.
    this.setPanelActive(false);

    // ---- tree-view control strip (#1270) ----
    //
    // Board CHROME, inside the overlay: it is a flex child of `.tasks-view`,
    // which floats over (or docks beside) the terminal exactly as it did
    // before. Nothing here is a sibling of #grid-area and nothing resizes a
    // PTY (hard constraint 1).
    this.filterEl = el("div", "tasks-filter");

    this.collapseAllBtn = el("button", "pane-btn tree-btn", "⊟") as HTMLButtonElement;
    this.collapseAllBtn.addEventListener("click", () => {
      this.collapsed = new Set(containerIds(this.tasks));
      this.onViewChanged();
    });
    this.expandAllBtn = el("button", "pane-btn tree-btn", "⊞") as HTMLButtonElement;
    this.expandAllBtn.addEventListener("click", () => {
      this.collapsed = new Set();
      this.onViewChanged();
    });
    this.filterEl.append(this.collapseAllBtn, this.expandAllBtn);

    this.searchInput = document.createElement("input");
    this.searchInput.className = "dlg-input tasks-search";
    this.searchInput.placeholder = "Find in this board…";
    this.searchInput.spellcheck = false;
    // stopPropagation for the same reason the Add field does it: the pane's
    // global keydown handler must not read typing here as a shortcut.
    this.searchInput.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape" && this.searchInput.value !== "") {
        // Cancel the in-flight debounce first: without this, a clear within
        // 120ms of the last keystroke is undone by the timer that was already
        // scheduled with the old text.
        clearTimeout(this.searchTimer);
        this.searchInput.value = "";
        this.filter = { ...this.filter, text: "" };
        this.onViewChanged();
      }
    });
    this.searchInput.addEventListener("input", () => {
      const text = this.searchInput.value;
      clearTimeout(this.searchTimer);
      this.searchTimer = window.setTimeout(() => {
        this.filter = { ...this.filter, text };
        this.onViewChanged();
      }, 120);
    });
    this.filterEl.append(this.searchInput);

    // The one quick-filter that is not a family: rows waiting on the human,
    // which is the question this board is usually opened with. Same set the
    // ❓/👀 marker chips are drawn from, so the toggle can never disagree with
    // what the rows themselves say.
    this.attentionBtn = el("button", "pane-btn filter-chip attention", "❗ needs you") as HTMLButtonElement;
    this.attentionBtn.addEventListener("click", () => {
      this.filter = { ...this.filter, attention: !this.filter.attention };
      this.onViewChanged();
    });
    this.filterEl.append(this.attentionBtn);

    this.filterChipsEl = el("span", "tasks-filter-chips");
    this.filterEl.append(this.filterChipsEl);

    this.filterCountEl = el("span", "tasks-filter-count");
    this.filterEl.append(this.filterCountEl);

    this.clearFilterBtn = el("button", "pane-btn tree-btn clear-filter", "✕") as HTMLButtonElement;
    this.clearFilterBtn.title = "Clear every filter";
    this.clearFilterBtn.addEventListener("click", () => {
      clearTimeout(this.searchTimer); // see the Escape handler above
      this.filter = { ...NO_FILTER };
      this.searchInput.value = "";
      this.onViewChanged();
    });
    this.filterEl.append(this.clearFilterBtn);

    this.listEl = el("div", "tasks-list");

    const foot = el("div", "tasks-add");
    this.addInput = document.createElement("input");
    this.addInput.className = "dlg-input";
    this.addInput.placeholder = "Add a task — the orchestrator is notified";
    this.addInput.spellcheck = false;
    this.addInput.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Enter") void this.addTask();
    });
    const addBtn = el("button", "dlg-btn primary", "Add") as HTMLButtonElement;
    addBtn.addEventListener("click", () => void this.addTask());
    foot.append(this.addInput, addBtn);

    this.toastEl = el("div", "git-toast");
    this.toastEl.hidden = true;

    this.el.append(head, this.filterEl, this.listEl, foot, this.toastEl);

    void this.loadPrefs();

    // Deferred refreshes (see refresh()) run once the editor loses focus.
    this.listEl.addEventListener("focusout", () => {
      window.setTimeout(() => {
        if (this.pendingRefresh && !this.isEditing()) this.refresh();
      }, 0);
    });

    void listen<{ group_id: string }>("orch-tasks-changed", ({ payload }) => {
      if (payload.group_id === this.groupId) this.refresh();
    }).then((u) => {
      if (this.disposed) u();
      else this.unlistenTasks = u;
    });
    // #1091 slice G: the marker chip's decision signal depends on the
    // questions list too, not just the board — both listeners share the one
    // `refresher` gate above, so a simultaneous burst on both streams still
    // coalesces to one refresh, exactly like the NEEDS-YOU panel's own pair
    // (decisionsview.ts).
    void listen<{ group_id: string }>("orch-questions-changed", ({ payload }) => {
      if (payload.group_id === this.groupId) this.refresh();
    }).then((u) => {
      if (this.disposed) u();
      else this.unlistenQuestions = u;
    });
  }

  /** Called by the pane whenever the view is (re)opened, in either mode. */
  show(): void {
    // Before the refresh below, so that refresh is the one this open owes
    // rather than the first thing the gate suppresses (#1318).
    this.wakeGate.wake();
    // Re-attempt a boot read that failed (#1270 review N5), but only while the
    // human has changed nothing — once they have, their gesture is newer than
    // the file and adopting over it would look like the click was ignored. The
    // store memoises a SUCCESSFUL read, so this is a no-op in the normal case.
    if (!this.prefsAdopted && !this.prefsTouched) void this.loadPrefs();
    this.refresh();
  }

  /** Called by the pane whenever the view is about to be hidden, in either
   *  mode — a close, a slot eviction, an un-dock (#1318).
   *
   *  The board had no `hide` hook because nothing on `EmbedEntry.hide` asked
   *  for one — see its doc for what was actually written there, and where the
   *  rule really lived. Nothing is lost by stopping: `show()` above refreshes
   *  unconditionally, so no staleness survives the panel being looked at.
   *
   *  Scope: this is the panel being CLOSED. A board left open in a background
   *  tab or a minimized pane never reaches here — #1465. */
  hide(): void {
    this.wakeGate.sleep();
  }

  /** Reflect which mode the pane currently has this view mounted in —
   *  called on creation and on every embed/un-embed. Pure display state; the
   *  pane owns the actual toggle (it has to move the view between an overlay
   *  host and the pane's single embed-panel slot). Named `setPanelActive`,
   *  not `setEmbedded`, across every embeddable view (#361 generalization) —
   *  `GitView` already has an unrelated ctor option called `embedded`
   *  (#217: is this view hosted as a whole content PANE?), and reusing that
   *  word for a same-named but different-meaning method on the same class
   *  would read as the two being connected when they aren't. */
  setPanelActive(active: boolean): void {
    this.embedBtn.classList.toggle("active", active);
    this.embedBtn.textContent = active ? "⬓" : "⬒";
    this.embedBtn.title = active
      ? "Un-embed — back to a floating overlay"
      : "Embed beside the terminal (resizes this pane)";
    // The overlay toggle (this button, the pane header's own board button)
    // is disabled while docked (#361 user-demo finding — see embedtoggle.ts):
    // only un-embedding closes a docked board now.
    this.closeBtn.disabled = active;
    this.closeBtn.title = active ? "Docked — un-embed it (side menu) to close" : "Close (Alt+T)";
  }

  dispose(): void {
    this.disposed = true;
    clearTimeout(this.toastTimer);
    clearTimeout(this.deleteDoneTimer);
    clearTimeout(this.deleteSelectedTimer);
    clearTimeout(this.searchTimer);
    // A pending save is FLUSHED rather than dropped (#1270): closing the board
    // is the commonest way a session ends, and a debounce timer cancelled here
    // would lose the last gesture — the same argument `flushTabs` makes for the
    // quit path. Fire-and-forget: nothing may block a close on a disk write.
    if (this.prefsSaveTimer !== undefined) {
      clearTimeout(this.prefsSaveTimer);
      this.prefsSaveTimer = undefined;
      this.savePrefsNow();
    }
    this.unlistenTasks?.();
    this.unlistenQuestions?.();
    this.el.remove();
  }

  private toast(msg: string): void {
    this.toastEl.textContent = msg;
    this.toastEl.hidden = false;
    clearTimeout(this.toastTimer);
    this.toastTimer = window.setTimeout(() => (this.toastEl.hidden = true), 4000);
  }

  /** Run a mutation; the resulting orch-tasks-changed event re-renders. */
  private async mutate(action: Promise<unknown>): Promise<void> {
    try {
      await action;
    } catch (err) {
      this.toast(String(err));
      this.refresh(); // resync UI with reality after a failed edit
    }
  }

  /** Write one edit to a row's replace-wholesale arrays (#1349).
   *
   *  **Every board edit to `deps` or `links` goes through here**, because each
   *  one composes a whole new array from the row as it was PAINTED — and the
   *  orchestrator writes the same three arrays through MCP while that paint sits
   *  on screen. `composeLinkArrayWrite` pairs the array with the `link_etag`
   *  that came with the very same row, so the backend can refuse a write built
   *  on a list that has since moved instead of silently replacing an edit
   *  nobody here ever saw.
   *
   *  **On a refusal: re-read, then re-apply the INTENT — once.** The retry sends
   *  the human's edit against the row as it is now, not the array they had, and
   *  the second attempt is guarded exactly like the first (its token comes from
   *  the re-read), so a third writer arriving in between refuses it rather than
   *  landing it blind. One retry and no loop: a second failure means the board
   *  is moving faster than the human can be shown anything useful, and the
   *  honest answer is the toast plus the repaint. `retriesAfterStale` decides
   *  which edits are eligible at all — a link removal names an index and is
   *  never re-applied.
   *
   *  Anything that is NOT a stale-token refusal (an unknown dep, a cycle, the
   *  link cap, a target naming a board task) falls straight through to
   *  `mutate`'s toast: those are the human's own edit being wrong, and the
   *  errors that teach are the ones this view has always shown them. */
  private writeLinkArray(t: OrchTask, edit: LinkArrayEdit, onLanded?: () => void): Promise<void> {
    const attempt = (row: OrchTask): Promise<void> =>
      invoke("orch_upsert_task", {
        groupId: this.groupId,
        id: row.id,
        ...composeLinkArrayWrite(edit, row),
      }).then(() => {
        onLanded?.();
      });
    return this.mutate(
      attempt(t).catch(async (err) => {
        if (!isStaleLinkEtag(err) || !retriesAfterStale(edit)) throw err;
        const board = await invoke<OrchTask[]>("orch_tasks", { groupId: this.groupId });
        // The row itself can be gone — the concurrent write that moved the
        // arrays may have been a delete. Re-throwing the ORIGINAL refusal is
        // right: `mutate` toasts it and refreshes, and the repaint is what tells
        // the human the row is no longer there.
        const fresh = board.find((x) => x.id === t.id);
        if (!fresh) throw err;
        return attempt(fresh);
      })
    );
  }

  /** True while the human is typing in an inline editor inside the list
   *  (title rename, note input) — re-rendering would destroy their edit. */
  private isEditing(): boolean {
    const a = document.activeElement;
    return !!a && this.listEl.contains(a) && (a.tagName === "INPUT" || a.tagName === "TEXTAREA");
  }

  /** Ask for a refresh. Coalesced — see `refresher`. The gate lives here rather
   *  than at each call site so a future caller cannot forget it, and so the
   *  event handler and the human's own actions share one in-flight run.
   *
   *  The visibility gate lives here too, ahead of the coalescer, and for the
   *  same reason (#1318): one funnel, one rule, no call site that can forget
   *  it. Ahead of it rather than inside `refreshNow` so a suppressed wake never
   *  touches the single-flight state at all. */
  private refresh(): void {
    if (!this.wakeGate.accepts()) return;
    this.refresher.request();
  }

  /** Read this group's durable board view (#1270) and adopt it.
   *
   *  Best-effort, like every other enrichment this view loads: a missing or
   *  corrupt blob just means the board opens expanded and unfiltered, which is
   *  exactly the pre-#1270 board.
   *
   *  `read` answers `null` only when the file could not be READ, which is not
   *  the same as a group having nothing stored — so this changes nothing and,
   *  crucially, does not mark the view adopted. `show()` retries on the next
   *  open, and until one succeeds a save is skipped entirely, so this view's
   *  defaults can never be published over a record nobody has read.
   *
   *  A gesture that beat the load WINS. The disk copy is what the human left
   *  last session; a chevron they have already clicked in this one is newer,
   *  and adopting the file over it would look like the click was ignored.
   *
   *  Never re-run: `show()` can fire many times as the overlay is toggled, and
   *  re-adopting the file each time would undo the session's own gestures. */
  private async loadPrefs(): Promise<void> {
    const view = await this.prefsStore.read(this.groupId);
    if (!view || this.disposed) return;
    // The read succeeded, so the record is known even if a gesture already beat
    // us to the view: the flag is what stops `show()` retrying forever, and it
    // is set before the touched check for exactly that reason.
    this.prefsAdopted = true;
    if (this.prefsTouched) return;
    this.collapsed = new Set(view.collapsed);
    this.filter = view.filter;
    this.searchInput.value = this.filter.text;
    // Only if there is something on screen to re-lay-out. Resolving before the
    // first `orch_tasks` would otherwise paint the "No tasks yet" empty state
    // on a board that simply has not loaded — and the first refresh renders
    // with the adopted view regardless.
    if (this.tasks.length > 0) this.render();
  }

  /** The collapse set or the filter changed: re-render now, persist shortly.
   *
   *  One path for every control, so no affordance can change the view without
   *  also marking it dirty and scheduling the save — the failure mode where a
   *  new button is added and its state silently stops persisting. */
  private onViewChanged(): void {
    this.prefsTouched = true;
    this.render();
    this.schedulePrefsSave();
  }

  /** Coalesce a burst of view gestures into one write. Ticking four status
   *  chips is four gestures and one file. */
  private schedulePrefsSave(): void {
    clearTimeout(this.prefsSaveTimer);
    this.prefsSaveTimer = window.setTimeout(() => {
      this.prefsSaveTimer = undefined;
      this.savePrefsNow();
    }, 400);
  }

  /** Hand this group's view to the store, which publishes the whole blob.
   *
   *  Fire-and-forget, the same best-effort contract `persistTabs` takes: a
   *  failed write just means this gesture is not durable until the next one,
   *  and the store keeps the newer value so the next one re-offers it. The
   *  store also DECLINES the write outright if it has not managed to read the
   *  file yet, and carries this record's unknown filter families over from disk
   *  rather than from here — see `BoardPrefsStore`, where both are tested. */
  private savePrefsNow(): void {
    // Nothing the human did, nothing to persist (#1270 review N5). Without this
    // the dispose flush publishes whatever the view happens to hold — which,
    // after a boot read that failed, is the constructed defaults, landing on top
    // of this group's real record.
    if (!this.prefsTouched) return;
    void this.prefsStore.write(
      this.groupId,
      { collapsed: [...this.collapsed], filter: this.filter },
      Date.now()
    );
  }

  /** One board refresh. Only `refresher` calls this. */
  private async refreshNow(): Promise<void> {
    if (this.disposed) return;
    if (this.isEditing()) {
      // Orchestrator updates mustn't clobber a human's half-typed edit;
      // the focusout handler re-runs this once they're done.
      this.pendingRefresh = true;
      return;
    }
    this.pendingRefresh = false;
    try {
      this.tasks = await invoke<OrchTask[]>("orch_tasks", { groupId: this.groupId });
    } catch (err) {
      this.toast(String(err));
      return;
    }
    try {
      const summary = await groupSummary(this.groupId);
      // #904: `null` when the backend refuses the group id — the same
      // best-effort degrade the `catch` below already takes, since an empty
      // live set reads every assignee as history rather than breaking the board.
      this.liveAgentIds = new Set(summary?.agents.map((a) => a.id) ?? []);
    } catch {
      // Best-effort enrichment, not core data: the board still renders on
      // the tasks alone, just with every assignee reading as history until
      // the next successful refresh — never a broken board over this.
      this.liveAgentIds = new Set();
    }
    // Best-effort: the gate-aware Approve label (#316) is an enrichment, not
    // the board's primary data — a failed read must not toast-error the whole
    // board (tasks above already succeeded), it just leaves Approve unlabeled.
    this.workflow = await workflowStatus(this.groupId).catch(() => null);
    // Best-effort like liveAgentIds/workflow above (#1091 slice G): the
    // decision-blocked marker is an enrichment on top of the tasks the board
    // already has, not core data — a failed read just means no marker until
    // the next successful refresh.
    this.questions = await questionsList(this.groupId).catch(() => []);
    // Drop any ticked rows that vanished from the board (orchestrator edit,
    // another delete) so the "delete selected" count can't outlive its rows.
    this.selected = retainExisting(this.selected, this.tasks);
    // Same for collapsed containers (#958) — housekeeping rather than
    // correctness (ids are monotonic, so a stale flag can never match a later
    // row): the set is kept to what the board actually holds.
    this.collapsed = retainExisting(this.collapsed, this.tasks);
    // Same for an open picker whose row has gone (#582, #958).
    if (this.picking && !this.tasks.some((t) => t.id === this.picking?.id)) this.picking = null;
    // Same housekeeping for a half-typed grounding link whose row has gone
    // (#1273): a draft nobody can ever see again is a leak that grows with the
    // session.
    this.linkDrafts = retainExistingKeys(this.linkDrafts, this.tasks);
    // NOTE (#1316): `expanded` (notes-expand toggle) belongs on this list too
    // — it's the same never-pruned shape as its siblings above — but is
    // deliberately NOT fixed here. PR #1470 (open, `perf/1317-polled-read-payloads`,
    // commit "fix(tasks): prune the expanded-row sets" — cited by subject, not
    // SHA, since #1470 rebases and a subject survives that where a hash does
    // not) already prunes it with this exact call, plus `expandedLinks`
    // alongside it, because that PR makes `expanded` a wire parameter
    // (`withNotes`) and needed the prune for correctness, not just hygiene.
    // Landing this comment costs a trivial textual conflict with
    // #1470 at this exact insertion point — worth it, since the alternative
    // is silently duplicating that PR's fix with no pointer explaining why one
    // of the six #1316 rows is missing from this one. See this PR's body for
    // the full accounting of all six rows.
    // RESOLVER: once #1470 merges, delete this whole comment — do not keep it
    // alongside the two lines it now describes as still-missing.
    this.render();
  }

  private async addTask(): Promise<void> {
    const title = this.addInput.value.trim();
    if (!title) return;
    this.addInput.value = "";
    await this.mutate(invoke("orch_upsert_task", { groupId: this.groupId, title }));
  }

  /** Reflect the archive on the three cleared-item buttons (#1152) — called
   *  from render() so every label matches the board it sits above.
   *
   *  The two counts are deliberately different questions: the clear button
   *  offers what it would archive NOW (`clearableCount` — done, not already
   *  archived), while the toggle names what is actually being hidden
   *  (`clearedIds` — the closure, so a cleared container still holding live work
   *  is not counted as out of sight when it isn't). */
  private updateCleared(): void {
    const clearable = clearableCount(this.tasks);
    this.clearDoneBtn.hidden = clearable === 0;
    this.clearDoneBtn.textContent = `📥 clear done (${clearable})`;
    this.clearDoneBtn.title =
      `Clear ${clearable} done item${clearable === 1 ? "" : "s"} out of this list. ` +
      `Nothing is deleted: they stay in the board file and the audit log, and 👁 brings them back.`;

    const archived = clearedIds(this.tasks).size;
    this.showClearedBtn.hidden = archived === 0;
    this.showClearedBtn.classList.toggle("active", this.showCleared);
    this.showClearedBtn.textContent = this.showCleared
      ? `👁 hide cleared (${archived})`
      : `👁 show cleared (${archived})`;
    this.showClearedBtn.title = this.showCleared
      ? `Hide the ${archived} cleared item${archived === 1 ? "" : "s"} again`
      : `Show the ${archived} cleared item${archived === 1 ? "" : "s"} — they are still on the board, just out of the way`;

    // Only offered while they are on screen — see the ctor comment.
    this.restoreClearedBtn.hidden = archived === 0 || !this.showCleared;
    this.restoreClearedBtn.textContent = `↩ restore all (${archived})`;
    // "All" means the rows the toggle beside it is hiding — the same set, so
    // the two counts can never disagree about what this button acts on. A
    // cleared container still holding live work is not in that set (it never
    // left the list); its own ↩ is how it comes back.
    this.restoreClearedBtn.title =
      `Bring back all ${archived} item${archived === 1 ? "" : "s"} the 👁 toggle hides`;
  }

  /** Archive every clearable done row in one backend call. Non-destructive and
   *  reversible, so no confirm step: the rows stay in `tasks.json` with a
   *  `cleared_ms` stamp, the write is audited, and 👁 / ↩ undo it. */
  private onClearDone(): void {
    void this.mutate(invoke("orch_clear_done_tasks", { groupId: this.groupId }));
  }

  /** Un-archive everything currently hidden, in one backend call. */
  private onRestoreAll(): void {
    const ids = [...clearedIds(this.tasks)];
    if (ids.length === 0) return;
    void this.mutate(invoke("orch_restore_cleared_tasks", { groupId: this.groupId, ids }));
  }

  /** Reflect the current done-count on the batch-delete button, resetting any
   *  pending confirm — called from render() so the label always matches the
   *  board (and a stale "sure?" can't linger after the set changes). */
  private updateDeleteDone(): void {
    clearTimeout(this.deleteDoneTimer);
    delete this.deleteDoneBtn.dataset.confirm;
    const n = doneCount(this.tasks);
    this.deleteDoneBtn.hidden = n === 0;
    this.deleteDoneBtn.textContent = `🗑 done (${n})`;
    // Every done row, cleared ones included — the archive is not a safe place
    // this button spares, and a count that quietly excluded it would understate
    // what one confirm click is about to destroy.
    this.deleteDoneBtn.title =
      `Delete all ${n} done task${n === 1 ? "" : "s"}, cleared ones included — permanent, ` +
      `and the orchestrator is notified once`;
  }

  /** Two-click confirm, then delete every done task in one backend call. The
   *  batch is a single operation so the orchestrator gets ONE board-change
   *  notice, not one per task (#120). */
  private onDeleteDone(): void {
    if (this.deleteDoneBtn.dataset.confirm) {
      clearTimeout(this.deleteDoneTimer);
      delete this.deleteDoneBtn.dataset.confirm;
      void this.mutate(invoke("orch_delete_done_tasks", { groupId: this.groupId }));
      return;
    }
    const n = doneCount(this.tasks);
    this.deleteDoneBtn.dataset.confirm = "1";
    this.deleteDoneBtn.textContent = `delete ${n}?`;
    this.deleteDoneTimer = window.setTimeout(() => this.updateDeleteDone(), 2500);
  }

  /** Reflect the current selection size on the delete-selected button and reset
   *  any pending confirm — called from render() so the label tracks the (pruned)
   *  selection and a stale "sure?" can't linger after the set changes. */
  private updateDeleteSelected(): void {
    clearTimeout(this.deleteSelectedTimer);
    delete this.deleteSelectedBtn.dataset.confirm;
    const n = this.selected.size;
    this.deleteSelectedBtn.hidden = n === 0;
    this.deleteSelectedBtn.textContent = `🗑 selected (${n})`;
    this.deleteSelectedBtn.title = `Delete the ${n} selected task${n === 1 ? "" : "s"} — the orchestrator is notified once`;
  }

  /** Two-click confirm, then delete every selected task in one backend call —
   *  by id, so exactly the ticked rows go (unknown ids are skipped backend-side
   *  if the board shifted). One coalesced board-change notice for the batch,
   *  mirroring "delete all done" (#120). Selection is cleared here; the refresh
   *  that follows the delete re-prunes it anyway. */
  private onDeleteSelected(): void {
    if (this.deleteSelectedBtn.dataset.confirm) {
      clearTimeout(this.deleteSelectedTimer);
      delete this.deleteSelectedBtn.dataset.confirm;
      const ids = [...this.selected];
      this.selected = new Set();
      void this.mutate(invoke("orch_delete_tasks", { groupId: this.groupId, ids }));
      return;
    }
    const n = this.selected.size;
    this.deleteSelectedBtn.dataset.confirm = "1";
    this.deleteSelectedBtn.textContent = `delete ${n}?`;
    this.deleteSelectedTimer = window.setTimeout(() => this.updateDeleteSelected(), 2500);
  }

  /** Reflect the *approvable* part of the selection on the bulk-approve button
   *  — ticking a `queued` row must not inflate a count of merge grants the
   *  human is about to issue (#507). Hidden when nothing ticked is at the
   *  gate, so the affordance only appears when it can actually do something. */
  private updateApproveSelected(): void {
    const picked = approvableSelection(this.selected, this.tasks);
    const n = picked.length;
    // The grant count is the LINKED-PR count, not the selection size — a gate
    // row with no PR is approved but never granted, so a tooltip promising one
    // grant per selected row would claim authority the backend won't issue.
    const grants = grantableCount(picked);
    this.approveSelectedBtn.hidden = n === 0;
    this.approveSelectedBtn.textContent = `✓ Approve selected (${n})`;
    this.approveSelectedBtn.title =
      `Approve the ${n} selected merge-gate task${n === 1 ? "" : "s"}: ` +
      (grants === 0
        ? "none has a PR linked, so no merge is authorized"
        : `${grants} one-time merge grant${grants === 1 ? "" : "s"} ` +
          `(one per linked PR, single-use, ~30 min)`) +
      `, and the orchestrator is notified once for the batch`;
  }

  /** Bulk merge-gate approve: one modal listing exactly what is about to be
   *  authorized, with an optional per-task note on each row, then ONE backend
   *  call. Same authority as clicking Approve on each row — N per-PR one-time
   *  grants — delivered as a single consolidated notice (#507). */
  private onApproveSelected(): void {
    if (this.dialogEl) return; // one dialog at a time
    const picked = approvableSelection(this.selected, this.tasks);
    if (picked.length === 0) return;
    const withPr = grantableCount(picked);

    const overlay = el("div", "tasks-dialog");
    const box = el("div", "tasks-dialog-box");
    box.append(
      el("div", "tasks-dialog-title", `Approve ${picked.length} selected — allow ${withPr} merge${withPr === 1 ? "" : "s"}`)
    );
    box.append(
      el(
        "div",
        "tasks-dialog-note",
        `${withPr === 0
          ? "None of these has a PR linked, so no merge is authorized — they are marked done and the orchestrator is told."
          : `This authorizes exactly one merge of each of the ${withPr} linked PR${withPr === 1 ? "" : "s"} ` +
            "(a separate single-use grant per PR, each expiring in ~30 min)."} ` +
          "The orchestrator is notified once for the whole batch. Notes below are optional."
      )
    );
    // #316, same as the single-approve dialog: an armed workflow gate refuses
    // the merge whatever the human grants here. Say which of the selected rows
    // that applies to, so a batch can't hide it behind a count.
    const gated = this.workflow
      ? picked.filter((t) => !approveWillMerge(this.workflow as WorkflowStatus, t).ok)
      : [];
    if (gated.length > 0) {
      box.append(
        el(
          "div",
          "tasks-dialog-note gate-warn",
          `The workflow merge gate will refuse ${gated.length} of these (${gated
            .map((t) => t.id)
            .join(", ")}). ${gateExitsMessage()}`
        )
      );
    }

    // One row per task: what it is, and its own note. Per-task rather than one
    // shared note because the notes ride to the orchestrator attached to their
    // PR ("squash this one", "rebase that one") — a single box for a batch
    // would force the human to write the attribution by hand.
    const list = el("div", "tasks-dialog-list");
    const inputs = new Map<string, HTMLInputElement>();
    for (const t of picked) {
      const row = el("div", "tasks-dialog-row");
      row.append(el("div", "tasks-dialog-row-title", `${t.id} — ${t.title}${t.pr ? ` (PR ${t.pr})` : " (no PR)"}`));
      const input = document.createElement("input");
      input.type = "text";
      input.className = "dlg-input";
      input.placeholder = "Optional note for this one — e.g. \"squash-merge\".";
      input.spellcheck = false;
      inputs.set(t.id, input);
      row.append(input);
      list.append(row);
    }

    const actions = el("div", "dlg-actions");
    const cancel = el("button", "dlg-btn", "Cancel") as HTMLButtonElement;
    const confirm = el("button", "dlg-btn primary", `Approve ${picked.length}`) as HTMLButtonElement;
    actions.append(cancel, confirm);
    box.append(list, actions);
    overlay.append(box);

    const close = () => {
      overlay.remove();
      this.dialogEl = null;
    };
    const submit = () => {
      close();
      const items = picked.map((t) => ({
        id: t.id,
        comment: normalizeComment(inputs.get(t.id)?.value ?? ""),
      }));
      // Untick only what was approved: rows ticked for a delete but not at the
      // gate stay selected, so the batch-delete count survives this action.
      for (const t of picked) this.selected.delete(t.id);
      void this.mutate(approveTasks(this.groupId, items));
    };
    cancel.addEventListener("click", close);
    confirm.addEventListener("click", submit);
    // Keep keystrokes off the underlying terminal; Esc cancels, Ctrl/⌘+Enter
    // confirms — same as the single-approve dialog, and plain Enter is left
    // inert on purpose: with one note field per row it would be ambiguous
    // which row the human meant to finish, and the action issues real grants.
    box.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") close();
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) submit();
    });
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) close();
    });

    this.dialogEl = overlay;
    this.el.appendChild(overlay);
    inputs.values().next().value?.focus();
  }

  /** Open a task's issue/PR reference — or a grounding link's URL (#1273) — in
   *  the default browser.
   *
   *  `kind` only ever picks between the `/issues/N` and `/pull/N` path segments
   *  backend-side, and `resolve_ref_url` returns an http(s) value VERBATIM
   *  before it consults `kind` at all. So a URL passes `"link"`: it changes
   *  nothing about where the click lands, and it keeps the audit line honest
   *  rather than filing every grounding URL as an issue open. */
  private openRef(kind: "issue" | "pr" | "link", value: string): void {
    invoke("orch_open_ref", { groupId: this.groupId, kind, value }).catch((err) =>
      this.toast(String(err))
    );
  }

  /** Merge-gate "approve & allow merge": a modal confirm that makes explicit
   *  this AUTHORIZES the merge (a real one-time grant, not just a status flip),
   *  with an optional instructions box delivered to the orchestrator. Empty
   *  comment = plain approve (grant only). The modal step is the confirm — a
   *  bare click never issues a grant. */
  private approveWithComment(t: OrchTask): void {
    if (this.dialogEl) return; // one dialog at a time
    const overlay = el("div", "tasks-dialog");
    const box = el("div", "tasks-dialog-box");
    box.append(
      el("div", "tasks-dialog-title", `${t.pr ? "Approve & allow merge" : "Approve"} — ${t.id}`)
    );
    box.append(
      el(
        "div",
        "tasks-dialog-note",
        t.pr
          ? "This authorizes exactly one merge of this PR (single-use grant, expires in ~30 min) " +
              "and tells the orchestrator to merge. Add optional instructions, or leave empty to just approve."
          : "This marks the item done and tells the orchestrator. No PR is linked, so no merge is " +
              "authorized. Add optional instructions, or leave empty to just approve."
      )
    );
    // #316: a human Approve grant is never what opens a workflow-gated merge
    // (#197/#222) — say so again here, not just on the button label, since a
    // human who clicked through to this dialog is the one about to act on it.
    const gate = this.workflow ? approveWillMerge(this.workflow, t) : { ok: true };
    if (!gate.ok && gate.reason) {
      const sentence = gate.reason[0].toUpperCase() + gate.reason.slice(1);
      box.append(el("div", "tasks-dialog-note gate-warn", `${sentence}. ${gateExitsMessage()}`));
    }

    const ta = document.createElement("textarea");
    ta.className = "dlg-input tasks-dialog-text";
    ta.placeholder = "Optional instructions for the agent — e.g. \"squash-merge and delete the branch\".";
    ta.spellcheck = false;
    ta.rows = 3;

    const actions = el("div", "dlg-actions");
    const cancel = el("button", "dlg-btn", "Cancel") as HTMLButtonElement;
    const confirm = el(
      "button",
      "dlg-btn primary",
      t.pr ? "Approve & allow merge" : "Approve"
    ) as HTMLButtonElement;
    actions.append(cancel, confirm);
    box.append(ta, actions);
    overlay.append(box);

    const close = () => {
      overlay.remove();
      this.dialogEl = null;
    };
    const submit = () => {
      close();
      // Empty/whitespace comment → null (grant only, no note).
      void this.mutate(approveTask(this.groupId, t.id, normalizeComment(ta.value)));
    };
    cancel.addEventListener("click", close);
    confirm.addEventListener("click", submit);
    // Keep keystrokes off the underlying terminal; Esc cancels, Ctrl/⌘+Enter confirms.
    ta.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") close();
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) submit();
    });
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) close();
    });

    this.dialogEl = overlay;
    this.el.appendChild(overlay);
    ta.focus();
  }

  /** Merge-gate "request changes": collect findings in a modal, then hand
   *  them to the orchestrator (which routes them back to a worker). */
  private requestChanges(t: OrchTask): void {
    if (this.dialogEl) return; // one at a time
    const overlay = el("div", "tasks-dialog");
    const box = el("div", "tasks-dialog-box");
    box.append(el("div", "tasks-dialog-title", `Request changes on ${t.id}`));

    const ta = document.createElement("textarea");
    ta.className = "dlg-input tasks-dialog-text";
    ta.placeholder = "What needs to change? These findings go to the orchestrator.";
    ta.spellcheck = false;
    ta.rows = 4;

    const actions = el("div", "dlg-actions");
    const cancel = el("button", "dlg-btn", "Cancel") as HTMLButtonElement;
    const send = el("button", "dlg-btn primary", "Send") as HTMLButtonElement;
    actions.append(cancel, send);
    box.append(ta, actions);
    overlay.append(box);

    const close = () => {
      overlay.remove();
      this.dialogEl = null;
    };
    const submit = () => {
      const findings = ta.value.trim();
      if (!findings) {
        ta.focus();
        return;
      }
      close();
      // Record the findings, then reopen the task as working (#339
      // refinement) — state honesty: the board must never keep showing the
      // Approve button on a task that just had changes requested on it.
      void this.mutate(
        invoke("orch_request_changes", { groupId: this.groupId, id: t.id, findings }).then(() =>
          invoke("orch_upsert_task", { groupId: this.groupId, id: t.id, status: REQUEST_CHANGES_STATUS })
        )
      );
    };
    cancel.addEventListener("click", close);
    send.addEventListener("click", submit);
    // Keep keystrokes off the underlying terminal; Esc cancels, Ctrl/⌘+Enter sends.
    ta.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") close();
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) submit();
    });
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) close();
    });

    this.dialogEl = overlay;
    this.el.appendChild(overlay);
    ta.focus();
  }

  private render(): void {
    this.updateCleared();
    this.updateDeleteDone();
    this.updateDeleteSelected();
    this.updateApproveSelected();
    this.renderWipChips();
    this.renderSprintLens();
    this.listEl.replaceChildren();
    if (this.tasks.length === 0) {
      // An empty board has nothing to collapse and nothing to filter, so the
      // strip goes away entirely rather than offering controls over nothing.
      this.renderFilterStrip(0, 0, 0);
      this.listEl.appendChild(el("div", "tasks-empty", "No tasks yet — the orchestrator adds them as work items come in, or add one below."));
      return;
    }
    // Whether this board uses dependencies at all (#582) — computed once for
    // the whole render, since the "ready" mark is a board-level decision (see
    // boardUsesDeps): on a board with no links every queued row is trivially
    // ready and marking them all would say nothing.
    const usesDeps = boardUsesDeps(this.tasks);
    // Same board-level gate for the nesting chrome (#958): a board that nests
    // nothing keeps exactly the rows it has today, with no collapse gutter.
    const usesHierarchy = boardUsesHierarchy(this.tasks);
    // Same board-level gate again, for the grounding-link COUNT (#1273): on a
    // board carrying no links at all, a column of `📎 0` down every row would
    // be chrome saying "no" — the pay-for-what-you-use rule the two gates above
    // already apply. The 📎 button itself stays on every row regardless, since
    // it is the only way a link-free board ever gains its first link.
    const usesLinks = boardUsesLinks(this.tasks);
    // t-N → q-N for every row a PENDING question cites (#1091 slice G) —
    // computed once for the whole render, like usesDeps/usesHierarchy above.
    // `isPending` is decisions.ts's own rule (never re-spelled here — see
    // `blockedTaskMap`'s doc), so a settled question can never leave a stale
    // marker on a row it no longer holds up.
    const blocked = blockedTaskMap(this.questions.filter(isPending));
    // Display order is DERIVED (#958, #1152) — the board array stays flat and
    // its order stays the human's priority order. What is derived from it: the
    // tree (from `parent`), finished subtrees sinking below the live work of
    // their own sibling group, and the archived rows dropping out until 👁.
    // Board-level, computed once for the whole render like `usesDeps`/`blocked`
    // above: which rows are finished subtrees (#1152). Each row's ▲/▼ needs it,
    // and re-deriving it per row would walk the tree once per row.
    const settled = settledIds(this.tasks);
    // The current sprint (#1272), board-level and computed once like the three
    // above — the row badges all ask the same question, and re-deriving it per
    // row would scan the board once per row.
    const current = currentSprint(this.tasks);
    // The ids the `needs you` quick-filter matches — the SAME rule the ❓/👀
    // marker chip is drawn from (`boardMarker`), not a second spelling of it,
    // so the toggle and the chips can never disagree about which rows are
    // waiting on the human.
    const attention = new Set(
      this.tasks.filter((t) => boardMarker(t, blocked) !== null).map((t) => t.id)
    );
    const rows = visibleRows(this.tasks, this.collapsed, this.showCleared, {
      filter: this.filter,
      attention,
    });
    // The universe the FILTER is choosing from — every row except the ones the
    // archive is hiding for its own unrelated reason (#1270 review N4). With
    // 300 of 400 rows cleared and the archive off screen, `tasks.length` made
    // the hint read "12 of 400" when there were only 100 rows to match against.
    // The no-matches line lost the same number in self-review; fixing one site
    // of a pair and leaving the other is the shape the one-rule convention
    // names, so it is the same rule here. `clearedIds` is the SAME closure the
    // archive itself renders from, so the two cannot disagree.
    const universe = this.showCleared
      ? this.tasks.length
      : this.tasks.length - clearedIds(this.tasks).size;
    this.renderFilterStrip(attention.size, rows.length, universe);
    if (rows.length === 0 && filterActive(this.filter)) {
      // Distinct from both empty states below: the board has rows and they are
      // not archived — this filter simply matches none of them. Saying "no
      // tasks yet" here would read as data loss, and saying "everything is
      // cleared" would send the human to the wrong affordance.
      this.listEl.appendChild(
        el(
          "div",
          "tasks-empty",
          "Nothing on this board matches the current filter. Clear it with ✕ above."
        )
      );
      return;
    }
    if (rows.length === 0) {
      // The board is not empty — everything on it is archived. Say that,
      // rather than showing the "no tasks yet" line for a board with 400 rows
      // in it, which would read as data loss.
      this.listEl.appendChild(
        el(
          "div",
          "tasks-empty",
          "Every item on this board is cleared. Nothing was deleted — use 👁 show cleared above to bring it back."
        )
      );
      return;
    }
    for (const row of rows) {
      this.listEl.appendChild(
        this.renderTask(row, usesDeps, usesHierarchy, usesLinks, blocked, settled, current)
      );
    }
    this.drainFocus();
  }

  /** Reflect the board on the tree-view strip (#1270): which chips are lit,
   *  what the collapse buttons can do, and how much the filter is hiding.
   *
   *  Rebuilt per render rather than diffed — it is at most ~15 small elements,
   *  and the alternative is a second model of what is on screen that can drift
   *  from the one `visibleRows` just produced. The search box is NOT rebuilt:
   *  it holds focus and a caret while the human types.
   *
   *  `shown` is the row count `visibleRows` actually returned and `universe`
   *  the number of rows the filter had to choose from, both threaded in rather
   *  than recomputed, so the hint cannot claim a number the list does not
   *  have. */
  private renderFilterStrip(attentionCount: number, shown: number, universe: number): void {
    // Nothing to collapse and nothing to filter on an empty board.
    this.filterEl.hidden = this.tasks.length === 0;
    if (this.tasks.length === 0) return;

    const containers = containerIds(this.tasks);
    const active = filterActive(this.filter);

    // Collapse-all/expand-all exist only on a board that nests something — the
    // same board-level gate the collapse gutter itself uses. While a filter is
    // armed they are OFF, because collapse has no observable effect then (see
    // taskboard.ts's filter rules): a kept container either holds a match, and
    // must expand to show it, or has nothing kept under it at all.
    const noTree = containers.length === 0;
    this.collapseAllBtn.hidden = noTree;
    this.expandAllBtn.hidden = noTree;
    this.collapseAllBtn.disabled = active;
    this.expandAllBtn.disabled = active;
    const frozen = " — collapse resumes when the filter is cleared";
    this.collapseAllBtn.title = active
      ? `Collapse everything${frozen}`
      : `Fold up all ${containers.length} container${containers.length === 1 ? "" : "s"}`;
    this.expandAllBtn.title = active ? `Expand everything${frozen}` : "Unfold everything";

    // The needs-you toggle hides itself when nothing is waiting: a quick filter
    // that can only ever empty the board is worse than absent.
    this.attentionBtn.hidden = attentionCount === 0 && !this.filter.attention;
    this.attentionBtn.classList.toggle("on", this.filter.attention);
    this.attentionBtn.textContent = `❗ needs you (${attentionCount})`;
    this.attentionBtn.title = this.filter.attention
      ? "Showing only items blocked on a decision or parked for a demo — click to show everything"
      : "Show only items blocked on a decision or parked for a demo";

    this.filterChipsEl.replaceChildren();
    // Kind chips only on a board that levels anything (`boardUsesHierarchy`'s
    // argument, applied to the other #1156 field): on a flat board of plain
    // rows every chip but `unlabelled` would match nothing, so the row of them
    // would be pure noise. A kind ALREADY selected keeps its chip regardless,
    // so a persisted filter can always be seen and turned off.
    const kinds = kindFilterChoices(this.tasks);
    const boardHasKinds = this.tasks.some((t) => !!t.kind);
    if (boardHasKinds || this.filter.kind.length > 0) {
      this.filterChipsEl.append(el("span", "filter-label", "level"));
      for (const k of kinds) {
        this.filterChipsEl.append(
          this.familyChip(k, k === UNLABELLED_KIND ? "none" : k, "kind")
        );
      }
    }
    this.filterChipsEl.append(el("span", "filter-label", "status"));
    for (const s of STATUSES) this.filterChipsEl.append(this.familyChip(s, s, "status"));

    // Sprint chips (#1272), on the same board-level gate the kind chips use:
    // a board that runs no sprints would get a row where every chip but
    // `backlog` matches nothing, which is noise. An armed sprint keeps its
    // chips regardless, so a persisted filter can always be seen and cleared.
    if (boardUsesSprints(this.tasks) || this.filter.sprint.length > 0) {
      this.filterChipsEl.append(el("span", "filter-label", "sprint"));
      // The armed values are threaded in so a chip the filter is holding
      // survives its last row being deleted — otherwise the board empties with
      // every chip unlit and nothing to click but the blanket ✕.
      for (const s of sprintFilterChoices(this.tasks, this.filter.sprint)) {
        this.filterChipsEl.append(
          this.familyChip(s, s === BACKLOG_SPRINT ? "backlog" : `#${s}`, "sprint")
        );
      }
    }

    this.clearFilterBtn.hidden = !active;
    this.filterCountEl.hidden = !active;
    this.filterCountEl.textContent = active ? `${shown} of ${universe}` : "";
    this.filterCountEl.title = this.showCleared
      ? "Rows on screen — matches plus the containers above them — out of every item on the board"
      : "Rows on screen — matches plus the containers above them — out of the items this board is currently showing (cleared items are not counted; 👁 brings them back)";
  }

  /** One toggle chip in a filter family. */
  private familyChip(
    value: string,
    label: string,
    family: "kind" | "status" | "sprint"
  ): HTMLElement {
    const on = this.filter[family].includes(value);
    const chip = el("button", `pane-btn filter-chip f-${family}`, label) as HTMLButtonElement;
    if (on) chip.classList.add("on");
    // "sprint #2" and "backlog" read better than "#2 items" / "backlog items",
    // and the sprint family is the one whose labels are not already nouns.
    const what = family === "sprint" ? (value === BACKLOG_SPRINT ? "the backlog" : `sprint ${value}`) : `${label} items`;
    chip.title = on ? `Stop filtering by ${label}` : `Show only ${what}`;
    chip.addEventListener("click", () => {
      const cur = this.filter[family];
      const next = on ? cur.filter((v) => v !== value) : [...cur, value];
      this.filter = { ...this.filter, [family]: next };
      this.onViewChanged();
    });
    return chip;
  }

  /** The header sprint lens (#1272): which sprint the board is on, how far it
   *  has got, and the two things a human does about it.
   *
   *  Every number here is DERIVED from the rows, never stored — the board has
   *  no sprint state of its own, and design note §5 is explicit that there
   *  must not be a second authority the rows can disagree with. So this is
   *  `currentSprint` + `sprintProgress` read on every render, exactly as the
   *  backend derives `current_sprint` on every read.
   *
   *  The progress denominator counts the sprint's WHOLE scope, archived rows
   *  included. A cleared row is still on the board and still in its sprint
   *  (`clearedIds` hides rows from the working view, it does not remove them),
   *  so excluding them would make the fraction disagree with `currentSprint`
   *  about which rows a sprint contains — the one-rule asymmetry, in the one
   *  place a human reads the sprint's state off a single line. */
  private renderSprintLens(): void {
    const uses = boardUsesSprints(this.tasks);
    this.sprintEl.hidden = !uses;
    if (!uses) return;
    const current = currentSprint(this.tasks);
    if (current === null) {
      // Rows carry sprints, but every one of them is done. Say so rather than
      // hiding the lens: "this board finished its sprints" is a real state and
      // a different one from "this board runs no sprints".
      this.sprintLensBtn.textContent = "sprints — all done";
      this.sprintLensBtn.classList.remove("on");
      this.sprintLensBtn.disabled = true;
      this.sprintLensBtn.title =
        "Every item carrying a sprint is done. Use the sprint chips below to look back at one, " +
        "or put an item in a new sprint to start the next.";
      this.sprintAdvanceBtn.hidden = true;
      return;
    }
    const { done, total } = sprintProgress(this.tasks, current);
    const armed = this.sprintLensArmed(current);
    this.sprintLensBtn.disabled = false;
    this.sprintLensBtn.textContent = `sprint ${current} — ${done}/${total} done`;
    this.sprintLensBtn.classList.toggle("on", armed);
    this.sprintLensBtn.title = armed
      ? `Showing only sprint ${current} — click to show the whole board again`
      : `Sprint ${current} is the lowest sprint with unfinished work in it (${done} of ${total} ` +
        `items done, archived ones counted). Click to show only its items.`;

    const plan = sprintAdvance(this.tasks, current);
    this.sprintAdvanceBtn.hidden = false;
    this.sprintAdvanceBtn.disabled = plan.to === null;
    this.sprintAdvanceBtn.title =
      plan.to === null
        ? `Sprint ${current} is the highest number a sprint can have, so there is nothing to ` +
          "move its remaining items into."
        : `Move the ${plan.rows.length} unfinished item${plan.rows.length === 1 ? "" : "s"} in ` +
          `sprint ${current} into sprint ${plan.to} — you will see exactly which ones first, ` +
          "and each is written on its own";
  }

  /** Is the sprint lens the ONLY thing the sprint family is filtering on?
   *  Reading it as "contains the current sprint" would light the lens up while
   *  three other sprints were also showing, and clicking it would then clear
   *  them without the human asking. */
  private sprintLensArmed(current: number): boolean {
    return this.filter.sprint.length === 1 && this.filter.sprint[0] === String(current);
  }

  /** The lens click: show only the current sprint, or stop.
   *
   *  It writes the concrete NUMBER, not a "current sprint" sentinel, and that
   *  is a decision rather than a shortcut. The filter is persisted per group
   *  (`boardprefs.ts`), and a stored `current` would silently change what the
   *  board shows the moment a sprint completed — the human would come back to
   *  a board they never re-aimed, with the rows they were working on gone.
   *  That is the same never-silent posture design note §5 takes on roll-over.
   *  Re-clicking the lens after an advance re-aims it, which is a gesture. */
  private toggleSprintLens(): void {
    const current = currentSprint(this.tasks);
    if (current === null) return;
    this.filter = {
      ...this.filter,
      sprint: this.sprintLensArmed(current) ? [] : [String(current)],
    };
    this.onViewChanged();
  }

  /** The advance affordance (#1272 §5): move every unfinished row out of the
   *  current sprint and into the next one.
   *
   *  Never silent, and never a bulk operation. The dialog names each row that
   *  would move — `blocked` ones included, since they are exactly what a silent
   *  roll-over would sweep up — and confirming performs ONE `orch_upsert_task`
   *  per row, so every move lands in the audit log on its own. There is no
   *  `advance_sprint` backend call to invoke: per-row upsert already expresses
   *  it, and a bulk one would be a second way to write `sprint`.
   *
   *  The list and the target number both come from `sprintAdvance`, one call,
   *  so what the human approved cannot differ from what is written. */
  private onAdvanceSprint(): void {
    if (this.dialogEl) return; // one dialog at a time
    const current = currentSprint(this.tasks);
    if (current === null) return;
    const { to, rows } = sprintAdvance(this.tasks, current);
    if (to === null || rows.length === 0) return;

    const overlay = el("div", "tasks-dialog");
    const box = el("div", "tasks-dialog-box");
    box.append(
      el(
        "div",
        "tasks-dialog-title",
        `Move ${rows.length} unfinished item${rows.length === 1 ? "" : "s"} from sprint ${current} to sprint ${to}`
      )
    );
    box.append(
      el(
        "div",
        "tasks-dialog-note",
        `Sprint ${current} stays current until its last unfinished item leaves it — this is ` +
          `what moves them. Each item below is written on its own, so each shows up in the ` +
          `audit log separately, and the orchestrator sees the board change. Nothing else on ` +
          `the board is touched, and done items keep sprint ${current}.`
      )
    );
    // An ARCHIVED row that is not done is on this list, and has to be: it is
    // open work, so `currentSprint` counts it and it holds the sprint open —
    // leaving it behind would make the advance fail to advance anything. It is
    // labelled rather than left looking like a live row, since the human is not
    // otherwise looking at it (#1152).
    const cleared = clearedIds(this.tasks);
    const list = el("div", "tasks-dialog-list");
    for (const t of rows) {
      const row = el("div", "tasks-dialog-row");
      row.append(
        el(
          "div",
          "tasks-dialog-row-title",
          `${t.id} — ${t.title} (${t.status}${cleared.has(t.id) ? ", cleared" : ""})`
        )
      );
      list.append(row);
    }

    const actions = el("div", "dlg-actions");
    const cancel = el("button", "dlg-btn", "Cancel") as HTMLButtonElement;
    const confirm = el("button", "dlg-btn primary", `Move ${rows.length} to sprint ${to}`) as HTMLButtonElement;
    actions.append(cancel, confirm);
    box.append(list, actions);
    overlay.append(box);

    const close = () => {
      overlay.remove();
      this.dialogEl = null;
    };
    // Guarded, and this is not belt-and-braces: the confirm button holds focus,
    // so pressing Enter on it fires the button's own click AND any Enter
    // handler on the box — two calls, two rounds of writes, and a roll-over
    // recorded twice in the audit log. The keydown below therefore handles
    // Escape only, and this flag closes the same door for any other double
    // path. (The approve dialog next door needs Ctrl+Enter instead because it
    // has text fields; this one has none.)
    let submitted = false;
    const submit = () => {
      if (submitted) return;
      submitted = true;
      close();
      // One write per row, in board order. Sequential and not `Promise.all`:
      // `tasks_lock` serializes board writes anyway, and a failure part-way
      // through must leave the rows it already moved moved — a half-applied
      // roll-over is recoverable and visible, where a racing batch would make
      // the audit order meaningless.
      void this.mutate(
        rows.reduce(
          (chain, t) =>
            chain.then(() =>
              invoke("orch_upsert_task", { groupId: this.groupId, id: t.id, sprint: to })
            ),
          Promise.resolve() as Promise<unknown>
        )
      );
    };
    cancel.addEventListener("click", close);
    confirm.addEventListener("click", submit);
    // Keep keystrokes off the terminal underneath. Escape only — Enter is the
    // focused confirm button's own business (see `submitted` above).
    box.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") close();
    });
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) close();
    });

    this.dialogEl = overlay;
    this.el.appendChild(overlay);
    confirm.focus();
  }

  /** The board's WIP chips (#1175) — `review 3/3` on the header, one per cap
   *  the repo declares.
   *
   *  On the HEADER and not on a per-status column, because this board has no
   *  status columns: it is one priority-ordered list whose order is meaning
   *  (top = next). Inventing columns to hang a chip on would reorder the board
   *  around a feature most repos never turn on. #1105's kanban view is where
   *  the columns arrive, and `wipChips` already returns exactly what a column
   *  header needs.
   *
   *  Rendered from `this.workflow`, which `refresh()` reads in the same pass as
   *  the rows — so a chip is at most one refresh out of step with the list
   *  under it, the same best-effort enrichment the live-agent set and the
   *  question markers already are. It is a count beside a board that is itself
   *  live; it authorizes nothing. */
  private renderWipChips(): void {
    const chips = wipChips(this.workflow?.wip);
    this.wipEl.replaceChildren();
    this.wipEl.hidden = chips.length === 0;
    for (const c of chips) {
      const chip = el("span", `wip-chip wip-${c.fill}`, c.text);
      chip.title = c.title;
      if (c.enforce) chip.classList.add("wip-enforced");
      this.wipEl.appendChild(chip);
    }
  }

  /** Bring a requested row into view and flash it (#1091 slice C).
   *
   *  Drained LAST in a render, once the rows exist, and consumed — so an
   *  ordinary refresh never yanks the viewport back to a row the human has
   *  already scrolled away from.
   *
   *  A target that names no row on this render used to be a silent no-op, which
   *  conflates two different situations (#1152 review round 1, finding 4). The
   *  request is CONSUMED either way, so a deep link from the NEEDS-YOU panel
   *  onto a row that is merely off-screen simply appeared to do nothing:
   *  already true for a row inside a collapsed container (#958), and #1152 adds
   *  a second way — a cleared row, hidden until 👁. So the two cases are now
   *  told apart by the board itself rather than by the human's guess:
   *
   *  - the id names NO row on the board — still a silent no-op, because the
   *    task really can be deleted between the request and the render, and that
   *    is not worth an error;
   *  - the id names a row that IS on the board but is not rendered — say so,
   *    and say which affordance brings it back.
   *
   *  A toast rather than an auto-reveal: flipping the human's view (expanding a
   *  container, turning the archive on) as a side effect of a click elsewhere
   *  is a bigger behaviour than the problem, and this at least makes the state
   *  legible. Covers the pre-existing collapsed case too, not just the new one. */
  private drainFocus(): void {
    const target = this.opts.takeFocus?.() ?? null;
    if (!target) return;
    const row = this.listEl.querySelector<HTMLElement>(`[data-item-id="${CSS.escape(target)}"]`);
    if (!row) {
      // Which of the three cases this is lives in `focusMiss` (DOM-free, and
      // tested there); this only turns it into words.
      const miss = focusMiss(target, this.tasks);
      if (miss === "cleared") {
        this.toast(`${target} is cleared — use 👁 show cleared above to bring it back into view.`);
      } else if (miss === "hidden") {
        this.toast(`${target} is on the board but hidden right now — expand the container it sits in.`);
      }
      return;
    }
    row.scrollIntoView({ block: "nearest" });
    row.classList.add("task-row-focused");
    window.setTimeout(() => row.classList.remove("task-row-focused"), 1600);
  }

  /** The dependency line under a task's row (#582): what is blocking it, what
   *  it is merely related to, and — when open — the picker for adding another
   *  dep. Returns null (no line at all) when the task has neither links nor an
   *  open picker, so a board that uses no dependencies keeps exactly the row
   *  height it has today.
   *
   *  Every edit here sends the WHOLE `deps` array through the board's existing
   *  `orch_upsert_task` path (the backend's array args are replace-or-untouched,
   *  never a delta), so it inherits that path's validation and error surfacing:
   *  a cycle, or an id that stopped existing between render and click, is
   *  refused backend-side and lands in this view's toast rather than being
   *  second-guessed here. */
  private renderLinks(t: OrchTask): HTMLElement | null {
    const deps = t.deps ?? [];
    const related = t.related ?? [];
    const picking = this.picking?.id === t.id;
    if (deps.length === 0 && related.length === 0 && !picking) return null;

    const line = el("div", "task-links");

    if (deps.length > 0) {
      line.appendChild(el("span", "task-links-label", "blocked by"));
      for (const id of deps) {
        // met / unmet / missing — the third is only reachable from a
        // hand-edited tasks.json (the backend validates ids on write and
        // strips them on delete), and it reads as its own state because the
        // fix is different: not "wait", but "this link names nothing".
        const state = depState(id, this.tasks);
        const dep = this.tasks.find((x) => x.id === id);
        const chip = el("span", `task-chip dep ${state}`);
        chip.appendChild(
          el("span", "dep-mark", state === "met" ? "✓" : state === "missing" ? "⚠" : "✗")
        );
        chip.appendChild(el("span", "dep-id", id));
        chip.title =
          state === "met"
            ? `${id} "${dep?.title ?? ""}" is done — this dependency is satisfied`
            : state === "missing"
              ? `${id} names no task on this board, so it counts as unmet and this task ` +
                `can never read as ready. Remove the link (✕) or re-create that task.`
              : `${id} "${dep?.title ?? ""}" is ${dep?.status ?? "?"} — this task waits ` +
                `until it is done`;
        const rm = el("button", "task-dep-remove", "✕") as HTMLButtonElement;
        rm.title = `Remove the dependency on ${id}`;
        rm.addEventListener("click", () =>
          void this.writeLinkArray(t, { kind: "dep-remove", id })
        );
        chip.appendChild(rm);
        line.appendChild(chip);
      }
    }

    // Read-only: `orch_upsert_task` deliberately takes `deps` and not
    // `related` — the orchestrator maintains see-also links through its own
    // tools, and the board renders them so the human can see the annotation
    // without it silently affecting anything.
    if (related.length > 0) {
      line.appendChild(el("span", "task-links-label", "see also"));
      for (const id of related) {
        const rel = this.tasks.find((x) => x.id === id);
        const chip = el("span", "task-chip related", id);
        chip.title = rel
          ? `${id} "${rel.title}" (${rel.status}) — a non-blocking link set by the orchestrator; ` +
            `it never affects whether this task is ready`
          : `${id} names no task on this board`;
        line.appendChild(chip);
      }
    }

    if (picking) {
      const field = this.picking?.field;
      line.appendChild(
        field === "parent"
          ? this.renderParentPicker(t)
          : field === "kind"
            ? this.renderKindPicker(t)
            : field === "sprint"
              ? this.renderSprintPicker(t)
              : this.renderDepPicker(t)
      );
    }
    return line;
  }


  /** Act on a click on one grounding link (#1273).
   *
   *  The decision is `linkOpenPlan`'s, not this method's: an issue/PR ref and
   *  an http(s) URL go to `orch_open_ref` — the only path to the external
   *  opener, which refuses a non-http(s) URL itself — and EVERYTHING else is
   *  copied instead. A link target is agent-writable free text (design note
   *  §4 validates its shape and never its meaning), so "copy unless it is one
   *  of the two shapes the board can name" is the safe default rather than a
   *  gap; copying a repo path is also what the NEEDS-YOU panel already does
   *  with a `demo_path`, and it is the plan's own stated fallback. */
  private openLink(target: string): void {
    const plan = linkOpenPlan(target);
    if (plan.action === "open") {
      this.openRef(plan.kind, plan.value);
      return;
    }
    void writeClipboard(plan.text).then((ok) =>
      this.toast(
        ok
          ? `Copied ${plan.text}`
          : "Copy failed — the clipboard is unavailable, so this target could not be copied."
      )
    );
  }

  /** The grounding-link detail (#1273): the typed list, and the editor that
   *  adds and removes entries.
   *
   *  Every write goes through `orch_upsert_task`'s `links` argument, which
   *  REPLACES the whole array — so both edits compose the new list from the one
   *  that was rendered (`withArtifactLink` / `withoutArtifactLinkAt`) and send
   *  it whole. Composing from the rendered list is exactly what an agent's
   *  concurrent write makes unsafe, so since #1349 both go through
   *  `writeLinkArray`, which pairs the composed array with the `link_etag` that
   *  came with the same row; a row that moved underneath refuses the write
   *  rather than silently replacing what it never saw. That is the same
   *  replace-or-untouched contract `deps` already
   *  has on this view, and it inherits the same error surfacing: the type
   *  vocabulary, the length caps, control characters and the refusal when a
   *  target names a live board task are all validated inside the backend's
   *  lock, and their errors reach the human through `mutate`'s toast. None of
   *  those rules is re-spelled here — a second copy could only ever disagree
   *  with the one that teaches. */
  private renderGroundings(t: OrchTask): HTMLElement {
    const links = t.links ?? [];
    const box = el("div", "task-groundings");
    const head = el("div", "task-groundings-head");
    head.appendChild(el("span", "task-links-label", "grounding"));
    head.appendChild(
      el(
        "span",
        "task-groundings-hint",
        links.length > 0
          ? "read first by any agent opened against this task"
          : "nothing recorded yet — an agent opened against this task gets no grounding section"
      )
    );
    box.appendChild(head);

    for (const [i, link] of links.entries()) {
      const row = el("div", "task-grounding");
      const known = (LINK_TYPES as readonly string[]).includes(link.type);
      const type = el("span", `task-chip ground-type lt-${known ? link.type : "unknown"}`);
      type.appendChild(el("span", "ground-glyph", linkTypeIcon(link.type)));
      type.appendChild(el("span", "ground-type-name", link.type));
      type.title = known
        ? `A ${link.type} link`
        : `${link.type} is not one of ${LINK_TYPES.join(" | ")} — only a hand-edited tasks.json can hold it`;
      row.appendChild(type);

      // The target is the payload, so the whole entry is the click target.
      // `linkOpenPlan` decides what that click does, and the tooltip says which
      // it will be — a click that silently copies when the human expected a
      // browser is the confusing half of a tolerate-everything target field.
      const plan = linkOpenPlan(link.target);
      const open = el("button", "task-grounding-open", linkDisplayText(link)) as HTMLButtonElement;
      open.title =
        (link.label ? `${link.label}\n${link.target}` : link.target) +
        (plan.action === "open" ? "\n\nClick to open it" : "\n\nClick to copy it");
      open.addEventListener("click", () => this.openLink(link.target));
      row.appendChild(open);
      // The raw target beside a labelled link: a label is a gloss, and a gloss
      // that hides what it points at makes the human unfold nothing to find out.
      if ((link.label ?? "").trim()) row.appendChild(el("span", "task-grounding-target", link.target));

      const rm = el("button", "task-dep-remove", "✕") as HTMLButtonElement;
      rm.title = `Remove this ${link.type} link`;
      rm.addEventListener("click", () =>
        // BY INDEX — a row may carry one target twice (nothing dedupes this
        // array), and the human clicked exactly one ✕. That is sound only
        // because the write carries this row's `link_etag` (#1349): an index
        // names what was clicked exactly while the list it was read from is
        // still the list being written, and the backend refuses it otherwise.
        // It is also why this one edit is never auto-re-applied — see
        // `retriesAfterStale`.
        void this.writeLinkArray(t, { kind: "link-remove-at", index: i })
      );
      row.appendChild(rm);
      box.appendChild(row);
    }

    // At the cap the editor goes INERT and says why, rather than offering an
    // add whose write the backend must reject — `MAX_SPRINT` and the ⏭ button
    // take the same position for the same reason. A hand-edited board can sit
    // ABOVE the cap; `artifactLinksAtCap` covers that too, so the human is told
    // to remove one rather than shown a form that cannot succeed.
    if (artifactLinksAtCap(links)) {
      box.appendChild(
        el(
          "div",
          "task-groundings-full",
          `This task is at the ${MAX_ARTIFACT_LINKS}-link limit — remove one to add another.`
        )
      );
      return box;
    }

    const add = el("div", "task-grounding-add");
    // Seeded from the row's draft, so a render that lands mid-typing — a
    // background board refresh, or the resync after a refused write — puts back
    // exactly what was in the boxes. `linkDrafts` is the authority; the elements
    // are a view of it.
    const draftState = this.linkDrafts.get(t.id);
    const type = document.createElement("select");
    type.className = "task-dep-picker ground";
    for (const lt of LINK_TYPES) {
      const opt = document.createElement("option");
      opt.value = lt;
      opt.textContent = `${linkTypeIcon(lt)} ${lt}`;
      type.appendChild(opt);
    }
    // `DEFAULT_LINK_TYPE`, never the literal "requirement": the vocabulary is
    // read out of the Rust source, so a reorder or a rename there must not leave
    // this line silently naming nothing — and the pristine test below compares
    // against the SAME const, because a default seeded here and a default
    // assumed there drifting apart is exactly what #1273 N4 was. A draft's
    // remembered type wins, unless the vocabulary no longer has it.
    type.value =
      draftState && (LINK_TYPES as readonly string[]).includes(draftState.type)
        ? draftState.type
        : DEFAULT_LINK_TYPE;
    type.title = "What kind of grounding this is — it decides nothing, it tells the next agent what it is reading";

    const target = document.createElement("input");
    target.className = "dlg-input task-grounding-target-input";
    target.placeholder = "#123, doc/design/x.md, or a URL";
    target.spellcheck = false;
    target.value = draftState?.target ?? "";
    target.title =
      "What the link points AT. Never checked for existence — the board stays editable offline — " +
      "but a target naming a task on THIS board is refused: that is what dependencies are for.";

    const label = document.createElement("input");
    label.className = "dlg-input task-grounding-label-input";
    label.placeholder = "label (optional)";
    label.spellcheck = false;
    label.value = draftState?.label ?? "";
    label.title = "A one-line gloss shown instead of the bare target";

    // Every keystroke and type change writes back, so what the view holds and
    // what is on screen cannot diverge.
    const remember = () => {
      const draft = { type: type.value, target: target.value, label: label.value };
      // Every field, not just the two text ones. A `<select>` raises no
      // `isEditing()`, so a type picked and not remembered here is reverted on
      // screen by the next background render and the link then lands under a
      // type nobody chose (#1273 N4). `linkDraftIsPristine` is the one rule.
      if (linkDraftIsPristine(draft)) this.linkDrafts.delete(t.id);
      else this.linkDrafts.set(t.id, draft);
    };
    target.addEventListener("input", remember);
    label.addEventListener("input", remember);
    type.addEventListener("change", remember);

    const submit = () => {
      const draft = artifactLinkDraft(type.value, target.value, label.value);
      // No target typed yet is not an error, it is an unfinished form — say
      // nothing and leave the caret where it is.
      if (!draft) {
        target.focus();
        return;
      }
      // The boxes are emptied ONLY once the write has actually landed. On a
      // refusal `mutate` toasts the backend's error — which for a target naming
      // a board task is the sentence that teaches `deps`/`related` — and
      // resyncs; the draft survives that render, so the human can act on what
      // they were just told instead of retyping it first.
      void this.writeLinkArray(t, { kind: "link-add", link: draft }, () => {
        this.linkDrafts.delete(t.id);
        // Render rather than clearing these two elements: the success event
        // may have rebuilt the form already, and blanking the detached copy
        // would leave the live one still holding the text. One authority.
        this.linkFocus = t.id;
        this.render();
      });
    };
    const btn = el("button", "dlg-btn", "Add link") as HTMLButtonElement;
    btn.addEventListener("click", submit);
    // Keep keystrokes off the terminal underneath — every inline editor in this
    // view does this — and let Enter commit from either field.
    for (const field of [type, target, label] as HTMLElement[]) {
      field.addEventListener("keydown", (e: KeyboardEvent) => {
        e.stopPropagation();
        if (e.key === "Enter") submit();
        if (e.key === "Escape") {
          // Esc backs out the whole fold, and takes the unwritten draft with it
          // — the same "backs out unwritten" the pickers mean by Escape.
          this.linkDrafts.delete(t.id);
          this.expandedLinks.delete(t.id);
          this.render();
        }
      });
    }
    add.append(type, target, label, btn);
    // Focus only on the render that follows a successful add, never on a
    // background refresh — the same one-shot rule `pickingFocus` follows, so a
    // re-render must not yank focus back from wherever the human has moved.
    if (this.linkFocus === t.id) {
      this.linkFocus = null;
      window.setTimeout(() => target.focus(), 0);
    }
    box.appendChild(add);
    return box;
  }

  /** Open (or close) one of the row pickers. One at a time across the whole
   *  board and across every field, so the human is never choosing a
   *  dependency, a container, an Agile level and a sprint at the same time in
   *  four places. */
  private togglePicker(id: string, field: PickerField): void {
    this.picking = nextPicker(this.picking, id, field);
    // Focus only when this click OPENED one — a close has nothing to focus.
    this.pickingFocus = this.picking !== null;
    this.render();
  }

  /** A picker's own deferred close (blur/Esc). Every picker calls THIS rather
   *  than each re-deriving the condition — the original two copies (dep,
   *  parent) had already drifted apart from `togglePicker`'s, and a close that
   *  reads fewer signals than the button that opens swallows a click exactly
   *  the width of the difference (see `pickerIsOpen`). One rule, one place —
   *  which is what let the kind picker (#958 slice K) become a third caller of
   *  this same close for free, rather than a third copy of the condition. */
  private closePicker(id: string, field: PickerField): void {
    if (!pickerIsOpen(this.picking, id, field)) return;
    this.picking = null;
    this.render();
  }

  /** The "⤵ nest under…" picker (#958): every row that could legally contain
   *  this one (#1156), plus a top-level escape when leaving its container is
   *  itself legal.
   *
   *  Like the dep picker it still does NOT pre-filter the choices the backend
   *  would refuse for a reason derived from the whole tree — its own
   *  descendants (a cycle) or a pick that would bust the depth cap. Those rules
   *  live in one authoritative place, inside the backend's lock, and their
   *  errors name the path; a second copy here could only ever disagree, and it
   *  surfaces through the same toast. `parentPickerChoices` decides everything
   *  this picker shows, so the legality question is answered DOM-free and once
   *  (see its doc for why the ladder is the one rule that is mirrored). */
  private renderParentPicker(t: OrchTask): HTMLElement {
    const { candidates: options, topLevel, emptyLabel } = parentPickerChoices(t, this.tasks);
    if (options.length === 0 && !topLevel) {
      return el("span", "task-links-label", emptyLabel);
    }
    const sel = document.createElement("select");
    sel.className = "task-dep-picker parent";
    const placeholder = document.createElement("option");
    placeholder.value = "";
    placeholder.textContent = "⤵ nest under…";
    sel.appendChild(placeholder);
    if (topLevel) {
      // The clear. `orch_upsert_task` reads an EMPTY parent as "take it to the
      // top level" (omitting the field means "leave it alone"), so the sentinel
      // below is mapped to "" on the way out — never sent as a literal id.
      // Offered only when the row's own level permits top level (#1156): a
      // `feature` promoted out of its epic is a write the backend refuses.
      const top = document.createElement("option");
      top.value = TOP_LEVEL_CHOICE;
      top.textContent = "↥ top level (leave its container)";
      sel.appendChild(top);
    }
    for (const c of options) {
      const opt = document.createElement("option");
      opt.value = c.id;
      opt.textContent = `${c.id} — ${c.title}`;
      sel.appendChild(opt);
    }
    sel.value = "";

    const close = () => this.closePicker(t.id, "parent");
    sel.addEventListener("change", () => {
      const pick = sel.value;
      if (!pick) return;
      const parent = pick === TOP_LEVEL_CHOICE ? "" : pick;
      this.picking = null;
      // Close on our own rather than waiting for the board-change event: if the
      // write is refused (a cycle, the depth cap), mutate() toasts the
      // backend's own error and resyncs.
      this.render();
      void this.mutate(
        invoke("orch_upsert_task", { groupId: this.groupId, id: t.id, parent })
      );
    });
    // Keep keystrokes off the terminal underneath; Esc backs out unwritten.
    sel.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") close();
    });
    sel.addEventListener("blur", () => window.setTimeout(close, 0));
    if (this.pickingFocus) {
      this.pickingFocus = false;
      window.setTimeout(() => sel.focus(), 0);
    }
    return sel;
  }

  /** The "🏷 set kind…" picker (#958 slice K): the Agile levels this row could
   *  legally take where it sits (#1156), plus the clear when that is legal too.
   *
   *  Since #1156 the level is enforced rather than a label, so this picker
   *  offers only what a write would accept — including, on a row whose
   *  container or children rule every level out, NOTHING. `kindPickerChoices`
   *  answers all of that DOM-free; the backend remains the authority and its
   *  refusal still surfaces through `mutate`'s toast. */
  private renderKindPicker(t: OrchTask): HTMLElement {
    const { candidates: options, clear: mayClear } = kindPickerChoices(t, this.tasks);
    if (options.length === 0 && !mayClear) {
      return el("span", "task-links-label", "no level this row can take here");
    }
    const sel = document.createElement("select");
    sel.className = "task-dep-picker kind";
    const placeholder = document.createElement("option");
    placeholder.value = "";
    placeholder.textContent = "🏷 set kind…";
    sel.appendChild(placeholder);
    if (mayClear) {
      // The clear. `orch_upsert_task` reads an EMPTY kind as "clear the
      // label" (omitting the field means "leave it alone"), so the sentinel
      // below is mapped to "" on the way out — never sent as a literal kind.
      // Withheld when this row holds levelled children (#1156): clearing would
      // leave them inside an unlevelled container, which the backend refuses.
      const clear = document.createElement("option");
      clear.value = CLEAR_KIND_CHOICE;
      clear.textContent = "— clear (plain task)";
      sel.appendChild(clear);
    }
    for (const k of options) {
      const opt = document.createElement("option");
      opt.value = k;
      opt.textContent = k;
      sel.appendChild(opt);
    }
    sel.value = "";

    const close = () => this.closePicker(t.id, "kind");
    sel.addEventListener("change", () => {
      const pick = sel.value;
      if (!pick) return;
      const kind = pick === CLEAR_KIND_CHOICE ? "" : pick;
      this.picking = null;
      // Close on our own rather than waiting for the board-change event: if
      // the write is refused (an out-of-vocabulary value — unreachable from
      // this picker, but mutate() still resyncs on any backend error),
      // mutate() toasts the backend's own error.
      this.render();
      void this.mutate(invoke("orch_upsert_task", { groupId: this.groupId, id: t.id, kind }));
    });
    // Keep keystrokes off the terminal underneath; Esc backs out unwritten.
    sel.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") close();
    });
    sel.addEventListener("blur", () => window.setTimeout(close, 0));
    if (this.pickingFocus) {
      this.pickingFocus = false;
      window.setTimeout(() => sel.focus(), 0);
    }
    return sel;
  }

  /** The "🎯 sprint…" picker (#1272): the sprints this board runs, the next
   *  unused number so a new one can be started, and — on a row that is in one —
   *  the way back to the backlog.
   *
   *  Unlike the parent picker there is no legality question to mirror or get
   *  wrong: a sprint gates nothing, and the backend validates only the type.
   *  The single refusal that exists is `MAX_SPRINT`, and `sprintPickerChoices`
   *  is where it lives, so this method reads a list and renders it. */
  private renderSprintPicker(t: OrchTask): HTMLElement {
    // No empty-state branch, unlike the other three pickers: this one can never
    // have nothing to offer. `sprintPickerChoices` always adds `highest + 1`
    // unless the board is at `MAX_SPRINT`, and there the row either carries a
    // sprint (so the clear is on the menu) or `MAX_SPRINT` itself is an option.
    // A branch guarding an unreachable state reads as though the state were
    // reachable, and no test could ever reach it to say otherwise — so the
    // invariant is pinned in `test/taskboard.test.ts` instead, where a future
    // change that made it false would redden.
    const { options, clear: mayClear } = sprintPickerChoices(t, this.tasks);
    const sel = document.createElement("select");
    sel.className = "task-dep-picker sprint";
    const placeholder = document.createElement("option");
    placeholder.value = "";
    placeholder.textContent = "🎯 set sprint…";
    sel.appendChild(placeholder);
    if (mayClear) {
      // `0` IS the clear on the wire (§8) — the numeric counterpart of the
      // empty string on `pr`/`kind`. No frontend sentinel is needed, and
      // `sprintPickerChoices` keeps 0 out of the real options so this option
      // and a sprint can never be the same value.
      const back = document.createElement("option");
      back.value = "0";
      back.textContent = "— back to the backlog";
      sel.appendChild(back);
    }
    for (const s of options) {
      const opt = document.createElement("option");
      opt.value = String(s);
      opt.textContent = `sprint ${s}`;
      sel.appendChild(opt);
    }
    sel.value = "";

    const close = () => this.closePicker(t.id, "sprint");
    sel.addEventListener("change", () => {
      const pick = sel.value;
      if (!pick) return;
      const sprint = Number(pick);
      this.picking = null;
      // Close on our own rather than waiting for the board-change event, the
      // same as every picker here: if the write is refused, mutate() toasts the
      // backend's own error and resyncs.
      this.render();
      void this.mutate(
        invoke("orch_upsert_task", { groupId: this.groupId, id: t.id, sprint })
      );
    });
    // Keep keystrokes off the terminal underneath; Esc backs out unwritten.
    sel.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") close();
    });
    sel.addEventListener("blur", () => window.setTimeout(close, 0));
    if (this.pickingFocus) {
      this.pickingFocus = false;
      window.setTimeout(() => sel.focus(), 0);
    }
    return sel;
  }

  /** The "＋ depends on…" picker: every other task on the board, minus the ones
   *  this one already depends on. Cycle-closing choices are deliberately NOT
   *  filtered out here — the backend rejects those inside its lock with an
   *  error naming the path, and duplicating that walk frontend-side would be a
   *  second copy of a rule that could only ever disagree with it. */
  private renderDepPicker(t: OrchTask): HTMLElement {
    const options = depCandidates(t, this.tasks);
    if (options.length === 0) {
      return el("span", "task-links-label", "no other task to depend on");
    }
    const sel = document.createElement("select");
    sel.className = "task-dep-picker";
    const placeholder = document.createElement("option");
    placeholder.value = "";
    placeholder.textContent = "＋ depends on…";
    sel.appendChild(placeholder);
    for (const c of options) {
      const opt = document.createElement("option");
      opt.value = c.id;
      opt.textContent = `${c.id} — ${c.title}`;
      sel.appendChild(opt);
    }
    sel.value = "";

    const close = () => this.closePicker(t.id, "dep");
    sel.addEventListener("change", () => {
      const pick = sel.value;
      if (!pick) return;
      this.picking = null;
      // Close the picker on our own rather than waiting for the board-change
      // event the write will raise: if the write is refused, mutate() toasts
      // and resyncs, and either way the picker has done its job.
      this.render();
      void this.writeLinkArray(t, { kind: "dep-add", id: pick });
    });
    // Keep keystrokes off the terminal underneath (every inline editor in this
    // view does this); Esc backs out without writing anything.
    sel.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") close();
    });
    sel.addEventListener("blur", () => window.setTimeout(close, 0));
    // Focus only on the render that opened it — a later background refresh
    // re-renders the open picker, and must not yank focus back from wherever
    // the human has moved since.
    if (this.pickingFocus) {
      this.pickingFocus = false;
      window.setTimeout(() => sel.focus(), 0);
    }
    return sel;
  }

  private renderTask(
    boardRow: BoardRow<OrchTask>,
    usesDeps: boolean,
    usesHierarchy: boolean,
    /** Does ANY row on this board carry grounding links (#1273)? Decides
     *  whether the 📎 button carries a count — see `render`. */
    usesLinks: boolean,
    blocked: ReadonlyMap<string, string>,
    settled: ReadonlySet<string>,
    /** The board's current sprint (#1272), derived once for the whole render
     *  like `usesDeps`/`settled` above rather than re-scanned per row. `null`
     *  on a board with no open sprint work. */
    current: number | null
  ): HTMLElement {
    const t = boardRow.task;
    const row = el("div", "task-row");
    // The anchor the focus hook scrolls to (#1091 slice C). A data attribute
    // rather than an id: several boards can be open at once across panes, and
    // duplicate DOM ids would make `querySelector` pick an arbitrary one.
    row.dataset.itemId = t.id;
    // Nesting depth (#958). Clamped: the backend caps writes at depth 4, but a
    // hand-edited tasks.json can be deeper and an unbounded indent would walk
    // the row off the right edge of the overlay.
    if (boardRow.depth > 0) row.classList.add(`task-depth-${indentLevel(boardRow.depth)}`);
    if (isAwaitingHuman(t.status)) row.classList.add("awaiting-human");
    const activity = taskActivityState(t.status, t.assignee, this.liveAgentIds);
    if (activity) row.classList.add(`task-row-${activity}`);
    // An archived row recedes further still, so the working items keep the eye
    // even in the show-cleared view. Usually that IS the show-cleared view —
    // but not only: a cleared container still holding live work is never
    // hidden (`clearedIds` is a whole-subtree closure), so it can carry this
    // class with the toggle off. See `BoardRow.cleared`.
    if (boardRow.cleared) row.classList.add("task-row-cleared");
    // A container on screen only because a match sits under it (#1270) reads as
    // scaffolding, not as a hit — otherwise a filtered board looks like it
    // matched every epic on it.
    if (boardRow.context) row.classList.add("task-row-context");
    // A queued task waiting on an unfinished dep recedes, so it can't be read
    // as work anyone could pick up right now (#582's core ask: blocked-queued
    // must not look like plain queued). Deliberately no new accent color —
    // the chips below name the blockers, and the amber/green accents already
    // mean "waiting on YOU" and "live work".
    //
    // A row whose CONTAINER is the one waiting recedes identically (#958 slice
    // R): it is unstartable for the same reason and must not read as startable.
    // No chip of its own — the container carries the ✗ chips that say what is
    // holding it, and a row is only ever visible when every container above it
    // is expanded, so the explanation is already on screen, one line up.
    const unmet = unmetDeps(t, this.tasks);
    const depBlocked =
      t.status === QUEUED_STATUS &&
      (unmet.length > 0 || blockingAncestor(t, this.tasks) !== null);
    if (depBlocked) row.classList.add("task-row-dep-blocked");

    // Multi-select: tick to add the row to the batch-delete set. A checkbox
    // (over ctrl/shift-click) keeps the affordance discoverable — the human
    // asked to "multi select the tasks and click a button" (#120). Selection is
    // frontend-only; the checked state is rebuilt from `selected` each render.
    const check = document.createElement("input");
    check.type = "checkbox";
    check.className = "task-select";
    check.checked = this.selected.has(t.id);
    check.title = "Select for a batch action (delete, or approve if it's at the merge gate)";
    check.addEventListener("change", () => {
      if (check.checked) this.selected.add(t.id);
      else this.selected.delete(t.id);
      this.updateDeleteSelected();
      this.updateApproveSelected();
    });

    // Reorder: board order is the priority order the orchestrator follows.
    // Sibling-scoped since #958 — a row moves among the rows sharing its
    // container, and a container carries its whole subtree with it, so
    // priority edits can never silently re-home a task. A step is one step
    // among the rows the human can SEE above/below it (#1152), so the sunk
    // finished rows in between are not positions a click can land on.
    const order = el("div", "task-order");
    const up = el("button", "task-btn", "▲") as HTMLButtonElement;
    const down = el("button", "task-btn", "▼") as HTMLButtonElement;
    const pos = siblingPosition(this.tasks, t.id, settled);
    up.disabled = pos.index <= 0;
    down.disabled = pos.index < 0 || pos.index === pos.count - 1;
    // A settled row reports {-1, 0} above, so both buttons are already off —
    // its place at the bottom is derived (most recently updated first) and a manual
    // step there would contradict the order the board just told the human it
    // was using. Say so rather than leaving two dead arrows unexplained.
    up.title = boardRow.settled
      ? "Finished work sits at the bottom, most recently updated first — reopen it to give it a priority again"
      : boardRow.depth > 0
        ? "Higher priority within its container"
        : "Higher priority";
    down.title = boardRow.settled
      ? "Finished work sits at the bottom, most recently updated first — reopen it to give it a priority again"
      : boardRow.depth > 0
        ? "Lower priority within its container"
        : "Lower priority";
    const move = (delta: number) => {
      const ids = reorderWithSubtree(this.tasks, t.id, delta);
      void this.mutate(invoke("orch_reorder_tasks", { groupId: this.groupId, ids }));
    };
    up.addEventListener("click", () => move(-1));
    down.addEventListener("click", () => move(1));
    order.append(up, down);

    const main = el("div", "task-main");
    const top = el("div", "task-top");
    // Collapse chevron (#958), leftmost so every row's text starts at the same
    // place whether or not it contains anything. Containers only — a leaf gets
    // an inert spacer of the same width rather than a button that does
    // nothing, so the affordance means "there is something inside here".
    // One scan for the two places this row states its child counts (the
    // chevron's tooltip and the rollup chip below) — they are the same two
    // numbers, and computing them twice scanned the board twice per container.
    const counts = boardRow.hasChildren ? childCounts(t.id, this.tasks) : null;
    const filtering = filterActive(this.filter);
    if (boardRow.hasChildren && counts) {
      const chevron = el(
        "button",
        "task-collapse",
        boardRow.collapsed ? "▸" : "▾"
      ) as HTMLButtonElement;
      // Inert while a filter is armed (#1270), rather than a button whose click
      // does nothing: under the filter rules a kept container is either holding
      // a match — so it must stay open — or has nothing kept under it, so
      // folding it changes nothing either way. Saying so beats a dead click.
      chevron.disabled = filtering;
      chevron.title = filtering
        ? "Collapse resumes when the filter is cleared"
        : boardRow.collapsed
          ? `Show what is inside (${counts.total} task${counts.total === 1 ? "" : "s"})`
          : "Hide what is inside";
      chevron.addEventListener("click", () => {
        if (this.collapsed.has(t.id)) this.collapsed.delete(t.id);
        else this.collapsed.add(t.id);
        this.onViewChanged();
      });
      top.appendChild(chevron);
    } else if (usesHierarchy) {
      // The gutter only exists on a board that nests something — see
      // boardUsesHierarchy. A flat board renders exactly the row it always has.
      top.appendChild(el("span", "task-collapse-spacer"));
    }
    top.appendChild(el("span", "task-id", t.id));

    // Unmistakable, not just a tint — but it sits AFTER the id, not in front of
    // it (#1152, human beta feedback). `.task-top` is a flex row, so a badge in
    // the leading slot pushed the id and everything after it right by its own
    // width, and an active row read as INDENTED — indistinguishable at a glance
    // from a row nested inside a container, which is a real and different thing
    // here (#958's `.task-depth-N` on the row itself). Every row's left edge is
    // now decided by its depth alone. The glow, pulse and left accent on the row
    // are what carry "active" the moment the eye lands on it; the badge names
    // WHO, one position further in.
    if (activity === "active") {
      const badge = el("span", "task-active-badge", `● ACTIVE — ${t.assignee}`);
      badge.title = `${t.assignee} is actively working on this right now`;
      top.appendChild(badge);
    }

    // Board marker + deep-link (#1091 slice G): an obvious chip on a row
    // that is blocked on a human decision or gated on a demo, routing
    // through the pane's focus hook to open the NEEDS-YOU panel at that
    // item. Placed right after the id, ahead of every other chip, so it is
    // the first chip the eye lands on after "which row is this". (On a live
    // row the ACTIVE badge above precedes it — that one is louder still, and
    // both sit behind the id so no row's left edge moves.)
    const marker: BoardMarker | null = boardMarker(t, blocked);
    if (marker) {
      const chip = el(
        "button",
        "task-chip marker",
        marker.kind === "decision" ? "❓ needs a decision" : "👀 needs a look"
      ) as HTMLButtonElement;
      chip.title =
        marker.kind === "decision"
          ? "A pending question is holding this up — open it in the NEEDS-YOU panel"
          : "Parked for a demo — open it in the NEEDS-YOU panel";
      chip.addEventListener("click", () => {
        if (!this.opts.onFocusDecision(marker.target)) {
          this.toast("The NEEDS-YOU panel isn't available on this pane.");
        }
      });
      top.appendChild(chip);
    }

    // Archived (#1152) — the row's own stamp, so it appears on every rendered
    // cleared row: the ones the 👁 toggle just revealed, and the cleared
    // container that was never hidden because live work sits inside it. It says
    // WHY the row is dimmed, and the ↩ below is its undo. See `BoardRow.cleared`.
    if (boardRow.cleared) {
      const chip = el("span", "task-chip cleared", "📥 cleared");
      chip.title = `Cleared from the working list on ${fmtTime(t.cleared_ms ?? 0)} — still on the board, nothing was deleted`;
      top.appendChild(chip);
    }

    // The Agile level (#958). Enforced since #1156 — where this row may sit is
    // decided by it — but still nothing outside the board reads it: no
    // permission, no merge decision, and not the claim guard.
    if (t.kind) {
      const known = (KINDS as readonly string[]).includes(t.kind);
      const kind = el("span", `task-chip kind k-${known ? t.kind : "unknown"}`, t.kind);
      kind.title = known
        ? `Agile level: ${t.kind} — ${levelRuleText(t.kind)}`
        : `${t.kind} is not one of ${KINDS.join(" | ")} — only a hand-edited tasks.json can hold it`;
      top.appendChild(kind);
    }

    // The sprint badge (#1272). Metadata like the level beside it: nothing
    // outside the board reads a sprint — not readiness, not the claim guard,
    // not WIP — so this says which batch the row is in and nothing more.
    //
    // Only on a row that HAS one. A backlog badge on every unbatched row would
    // put a chip on most of the board to say "no", where the absence already
    // says it; the `backlog` filter chip is how the backlog is asked for.
    if (typeof t.sprint === "number") {
      const chip = el("span", "task-chip sprint", `sprint ${t.sprint}`);
      if (t.sprint === current) chip.classList.add("current");
      chip.title =
        t.sprint === current
          ? `Sprint ${t.sprint} — the current sprint, so this is work the orchestrator picks up ahead of anything else`
          : current !== null && t.sprint < current
            ? `Sprint ${t.sprint} — an earlier sprint than the current one (${current})`
            : `Sprint ${t.sprint} — a later sprint${current === null ? "" : ` than the current one (${current})`}, so it waits behind it`;
      top.appendChild(chip);
    }

    const status = document.createElement("select");
    status.className = `task-status st-${t.status}`;
    for (const s of STATUSES) {
      const opt = document.createElement("option");
      opt.value = s;
      opt.textContent = s;
      status.appendChild(opt);
    }
    status.value = t.status;
    status.addEventListener("change", () =>
      void this.mutate(invoke("orch_upsert_task", { groupId: this.groupId, id: t.id, status: status.value }))
    );
    top.appendChild(status);

    // Title: double-click to edit in place.
    const title = el("span", "task-title", t.title);
    title.title = "Double-click to edit";
    title.addEventListener("dblclick", () => {
      const input = document.createElement("input");
      input.className = "dlg-input task-title-input";
      input.value = t.title;
      title.replaceWith(input);
      input.focus();
      input.select();
      const commit = (save: boolean) => {
        // Enter/Escape/click all commit, and detaching the focused input fires
        // blur → a second commit; swapIfConnected keeps that redundant call (or
        // a background re-render having already removed the row) from throwing
        // NotFoundError out into the app-wide error banner.
        if (!swapIfConnected(input, title)) return;
        const v = input.value.trim();
        if (save && v && v !== t.title) {
          void this.mutate(invoke("orch_upsert_task", { groupId: this.groupId, id: t.id, title: v }));
        }
      };
      input.addEventListener("keydown", (e) => {
        e.stopPropagation();
        if (e.key === "Enter") commit(true);
        if (e.key === "Escape") commit(false);
      });
      input.addEventListener("blur", () => commit(true));
    });
    top.appendChild(title);

    // Meta chips: issue / PR / assignee / resumable session. Issue and PR
    // refs are clickable — they open in the browser (see openRef).
    for (const [cls, label, kind] of [
      ["issue", t.issue, "issue"],
      ["pr", t.pr, "pr"],
    ] as const) {
      if (!label) continue;
      const chip = el("button", `task-chip ${cls} link`, label) as HTMLButtonElement;
      chip.title = `Open ${kind === "issue" ? "issue" : "PR"} ${label} in browser`;
      chip.addEventListener("click", () => this.openRef(kind, label));
      top.appendChild(chip);
    }
    // The assignee chip is LIVE or HISTORY (#339 refinement) — an old
    // assignee from a killed/resumed/reassigned session must read as past,
    // never blend in as if the same agent were still sitting there.
    if (t.assignee) {
      const isLive = this.liveAgentIds.has(t.assignee);
      const chip = el("span", `task-chip agent ${isLive ? "live" : "history"}`, t.assignee);
      chip.title = isLive
        ? "Currently live agent"
        : "Assigned in a past session — this agent is not currently live";
      top.appendChild(chip);
    }
    if (t.session) {
      const chip = el("span", "task-chip session", `⟲ ${t.session.slice(0, 8)}`);
      chip.title = `Resumable session ${t.session} — the orchestrator can reopen this task's agent for follow-ups`;
      top.appendChild(chip);
    }

    // Child rollup (#958). DIRECT children only, because these are the same
    // two numbers the orchestrator's `list_tasks` row carries (`children` /
    // `children_done`) and the two readers must not be shown different counts
    // for the same thing.
    if (counts) {
      const chip = el("span", "task-chip children", `${counts.done}/${counts.total}`);
      // What #1270 adds to this chip, and the only thing it adds: whether any
      // of those children are currently OFF SCREEN. The counts themselves are
      // unchanged and still the orchestrator's own `children`/`children_done`.
      //
      // Folding a container up used to hide its contents AND every trace of how
      // much it hid — "3/7" reads identically whether all seven rows are under
      // it or none are. `shownKids` comes from the projection that just ran, so
      // this cannot disagree with what is actually on the screen.
      const withheld = counts.total - boardRow.shownKids;
      if (withheld > 0) chip.classList.add("hiding");
      chip.title =
        `${counts.done} of ${counts.total} task${counts.total === 1 ? "" : "s"} directly inside ` +
        `this one ${counts.total === 1 ? "is" : "are"} done` +
        (withheld > 0
          ? ` — ${withheld} not on screen (${boardRow.collapsed ? "this row is folded up" : "hidden by the filter"})`
          : "");
      top.appendChild(chip);
      // The nudge (#958): everything underneath is finished but this row's own
      // status hasn't caught up. A PROMPT, never a write — a derived status
      // write-back is exactly the wedge that keeping `ready` derived avoids,
      // and status here has two authors (the human and the orchestrator).
      // Whole subtree, not just the direct children, so the claim it makes is
      // one that can't be false with an open grandchild.
      if (t.status !== DONE_STATUS && subtreeAllDone(t.id, this.tasks)) {
        const nudge = el("span", "task-chip rollup-done", "⤴ all inside done");
        nudge.title =
          `Every task under ${t.id} is done, but ${t.id} itself is ${t.status}. ` +
          `Nothing has been changed — set its status yourself if that's right.`;
        top.appendChild(nudge);
      }
    }
    // A container that names no row on the board (#958) — only reachable by
    // hand-editing tasks.json, since the backend validates on write and
    // re-homes survivors on delete. The row renders at top level, and this
    // says why rather than leaving it looking like ordinary top-level work.
    if (hasMissingParent(t, this.tasks)) {
      const chip = el("span", "task-chip parent-missing", `⚠ in ${t.parent}`);
      chip.title =
        `${t.parent} names no task on this board, so this row shows at the top level. ` +
        `Re-nest it with ⤵, or move it to the top level from the same picker.`;
      top.appendChild(chip);
    }

    // "ready" (#582): this queued item's dependencies are all done, so it can
    // start now. Only on a board that actually uses deps — see boardUsesDeps —
    // and it sits next to Start because that is the action it enables.
    if (usesDeps && isReady(t, this.tasks)) {
      const ready = el("span", "task-chip ready", "▸ ready");
      ready.title =
        (t.deps?.length ?? 0) > 0
          ? "Every task this depends on is done — this one can start now"
          : "Nothing blocks this one — it can start now";
      top.appendChild(ready);
    }

    // Start: the human's nudge to begin a queued item now. Delivers a prompt
    // to the orchestrator (which assigns a worker and flips the status then);
    // shown only on queued items, where starting is meaningful.
    if (t.status === "queued") {
      const start = el("button", "task-btn start", "▶ Start") as HTMLButtonElement;
      start.title = "Tell the orchestrator to begin work on this task now";
      start.addEventListener("click", () => {
        // Start doesn't flip the status, so — unlike Approve — the button
        // isn't removed by the mutation, and mutate() doesn't re-render on
        // success. Disable on click so an accidental double-click can't fire
        // two nudges (two prompts + two identical notes) for one intent; it
        // stays disabled until the board refresh (triggered by the note write,
        // or by mutate's resync on error) rebuilds this row.
        start.disabled = true;
        void this.mutate(invoke("orch_start_task", { groupId: this.groupId, id: t.id }));
      });
      top.appendChild(start);
    }

    // Merge-gate actions: the human's approve / request-changes touchpoints,
    // shown only where they belong — on items awaiting the merge decision.
    // Once changes are requested (see requestChanges below), the status
    // moves off pr/human-testing and canApprove goes false on its own — a
    // reopened task can never keep showing a stale Approve button.
    if (canApprove(t.status)) {
      // #316: an Approve that CANNOT succeed under an armed workflow gate says
      // so up front — the button stays clickable (the grant/note still get
      // recorded, and Approve is the human's own gate regardless — see
      // approveWithComment's dialog note), it just never claims a merge that
      // won't happen. `this.workflow === null` (read failed, or hasn't landed
      // yet) reads as "no gate known", the same conservative default the
      // no-PR case already had.
      const gate = this.workflow ? approveWillMerge(this.workflow, t) : { ok: true };
      const approve = el(
        "button",
        "task-btn approve",
        gate.ok ? (t.pr ? "✓ Approve & allow merge" : "✓ Approve") : `✓ Approve (${gate.reason})`
      ) as HTMLButtonElement;
      approve.title = gate.ok
        ? t.pr
          ? "Authorize the merge: write a one-time grant for this PR and tell the orchestrator to merge " +
            "(optionally with instructions). The grant is single-use and expires in ~30 min."
          : "Approve: mark this item done and tell the orchestrator (optionally with instructions). " +
            "No PR is linked, so no merge is authorized."
        : `This still records your grant/note, but the workflow merge gate will refuse the merge. ${gateExitsMessage()}`;
      if (!gate.ok) approve.classList.add("gated");
      approve.addEventListener("click", () => this.approveWithComment(t));
      const changes = el("button", "task-btn changes", "✎ Changes") as HTMLButtonElement;
      changes.title = "Request changes — send findings back to the orchestrator";
      changes.addEventListener("click", () => this.requestChanges(t));
      top.append(approve, changes);
    }

    // Proceed: the human's promote verdict on a prototype (#147). Flips the item
    // to in-progress and tells the orchestrator to run the full production build.
    // Two-click confirm (like delete) — promoting kicks off real work, so a
    // mis-click shouldn't launch it.
    if (canProceed(t.status)) {
      const proceed = el("button", "task-btn proceed", "▶ Proceed") as HTMLButtonElement;
      proceed.title = "Promote this prototype — tell the orchestrator to build the production version";
      proceed.addEventListener("click", () => {
        if (proceed.dataset.confirm) {
          void this.mutate(invoke("orch_proceed_task", { groupId: this.groupId, id: t.id }));
        } else {
          proceed.dataset.confirm = "1";
          proceed.textContent = "promote?";
          window.setTimeout(() => {
            delete proceed.dataset.confirm;
            proceed.textContent = "▶ Proceed";
          }, 2500);
        }
      });
      top.appendChild(proceed);
    }

    // Add a dependency (#582): toggles the picker on the links line below.
    // One entry point, always in the same place, whether or not the task has
    // links yet — the links line itself stays absent on a link-free row so a
    // dep-free board keeps exactly the row height it has today.
    const linkBtn = el("button", "task-btn deplink", "🔗") as HTMLButtonElement;
    linkBtn.title = "Add a dependency — this task waits until the one you pick is done";
    linkBtn.addEventListener("click", () => this.togglePicker(t.id, "dep"));
    top.appendChild(linkBtn);

    // Nest (#958): put this row inside another one, or take it back to the top
    // level. Containment, not ordering — deliberately a separate control from
    // 🔗 above, since a container never blocks anything by being a container.
    const nestBtn = el("button", "task-btn nest", "⤵") as HTMLButtonElement;
    nestBtn.title = t.parent
      ? `Move this task into a different container, or back to the top level (it is in ${t.parent})`
      : "Move this task inside another one — grouping only, it changes nothing about what blocks it";
    nestBtn.addEventListener("click", () => this.togglePicker(t.id, "parent"));
    top.appendChild(nestBtn);

    // Set kind (#958 slice K): the Agile-level picker. Always present, like
    // 🔗/⤵ above — one entry point in the same place whether or not the row
    // already carries a label, rather than making the badge itself (absent on
    // most rows) the only way in.
    const kindBtn = el("button", "task-btn kindpick", "🏷") as HTMLButtonElement;
    kindBtn.title = t.kind
      ? `Change this row's Agile level (currently ${t.kind}) — ${levelRuleText(t.kind)}`
      : "Set this row's Agile level — epic (top level) ⊃ feature ⊃ story ⊃ task; a row with no level may sit anywhere";
    kindBtn.addEventListener("click", () => this.togglePicker(t.id, "kind"));
    top.appendChild(kindBtn);

    // Set sprint (#1272): the fourth picker, in the same place as the three
    // above and present on every row for the same reason — the badge is absent
    // on a backlog row, so making the badge the way in would leave the backlog
    // with no way out of it.
    const sprintBtn = el("button", "task-btn sprintpick", "🎯") as HTMLButtonElement;
    sprintBtn.title =
      typeof t.sprint === "number"
        ? `Move this item to another sprint, or back to the backlog (it is in sprint ${t.sprint})`
        : "Put this item in a sprint — a numbered batch that says what gets picked up first; it changes nothing else about the item";
    sprintBtn.addEventListener("click", () => this.togglePicker(t.id, "sprint"));
    top.appendChild(sprintBtn);

    // Grounding links (#1273): the fifth per-row entry point, in the same slot
    // as the four above and present on every row for the same reason the 🎯
    // picker is — a row with no links has no chip to click, so making a chip
    // the way in would leave it with no way to gain its first one.
    //
    // It carries the COUNT, exactly like 🗨 beside it, so the row says how much
    // grounding it has without anything being unfolded. The count is dropped on
    // a board that uses no links at all (`usesLinks`): a column of `📎 0` says
    // nothing, and the absence already says it — the same argument the sprint
    // badge makes for the backlog.
    const linkCount = t.links?.length ?? 0;
    const groundBtn = el(
      "button",
      "task-btn ground",
      usesLinks ? `📎 ${linkCount}` : "📎"
    ) as HTMLButtonElement;
    if (linkCount > 0) groundBtn.classList.add("has-links");
    groundBtn.title =
      linkCount > 0
        ? `${linkCount} grounding link${linkCount === 1 ? "" : "s"} — what an agent opened against this task is told to read first. Click to see, open, add or remove them.`
        : "Grounding links — record what governs this work (a requirement, a spec, a design note, a test case, a doc) so an agent opened against it reads them first";
    groundBtn.addEventListener("click", () => {
      if (this.expandedLinks.has(t.id)) this.expandedLinks.delete(t.id);
      else this.expandedLinks.add(t.id);
      this.render();
    });
    top.appendChild(groundBtn);

    const notesBtn = el("button", "task-btn notes", `🗨 ${t.notes.length}`) as HTMLButtonElement;
    notesBtn.title = "Notes";
    notesBtn.addEventListener("click", () => {
      if (this.expanded.has(t.id)) this.expanded.delete(t.id);
      else this.expanded.add(t.id);
      this.render();
    });
    top.appendChild(notesBtn);

    // Per-row un-archive (#1152). No confirm: it puts a row back into a list,
    // which is the reversible direction of a reversible action.
    if (boardRow.cleared) {
      const restore = el("button", "task-btn restore", "↩") as HTMLButtonElement;
      restore.title = "Bring this one back into the working list";
      restore.addEventListener("click", () =>
        void this.mutate(
          invoke("orch_restore_cleared_tasks", { groupId: this.groupId, ids: [t.id] })
        )
      );
      top.appendChild(restore);
    }

    // Delete with a two-click confirm, mirroring the git view's pattern.
    const del = el("button", "task-btn danger", "✕") as HTMLButtonElement;
    del.title = "Delete task";
    del.addEventListener("click", () => {
      if (del.dataset.confirm) {
        void this.mutate(invoke("orch_delete_task", { groupId: this.groupId, id: t.id }));
      } else {
        del.dataset.confirm = "1";
        del.textContent = "sure?";
        window.setTimeout(() => {
          delete del.dataset.confirm;
          del.textContent = "✕";
        }, 2500);
      }
    });
    top.appendChild(del);
    main.appendChild(top);

    const links = this.renderLinks(t);
    if (links) main.appendChild(links);

    // The grounding detail (#1273) sits between the dep chips and the notes:
    // above the conversation, below the structure, which is the order a human
    // reads a row in.
    if (this.expandedLinks.has(t.id)) main.appendChild(this.renderGroundings(t));

    if (this.expanded.has(t.id)) {
      const notes = el("div", "task-notes");
      for (const n of t.notes) {
        const line = el("div", "task-note");
        line.append(
          el("span", "task-note-meta", `${n.author} · ${fmtTime(n.ts_ms)}`),
          el("span", "task-note-text", n.text)
        );
        notes.appendChild(line);
      }
      const addRow = el("div", "task-note-add");
      const input = document.createElement("input");
      input.className = "dlg-input";
      input.placeholder = "Add a note…";
      input.spellcheck = false;
      const submit = () => {
        const text = input.value.trim();
        if (!text) return;
        input.value = "";
        void this.mutate(invoke("orch_upsert_task", { groupId: this.groupId, id: t.id, note: text }));
      };
      input.addEventListener("keydown", (e) => {
        e.stopPropagation();
        if (e.key === "Enter") submit();
      });
      const btn = el("button", "dlg-btn", "Note") as HTMLButtonElement;
      btn.addEventListener("click", submit);
      addRow.append(input, btn);
      notes.appendChild(addRow);
      main.appendChild(notes);
    }

    row.append(check, order, main);
    return row;
  }
}

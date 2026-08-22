// The WORKFLOW pane (#222, restructured by #880): the repo's workflow file, made configurable.
//
// ONE buffer, and the buffer is the FILE (the Kestra pattern — an inspector edit rewrites the
// YAML under the hood; the YAML is never a stale export of some hidden canvas state). What
// changed with #880 is not the model, it is the SHAPE OF THE SCREEN:
//
//   ┌────────────┬───────────────────────────────┬──────────────┐
//   │  roster    │  the canvas (primary surface) │  inspector   │
//   │  (blocks,  │  ── or the raw YAML, which is │  (whatever   │
//   │   gate)    │     a toggle over the same    │   is         │
//   │            │     space)                    │   selected)  │
//   └────────────┴───────────────────────────────┴──────────────┘
//
// The canvas is the primary surface and the inspector is DOCKED beside it, because the thing
// #880 was opened about is that clicking a block did not visibly do anything: the property form
// lived behind a "Blocks" tab, the canvas behind a "Graph" tab, and so selecting something and
// showing it were two separate acts — of which the canvas performed only the first. Docking the
// editor removes the second act rather than remembering to perform it. The roster stays as the
// left column (it is also the keyboard/accessibility path to every selection), and the raw YAML
// stays first-class as a toggle over the canvas: it is a different modality over the same file,
// not a lesser one, and the file remains the source of truth.
//
// The canvas EDITS the file (#222 v2 — it was read-only in v1 and the human asked for more). It
// draws the declared happy path (ADVISORY edges: the orchestrator still schedules) and the merge
// gate (ENFORCED: the `gh` shim refuses a merge until the named reviewers' verdicts are PASS),
// and the two are drawn differently BECAUSE they mean different things. Every gesture goes out
// through the same pure model as a form edit, so it can never become a second source of truth.
//
// All the thinking lives in the pure `workflowmodel.ts` (parse / serialize / validate / derive)
// and `workflowpane.ts` (the pane's own decisions — which surface, what the inspector shows),
// which is where the tests are. This file is DOM: rendering, focus, dialogs, and the read/write
// path through the hash-guarded `ft*` file commands.
//
// The one rule the sync has to obey: while the YAML does not PARSE, the inspector is disabled.
// An inspector edit serializes the model back over the buffer, and serializing a model we only
// half-understood would silently destroy the broken text the human is in the middle of
// fixing. So a syntax error disables the editor and says why; every other kind of breakage
// (an unknown kind, a dangling edge) still renders — as a stub, with a finding — because
// a block you cannot see is a block you cannot repair.

import {
  analyzeWorkflow,
  parseWorkflow,
  serializeWorkflow,
  serializeWorkflowPreserving,
  formatWorkflowText,
  isUnreadable,
  scaffoldWorkflowText,
  removeBlockAt,
  newBlock,
  connectBlocks,
  disconnectBlocks,
  connectionError,
  isValidBlockId,
  isBlockKind,
  isReviewingBlock,
  isWorkflowCli,
  isValidResourceName,
  roleHintsForKind,
  allowDenialReason,
  hasErrors,
  BLOCK_KINDS,
  WORKFLOW_CLIS,
  GATE_REQUIRES,
  WORKFLOW_FILE,
  legacyFallbackFor,
  INTAKE_SOURCES,
  INTAKE_LABEL_KEYS,
  ID_MAX_CHARS,
  RESOURCES_MAX,
  RESOURCE_SLOTS_MIN,
  RESOURCE_SLOTS_MAX,
  RESOURCE_MAX_HOLD_MINUTES_MIN,
  RESOURCE_MAX_HOLD_MINUTES_MAX,
  MERGE_QUEUE_CHECKS_TIMEOUT_MIN,
  MERGE_QUEUE_CHECKS_TIMEOUT_MAX,
  POLICY_BOUNDS,
  type FieldBounds,
  type Workflow,
  type WorkflowBlock,
  type WorkflowAnalysis,
  type WorkflowResource,
  type IntakeLabelKey,
  type Finding,
  type FindingSection,
  type GraphNode,
} from "./workflowmodel";
import { agentCliKnobs } from "./pty";
import { knobState, type CliKnobs, type KnobStates } from "./selectorknobs";
import { blockModelOptions, type CliProbe } from "./modelcatalog";
import { modelCatalog } from "./modelprobe";
import { ModelPicker } from "./modelpicker";
import { BLOCK_DEFAULT_MODEL_LABEL } from "./modelnames";
import { BlockKnobFields, type KnobFieldSpec } from "./workflowknobs";
import {
  layoutFileFor,
  parseLayout,
  serializeLayout,
  emptyLayout,
  layoutEquals,
  pruneLayout,
  withPosition,
  resolvePositions,
  freeSlot,
  rectOf,
  outPort,
  inPort,
  edgePath,
  edgeMidpoint,
  hitTestNodes,
  hitTestEdges,
  blockKey,
  ghostKey,
  NODE_W,
  NODE_H,
  PAD,
  type Point,
  type Rect,
  type WorkflowLayout,
} from "./workflowlayout";
import { ftReadFile, ftWriteFile, ftListDir, errorCode, errorMessage, type FileRead } from "./fileapi";
import { fmNewFolder, fmNewFile, fmErrorCode } from "./filemgr";
import {
  paneSurface,
  createAllowed,
  savePlan,
  layoutPruneIds,
  rewriteImpact,
  rewriteImpactMessage,
  inspectorTarget,
  inspectorHeading,
  surfaceForFinding,
  canvasDeleteAllowed,
  type LayoutWrite,
  type Selection,
  type Surface,
} from "./workflowpane";
import { appVersion } from "./pty";
import { closeDecision, discardEdits, type ConflictChoice } from "./dirtystate";
import { showToast } from "./toast";
import { modal, promptModal } from "./modal";
import { IDENTITY, SEMANTIC } from "./theme.ts";

/** What the hosting pane provides. Only one host today (the workflow PANE — a workflow
 *  builder is a station you keep open beside an agent, never a glance-and-dismiss
 *  overlay), but the shape mirrors `FileEditHost` so the pane wires it the same way. */
export interface WorkflowHost {
  /** The repo/folder the workflow file lives under (the pane's root). */
  getRoot(): string | null;
  /** Root-relative path of the workflow file. Defaults to `.orrerix/workflow.yml`, falling back to `.loomux/workflow.yml` when only that exists. */
  getFile?(): string;
  /** Never called in embedded mode — the pane's own ✕ closes it (and asks first). */
  onClose(): void;
  /** This view IS a pane's content: no ✕, no Esc-to-close. Same fork as FileEditView. */
  embedded?: boolean;
}

function el(tag: string, cls: string, text?: string): HTMLElement {
  const e = document.createElement(tag);
  e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

const svg = (tag: string): SVGElement => document.createElementNS("http://www.w3.org/2000/svg", tag);

// The graph's geometry now lives in `workflowlayout.ts` (imported above) — fixed, not
// measured, and pure, which is what lets the hit-testing and edge-routing be tested as
// arithmetic instead of by dragging things around and squinting.

export class WorkflowView {
  readonly el: HTMLElement;

  private readonly host: WorkflowHost;
  private root: string | null = null;
  private rel: string = WORKFLOW_FILE;

  /** The live buffer — the single source of truth for every surface. The form serializes
   *  INTO it; the text editor edits it directly; the graph is derived from it. */
  private text = "";
  /** The buffer as last written to (or read from) disk. `dirty` is text !== savedText. */
  private savedText = "";
  /** The on-disk hash at read time, echoed back on write so a concurrent change (an agent,
   *  git, another editor) is a CONFLICT rather than a silent overwrite. "" = no file yet. */
  private savedHash = "";
  /** False until the file exists on disk (a repo that has never had a workflow). */
  private exists = false;
  /** Why the workflow file could not be READ, when it is there but we can't show it. Distinct
   *  from "there isn't one" — see the error surface. Null when the file loaded (or is simply
   *  absent, which is not an error). */
  private loadError: string | null = null;
  /** Node positions (`workflow.layout.json`, beside the workflow file). NOT part of the workflow: a drag changes
   *  this and nothing else, and it is never serialized into the semantic file (§4). */
  private layout: WorkflowLayout = emptyLayout();
  /** The layout as last written, so a drag that ends where it began writes nothing. */
  private savedLayout: WorkflowLayout = emptyLayout();

  private analysis: WorkflowAnalysis;
  private selection: Selection = { kind: "workflow" };
  /** Which modality owns the middle of the pane. The canvas is primary; the raw YAML is a
   *  toggle over it (#880). The inspector is beside BOTH, so it is not on this axis. */
  private surface: Surface = "canvas";
  private disposed = false;
  /** This build's version, for `authored_with:` on a workflow this pane CREATES. Empty
   *  until the async lookup lands (and if it never does — the key is simply not written,
   *  which beats writing `authored_with: unknown`). */
  private appVersion = "";

  // Header
  private pathLabel: HTMLElement;
  private dirtyDot: HTMLElement;
  private saveBtn: HTMLButtonElement;
  private yamlBtn: HTMLButtonElement;
  private statusEl: HTMLElement;

  // Body
  private rosterEl: HTMLElement;
  /** The docked inspector's header (what is selected) and its body (the editor for it). */
  private inspTitleEl: HTMLElement;
  private inspSubEl: HTMLElement;
  private formPane: HTMLElement;
  private yamlPane: HTMLElement;
  private yamlArea: HTMLTextAreaElement;
  private graphPane: HTMLElement;
  private findingsEl: HTMLElement;
  private emptyEl: HTMLElement;
  private errorEl: HTMLElement;
  private errorTextEl: HTMLElement;
  private bodyEl: HTMLElement;
  /** The create button, and the two labels that name the file. All three are re-stated in
   *  `render()` rather than fixed at construction: the button because being pressable is a
   *  DECISION (`createAllowed`) and not a side-effect of being on screen, and the labels because
   *  this pane opens on any `.yml` the file browser hands it (#217's `file`), so a pane rooted on
   *  `ci/flow.yml` that says the default workflow path is telling the human about a file they are
   *  not looking at — which, on the error surface, means naming the wrong file as unreadable. */
  private starterBtn: HTMLButtonElement;
  private startPathEl: HTMLElement;
  private errorTitleEl: HTMLElement;

  /** What each CLI can do with the model knobs (#687), as the BACKEND reports it
   *  (`agent_cli_knobs`): `undefined` = not asked yet, `null` = asked and the
   *  lookup failed, a record = the answer. The pane never mirrors a capability
   *  of its own — see `knobLookup`. */
  private cliKnobs = new Map<string, CliKnobs | null>();
  /** CLIs already asked about, so a re-analysis per keystroke is not a fetch per
   *  keystroke. Separate from `cliKnobs` because "asked, still in flight" and
   *  "asked, failed" are different states and only one of them is answerable. */
  private knobsAsked = new Set<string>();
  /** One model probe per CLI per PANE — deliberately not per paint, and not once
   *  per app run either.
   *
   *  Not per paint: the block form re-renders on every knob edit, and the catalog
   *  no longer keeps an answer that carried nothing (`worthKeeping`), so probing
   *  from the render path would be a subprocess per paint for exactly the CLIs
   *  that have no answer to give.
   *
   *  Not once per app run: per pane IS the recovery granularity the app-wide memo
   *  would otherwise cost. Install a CLI mid-session, open a workflow pane, and it
   *  is asked again — which is what the pre-#935 per-form memo gave for free.
   *
   *  The PROMISE is what's held, not an "already asked" flag, so a second block
   *  form painted while the first probe is still in flight still gets its
   *  re-set. */
  private modelProbes = new Map<string, Promise<CliProbe>>();
  /** Redraws the block form's two knob rows in place, or `null` when no block
   *  form is on screen. The form deliberately does not re-render on a model edit
   *  (it would rebuild the input under the caret), so the rows that depend on the
   *  model — and on a capability reply that lands whenever the IPC happens to
   *  resolve — need a way to be refreshed without one (#935). */
  private repaintBlockKnobs: (() => void) | null = null;
  /** Rebuilds the block form's model dropdown as soon as doing so stops being
   *  destructive, or `null` when no block form is on screen (#1020).
   *
   *  The sibling of {@link repaintBlockKnobs}, and nulled by the same line for
   *  the same reason. A detection reply now arrives on the sweep's schedule
   *  rather than after a click, so "which picker is live, and is the human
   *  inside its custom-id box right now" is knowledge only the form has —
   *  it installs this, and `renderInspector` takes it away when those controls
   *  are detached. */
  private refreshBlockModels: (() => void) | null = null;
  /** Releases this pane's `modelCatalog.onReport` subscription. Held so
   *  `dispose()` can call it — see the subscription itself. */
  private unsubscribeReports: (() => void) | null = null;
  /** CLIs a detection reply has already been applied to this pane for (#1020).
   *
   *  The push and the pull are two deliveries of ONE sweep answer and both can
   *  land — the lookup's `.then` is already attached when the event arrives, and
   *  neither call site can tell that the other got there first. Deduping in the
   *  funnel they share, rather than at each call site, is what makes "the two
   *  routes must not repaint a form twice" hold for BOTH orderings instead of
   *  the one a guard captured at paint time covers (rev-713 non-blocking 3). */
  private detectionsApplied = new Set<string>();

  // Canvas interaction state. All three are transient — none of them is ever serialized, and
  // the model never learns they existed.
  /** A node being dragged: which one, and where the pointer grabbed it. */
  private dragging: { key: string; id: string; grab: Point; at: Point } | null = null;
  /** An edge being drawn: the block it left, and where the pointer is now. */
  private connecting: { from: string; at: Point } | null = null;

  constructor(host: WorkflowHost) {
    this.host = host;
    this.analysis = analyzeWorkflow("");

    this.el = el("div", "wf");
    // Focusable like every other content view, so Alt+arrow nav / dock-restore / window
    // refocus land ON the surface without grabbing one of its inner controls.
    this.el.tabIndex = -1;

    // Take the startup sweep's answers as they land (#1020) — the push half of
    // detection, for a pane that was already open when one arrived. The pull
    // half is in `blockForm`, for a form opened after the sweep finished.
    //
    // Not filtered by program, deliberately: only CLIs with a `PROTOCOLS` row
    // are ever swept (one today), so this fires at most a handful of times in an
    // app run, and deciding whether a report is "relevant" would mean deciding
    // what a block with no `cli:` inherits — a question `analyzeWorkflow` is
    // about to re-answer anyway.
    //
    // `disposed` is the liveness answer `onReport` asks for: a closed pane must
    // stop being repainted, and this is the only teardown signal it has.
    // The unsubscribe is HELD and called from `dispose()`, not discarded: the
    // catalog is app-scoped and this pane is not, so a subscription nobody
    // releases retains the whole view — its analysis, its detached DOM — for the
    // life of the process (rev-713 blocking 2). `disposed` is the same answer
    // given to the catalog's own prune, for a pane that is closed some other
    // way.
    this.unsubscribeReports = modelCatalog.onReport(
      (program) => this.applyDetection(program),
      () => !this.disposed
    );

    // ---- header ----
    const head = el("div", "wf-head");
    this.pathLabel = el("span", "wf-path");
    this.dirtyDot = el("span", "wf-dirty", "●");
    this.dirtyDot.title = "Unsaved changes";
    this.dirtyDot.hidden = true;
    this.statusEl = el("span", "wf-status");

    this.saveBtn = document.createElement("button");
    this.saveBtn.className = "wf-btn";
    this.saveBtn.textContent = "Save";
    this.saveBtn.title = "Save (Ctrl+S)";
    this.saveBtn.disabled = true;
    this.saveBtn.addEventListener("click", () => void this.save());

    const formatBtn = document.createElement("button");
    formatBtn.className = "wf-btn";
    formatBtn.textContent = "Format";
    formatBtn.title = "Rewrite the file in canonical form (fixed key order, references in roster order)";
    formatBtn.addEventListener("click", () => void this.format());

    const reloadBtn = document.createElement("button");
    reloadBtn.className = "wf-btn";
    reloadBtn.textContent = "Reload";
    reloadBtn.title = "Re-read the file from disk";
    reloadBtn.addEventListener("click", () => void this.reload());

    // The YAML toggle — the one surface control left now that the tabs are gone (#880). It is a
    // toggle and not a tab because the two are not peers on screen: the canvas is where the pane
    // lives, and the raw text is a modality you switch INTO deliberately and come back from.
    this.yamlBtn = document.createElement("button");
    this.yamlBtn.className = "wf-btn";
    this.yamlBtn.textContent = "YAML";
    this.yamlBtn.title = "Edit the raw file instead of the canvas (the same buffer, the other way round)";
    this.yamlBtn.addEventListener("click", () =>
      this.setSurface(this.surface === "yaml" ? "canvas" : "yaml")
    );

    const spacer = el("span", "wf-spacer");
    head.append(
      this.pathLabel,
      this.dirtyDot,
      this.statusEl,
      spacer,
      this.yamlBtn,
      formatBtn,
      reloadBtn,
      this.saveBtn
    );
    if (!host.embedded) {
      const closeBtn = document.createElement("button");
      closeBtn.className = "wf-btn";
      closeBtn.textContent = "✕";
      closeBtn.addEventListener("click", () => void this.requestClose());
      head.append(closeBtn);
    }

    // ---- the START surface (no workflow file yet) ----
    //
    // Not a big empty box with a sentence in it. A repo with no workflow is the NORMAL
    // starting point — it is where every repo begins — so this is the pane's front door, and
    // a front door should be the shortest path to being inside. One line of what a workflow
    // is, one button that writes a real, commented, valid one, and the roster it will contain
    // so nobody has to press the button to find out what it does.
    this.emptyEl = el("div", "wf-start");
    const startHead = el("div", "wf-start-head");
    this.startPathEl = el("span", "wf-start-path", WORKFLOW_FILE);
    startHead.append(el("span", "wf-start-title", "Start a workflow"), this.startPathEl);
    const startBody = el(
      "div",
      "wf-start-body",
      "Declares the agent blocks a run may use, the path between them, and the gate that must " +
        "pass before a merge. Committed, so everyone who clones the repo gets it. Orrerix reads " +
        "it only when Advanced orchestrator is ticked."
    );
    const starterBtn = document.createElement("button");
    this.starterBtn = starterBtn;
    starterBtn.className = "wf-btn wf-btn-primary";
    starterBtn.textContent = "Create workflow";
    starterBtn.title = "";  // set from `this.rel` in render() — see the startPathEl note there
    starterBtn.addEventListener("click", () => void this.scaffold());

    // What the button is about to write. A preview is cheaper than a paragraph and it is the
    // thing they actually want to know.
    const preview = el("div", "wf-start-preview");
    for (const [kind, label] of [
      ["planner", "Planner"],
      ["worker", "Worker"],
      ["reviewer", "Reviewer"],
    ] as const) {
      const chip = el("span", `wf-chip wf-chip-${kind}`, label);
      preview.append(chip);
    }
    preview.append(el("span", "wf-start-gate", "→ merge gate: the reviewer must PASS"));

    const startRow = el("div", "wf-start-row");
    startRow.append(starterBtn, preview);
    this.emptyEl.append(startHead, startBody, startRow);
    this.emptyEl.hidden = true;

    // ---- the ERROR surface (a workflow file that exists but cannot be read) ----
    //
    // Its own state, and that is the whole point (v2 bug 1). This used to fall through to the
    // empty state: a file that WAS there — saved as UTF-16 by a PowerShell redirect, say —
    // reported "No workflow in this repo yet" and offered to create one over the top of it.
    // The pane must never invite you to overwrite a file it refused to show you.
    this.errorEl = el("div", "wf-start");
    this.errorTextEl = el("div", "wf-start-body");
    this.errorTitleEl = el("div", "wf-start-title", `Can't read ${WORKFLOW_FILE}`);
    const retry = document.createElement("button");
    retry.className = "wf-btn";
    retry.textContent = "Retry";
    retry.addEventListener("click", () => void this.load());
    const errRow = el("div", "wf-start-row");
    errRow.append(retry);
    this.errorEl.append(this.errorTitleEl, this.errorTextEl, errRow);
    this.errorEl.hidden = true;

    // ---- roster (left) ----
    this.rosterEl = el("div", "wf-roster");

    // ---- the primary surface (middle) and the docked inspector (right) ----
    this.formPane = el("div", "wf-form");
    this.yamlPane = el("div", "wf-yaml");
    this.yamlArea = document.createElement("textarea");
    this.yamlArea.className = "wf-yaml-area";
    this.yamlArea.spellcheck = false;
    this.yamlArea.addEventListener("input", () => {
      // The text is the buffer. Re-read the model from it, refresh every OTHER surface,
      // and leave the textarea alone — rewriting it under the caret is how an editor
      // eats a keystroke.
      this.text = this.yamlArea.value;
      this.reanalyze();
      this.renderSelection();
      this.renderFindings();
      this.updateDirty();
    });
    this.yamlPane.append(this.yamlArea);
    this.graphPane = el("div", "wf-graph");

    // The primary surface: the canvas, or the raw YAML in its place. Exactly one is on screen,
    // and `hidden` is what says which (styles.css's `[hidden] { display: none !important }` is
    // load-bearing here — see test/hiddenrule.test.ts for why that is not belt and braces).
    const surfaceEl = el("div", "wf-surface");
    surfaceEl.append(this.graphPane, this.yamlPane);

    // The inspector, docked. Its HEAD is the part that makes a canvas click legible from across
    // the pane: it names what is selected, by id, beside the node you just clicked.
    this.inspTitleEl = el("div", "wf-insp-title");
    this.inspSubEl = el("div", "wf-insp-sub");
    const inspHead = el("div", "wf-insp-head");
    inspHead.append(this.inspTitleEl, this.inspSubEl);
    const inspector = el("div", "wf-inspector");
    inspector.append(inspHead, this.formPane);

    const main = el("div", "wf-main");
    main.append(surfaceEl, inspector);

    this.bodyEl = el("div", "wf-body");
    this.bodyEl.append(this.rosterEl, main);

    this.findingsEl = el("div", "wf-findings");

    // All FIVE surfaces. `errorEl` was built and never appended (rev-15 F1), so the state
    // added to fix the UTF-16 bug rendered as a blank pane — the fix's own headline case was
    // the one thing that didn't work. `render()` only toggles `hidden`; a surface that is not
    // in the document has nothing to un-hide.
    this.el.append(head, this.errorEl, this.emptyEl, this.bodyEl, this.findingsEl);

    // Which primary surface is showing, stated once BEFORE the first load resolves — otherwise
    // both the canvas and the raw YAML sit un-hidden until `render()` first reaches its body
    // surface, and a pane that opens on the error or start surface never gets there at all.
    this.applySurface();

    // Ctrl+S saves from anywhere in the pane — including from inside the textarea, where
    // the browser would otherwise do nothing at all.
    this.el.addEventListener("keydown", (e) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        void this.save();
        return;
      }
      // Delete removes what the CANVAS has selected — and only on the canvas, and never from
      // inside a field. Both halves are `canvasDeleteAllowed` (workflowpane.ts), and the second
      // half matters more now than it did under the tabs: the inspector is docked BESIDE the
      // canvas, so "typing in this block's prompt" and "this block is selected on the canvas"
      // are now the normal state rather than mutually exclusive tabs.
      if (e.key === "Delete" || e.key === "Backspace") {
        const inField = !!(e.target as HTMLElement | null)?.closest?.("input, textarea, select");
        if (!canvasDeleteAllowed({ surface: this.surface, inField })) return;
        e.preventDefault();
        this.deleteSelection();
      }
    });
  }

  // ---------- lifecycle ----------

  /** Load the file and render. Called by the pane once the view is in the document. */
  show(): void {
    this.el.hidden = false;
    void appVersion().then((v) => {
      if (!this.disposed) this.appVersion = v;
    });
    this.root = this.host.getRoot();
    this.retarget(this.host.getFile?.() || WORKFLOW_FILE);
    void this.load();
  }

  hide(): void {
    this.el.hidden = true;
  }

  dispose(): void {
    this.disposed = true;
    // Released here rather than left to the catalog's next prune: this is the
    // moment the view becomes garbage, and the prune only runs when something
    // else subscribes — which may be never.
    this.unsubscribeReports?.();
    this.unsubscribeReports = null;
    this.el.remove();
  }

  focus(): void {
    (this.surface === "yaml" ? this.yamlArea : this.el).focus();
  }

  // ---------- the unsaved-work contract (shared with the editor pane, #219) ----------

  /** Unsaved edits right now — asked WITHOUT prompting. The tab-close path needs the fact
   *  before it can decide how to ask. */
  get dirty(): boolean {
    return this.text !== this.savedText;
  }

  /** The file this view holds, for the persisted layout (#217's `file` field). */
  get openPathRel(): string {
    return this.rel;
  }

  /** May the pane close? Clean → yes; dirty → ask, and a confirmed discard ACTUALLY
   *  discards (the same `discardEdits` rule the editor obeys, stated once in
   *  dirtystate.ts so this view cannot quietly re-implement "discard" as "hide"). */
  async canDiscard(): Promise<boolean> {
    if (closeDecision(this.dirty) === "close") return true;
    const discard = await modal<boolean>((resolve) => ({
      title: "Discard unsaved workflow changes?",
      body: `${this.rel} has unsaved edits. Discarding drops them — the workflow goes back to what's on disk.`,
      buttons: [
        { label: "Cancel", value: false },
        { label: "Discard", value: true, kind: "danger" },
      ],
      onKey: (k) => (k === "Escape" ? resolve(false) : undefined),
    }));
    if (discard) {
      this.setText(discardEdits(this.savedText));
      this.render();
    }
    return discard;
  }

  /** What this view is holding, for the app-quit guard's enumeration (#219). */
  bufferReport(): { file: string | null; dirty: boolean } | null {
    return { file: this.rel, dirty: this.dirty };
  }

  private async requestClose(): Promise<void> {
    if (!(await this.canDiscard())) return;
    this.host.onClose();
  }

  // ---------- disk ----------

  /** Read the workflow, and the canvas layout beside it.
   *
   *  THE BUG THIS METHOD USED TO HAVE (v2 bug 1, and it is the one the human hit): it treated
   *  EVERY read failure as "there is no workflow here". Only `not-found` means that. A file
   *  that exists but cannot be decoded — and the ordinary way to produce one on Windows is to
   *  create it from PowerShell, whose `>` and `Out-File` write UTF-16, which is not valid
   *  UTF-8, which the backend correctly reports as `binary` — rendered the "no workflow yet"
   *  empty state behind a toast that had already gone. The pane then offered to CREATE a
   *  starter over the top of a file it had refused to show. So the two are now separate
   *  states, and the error one has no create button in it. */
  private async load(): Promise<void> {
    if (!this.root) {
      this.setText("");
      this.render();
      return;
    }
    // A fresh read is a different file (or a different version of one), so a rewrite the human
    // consented to earlier was consent about text that is no longer there.
    this.rewriteConfirmed = false;
    try {
      const fr = await ftReadFile(this.root, this.rel);
      if (this.disposed) return;
      this.exists = true;
      this.loadError = null;
      this.savedHash = fr.hash;
      this.savedText = fr.content;
      this.text = fr.content;
    } catch (err) {
      if (this.disposed) return;
      const code = errorCode(err);
      // #1153 phase 4: the DEFAULT path missing is the one case that means
      // "maybe this repo still uses the old `.loomux/` spelling". Adopting it is
      // conditional on the legacy read SUCCEEDING — a repo with neither file
      // must stay on the preferred path, or the empty state would offer to
      // create a workflow at the deprecated name. `legacyFallbackFor` returns
      // null for the legacy path itself and for any explicit host-supplied file,
      // so at most one extra read happens and only from the default. A
      // `binary`/permission error is NOT a fallback trigger: that file is there,
      // and quietly opening a different one would hide it.
      const legacy = code === "not-found" ? legacyFallbackFor(this.rel) : null;
      if (legacy) {
        const found = await this.readLegacy(this.root, legacy);
        if (this.disposed) return;
        if (found) {
          this.retarget(legacy);
          this.exists = true;
          this.loadError = null;
          this.savedHash = found.hash;
          this.savedText = found.content;
          this.text = found.content;
          await this.loadLayout();
          this.reanalyze();
          this.render();
          return;
        }
      }
      this.exists = false;
      this.savedHash = "";
      this.savedText = "";
      this.text = "";
      // "not-found" is not an error: it is a repo that hasn't written a workflow yet, which is
      // where every repo starts. ANYTHING else means the file is there and we can't read it.
      this.loadError =
        code === "not-found"
          ? null
          : code === "binary"
            ? `The file is there, but it isn't valid UTF-8 text — so orrerix can't read it, and neither can the backend. A workflow written from PowerShell with \`>\` or \`Out-File\` is UTF-16; re-save it as UTF-8 (\`Set-Content -Encoding utf8NoBOM\`) and it will open.`
            : `${errorMessage(err)}`;
    }
    await this.loadLayout();
    this.reanalyze();
    this.render();
  }

  /** Read a fallback workflow path, or null if there is nothing usable there. A read that
   *  fails for ANY reason returns null: the preferred path's own error is the one the pane
   *  reports, and a second file's permission problem must not replace it. */
  private async readLegacy(root: string, rel: string): Promise<FileRead | null> {
    try {
      return await ftReadFile(root, rel);
    } catch {
      return null;
    }
  }

  /** Point this pane at `rel` — the path its header shows, its saves write, and its layout
   *  sibling is derived from. One setter, so those three cannot drift apart. */
  private retarget(rel: string): void {
    this.rel = rel;
    this.pathLabel.textContent = rel;
    this.pathLabel.title = this.root ? `${this.root} · ${rel}` : rel;
  }

  /** The canvas positions. A layout that is missing or corrupt is simply COMPUTED instead —
   *  never a finding, never a dialog, never a reason not to open the workflow. Nothing in that
   *  file is anyone's work; it is a picture we can redraw. */
  private async loadLayout(): Promise<void> {
    if (!this.root) return;
    try {
      const fr = await ftReadFile(this.root, layoutFileFor(this.rel));
      if (this.disposed) return;
      this.layout = parseLayout(fr.content);
    } catch {
      this.layout = emptyLayout();
    }
    this.savedLayout = this.layout;
  }

  private async reload(): Promise<void> {
    if (this.dirty && !(await this.canDiscard())) return;
    await this.load();
  }

  private async save(): Promise<void> {
    if (!this.root || !this.dirty) return;
    // No rewrite-impact gate here (#233): every form/canvas edit already went through
    // `commit()`, which reuses the ORIGINAL text for whatever it didn't touch — so by the
    // time a save happens, `this.text` is not a blind canonical rewrite of the whole file.
    // The one operation left that still rewrites wholesale on purpose is Format, and it asks
    // there, not here.
    //
    // Saving a file whose YAML doesn't parse is allowed on purpose: it is text, the human
    // may be mid-edit, and a half-finished workflow on disk is recoverable while a lost
    // one is not. The findings strip is what says it isn't runnable yet.
    try {
      await this.ensureConfigDir();
      // CREATING vs EDITING are different writes, and conflating them destroyed files
      // (rev-15 F2). When we believe there is no file, we cannot write with a null expected
      // hash — `write_file` reads that as "write unconditionally", so a workflow that appeared
      // AFTER the pane opened (an agent wrote one, a `git pull` brought one in, a teammate's
      // branch landed) was overwritten by our scaffold, and the pane said "Saved".
      //
      // So a create CLAIMS THE PATH first, atomically: `fm_new_file` is `create_new(true)`,
      // which refuses — without truncating — if anything is already there. Then we read the
      // (empty) file we just made and write against ITS hash, so even the sliver between the
      // claim and the write is guarded by the same conflict machinery as every other save.
      const plan = savePlan({ exists: this.exists, savedHash: this.savedHash });
      const hash = plan.kind === "guarded-write" ? plan.expectedHash : await this.claimFile();
      if (hash === null) return; // the path was taken; the error surface now says so
      const res = await ftWriteFile(this.root, this.rel, this.text, hash);
      this.savedText = this.text;
      this.savedHash = res.hash;
      this.exists = true;
      this.updateDirty();
      await this.saveLayout("save"); // the roster on disk and in memory are the same roster now
      showToast(`Saved ${this.rel}`, "info");
    } catch (err) {
      if (errorCode(err) === "conflict") await this.resolveConflict();
      else showToast(`Save failed: ${errorMessage(err)}`);
    }
  }

  /** Ask ONCE, before the first **Format** that would rewrite a human-authored file into fully
   *  canonical form — and only when that rewrite actually costs them something (rev-15 F6,
   *  moved here from every save by #233).
   *
   *  Before #233, EVERY form or canvas edit re-serialized the whole workflow from the model,
   *  unconditionally, and the model did not carry comments — so this guarded every `Ctrl+S`.
   *  Now `commit()` (below) reuses the original text for whatever an edit didn't touch, so an
   *  ordinary save no longer performs the all-or-nothing rewrite this dialog is about. The one
   *  place that rewrite still happens ON PURPOSE is the explicit **Format** button — a human
   *  asking to canonicalize the whole file, comments and all, in one step — and that is the
   *  only place left that needs to say so first.
   *
   *  ONCE per file, not once per Format press: a human who has said "yes, canonicalize it" has
   *  said it about that file, and asking again on every press is how you train someone to stop
   *  reading the question. Reset by `load()`, because that is a different file (or a different
   *  version of it) and the answer was about the old one.
   *
   *  CANCEL IS THE DEFAULT — the affirmative button is deliberately not the focused one here,
   *  which is the opposite of every other dialog in this pane. Everything else asks about
   *  something recoverable; this asks about work that is not. */
  private rewriteConfirmed = false;

  private async confirmFormatRewrite(canonical: string): Promise<boolean> {
    if (this.rewriteConfirmed) return true;
    const impact = rewriteImpact(this.text, canonical, (t) => formatWorkflowText(t) === t);
    if (!impact) return true; // a faithful rewrite — silent, as it should be

    const ok = await modal<boolean>((resolve) => ({
      title: "This rewrites the file",
      body: rewriteImpactMessage(impact, this.rel),
      buttons: [
        { label: "Rewrite and format", value: true, kind: "danger" },
        { label: "Cancel", value: false },
      ],
      onKey: (k) => (k === "Escape" ? resolve(false) : undefined),
    }));
    if (ok) this.rewriteConfirmed = true;
    return ok;
  }

  /** Claim `this.rel` for a file that does not exist yet, and return the hash to write
   *  against — or null when something got there first, in which case the pane is now showing
   *  the error surface and the caller must not write.
   *
   *  `fm_new_file` is the atomic half: `create_new(true)` ("create, but only if it isn't
   *  there") is one syscall, so there is no window between the check and the create. The
   *  `ftReadFile` after it is what turns the rest of the save into an ordinary hash-guarded
   *  write — if anything touches the file between our claim and our write, that is a conflict
   *  and the human gets the same three-way choice as always, instead of a silent overwrite. */
  private async claimFile(): Promise<string | null> {
    const root = this.root!;
    const parts = this.rel.split(/[\\/]/);
    const name = parts.pop() ?? WORKFLOW_FILE;
    const dir = parts.join("/");
    try {
      await fmNewFile(root, dir, name);
    } catch (err) {
      if (fmErrorCode(err) !== "exists") throw err;
      // Something wrote a workflow while this pane was sitting on its start surface. Do NOT
      // scaffold over it — it is somebody's work, it is probably the thing they wanted, and
      // this pane has never even shown it to them. Say so, and let Retry read it.
      this.loadError =
        `A workflow appeared at ${this.rel} while this pane was open — written by an agent, a git pull, or another editor. ` +
        `It has NOT been overwritten. Retry to load it (your unsaved text is discarded).`;
      this.render();
      showToast(`${this.rel} already exists — nothing was overwritten.`);
      return null;
    }
    const fresh = await ftReadFile(root, this.rel); // the empty file we just created
    return fresh.hash;
  }

  /** Make sure the workflow file's directory exists before writing into it.
   *
   *  THE OTHER HALF OF v2 BUG 1, and it made the pane's headline feature a lie: `ft_write_file`
   *  writes atomically (temp file + rename) and does NOT create parent directories, so in a
   *  repo with no config dir — i.e. EVERY repo that has never had a workflow, which is exactly
   *  the repo the "create a workflow" button exists for — the write failed with a raw io error
   *  ("The system cannot find the path specified"). The button appeared to work, the toast
   *  said "Save failed", and reopening the pane showed the empty state again, because nothing
   *  had ever been written. Between the two halves, the pane both mis-reported an existing
   *  workflow as absent AND could not create the one it offered to create.
   *
   *  No new backend command: `fm_new_folder` (#214, the file manager's "New folder") already
   *  does exactly this, through the same root+rel path safety. An "it already exists" failure
   *  is the success case here, so every error is swallowed and the WRITE is left to be the
   *  thing that reports a real problem — it is the one that knows whether it worked. */
  private async ensureConfigDir(): Promise<void> {
    if (!this.root) return;
    const dir = this.rel.split(/[\\/]/).slice(0, -1).join("/");
    if (!dir) return; // a workflow file at the repo root needs no directory
    try {
      await ftListDir(this.root, dir);
      return; // already there
    } catch {
      // Not there (or not readable) — try to create it. One level is all the schema needs.
      try {
        await fmNewFolder(this.root, "", dir);
      } catch {
        // Swallowed on purpose: a race with something else creating it lands here too, and
        // the write immediately after is the honest test of whether we can proceed.
      }
    }
  }

  /** Write the canvas positions, if they changed.
   *
   *  Deliberately NOT part of the dirty/unsaved-work contract: a node's x/y is not the human's
   *  WORK, and a dialog asking whether to save the fact that you nudged a box is a dialog that
   *  teaches people to click through dialogs. A drag writes it directly; a real save writes it
   *  too. No hash guard — this file is ours, nobody else writes it, and a lost position costs a
   *  drag.
   *
   *  `prune` is only ever true from `save()`, and that is the whole of rev-15 F5. Pruning drops
   *  the positions of blocks that "no longer exist" — but a DRAG happens against the unsaved
   *  buffer, where a block the human has deleted-but-not-saved does not exist *yet*. Pruning
   *  there wrote the deletion into `workflow.layout.json` on disk before the human had committed
   *  it to `workflow.yml`, so discarding the edit brought the block back with its position gone.
   *  Pruning belongs where its own comment always claimed it was: at a save, just after the
   *  workflow write succeeded — which is the one moment the roster on disk and the roster in
   *  memory are the same roster. */
  private async saveLayout(when: LayoutWrite = "drag"): Promise<void> {
    if (!this.root) return;
    // WHAT MAY BE FORGOTTEN is a rule (`workflowpane.layoutPruneIds`), not a flag: on a save the
    // roster on disk and the roster in memory are the same, so pruning against it is safe; on a
    // drag they are not, so the union of the two is what survives.
    const saved = this.savedText.trim() ? parseWorkflow(this.savedText).workflow : null;
    const next = pruneLayout(this.layout, layoutPruneIds(saved, this.analysis.workflow, when));
    this.layout = next;
    if (layoutEquals(next, this.savedLayout)) return;
    try {
      await this.ensureConfigDir();
      await ftWriteFile(this.root, layoutFileFor(this.rel), serializeLayout(next), null);
      this.savedLayout = next;
    } catch {
      // A layout we couldn't save is a picture that comes back computed instead. Not worth a
      // toast, and certainly not worth failing the workflow save that may have preceded it.
    }
  }

  /** Write the scaffold — a commented, valid workflow — into the buffer, and save it. The one
   *  moment `authored_with:` is stamped, because this is the one moment the pane AUTHORS a
   *  file rather than editing one. */
  private async scaffold(): Promise<void> {
    // THE LAST WORD ON THE CREATE PATH (#222 live bug 3). A create is allowed on the start
    // surface and nowhere else — `createAllowed` is the same decision that draws the button, so
    // reaching here in any other state means the DOM has drifted from the rules, which is exactly
    // what happened: a stylesheet left the button on screen over a loaded workflow, and pressing
    // it scaffolded over that workflow with a hash-guarded write that the backend was right to
    // honour. Refusing here means no future wiring mistake, CSS or otherwise, can turn "Create"
    // into "destroy" — the guard no longer depends on the button being where we think it is.
    if (!createAllowed({ loadError: this.loadError, exists: this.exists, text: this.text })) {
      // Two states refuse a create, and the message has to be true in BOTH (rev-17 F5): a workflow
      // is loaded, OR a file is there that we could not read. "Already open" is a lie in the second
      // one — the file precisely did not open, which is the whole reason we won't scaffold over it.
      // What holds either way is the only thing worth saying: nothing was destroyed.
      showToast("Nothing was created or overwritten — Create is only offered where there's no workflow.");
      this.render();
      return;
    }
    // `this.rel` — the path `save()` is about to write to — not the default, so the
    // header names the file that will actually exist (#1153 phase 4).
    this.setText(scaffoldWorkflowText(this.appVersion, this.rel));
    this.render();
    await this.save();
    // Land them on the canvas, looking at the thing they just made. Since #880 that is simply
    // the pane's normal state — the canvas is the primary surface and the inspector is beside
    // it — so all this has to do is make sure a YAML toggle left over from a previous file
    // isn't sitting in front of it.
    this.setSurface("canvas");
  }

  /** The file changed under us since we read it — an agent, git, or another editor. Same
   *  three-way choice the file editor offers, for the same reason: an agent rewriting the
   *  workflow it is running under is a real scenario here, not a hypothetical one. */
  private async resolveConflict(): Promise<void> {
    const root = this.root;
    if (!root) return; // unreachable: only a save can conflict, and a save needs a root
    const choice = await modal<ConflictChoice>((resolve) => ({
      title: "Workflow changed on disk",
      body: `${this.rel} was modified since you opened it (by an agent, another tool, or git). Overwrite it with your version, reload the on-disk version (losing your edits), or cancel?`,
      buttons: [
        { label: "Cancel", value: "cancel" },
        { label: "Reload", value: "reload" },
        { label: "Overwrite", value: "overwrite", kind: "danger" },
      ],
      onKey: (k) => (k === "Escape" ? resolve("cancel") : undefined),
    }));
    if (choice === "cancel") return;
    if (choice === "reload") {
      await this.load();
      return;
    }
    try {
      const res = await ftWriteFile(root, this.rel, this.text, null);
      this.savedText = this.text;
      this.savedHash = res.hash;
      this.exists = true;
      this.updateDirty();
      showToast("Overwrote on-disk changes");
    } catch (err) {
      showToast(`Save failed: ${errorMessage(err)}`);
    }
  }

  // ---------- the buffer ----------

  private setText(text: string): void {
    this.text = text;
    this.yamlArea.value = text;
    this.reanalyze();
  }

  /** Write the model back into the buffer. EVERY form edit goes through here: the YAML is
   *  the source of truth, so a form edit is not "state the file will catch up with later"
   *  — it IS a file edit, immediately.
   *
   *  Comment-preserving, not a blind canonical rewrite (#233): `serializeWorkflowPreserving`
   *  reuses `this.text` — the buffer as it stood a moment ago — for every top-level piece the
   *  edit didn't touch, and only falls back to the canonical form for the piece that changed.
   *  That is what makes dragging one edge in a heavily-commented file a one-section diff
   *  instead of the whole file. */
  private commit(w: Workflow): void {
    this.setText(serializeWorkflowPreserving(w, this.text));
  }

  private reanalyze(): void {
    this.analysis = analyzeWorkflow(this.text, this.knobLookup);
    this.ensureCliKnobs();
  }

  /** The capability answer the model's validation pass asks for (#687).
   *
   *  `undefined` in the map = not fetched yet, and the lookup returns `null` for
   *  it — NOT an answer (see `KnobLookup`), so the pass defers instead of
   *  inventing a finding out of its own ignorance. `null` in the map = we asked
   *  and the call failed, which `knobState` renders as disabled-with-a-reason. */
  private knobLookup = (cli: string, model: string): KnobStates | null => {
    const caps = this.cliKnobs.get(cli);
    // #993: the detected per-model levels narrow the CLI's general set. The
    // validation pass reads the same lookup the editor's controls do, so a
    // block cannot be flagged for a level the picker was still offering.
    return caps === undefined ? null : knobState(caps, cli, model, modelCatalog.detail(cli, model));
  };

  /** Everything a list-models reply owes this pane (#993, #1020).
   *
   *  **A detection reply owes every surface `agent_cli_knobs` owes.** It is
   *  precisely the answer that makes {@link knobLookup} respond differently, so
   *  repainting only the dropdown leaves the Thinking-level row offering levels
   *  this pane's own validator then rejects — the human picks `xhigh`, the next
   *  mutation re-renders the row disabled, and the findings flag the block. The
   *  treatment below is `ensureCliKnobs`'s, deliberately identical: same pass,
   *  same three renders, same in-place knob repaint when the form must not be
   *  rebuilt.
   *
   *  One method rather than one per route, because both routes owe the same
   *  work: the lookup a block form fires when it paints, and the sweep's push
   *  (`modelCatalog.onReport`) for a form that was already open when the answer
   *  landed. A second copy is the second place a fix has to be remembered —
   *  which is the bug #997 caught here in the first place.
   *
   *  **It never rebuilds the form unconditionally.** `replaceChildren` destroys
   *  the input under the caret, so the pane's own rule holds: the form is
   *  redrawn only when the human is not inside it, and repainted in place when
   *  they are. The menu goes through {@link refreshBlockModels}, which is
   *  `null` when no form is on screen and defers past the mid-type window when
   *  one is.
   *
   *  **Idempotent per CLI**, which is where the two routes are reconciled: the
   *  first delivery to arrive does the work and the second is a no-op, whichever
   *  order they land in. A second application could only ever repeat the first —
   *  the sweep asks each CLI once, so there is no later answer for the same one
   *  to carry. */
  private applyDetection(program: string): void {
    if (this.detectionsApplied.has(program)) return;
    this.detectionsApplied.add(program);
    // The findings are the pane's, not any one form's, so they are recomputed
    // and repainted whatever happened to the form meanwhile — a reply that
    // landed after the human moved on still corrects the file's analysis.
    this.analysis = analyzeWorkflow(this.text, this.knobLookup);
    this.renderRoster();
    this.renderFindings();
    this.renderGraph();
    // Through the LIVE hooks, never a captured closure: `renderInspector()`
    // nulls both precisely so a late reply cannot paint into a detached row.
    this.refreshBlockModels?.();
    if (this.formPane.contains(document.activeElement)) this.repaintBlockKnobs?.();
    else this.renderInspector();
  }

  /** Ask what models `cli` reports, at most once per pane — see {@link modelProbes}. */
  private probeModels(cli: string): Promise<CliProbe> {
    let p = this.modelProbes.get(cli);
    if (!p) {
      p = modelCatalog.probe(cli);
      this.modelProbes.set(cli, p);
    }
    return p;
  }

  /** Fetch `agent_cli_knobs` for every CLI the file names, once each (#687).
   *
   *  The pane mirrors no vendor capability of its own — which knobs a CLI has, and
   *  the reason it lacks one, are the backend's `CLI_CAPS` row, asked for. Each
   *  reply re-runs the analysis so the knob findings and the form's controls
   *  appear the moment the answer lands, without blocking the file from opening
   *  on an IPC round-trip. */
  private ensureCliKnobs(): void {
    for (const b of this.analysis.workflow.blocks) {
      const cli = b.cli.trim();
      if (!cli || this.knobsAsked.has(cli)) continue;
      this.knobsAsked.add(cli);
      void agentCliKnobs(cli).then((caps) => {
        this.cliKnobs.set(cli, caps);
        // Re-run the same pass the pane would have run had the reply been in
        // hand when the file opened.
        this.analysis = analyzeWorkflow(this.text, this.knobLookup);
        // NOT `render()`: this lands whenever the IPC happens to resolve, which
        // can be mid-keystroke — and `render()` rewrites the YAML textarea from
        // the model, which is how an editor eats a keystroke (the same reason the
        // textarea's own input handler refreshes every surface BUT itself). The
        // form is redrawn only when the human isn't inside it.
        this.renderRoster();
        this.renderFindings();
        this.renderGraph();
        // …but the knob rows are exactly what this reply is the answer for, so
        // when the inspector can't be redrawn they are repainted in place instead
        // of being left saying "reading this CLI's capabilities…" until the human
        // clicks elsewhere and back (#935).
        if (this.formPane.contains(document.activeElement)) this.repaintBlockKnobs?.();
        else this.renderInspector();
      });
    }
  }

  /** The explicit "rewrite this whole file in canonical form" action — the one place left
   *  that drops comments on purpose, in one step, and the one place that still asks first
   *  (`confirmFormatRewrite`). Everyday form/canvas edits go through `commit()` instead, which
   *  preserves comments for whatever they didn't touch. */
  private async format(): Promise<void> {
    if (this.syntaxBroken()) {
      showToast("Fix the YAML syntax first — formatting a file we can't read would rewrite it wrong.");
      return;
    }
    const canonical = serializeWorkflow(this.analysis.workflow);
    if (!(await this.confirmFormatRewrite(canonical))) return;
    this.setText(canonical);
    this.render();
  }

  /** True while the text cannot be read at all. The form is disabled here — see the note
   *  at the top of the file: serializing a half-understood model back over the buffer
   *  would destroy the broken text the human is trying to fix.
   *
   *  `isUnreadable` (workflowmodel.ts) is the same predicate `serializeWorkflowPreserving`
   *  gates its own fallback on (#233 B3) — the two must agree, or a file this view still lets
   *  the human edit (e.g. `version: 2`, unsupported but readable) would silently full-rewrite
   *  on its very first edit for a reason never shown here. */
  private syntaxBroken(): boolean {
    return isUnreadable(this.analysis.findings);
  }

  private updateDirty(): void {
    this.dirtyDot.hidden = !this.dirty;
    this.saveBtn.disabled = !this.dirty;
  }

  // ---------- render ----------

  /** Three states, and telling them apart is the fix for v2 bug 1:
   *
   *    ERROR — the file is THERE and we cannot read it. Say why; offer Retry; offer NOTHING
   *            that writes, because writing here means overwriting a file we refused to show.
   *    START — there is no file. The normal beginning of every repo, so this is a front door,
   *            not an apology: one line, one button, and the roster it is about to write.
   *    BODY  — a workflow. The roster, the form, the canvas, the YAML, the findings. */
  private render(): void {
    // WHICH SURFACE is a rule, and it lives in `workflowpane.paneSurface` — pure, and tested.
    // The last time this view worked it out for itself, it showed "there is no workflow here"
    // for a file that was there and merely unreadable, and then offered to create one over it.
    const state = { loadError: this.loadError, exists: this.exists, text: this.text };
    const surface = paneSurface(state);
    const error = surface === "error";
    const start = surface === "start";
    this.errorEl.hidden = !error;
    this.errorTextEl.textContent = this.loadError ?? "";
    this.emptyEl.hidden = !start;
    this.bodyEl.hidden = error || start;
    this.findingsEl.hidden = error || start;
    // EVERY surface here names the file this pane is actually open on, not the default
    // one — including the Create button's tooltip, which used to be a static literal
    // naming `.loomux/workflow.yml` while the preview beside it read `.orrerix/...`
    // (#1153 phase 4, rev-lead round 1 B2). The empty state must never advertise the
    // deprecated spelling as the thing it is about to create.
    this.errorTitleEl.textContent = `Can't read ${this.rel}`;
    this.startPathEl.textContent = this.rel;
    this.starterBtn.title = `Scaffold a commented ${this.rel} — today's pipeline, ready to edit`;
    // Pressability is the RULE, not a side-effect of being on screen. `hidden` is now honoured
    // (styles.css `[hidden]`), so this is belt and braces — but it is the belt that matters: the
    // live bug was a create button the human could press over a loaded workflow, and the thing
    // that made it pressable was a stylesheet. A `disabled` that follows the same decision as the
    // surface cannot be undone by one.
    this.starterBtn.disabled = !createAllowed(state);
    this.yamlArea.value = this.text;
    this.updateDirty();
    if (error || start) {
      this.statusEl.textContent = "";
      this.statusEl.className = "wf-status";
      return;
    }
    this.renderSelection();
    this.renderFindings();
    this.applySurface();
  }

  /** Switch the primary surface. There is no `renderInspector()` here on purpose: the inspector
   *  is docked beside BOTH surfaces, so switching one does not change what it is showing — which
   *  is the whole reason the tabs went. */
  private setSurface(surface: Surface): void {
    this.surface = surface;
    this.applySurface();
    if (surface === "yaml") this.yamlArea.focus();
  }

  private applySurface(): void {
    this.graphPane.hidden = this.surface !== "canvas";
    this.yamlPane.hidden = this.surface !== "yaml";
    this.yamlBtn.classList.toggle("active", this.surface === "yaml");
    this.yamlBtn.setAttribute("aria-pressed", String(this.surface === "yaml"));
  }

  /** The roster: the workflow itself, each block, and the gate — one column, one click to
   *  the form for any of them. A block with an ERROR carries a marker here, so a broken
   *  block is visible without opening it. */
  private renderRoster(): void {
    const w = this.analysis.workflow;
    const rows: HTMLElement[] = [];

    const row = (sel: Selection, title: string, sub: string, bad: boolean): HTMLElement => {
      const r = el("button", "wf-row");
      const cur = this.selection;
      const active =
        cur.kind === sel.kind &&
        (sel.kind !== "block" || (cur as { index: number }).index === sel.index);
      r.classList.toggle("active", active);
      const main = el("span", "wf-row-main", title);
      const meta = el("span", "wf-row-sub", sub);
      r.append(main, meta);
      if (bad) r.append(el("span", "wf-row-bad", "!"));
      r.addEventListener("click", () => this.selectItem(sel));
      return r;
    };

    rows.push(el("div", "wf-roster-head", "Workflow"));
    rows.push(row({ kind: "workflow" }, w.name || "(unnamed)", `version ${w.version}`, false));

    rows.push(el("div", "wf-roster-head", "Blocks"));
    w.blocks.forEach((b, i) => {
      const bad = this.blockFindings(b).some((f) => f.severity === "error");
      // #687: a pinned thinking level / context window is part of what this block
      // will actually run, so the row says so. Unpinned adds nothing — the row
      // stays the line it has always been.
      const knobs = `${b.effort ? ` · effort: ${b.effort}` : ""}${b.context ? ` · context: ${b.context}` : ""}`;
      rows.push(
        row(
          { kind: "block", index: i },
          b.name || b.id || "(no id)",
          `${b.kind || "?"} · ${b.cli || "?"}${knobs}`,
          bad
        )
      );
    });

    const add = el("button", "wf-add", "+ Add block");
    add.addEventListener("click", () => void this.createBlock());
    (add as HTMLButtonElement).disabled = this.syntaxBroken();
    rows.push(add);

    rows.push(el("div", "wf-roster-head", "Gate"));
    const gate = w.gates.merge;
    const gateBad = this.analysis.findings.some((f) => f.code.startsWith("gate-"));
    rows.push(
      row(
        { kind: "gate" },
        "Merge",
        gate ? `${gate.require} · ${gate.reviewers.length} reviewer(s)` : "none — any review merges",
        gateBad
      )
    );

    // The three OPTIONAL policy sections (#1020), beside the gate for the same reason the gate
    // is beside the blocks: they are edited the same way, and a second place to click would be
    // a second place to look. Each sub-line answers the one question that matters about an
    // optional section — does this FILE say anything, or is loomux's own default in force? —
    // because a form full of empty fields cannot distinguish those two by itself.
    rows.push(el("div", "wf-roster-head", "Policy"));
    const intake = w.intake;
    const declaredLabels = intake?.labels
      ? Object.keys(intake.labels).filter((k) => k !== "extra").length
      : 0;
    rows.push(
      row(
        { kind: "intake" },
        "Intake",
        intake
          ? `${intake.source || "inherited source"}${declaredLabels ? ` · ${declaredLabels} label(s)` : ""}`
          : "not declared — orrerix's default",
        this.sectionBad("intake")
      )
    );
    const mq = w.merge_queue;
    rows.push(
      row(
        { kind: "merge_queue" },
        "Merge queue",
        mq
          ? `${mq.enabled ? "on" : "off"}${mq.max_batch !== undefined ? ` · batch ${mq.max_batch}` : ""}`
          : "not declared — off",
        this.sectionBad("merge_queue")
      )
    );
    const resourceCount = Object.keys(w.resources ?? {}).length;
    rows.push(
      row(
        { kind: "resources" },
        "Resources",
        w.resources ? `${resourceCount} resource(s)` : "not declared — no locks",
        this.sectionBad("resources")
      )
    );

    this.rosterEl.replaceChildren(...rows);
  }

  /** Does this policy section carry an ERROR? Routed by the finding's own `section` rather
   *  than by matching its message, so a reworded message can never quietly stop marking the
   *  row it is about. */
  private sectionBad(section: FindingSection): boolean {
    return this.sectionFindings(section).some((f) => f.severity === "error");
  }

  private sectionFindings(section: FindingSection): Finding[] {
    return this.analysis.findings.filter((f) => f.section === section);
  }

  /** The findings about ONE block row. A finding names a block by ID, because that is what
   *  a human reads — so an id-LESS stub takes the id-less findings ("a block has no id"),
   *  and where there are two such stubs they each show it. That is not a compromise: the
   *  finding is the same finding, and it is true of both. */
  private blockFindings(b: WorkflowBlock): Finding[] {
    return this.analysis.findings.filter((f) => f.blockId === (b.id || ""));
  }

  /** Point the pane at something — from the roster, the canvas, or a finding. ONE path, because
   *  the bug #880 is about was two paths that were supposed to agree and didn't: the gate box
   *  remembered to bring the editor into view and the node handler didn't, so clicking a block
   *  looked like a dead click. Every selecting gesture now goes through here, and the three
   *  surfaces that show a selection are refreshed together or not at all. */
  private selectItem(sel: Selection): void {
    this.selection = sel;
    this.renderSelection();
  }

  /** Re-render the three surfaces that DISPLAY the selection, in the one order that keeps them
   *  agreeing with each other.
   *
   *  THE INSPECTOR GOES FIRST, and that is the whole reason this is a method rather than three
   *  calls at four call sites. `renderInspector` is the render that NORMALIZES the selection —
   *  it asks `inspectorTarget` what is actually still there and adopts the answer — while the
   *  roster and the canvas merely *highlight* whatever `this.selection` currently says. Render
   *  them first and a stale selection lights nothing at all: select the last block, let an agent
   *  rewrite `workflow.yml` without it, press Reload, and the roster draws with an index no row
   *  answers to while the inspector then quietly falls back to the workflow's own settings. The
   *  two disagree until something else re-renders the roster.
   *
   *  It was written correctly in `mutate` and open-coded the wrong way round in the other three
   *  places, which is the argument for stating it once: an ordering rule that lives in a comment
   *  next to one of its four call sites is a rule the next three call sites will get wrong. */
  private renderSelection(): void {
    this.renderInspector();
    this.renderRoster();
    this.renderGraph();
  }

  /** The docked inspector: a header naming what is selected, and the editor for it.
   *
   *  WHAT IS SHOWN is `inspectorTarget` (workflowpane.ts) — including the two ways a selection
   *  can outlive the thing it points at (a block deleted from under it, an edge erased) and the
   *  one state where nothing may be edited at all. This used to be a chain of inline checks that
   *  reassigned `this.selection` and re-entered itself; the reassignment is still needed — the
   *  roster highlights the SELECTION, so a fallback the roster never hears about would leave a
   *  stale row lit next to a different editor — but it happens once, here, from the answer. */
  private renderInspector(): void {
    // Whatever the last form left here belongs to controls that are about to be
    // replaced. Cleared FIRST, so a late `agent_cli_knobs` reply can never paint
    // into a detached row (#935) — every path below ends in a `formPane` swap.
    // Same for the model dropdown's deferred rebuild (#1020): a detection reply
    // that lands after the human selected another block must not reach the
    // picker they left behind.
    this.repaintBlockKnobs = null;
    this.refreshBlockModels = null;
    const w = this.analysis.workflow;
    const target = inspectorTarget(this.selection, w, this.syntaxBroken());
    // Adopt the fallback so the roster and the canvas agree with the editor. Never while
    // `blocked`: that state is about the BUFFER, not the selection, and forgetting which block
    // the human was on because they typo'd a colon would be its own small insult.
    if (target.kind !== "blocked") this.selection = target;

    const heading = inspectorHeading(target, w);
    this.inspTitleEl.textContent = heading.title;
    this.inspTitleEl.title = heading.title;
    this.inspSubEl.textContent = heading.sub;

    if (target.kind === "blocked") {
      const warn = el(
        "div",
        "wf-blocked",
        "The YAML doesn't parse, so the editor is disabled — editing it here would rewrite the text you're fixing. " +
          "Fix the error below in the raw file and the editor comes back."
      );
      const toYaml = document.createElement("button");
      toYaml.className = "wf-btn";
      toYaml.textContent = "Edit the YAML";
      toYaml.addEventListener("click", () => this.setSurface("yaml"));
      warn.append(toYaml);
      this.formPane.replaceChildren(warn);
      return;
    }
    if (target.kind === "block") {
      this.formPane.replaceChildren(this.blockForm(w, w.blocks[target.index]!, target.index));
      return;
    }
    if (target.kind === "edge") {
      this.formPane.replaceChildren(this.edgeForm(target.from, target.to));
      return;
    }
    if (target.kind === "intake") {
      this.formPane.replaceChildren(this.intakeForm(w));
      return;
    }
    if (target.kind === "merge_queue") {
      this.formPane.replaceChildren(this.mergeQueueForm(w));
      return;
    }
    if (target.kind === "resources") {
      this.formPane.replaceChildren(this.resourcesForm(w));
      return;
    }
    this.formPane.replaceChildren(target.kind === "gate" ? this.gateForm(w) : this.workflowForm(w));
  }

  // ---------- forms ----------

  private field(label: string, control: HTMLElement, hint?: string): HTMLElement {
    const f = el("label", "wf-field");
    f.append(el("span", "wf-label", label), control);
    if (hint) f.append(el("span", "wf-hint", hint));
    return f;
  }

  private textInput(value: string, onChange: (v: string) => void, placeholder = ""): HTMLInputElement {
    const i = document.createElement("input");
    i.className = "wf-input";
    i.type = "text";
    i.value = value;
    i.placeholder = placeholder;
    // `input`, not `change`: the file is the source of truth, so it should follow what the
    // human typed as they type it. The form is NOT re-rendered on these (that would move
    // the caret) — only the roster, the findings and the graph are.
    i.addEventListener("input", () => onChange(i.value));
    return i;
  }

  /** One model-knob field (#687): the label, the select, and the hint that
   *  carries the vendor's reason where loomux cannot deliver the knob — plus the
   *  `paint` that redraws all three from a fresh spec.
   *
   *  It is repaintable rather than rebuilt because the answer moves under a form
   *  that must not re-render: `context` is only available where the SELECTED
   *  model has a documented `[1m]` form, so it changes as the human types a model
   *  id, and re-rendering the form on a keystroke would rebuild the input under
   *  their caret. What to show is `workflowknobs.ts`' (`KnobFieldSpec`); this is
   *  the DOM half. */
  private knobRow(
    label: string,
    spec: KnobFieldSpec,
    onChange: (v: string) => void
  ): { field: HTMLElement; paint: (next: KnobFieldSpec) => void } {
    const s = document.createElement("select");
    s.className = "wf-input";
    s.addEventListener("change", () => onChange(s.value));
    const hint = el("span", "wf-hint");
    const field = el("label", "wf-field");
    field.append(el("span", "wf-label", label), s, hint);
    const paint = (next: KnobFieldSpec): void => {
      s.replaceChildren(
        ...next.options.map((o) => {
          const opt = document.createElement("option");
          opt.value = o.value;
          opt.textContent = o.label;
          return opt;
        })
      );
      s.value = next.selected;
      s.disabled = next.disabled;
      hint.textContent = next.hint;
    };
    paint(spec);
    return { field, paint };
  }

  private select(
    options: readonly string[],
    value: string,
    onChange: (v: string) => void
  ): HTMLSelectElement {
    const s = document.createElement("select");
    s.className = "wf-input";
    for (const o of options) {
      const opt = document.createElement("option");
      opt.value = o;
      opt.textContent = o;
      s.append(opt);
    }
    // A value the enum doesn't contain still SHOWS — as itself, marked. Dropping it would
    // silently rewrite the user's file to something they never chose the moment they
    // touched any other field on the block.
    if (value && !options.includes(value)) {
      const opt = document.createElement("option");
      opt.value = value;
      opt.textContent = `${value} (unknown)`;
      s.append(opt);
    }
    s.value = value;
    s.addEventListener("change", () => onChange(s.value));
    return s;
  }

  /** A select whose options have a LABEL distinct from their value — the shape every
   *  optional field here needs, because the empty value is a real choice ("inherit
   *  loomux's default") that has to read as one rather than as a blank row. The plain
   *  `select` above stays as it is: its values ARE their labels, which is right for a
   *  closed enum like `kind`. */
  private labelledSelect(
    options: readonly { value: string; label: string }[],
    value: string,
    onChange: (v: string) => void
  ): HTMLSelectElement {
    const s = document.createElement("select");
    s.className = "wf-input";
    for (const o of options) {
      const opt = document.createElement("option");
      opt.value = o.value;
      opt.textContent = o.label;
      s.append(opt);
    }
    // Same rule as `select`: a value this build doesn't offer still SHOWS, marked, so that
    // touching another field can never silently rewrite it to something nobody chose.
    if (value && !options.some((o) => o.value === value)) {
      const opt = document.createElement("option");
      opt.value = value;
      opt.textContent = `${value} (unknown)`;
      s.append(opt);
    }
    s.value = value;
    s.addEventListener("change", () => onChange(s.value));
    return s;
  }

  /** A bounded whole-number field for the policy sections, or EMPTY for "loomux's default".
   *
   *  The bounds are the engine's own (`RESOURCE_SLOTS_MAX`, `RESOURCES_MAX`, … — mirrored in
   *  workflowmodel.ts), and they are enforced on the way into the MODEL rather than only as
   *  `min`/`max` attributes: a spinner's attributes are advisory, and a typed `9999` would
   *  otherwise be written into a file the engine then refuses to load. The clamp is shown
   *  back on blur, so it is never a value the human can't see. A hand-written out-of-range
   *  value still gets its finding — this stops the FORM from producing one. */
  private boundedNumber(
    value: number | undefined,
    bounds: FieldBounds,
    onChange: (v: number | undefined) => void,
    placeholder = "orrerix's default"
  ): HTMLInputElement {
    const i = document.createElement("input");
    i.className = "wf-input";
    i.type = "number";
    i.min = String(bounds.min);
    // NO `max` attribute where the schema declares no ceiling. An absent `max` in
    // `POLICY_BOUNDS` is a statement — the engine accepts anything above the floor — and a
    // form that invented one would rewrite a legal `max_batch: 100` to whatever it made up
    // (#1020 review, finding 2). The floor is real everywhere, so it is always applied.
    if (bounds.max !== undefined) i.max = String(bounds.max);
    i.value = value === undefined ? "" : String(value);
    i.placeholder = placeholder;
    const clamp = (n: number): number => {
      const atLeast = Math.max(bounds.min, Math.round(n));
      return bounds.max === undefined ? atLeast : Math.min(bounds.max, atLeast);
    };
    i.addEventListener("input", () => {
      const raw = i.value.trim();
      if (!raw) {
        onChange(undefined);
        return;
      }
      const n = Number(raw);
      if (!Number.isFinite(n)) return; // a half-typed "-" or "e" — wait for the rest
      onChange(clamp(n));
    });
    // Show the clamp once they stop typing. Doing it on `input` would fight the caret of
    // someone typing "480" one digit at a time (the "4" would become the minimum).
    i.addEventListener("change", () => {
      const raw = i.value.trim();
      if (!raw) return;
      const n = Number(raw);
      if (Number.isFinite(n)) i.value = String(clamp(n));
    });
    return i;
  }

  /** The enable-toggle every optional section is edited through, and the reason all three
   *  forms are shaped like `gateForm`: the checkbox IS the section's presence in the file.
   *
   *  Off writes nothing at all — not `enabled: false`, not a block of defaults — because the
   *  model emits only what is declared, so an untouched (or re-untouched) section leaves the
   *  file exactly as it found it. That is the property a human relies on when they open this
   *  form to look rather than to change something. */
  private sectionToggle(label: string, on: boolean, onChange: (on: boolean) => void): HTMLElement {
    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.checked = on;
    cb.addEventListener("change", () => onChange(cb.checked));
    const line = el("label", "wf-check");
    line.append(cb, el("span", "wf-check-label", label));
    return line;
  }

  /** The findings for one policy section, rendered inline under its form — the same
   *  treatment `blockForm` gives a block's own findings, and for the same reason: the
   *  place to say what is wrong with a value is beside the field that sets it. */
  private sectionFindingList(section: FindingSection): HTMLElement | null {
    const found = this.sectionFindings(section);
    if (!found.length) return null;
    const list = el("ul", "wf-inline-findings");
    for (const f of found) list.append(el("li", `wf-finding wf-${f.severity}`, f.message));
    return list;
  }

  private workflowForm(w: Workflow): HTMLElement {
    const box = el("div", "wf-fields");
    box.append(
      this.field(
        "Name",
        this.textInput(w.name, (v) => {
          this.mutate((next) => {
            next.name = v;
          }, false);
        }),
        "Names the workflow in the audit record. Display only."
      )
    );
    const version = document.createElement("input");
    version.className = "wf-input";
    version.value = String(w.version);
    version.disabled = true;
    box.append(this.field("Schema version", version, "Set by orrerix; a newer version needs a newer build."));
    box.append(
      el(
        "p",
        "wf-note",
        "Edges are ADVISORY — they declare the intended path; the orchestrator still decides when to spawn what. " +
          "The merge gate is ENFORCED: orrerix refuses `gh pr merge` until every reviewer it names has recorded a PASS."
      )
    );
    return box;
  }

  private blockForm(w: Workflow, b: WorkflowBlock, index: number): HTMLElement {
    const box = el("div", "wf-fields");

    /** Edit THIS row, by index. Never by id: the rows that most need editing are the ones
     *  whose id is missing or duplicated, and an id lookup would edit the wrong one. */
    const edit = (f: (t: WorkflowBlock) => void, rerenderForm = true): void =>
      this.mutate((next) => {
        const t = next.blocks[index];
        if (t) f(t);
      }, rerenderForm);

    // The id is IMMUTABLE — once it is a usable identity. An id that is missing, malformed
    // or duplicated is not one: nothing can legally reference it, so nothing breaks when it
    // changes, and locking the field would leave the human staring at a validation error
    // with no way to fix the thing it is about (in the form, which is where they are). So
    // the field is editable in exactly the case where immutability protects nothing.
    const dupe = w.blocks.filter((x) => x.id === b.id).length > 1;
    const fixable = !b.id || !isValidBlockId(b.id) || dupe;
    const idInput = this.textInput(b.id, (v) => edit((t) => (t.id = v), false));
    idInput.disabled = !fixable;
    box.append(
      this.field(
        "Id",
        idInput,
        fixable
          ? "This id isn't usable yet, so it can still be set. Once it is valid and unique it becomes immutable — edges and the gate reference it."
          : "Immutable. Edges and the gate reference this id — renaming it would break them silently (the n8n bug)."
      )
    );

    box.append(
      this.field(
        "Name",
        this.textInput(b.name, (v) => edit((t) => (t.name = v), false)),
        "Display only — safe to rename at any time."
      )
    );

    box.append(
      this.field(
        "Kind",
        this.select(BLOCK_KINDS, b.kind, (v) => edit((t) => (t.kind = v))),
        "The capability class. A workflow defines personas, never capabilities: a planner is read-only, " +
          "a reviewer can never push, a worker gets a worktree."
      )
    );

    // The role hint (#250/#324): a persona/template/badge MARKER, never a capability — and
    // the offer is DERIVED from the same pairing rule the validator applies
    // (`roleHintsForKind`), so this picker cannot spell a combination the parser rejects, and
    // a hint added to the model shows up here without an edit. A block already declaring one
    // its kind can't carry still shows it, marked, because that is the finding it needs to fix.
    const hints = roleHintsForKind(b.kind);
    box.append(
      this.field(
        "Role hint",
        this.labelledSelect(
          [{ value: "", label: "none" }, ...hints.map((h) => ({ value: h, label: h }))],
          b.role_hint ?? "",
          (v) =>
            edit((t) => {
              if (v) t.role_hint = v;
              else delete t.role_hint;
            })
        ),
        hints.length
          ? "Optional and INERT: it picks a persona/template fragment and a badge. Capability still comes from kind alone."
          : `No role hint applies to a ${b.kind || "block"} — each hint requires the one kind it is meaningless without.`
      )
    );

    box.append(
      this.field("Agent CLI", this.select(WORKFLOW_CLIS, b.cli, (v) => edit((t) => (t.cli = v))))
    );

    // The model field is the SAME control the launcher renders (#935): one
    // dropdown, one catalog — the CLI's own reported models merged over this
    // repo's curated suggestions — with the `custom…` escape that keeps it a
    // wider field than the free-text box it replaces, not a narrower one. A CLI
    // this repo has no curated row for (`gemini`) is still probed like any
    // other — it is the REPLY that carries nothing today — and with nothing on
    // either side the picker opens straight onto that custom input.
    const cli = b.cli.trim();
    const repaint = (): void => {
      const now = this.analysis.workflow.blocks[index]?.model ?? picker.value;
      picker.setOptions(blockModelOptions(modelCatalog.models(cli)), now, cli);
    };
    const picker = new ModelPicker({
      selectClass: "wf-input",
      inputClass: "wf-input",
      placeholder: "model id…",
      blankLabel: BLOCK_DEFAULT_MODEL_LABEL,
      // #993. The lookup is live rather than a snapshot: the catalog's answer
      // can arrive after this control was built, and a picker holding a copy
      // taken at construction would show the old one forever.
      detailFor: (id) => modelCatalog.detail(cli, id),
    });
    repaint();
    box.append(
      this.field(
        "Model",
        picker.root,
        "The CLI's own list, merged over orrerix's suggestions — or type any id (a Bedrock " +
          "profile, a gateway deployment, a model newer than this build). Unset leaves it to orrerix."
      )
    );

    // The two model knobs (#687). Their VALUES and their availability come from
    // the backend's capability row for this block's CLI (`agent_cli_knobs`) — the
    // pane states no vendor fact of its own — narrowed by what the selected model
    // can carry. A knob this CLI/model cannot take renders disabled with that
    // reason as the hint, which is also the finding the validation pass raises if
    // the file declares one anyway.
    const knobs = new BlockKnobFields(this.knobLookup, cli, b.model, b);
    const effortRow = this.knobRow("Thinking level", knobs.effort, (v) =>
      edit((t) => {
        if (v) t.effort = v;
        else delete t.effort;
      })
    );
    const contextRow = this.knobRow("Context window", knobs.context, (v) =>
      edit((t) => {
        if (v) t.context = v;
        else delete t.context;
      })
    );
    box.append(effortRow.field, contextRow.field);

    /** Redraw the knob rows from whatever the model and the capability record now
     *  say — the repaint that a form which must not re-render still owes them. */
    const repaintKnobs = (): void => {
      effortRow.paint(knobs.effort);
      contextRow.paint(knobs.context);
    };
    this.repaintBlockKnobs = repaintKnobs;
    // The menu's half of the same contract (#1020). Deferred past the mid-type
    // window rather than dropped — rebuilding under a half-typed id resolves it
    // to the dropdown branch and hides the input beneath the caret (#997 review
    // NB-3) — and installed as a LIVE hook so `renderInspector` can take it
    // away when these controls are detached.
    this.refreshBlockModels = () => picker.runWhenNotEditing(repaint);

    // Fires for a dropdown pick AND for every keystroke in the `custom…` box.
    // The keystroke is the case that was broken: `context` is only offered where
    // the selected model has a documented `[1m]` form, so typing a model over one
    // that has none (or vice versa) has to re-derive the knob — and a `change`
    // listener on the select alone never sees a typed id at all.
    //
    // `rerenderForm: false`, like every other free-text control here: rebuilding
    // the form on a keystroke would rebuild the input the human is typing into
    // and drop the caret at its end. That suppression is exactly why the repaint
    // has to be explicit.
    picker.onChange = () => {
      const model = picker.value;
      edit((t) => (t.model = model), false);
      knobs.setModel(model);
      repaintKnobs();
    };

    // What the CLI on THIS machine reports, once it answers. Only re-set when it
    // reported something — re-setting an identical list would rebuild the menu
    // for no gain — and only while this form is still the one on screen. The
    // fallback is re-read from the MODEL rather than closed over from `b`: by the
    // time this lands the human may have chosen the blank row, and a stale
    // `b.model` would re-select the id they just cleared.
    if (cli) {
      void this.probeModels(cli).then((p) => {
        if (!p.models.length || !this.formPane.contains(picker.root)) return;
        // Never under the caret. `setOptions` re-runs `pickerSelection`, and an
        // id the probe turns out to carry resolves to the DROPDOWN branch —
        // which hides the custom input being typed into, sending the rest of the
        // keystrokes nowhere. The pane takes the same care with the capability
        // reply (`ensureCliKnobs`), and for the same reason. The menu is not
        // lost: the next form render paints it from the resolved catalog.
        if (picker.root.contains(document.activeElement)) return;
        const now = this.analysis.workflow.blocks[index]?.model ?? "";
        picker.setOptions(blockModelOptions(modelCatalog.models(cli)), now, cli);
      });
      // And the detection LOOKUP (#1020) — fired from this render path, which
      // #993 forbade and this slice makes correct: it cannot spawn an agent CLI,
      // because the backend swept them once at startup and this reads what it
      // left (`src-tauri/src/modelwire.rs`).
      //
      // **Guarded on `report(cli)` being absent, and that guard is what makes it
      // terminate.** `applyDetection` ends in `renderInspector()`, which rebuilds
      // this form and re-runs this line: without the guard, every reply would
      // re-enter the render it was answering. A reply worth having sets
      // `report(cli)`, so the rebuilt form skips this; one that carries nothing
      // returns below before refreshing anything. Both routes out are dead ends,
      // which is the property to check if this ever grows a third.
      if (!modelCatalog.report(cli)) {
        void modelCatalog.detect(cli).then((r) => {
          if (!r.models.length) return;
          this.applyDetection(cli);
        });
      }
    }

    // Persona: inline prompt, a profile file, or neither (the built-in role template).
    // Exactly one, enforced here rather than only reported: the two compile to different
    // native flags (`claude --agents '<json>'` inline vs `copilot --agent <name>`), so a
    // block with both has no single answer.
    const personaKind: "none" | "prompt" | "profile" =
      b.prompt !== undefined ? "prompt" : b.profile !== undefined ? "profile" : "none";
    box.append(
      this.field(
        "Persona",
        this.select(["none", "prompt", "profile"], personaKind, (v) =>
          edit((t) => {
            delete t.prompt;
            delete t.profile;
            if (v === "prompt") t.prompt = b.prompt ?? "";
            if (v === "profile") t.profile = b.profile ?? "";
          })
        ),
        "none = orrerix's built-in role instructions. prompt = inline (compiled to the CLI's native inline agent). " +
          "profile = a .github/agents/*.md file (Copilot's native --agent)."
      )
    );

    if (personaKind === "prompt") {
      const ta = document.createElement("textarea");
      ta.className = "wf-input wf-textarea";
      ta.value = b.prompt ?? "";
      ta.spellcheck = false;
      ta.rows = 8;
      ta.addEventListener("input", () => edit((t) => (t.prompt = ta.value), false));
      box.append(
        this.field(
          "Prompt",
          ta,
          "Appended to the role's mechanics — it cannot drop the report/git/MCP contract."
        )
      );
    }
    if (personaKind === "profile") {
      box.append(
        this.field(
          "Profile path",
          this.textInput(
            b.profile ?? "",
            (v) => edit((t) => (t.profile = v), false),
            ".github/agents/reviewer.md"
          ),
          "Repo-relative. A Copilot block launches with --agent <name> resolved from this file."
        )
      );
    }

    // `allow:` — extra pre-approved tool patterns (#222), a tag list rather than one
    // comma-separated field for a reason that would otherwise corrupt the value: a real
    // pattern CONTAINS commas (`Bash(gh pr view --json title,body)`), so a comma cannot also
    // be the separator. One row per pattern, and the row is the whole editor for it.
    //
    // It is RESTRICT-ONLY, and the form says so out loud: deny beats allow on both CLIs, so a
    // pattern here can never re-grant something loomux's containment took away — it only
    // pre-approves something the block could already have been asked to approve. That is why
    // the two kinds that may not declare it at all (the orchestrator, and the read-only class)
    // are refused rather than merely warned.
    // THE ROWS ARE LOCAL; the FILE is what is left when the empty ones are dropped.
    //
    // That one rule replaces the draft-row special case the first cut had, and closes the
    // hole it left (#1020 review, finding 5): a *committed* row cleared with select-all-
    // delete wrote `allow: [""]` and then raised the "dropped, and pre-approves nothing"
    // warning about it — the pane complaining about its own keystroke, which is exactly
    // what the draft row existed to avoid, reached from the other direction. An empty row
    // is now a row you are in the middle of typing, wherever it came from, and it reaches
    // the file only once it has something in it.
    const denial = allowDenialReason(b.kind);
    const rows: string[] = [...(b.allow ?? [])];
    const allowList = el("div", "wf-checks");

    /** Write the non-empty rows, in order. The key goes entirely when nothing is left: an
     *  `allow: []` is a line that declares nothing, and the model emits only what is
     *  declared. `rerenderForm: false` — this runs on every keystroke. */
    const commitRows = (): void =>
      edit((t) => {
        const kept = rows.filter((p) => p.trim() !== "");
        if (kept.length) t.allow = kept;
        else delete t.allow;
      }, false);

    /** Rebuild the row DOM from `rows`. Only ever called from add/remove — deliberate
     *  clicks, with no caret to protect — so the indices every row closure captures are
     *  rebuilt at exactly the moments they would otherwise go stale. A keystroke mutates
     *  `rows[i]` in place and repaints nothing. */
    const paintRows = (): void => {
      const built = rows.map((value, i) => {
        const line = el("div", "wf-check");
        const input = this.textInput(
          value,
          (v) => {
            rows[i] = v;
            commitRows();
          },
          "Bash(npm test *)"
        );
        const del = document.createElement("button");
        del.className = "wf-btn wf-btn-danger";
        del.textContent = "✕";
        del.title = "Remove this pattern";
        del.addEventListener("click", () => {
          rows.splice(i, 1);
          commitRows();
          paintRows();
        });
        line.append(input, del);
        return line;
      });
      const addPattern = el("button", "wf-add", "+ Add pattern") as HTMLButtonElement;
      addPattern.disabled = !!denial;
      addPattern.addEventListener("click", () => {
        rows.push("");
        paintRows();
        // Focus the row just added — the point of pressing the button is to type in it.
        const inputs = allowList.querySelectorAll<HTMLInputElement>("input.wf-input");
        inputs[inputs.length - 1]?.focus();
      });
      const children: HTMLElement[] = [...built, addPattern];
      if (!rows.length && !denial) {
        children.push(
          el("span", "wf-hint", "None — the block runs with its class's own tool surface.")
        );
      }
      allowList.replaceChildren(...children);
    };
    paintRows();
    box.append(
      this.field(
        "Extra allowed tools",
        allowList,
        denial
          ? `A ${b.kind} block may not declare allow: — ${denial}.`
          : "Pre-approved tool patterns, passed to the CLI's own --allowedTools/--allow-tool. " +
              "RESTRICT-ONLY: deny beats allow on both CLIs, so this can never re-grant what the " +
              "block's kind takes away. orrerix passes only letters, digits and ( ) : * _ - . / , and spaces."
      )
    );

    // Outgoing edges, edited as "what runs after this" — the honest phrasing for an
    // advisory edge, and the only edge editing the form needs: every edge has a source.
    const targets = el("div", "wf-checks");
    if (!b.id) {
      // An edge is a pair of IDS. A block without one cannot be an endpoint, and offering
      // checkboxes that would write `from: ""` would manufacture the dangling references
      // this pane exists to catch.
      targets.append(el("span", "wf-hint", "Give this block an id before wiring edges to it."));
    } else {
      for (const other of w.blocks) {
        if (other.id === b.id || !other.id) continue;
        const line = el("label", "wf-check");
        const cb = document.createElement("input");
        cb.type = "checkbox";
        cb.checked = w.edges.some((e) => e.from === b.id && e.to === other.id);
        cb.addEventListener("change", () =>
          this.mutate((next) => {
            next.edges = cb.checked
              ? [...next.edges, { from: b.id, to: other.id }]
              : next.edges.filter((e) => !(e.from === b.id && e.to === other.id));
          })
        );
        line.append(cb, el("span", "wf-check-label", `${other.name || other.id} (${other.id})`));
        targets.append(line);
      }
      if (!targets.children.length) {
        targets.append(el("span", "wf-hint", "Add another block to draw an edge."));
      }
    }
    box.append(
      this.field("Then run", targets, "Advisory: the declared happy path. The orchestrator still schedules.")
    );

    const inline = this.blockFindings(b);
    if (inline.length) {
      const list = el("ul", "wf-inline-findings");
      for (const f of inline) list.append(el("li", `wf-finding wf-${f.severity}`, f.message));
      box.append(list);
    }

    const del = document.createElement("button");
    del.className = "wf-btn wf-btn-danger";
    del.textContent = "Delete block";
    del.addEventListener("click", () => void this.deleteBlock(b, index));
    box.append(del);
    return box;
  }

  /** The panel for a selected EDGE. Short, because an edge is a short thing: it has no
   *  properties — it is a pair of ids — so all there is to say is what it means and how to
   *  remove it. Saying *what it means* is the part that earns the panel: this is the one place
   *  a human clicks on an advisory edge, and it is where they should learn that it is advisory. */
  private edgeForm(from: string, to: string): HTMLElement {
    const box = el("div", "wf-fields");
    box.append(
      el(
        "p",
        "wf-note",
        "An ADVISORY edge: it declares the intended path. The orchestrator still decides when to " +
          "spawn what — its judgment about what can run in parallel is the thing that makes it good, " +
          "and a static DAG would replace that with something dumber. The half that is actually " +
          "enforced is the merge gate."
      )
    );
    const del = document.createElement("button");
    del.className = "wf-btn wf-btn-danger";
    del.textContent = "Delete edge";
    del.addEventListener("click", () => this.eraseEdge(from, to));
    box.append(del);
    return box;
  }

  private gateForm(w: Workflow): HTMLElement {
    const box = el("div", "wf-fields");
    box.append(
      el(
        "p",
        "wf-note",
        "ENFORCED, not advised: orrerix refuses `gh pr merge` (via the PATH shim an agent cannot get around) " +
          "until every reviewer this gate names has recorded a verdict of PASS. This is what makes a second " +
          "reviewer more than a suggestion."
      )
    );

    const gate = w.gates.merge;
    const on = document.createElement("input");
    on.type = "checkbox";
    on.checked = !!gate;
    on.addEventListener("change", () =>
      this.mutate((next) => {
        next.gates = {
          ...next.gates,
          merge: on.checked
            ? {
                require: "all-pass",
                // Reviewer-kind minus the liaison (#891 S4): filling this with a
                // bare `kind` filter made ticking the gate on author a file the
                // pane's own validator flags `gate-not-a-reviewer` in the same
                // breath — the human never named the liaison, the checkbox did.
                reviewers: next.blocks.filter(isReviewingBlock).map((b) => b.id),
                also: [],
              }
            : undefined,
        };
      })
    );
    const onLine = el("label", "wf-check");
    onLine.append(on, el("span", "wf-check-label", "Gate merges on review verdicts"));
    box.append(onLine);

    if (!gate) return box;

    box.append(
      this.field(
        "Require",
        this.select(GATE_REQUIRES, gate.require, (v) =>
          this.mutate((next) => {
            const g = next.gates.merge!;
            g.require = v;
            if (v === "threshold" && g.threshold === undefined) g.threshold = g.reviewers.length || 1;
            if (v === "all-pass") delete g.threshold;
          })
        ),
        "all-pass = every named reviewer. threshold = at least N of them."
      )
    );

    if (gate.require === "threshold") {
      // Through the same bounded control, and the same `POLICY_BOUNDS` row, as every other
      // number in this pane (#1020 review, finding 7). It used to hand-roll its own input
      // whose floor was the string "1" and whose empty state wrote `Number("") || 1` — the
      // pane inventing a threshold nobody typed, which is the same defect as the invented
      // `max_batch` ceiling one finding earlier. Empty now means UNDECLARED, and a
      // threshold gate with no threshold is exactly what `gate-bad-threshold` is for: the
      // human is told what the gate needs instead of being given a number they didn't ask
      // for.
      box.append(
        this.field(
          "Threshold",
          this.boundedNumber(
            gate.threshold,
            POLICY_BOUNDS["gate.threshold"]!,
            (v) =>
              this.mutate((next) => {
                const g = next.gates.merge!;
                if (v === undefined) delete g.threshold;
                else g.threshold = v;
              }, false),
            "how many must pass"
          ),
          "How many of the named reviewers must record a PASS. There is no default — a threshold gate says the number."
        )
      );
    }

    const reviewers = el("div", "wf-checks");
    // Same predicate as the fill-in above, so the offer list and what it fills
    // in agree. A liaison already NAMED by a hand-edited file is not hidden by
    // this — it falls through to the `wf-bad` row below, labelled and
    // untickable, which is where the file's own finding can be acted on.
    const reviewerBlocks = w.blocks.filter((b) => isReviewingBlock(b) && b.id);
    for (const b of reviewerBlocks) {
      const line = el("label", "wf-check");
      const cb = document.createElement("input");
      cb.type = "checkbox";
      cb.checked = gate.reviewers.includes(b.id);
      cb.addEventListener("change", () =>
        this.mutate((next) => {
          const g = next.gates.merge!;
          g.reviewers = cb.checked
            ? [...g.reviewers, b.id]
            : g.reviewers.filter((r) => r !== b.id);
        })
      );
      line.append(cb, el("span", "wf-check-label", `${b.name || b.id} (${b.id})`));
      reviewers.append(line);
    }
    // A gate reviewer that isn't a reviewer block (or doesn't exist) can't be a checkbox —
    // but it IS in the file, and hiding it would make the finding about it unfixable here.
    for (const id of gate.reviewers.filter((r) => !reviewerBlocks.some((b) => b.id === r))) {
      const line = el("label", "wf-check");
      const cb = document.createElement("input");
      cb.type = "checkbox";
      cb.checked = true;
      cb.addEventListener("change", () =>
        this.mutate((next) => {
          const g = next.gates.merge!;
          g.reviewers = g.reviewers.filter((r) => r !== id);
        })
      );
      // A liaison lands here too, and "not a reviewer block" would be wrong
      // about it in a way the author can see is wrong (their file says
      // `kind: reviewer`) — so say the thing that is actually true of it.
      const why =
        !!id && w.blocks.some((b) => b.id === id && b.kind === "reviewer" && !isReviewingBlock(b))
          ? "a liaison, which records no verdict"
          : "not a reviewer block";
      line.append(cb, el("span", "wf-check-label wf-bad", `${id} — ${why}`));
      reviewers.append(line);
    }
    if (!reviewers.children.length) {
      reviewers.append(el("span", "wf-hint", "No reviewer blocks yet — add one, and it can gate the merge."));
    }
    box.append(this.field("Reviewers", reviewers));

    box.append(
      this.field(
        "Also require",
        this.textInput(
          gate.also.join(", "),
          (v) =>
            this.mutate((next) => {
              next.gates.merge!.also = v
                .split(",")
                .map((s) => s.trim())
                .filter(Boolean);
            }, false),
          "ci-green"
        ),
        "Comma-separated extra conditions, enforced by the backend (#197). Known: ci-green, body-unchanged, base-green — one this build cannot check refuses the merge rather than being ignored."
      )
    );

    // #1174's small-batch clause. Declared-only, like `threshold` above: empty means
    // UNDECLARED (no limit), never `0` — which the engine refuses outright, so a form
    // that wrote one would produce a file that will not load.
    box.append(
      this.field(
        "Max diff lines",
        this.boundedNumber(
          gate.max_diff_lines,
          POLICY_BOUNDS["gate.max_diff_lines"]!,
          (v) =>
            this.mutate((next) => {
              const g = next.gates.merge!;
              if (v === undefined) delete g.max_diff_lines;
              else g.max_diff_lines = v;
            }, false),
          "no limit"
        ),
        "Refuse a merge whose PR changes more than this many lines (additions + deletions). Leave empty for no limit."
      )
    );
    return box;
  }

  // ---------- the policy sections (#1020) ----------
  //
  // Three optional sections the file could always carry and the pane could never edit:
  // `intake:` (#382 — where autonomous work comes from), `merge_queue:` (#581) and
  // `resources:` (#858). All three are shaped like `gateForm` — an enable-toggle whose
  // state IS the section's presence in the file, then the fields — and all three lean on
  // the model's declared-only emission: a field left blank writes NO line, so opening a
  // form to read it can never turn "inherit loomux's default" into a pin.

  private intakeForm(w: Workflow): HTMLElement {
    const box = el("div", "wf-fields");
    box.append(
      el(
        "p",
        "wf-note",
        "Where autonomous work comes from: which source the orchestrator polls, and the label " +
          "vocabulary it matches on. Every field is optional — an undeclared one inherits orrerix's " +
          "built-in profile, so a repo can override one label and keep the other four."
      )
    );

    const intake = w.intake;
    box.append(
      this.sectionToggle("This repo declares its own intake", !!intake, (on) =>
        this.mutate((next) => {
          if (on) next.intake = {};
          else delete next.intake;
        })
      )
    );
    if (!intake) return box;

    box.append(
      this.field(
        "Source",
        this.labelledSelect(
          [
            { value: "", label: "inherit orrerix's default" },
            ...INTAKE_SOURCES.map((s) => ({ value: s, label: s })),
          ],
          intake.source ?? "",
          (v) =>
            this.mutate((next) => {
              const i = next.intake!;
              if (v) i.source = v;
              else delete i.source;
            })
        ),
        "github-labels polls the repo's issues; board reads the task board; none disables autonomous intake."
      )
    );

    const LABEL_HINTS: Record<IntakeLabelKey, string> = {
      ready: "Groomed — an agent may start this.",
      investigate: "Research only: post findings, write no code.",
      owned: "An orchestrator has taken this issue.",
      prototype: "Build for a demo, not for merge.",
      hold: "The veto (#778): held by the human — do not start this, even under full autonomy.",
    };
    for (const key of INTAKE_LABEL_KEYS) {
      const value = intake.labels?.[key];
      box.append(
        this.field(
          `Label · ${key}`,
          this.textInput(
            value ?? "",
            (v) =>
              this.mutate((next) => {
                const i = next.intake!;
                const labels = i.labels ?? {};
                if (v.trim()) labels[key] = v.trim();
                else delete labels[key];
                // An empty `labels:` mapping is a section nobody declared anything in — drop
                // it rather than writing `labels: {}`, which would be a statement of its own.
                if (Object.keys(labels).length) i.labels = labels;
                else delete i.labels;
              }, false),
            "inherit"
          ),
          LABEL_HINTS[key]
        )
      );
    }
    box.append(
      el(
        "p",
        "wf-note",
        `A label is letters, digits, - and _ (no leading -, at most ${ID_MAX_CHARS} characters). ` +
          "orrerix rejects anything else rather than rewriting it, so the label it looks for stays " +
          "the one your repo actually has."
      )
    );
    const findings = this.sectionFindingList("intake");
    if (findings) box.append(findings);
    return box;
  }

  private mergeQueueForm(w: Workflow): HTMLElement {
    const box = el("div", "wf-fields");
    box.append(
      el(
        "p",
        "wf-note",
        "The bisecting merge queue: approved sub-PRs land as one batch, and a batch whose checks " +
          "fail is bisected rather than dropped. An absent merge_queue: block means the feature is " +
          "OFF — which is why unticking below removes the section instead of writing enabled: false."
      )
    );

    const mq = w.merge_queue;
    box.append(
      this.sectionToggle("This repo declares a merge queue", !!mq, (on) =>
        this.mutate((next) => {
          if (on) next.merge_queue = { enabled: true };
          else delete next.merge_queue;
        })
      )
    );
    if (!mq) return box;

    // A THREE-WAY control, because the file has three states and a checkbox has two
    // (#1020 review, finding 4). The old checkbox claimed, in its own comment, never to
    // invent `enabled: false` — and then did, across two clicks: ticking wrote `true`, and
    // unticking found the key defined and wrote `false` onto a file that had never carried
    // it. Every repair that keeps a checkbox loses a state instead: untick-always-deletes
    // silently drops an explicit `enabled: false` a human wrote.
    //
    // So the control shows what the file says. Absent and `false` mean the same thing to
    // the engine (`#[serde(default)]`), which is exactly why the pane must not silently
    // convert between them — it is the human's line, not ours, and this is the one form in
    // the pane whose entire subject is what the file declares.
    box.append(
      this.field(
        "Enabled",
        this.labelledSelect(
          [
            { value: "", label: "not declared — off (orrerix's default)" },
            { value: "true", label: "true — run the queue" },
            { value: "false", label: "false — declared off" },
          ],
          mq.enabled === undefined ? "" : String(mq.enabled),
          (v) =>
            this.mutate((next) => {
              const q = next.merge_queue!;
              if (v === "") delete q.enabled;
              else q.enabled = v === "true";
            })
        ),
        "An absent enabled: is off — the same thing the engine reads from enabled: false, kept apart here because the line is yours."
      )
    );

    box.append(
      this.field(
        "Max batch",
        this.boundedNumber(mq.max_batch, POLICY_BOUNDS["merge_queue.max_batch"]!, (v) =>
          this.mutate((next) => {
            const q = next.merge_queue!;
            if (v === undefined) delete q.max_batch;
            else q.max_batch = v;
          }, false)
        ),
        "How many approved sub-PRs one batch may carry. Empty inherits orrerix's default; a batch of none could never land anything."
      )
    );

    box.append(
      this.field(
        "Checks timeout (minutes)",
        this.boundedNumber(
          mq.checks_timeout_minutes,
          POLICY_BOUNDS["merge_queue.checks_timeout_minutes"]!,
          (v) =>
            this.mutate((next) => {
              const q = next.merge_queue!;
              if (v === undefined) delete q.checks_timeout_minutes;
              else q.checks_timeout_minutes = v;
            }, false)
        ),
        `How long to wait for a batch's checks before calling it unverifiable. orrerix clamps this to ${MERGE_QUEUE_CHECKS_TIMEOUT_MIN}–${MERGE_QUEUE_CHECKS_TIMEOUT_MAX}.`
      )
    );
    const findings = this.sectionFindingList("merge_queue");
    if (findings) box.append(findings);
    return box;
  }

  private resourcesForm(w: Workflow): HTMLElement {
    const box = el("div", "wf-fields");
    box.append(
      el(
        "p",
        "wf-note",
        "Named locks agents take turns on — a build directory, a test database, anything two agents " +
          "must not hold at once. orrerix never learns what a name MEANS: it counts slots and bounds " +
          "how long a hold may last, and the agents' own briefs say what to acquire."
      )
    );

    const resources = w.resources;
    box.append(
      this.sectionToggle("This repo declares shared resources", !!resources, (on) =>
        this.mutate((next) => {
          if (on) next.resources = {};
          else delete next.resources;
        })
      )
    );
    if (!resources) return box;

    // Sorted, matching the emitter (and the engine's BTreeMap): a resource map has no
    // authored order to preserve, unlike the roster, where the order is meaning.
    const names = Object.keys(resources).sort();
    for (const name of names) {
      const r = resources[name]!;
      // A plain div, not `this.field(...)`: the card holds several inputs and a button, and
      // wrapping that in the `<label>` `field` produces would nest labels around controls
      // that already have their own.
      const card = el("div", "wf-fields");
      const head = el("div", "wf-check");
      head.append(el("span", "wf-label", name));
      const del = document.createElement("button");
      del.className = "wf-btn wf-btn-danger";
      del.textContent = "Remove";
      del.addEventListener("click", () =>
        this.mutate((next) => {
          if (next.resources) delete next.resources[name];
        })
      );
      head.append(del);
      card.append(head);
      const num = (
        label: string,
        key: keyof Pick<WorkflowResource, "slots" | "max_hold_minutes">,
        hint: string
      ): void => {
        card.append(
          this.field(
            label,
            this.boundedNumber(r[key], POLICY_BOUNDS[`resource.${key}`]!, (v) =>
              this.mutate((next) => {
                const target = next.resources?.[name];
                if (!target) return;
                if (v === undefined) delete target[key];
                else target[key] = v;
              }, false)
            ),
            hint
          )
        );
      };
      num(
        "Slots",
        "slots",
        `How many agents may hold it at once (${RESOURCE_SLOTS_MIN}–${RESOURCE_SLOTS_MAX}). Empty inherits orrerix's default.`
      );
      num(
        "Max hold (minutes)",
        "max_hold_minutes",
        `How long one hold may last before it expires (${RESOURCE_MAX_HOLD_MINUTES_MIN}–${RESOURCE_MAX_HOLD_MINUTES_MAX}). Empty inherits orrerix's default.`
      );
      box.append(card);
    }

    const add = el("button", "wf-add", "+ Add resource") as HTMLButtonElement;
    add.disabled = names.length >= RESOURCES_MAX;
    add.addEventListener("click", () => void this.addResource(names));
    box.append(add);
    if (names.length >= RESOURCES_MAX) {
      box.append(
        el(
          "span",
          "wf-hint",
          `${RESOURCES_MAX} is the maximum — every name is listed in the acquire_lock tool description every agent in the group reads.`
        )
      );
    }
    const findings = this.sectionFindingList("resources");
    if (findings) box.append(findings);
    return box;
  }

  /** Add a resource — ASKING for the name, the same commitment `createBlock` makes about a
   *  block id and for the same reason: the name is what an agent's own `acquire_lock` call
   *  spells, loomux rejects rather than rewrites anything outside its alphabet, and a name
   *  validated as it is typed never becomes a finding to decode afterwards. */
  private async addResource(existing: readonly string[]): Promise<void> {
    const name = await promptModal({
      title: "New resource",
      body:
        "The name is what an agent asks for by (acquire_lock \"build\"). Letters, digits, - and _; " +
        `at most ${ID_MAX_CHARS} characters.`,
      label: "Resource name",
      placeholder: "build",
      affirm: "Add",
      validate: (v) => {
        if (!v.trim()) return "A resource needs a name.";
        if (!isValidResourceName(v)) {
          return `Use letters, digits, - and _ (at most ${ID_MAX_CHARS} characters).`;
        }
        if (existing.includes(v.trim())) return `This workflow already declares "${v.trim()}".`;
        return null;
      },
    });
    if (!name) return;
    this.mutate((next) => {
      const resources = next.resources ?? {};
      // `{}` — declared with loomux's defaults, which is what a human means by adding a name
      // and setting nothing. It emits as `build: {}`, the spelling the engine's serde accepts.
      resources[name.trim()] = {};
      next.resources = resources;
    });
  }

  /** Apply an edit to the model and write it straight back into the YAML.
   *
   *  `rerenderForm` is false for the free-text controls: re-rendering the inspector on every
   *  keystroke would rebuild the very input the human is typing into and drop the caret at
   *  its end. Structural edits (a kind change, an edge toggle, a persona switch) DO
   *  re-render, because they change which controls exist. */
  private mutate(edit: (w: Workflow) => void, rerenderForm = true): void {
    const next: Workflow = structuredClone(this.analysis.workflow);
    edit(next);
    this.commit(next);
    if (rerenderForm) {
      this.renderSelection();
    } else {
      // The one path that may skip the inspector, and it is safe to: `rerenderForm` is false
      // only for the free-text controls (a name, a model, a prompt body), and typing in one can
      // never remove the block or edge that is selected. There is no stale selection for
      // `renderSelection`'s ordering rule to protect against here — only a caret to protect.
      this.renderRoster();
      this.renderGraph();
    }
    this.renderFindings();
    this.updateDirty();
  }

  /** Create a block — from the roster's "+ Add block" or the canvas's "+ Block", the same one
   *  path.
   *
   *  IT ASKS FOR THE ID, and that is a design commitment rather than a dialog I forgot to
   *  remove (§4): an id is immutable and human-meaningful, edges and gates reference it, and it
   *  is the thing you read in a diff. Dify mints `node_1720794829558`; n8n keys the graph by
   *  the DISPLAY NAME so a rename silently breaks every reference. Asking costs one dialog,
   *  once, and it is validated as they type — a malformed or duplicate id can't be confirmed at
   *  all, so it never becomes a finding they have to go and decode afterwards.
   *
   *  Everything ELSE about the block (kind, cli, model, prompt/profile) is configured in the
   *  property form, which the new block is immediately selected in. That split is deliberate:
   *  the id is the one field that can never be changed later, so it is the one field worth
   *  interrupting for. */
  private async createBlock(at?: Point): Promise<void> {
    const w = this.analysis.workflow;
    const id = await promptModal({
      title: "New block",
      body: "The id is the block's identity — edges and the merge gate reference it, and it can never be changed. Make it something you'd want to read in a diff (rev-security, worker, planner).",
      label: "Block id",
      placeholder: "rev-security",
      affirm: "Create",
      validate: (v) => {
        if (!v) return "A block needs an id.";
        if (!isValidBlockId(v)) return "Use lowercase letters, digits, - and _ (e.g. rev-security).";
        if (w.blocks.some((b) => b.id === v)) return `This workflow already has a block called "${v}".`;
        return null;
      },
    });
    if (!id) return;

    const index = w.blocks.length;
    this.mutate((next) => {
      next.blocks = [...next.blocks, newBlock(id, id)];
    });
    // Put it where the human asked for it (a canvas right-click carries the point), or in the
    // first free slot. Either way it is placed BEFORE it is drawn, so it never flashes at the
    // origin on top of something else.
    this.layout = withPosition(this.layout, id, at ?? freeSlot(this.positions()));
    void this.saveLayout();
    this.selectItem({ kind: "block", index });
  }

  private async deleteBlock(b: WorkflowBlock, index: number): Promise<void> {
    const refs = b.id
      ? this.analysis.workflow.edges.filter((e) => e.from === b.id || e.to === b.id).length
      : 0;
    const gated = (b.id && this.analysis.workflow.gates.merge?.reviewers.includes(b.id)) || false;
    const extra =
      refs || gated
        ? ` Its ${[refs ? `${refs} edge(s)` : "", gated ? "seat on the merge gate" : ""]
            .filter(Boolean)
            .join(" and ")} go with it.`
        : "";
    const ok = await modal<boolean>((resolve) => ({
      title: `Delete block "${b.name || b.id}"?`,
      body: `The block is removed from the workflow.${extra}`,
      buttons: [
        { label: "Cancel", value: false },
        { label: "Delete", value: true, kind: "danger" },
      ],
      onKey: (k) => (k === "Escape" ? resolve(false) : undefined),
    }));
    if (!ok) return;
    // removeBlockAt takes the references with it — a delete that left them behind would
    // turn one click into three validation errors.
    this.commit(removeBlockAt(this.analysis.workflow, index));
    this.selection = { kind: "workflow" };
    this.render();
  }

  // ---------- findings ----------

  private renderFindings(): void {
    const findings = this.analysis.findings;
    const errors = findings.filter((f) => f.severity === "error").length;
    const warnings = findings.length - errors;
    this.statusEl.textContent = findings.length
      ? `${errors} error${errors === 1 ? "" : "s"}, ${warnings} warning${warnings === 1 ? "" : "s"}`
      : "valid";
    this.statusEl.className = `wf-status ${hasErrors(findings) ? "wf-error" : warnings ? "wf-warning" : "wf-ok"}`;

    if (!findings.length) {
      this.findingsEl.replaceChildren(
        el("div", "wf-finding wf-ok", "No problems found — every block, edge and gate reference resolves.")
      );
      return;
    }
    const rows = findings.map((f) => {
      const r = el("button", `wf-finding wf-${f.severity}`);
      const where = f.line ? `line ${f.line}` : f.blockId || f.section || "";
      if (where) r.append(el("span", "wf-finding-where", where));
      r.append(el("span", "wf-finding-msg", f.message));
      // Click a finding, land on the thing it is about — the whole value of a pre-run
      // validation pass is that it tells you WHERE.
      r.addEventListener("click", () => {
        // WHICH SURFACE a finding needs is a rule (`surfaceForFinding`): a line wants the caret,
        // which lives in the YAML; a block wants its editor, which — docked — is already on
        // screen, so switching surface would drag the human off the canvas for nothing.
        const surface = surfaceForFinding(f);
        if (surface) this.setSurface(surface);
        if (f.line) {
          this.focusLine(f.line);
          return;
        }
        // A policy-section finding names its own section, which IS a selection — so the
        // click lands on the form that can fix it, exactly like a block finding does.
        if (f.section) {
          this.selectItem({ kind: f.section });
          return;
        }
        // A finding names a block by id; the inspector is keyed by ROW. Land on the first row
        // that answers to that id — which for a duplicate pair is the first of the two,
        // and the duplication is reported on both, so the human sees the pair either way.
        const index = this.analysis.workflow.blocks.findIndex((b) => b.id === f.blockId);
        if (index < 0) return;
        this.selectItem({ kind: "block", index });
      });
      return r;
    });
    this.findingsEl.replaceChildren(...rows);
  }

  /** Put the caret on `line` in the YAML view — the follow-through a clickable line number
   *  promises. */
  private focusLine(line: number): void {
    const lines = this.text.split("\n");
    const at = lines.slice(0, line - 1).reduce((n, l) => n + l.length + 1, 0);
    this.yamlArea.focus();
    this.yamlArea.setSelectionRange(at, at + (lines[line - 1]?.length ?? 0));
  }

  // ---------- the canvas (#222 v2: it EDITS the file now) ----------
  //
  // The graph was read-only in v1, on the reasoning that a canvas which can corrupt the file is
  // worse than no canvas. The human demoed it and asked for an editable one. So it edits — and
  // the original reasoning is ANSWERED rather than abandoned: every gesture goes through the
  // pure model (`connectBlocks`, `addBlock`, `removeBlockAt`) and out through the same
  // canonical formatter as every other edit. The canvas cannot express anything the YAML
  // can't, it cannot write a position into the workflow, and it cannot invent an id. It is a
  // second way to EDIT the file, not a second source of truth.
  //
  // Drag a node (position → the LAYOUT file, never the workflow) · drag from a node's port to
  // another node to draw an advisory edge · click an edge to select it, ✕ to erase it · +Block
  // to add one (it asks for the id) · Delete to remove what's selected.

  /** Every node's position right now: stored where the human has dragged one, computed
   *  everywhere else, and overridden by the drag in flight. */
  private positions(): Map<string, Point> {
    const pos = resolvePositions(this.analysis.graph, this.layout, this.ghosts());
    if (this.dragging) pos.set(this.dragging.key, this.dragging.at);
    return pos;
  }

  private nodeRects(): Map<string, Rect> {
    return new Map([...this.positions()].map(([k, p]) => [k, rectOf(p)] as const));
  }

  /** The names an edge mentions that no block answers to. Drawn, because a graph that quietly
   *  omitted them would disagree with the file it exists to show you. */
  private ghosts(): string[] {
    const g = this.analysis.graph;
    const known = new Set(g.nodes.map((n) => n.block.id).filter(Boolean));
    return [...new Set(g.edges.flatMap((e) => [e.from, e.to]).filter((id) => id && !known.has(id)))];
  }

  /** The block (or ghost) a name resolves to. A duplicate id draws to the FIRST row answering
   *  to it — that is a validation error either way, and drawing to one of them beats drawing to
   *  neither. */
  private keyOf(id: string): string | null {
    const n = this.analysis.graph.nodes.find((x) => x.block.id === id);
    if (n) return blockKey(n.index);
    return this.ghosts().includes(id) ? ghostKey(id) : null;
  }

  /** Pointer → canvas coordinates. The SVG renders at natural size (no zoom, no viewBox
   *  scaling), so this is a translation and nothing more — which is why there is no transform
   *  maths anywhere else in here to get wrong. */
  private canvasPoint(e: PointerEvent, root: SVGElement): Point {
    const r = (root as unknown as HTMLElement).getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  }

  /** The drawn edges as GEOMETRY, in render order — the list the pure hit-test is asked about. */
  private drawnEdges(
    rects: ReadonlyMap<string, Rect>
  ): { edge: { from: string; to: string }; geom: { from: Point; to: Point } }[] {
    const out: { edge: { from: string; to: string }; geom: { from: Point; to: Point } }[] = [];
    for (const e of this.analysis.graph.edges) {
      const a = this.keyOf(e.from);
      const b = this.keyOf(e.to);
      const ra = a ? rects.get(a) : undefined;
      const rb = b ? rects.get(b) : undefined;
      if (!ra || !rb) continue;
      out.push({ edge: { from: e.from, to: e.to }, geom: { from: outPort(ra), to: inPort(rb) } });
    }
    return out;
  }

  private renderGraph(): void {
    const g = this.analysis.graph;

    const bar = el("div", "wf-graph-bar");
    const addBtn = document.createElement("button");
    addBtn.className = "wf-btn";
    addBtn.textContent = "+ Block";
    addBtn.disabled = this.syntaxBroken();
    addBtn.addEventListener("click", () => void this.createBlock());
    bar.append(
      addBtn,
      el(
        "span",
        "wf-graph-hint",
        "Drag a node to move it · drag from its ● to another node to connect · click an edge to select it · double-click the canvas to add a block"
      )
    );
    const legend = el("div", "wf-legend");
    legend.append(
      el("span", "wf-legend-item wf-legend-edge", "— advisory edge (the declared path)"),
      el("span", "wf-legend-item wf-legend-gate", "-- enforced gate (blocks the merge)")
    );
    bar.append(legend);

    if (!g.nodes.length) {
      this.graphPane.replaceChildren(bar, el("div", "wf-hint", "No blocks yet — “+ Block” adds one."));
      return;
    }

    const pos = this.positions();
    const rects = this.nodeRects();
    const ghosts = this.ghosts();

    // The gate hangs off the reviewers it names, to the right of everything else. It is NOT a
    // draggable, wireable node: it is not a block, it is a rule ABOUT blocks, and letting it be
    // dragged around like one would imply it can be rewired like one — which is the single most
    // important thing about it that isn't true.
    const gate = g.gates[0];
    const right = Math.max(...[...pos.values()].map((p) => p.x + NODE_W), PAD);
    const gateX = right + 96;
    const gateH = gate ? Math.max(NODE_H, Math.max(1, gate.reviewers.length) * 22 + 30) : 0;
    const gateY = PAD;

    const bottom = Math.max(...[...pos.values()].map((p) => p.y + NODE_H), gateY + gateH);
    const width = (gate ? gateX + NODE_W : right) + PAD * 4;
    const height = bottom + PAD * 4;

    const root = svg("svg");
    root.setAttribute("class", "wf-graph-svg");
    root.setAttribute("width", String(width));
    root.setAttribute("height", String(height));

    const defs = svg("defs");
    // An SVG <marker>'s fill is a presentation attribute on an element the stylesheet does
    // not reach, so these two take their values from theme.ts directly rather than through a
    // custom property (#879 slice B). They mirror `.wf-edge` / `.wf-edge-gate` in styles.css:
    // a plain edge is a faint rule, a gate edge is the identity amber the gate lane uses.
    defs.append(
      arrowMarker("wf-arrow", SEMANTIC.inkFaint),
      arrowMarker("wf-arrow-gate", IDENTITY.amber)
    );
    root.append(defs);

    // ---- advisory edges: solid, selectable, erasable ----
    for (const e of g.edges) {
      const aKey = this.keyOf(e.from);
      const bKey = this.keyOf(e.to);
      const a = aKey ? rects.get(aKey) : undefined;
      const b = bKey ? rects.get(bKey) : undefined;
      if (!a || !b) continue;
      const from = outPort(a);
      const to = inPort(b);
      const selected =
        this.selection.kind === "edge" && this.selection.from === e.from && this.selection.to === e.to;

      const group = svg("g");
      group.setAttribute("class", `wf-edge-g${selected ? " selected" : ""}`);
      const path = svg("path");
      path.setAttribute("d", edgePath(from, to));
      path.setAttribute("class", e.resolved ? "wf-edge" : "wf-edge wf-edge-broken");
      path.setAttribute("marker-end", "url(#wf-arrow)");
      group.append(path);

      // The ✕ hangs off the CURVE's midpoint — on an edge that doubles back (the reviewer →
      // worker rework loop, a real workflow) the straight-line middle is nowhere near the line
      // you can see, and a ✕ floating in empty space is a ✕ nobody trusts.
      const mid = edgeMidpoint(from, to);
      const del = svg("g");
      del.setAttribute("class", "wf-edge-del");
      const disc = svg("circle");
      disc.setAttribute("cx", String(mid.x));
      disc.setAttribute("cy", String(mid.y));
      disc.setAttribute("r", "9");
      const glyph = text(mid.x, mid.y + 4, "✕", "wf-edge-del-x");
      glyph.setAttribute("text-anchor", "middle");
      del.append(disc, glyph);
      del.addEventListener("pointerdown", (ev) => {
        ev.stopPropagation(); // this is a click on the ✕, not a canvas gesture
        this.eraseEdge(e.from, e.to);
      });
      group.append(del);
      root.append(group);
    }

    // ---- the edge being drawn ----
    if (this.connecting) {
      const fromKey = this.keyOf(this.connecting.from);
      const a = fromKey ? rects.get(fromKey) : undefined;
      if (a) {
        const rubber = svg("path");
        rubber.setAttribute("d", edgePath(outPort(a), this.connecting.at));
        rubber.setAttribute("class", "wf-edge wf-edge-draft");
        rubber.setAttribute("marker-end", "url(#wf-arrow)");
        root.append(rubber);
      }
    }

    // ---- nodes ----
    for (const n of g.nodes) {
      const selected = this.selection.kind === "block" && this.selection.index === n.index;
      root.append(nodeGroup(rects.get(blockKey(n.index))!, n, selected));
    }
    for (const id of ghosts) root.append(ghostGroup(rects.get(ghostKey(id))!, id));

    // ---- the ENFORCED gate ----
    if (gate) {
      for (const rid of gate.reviewers) {
        const rKey = this.keyOf(rid);
        const a = rKey ? rects.get(rKey) : undefined;
        if (!a) continue;
        const line = svg("path");
        line.setAttribute("d", edgePath(outPort(a), { x: gateX, y: gateY + gateH / 2 }));
        line.setAttribute("class", "wf-edge wf-edge-gate");
        line.setAttribute("marker-end", "url(#wf-arrow-gate)");
        root.append(line);
      }
      const box = svg("rect");
      box.setAttribute("x", String(gateX));
      box.setAttribute("y", String(gateY));
      box.setAttribute("width", String(NODE_W));
      box.setAttribute("height", String(gateH));
      box.setAttribute("rx", "8");
      box.setAttribute("class", "wf-gate-box");
      box.addEventListener("pointerdown", (ev) => {
        ev.stopPropagation();
        this.selectItem({ kind: "gate" });
      });
      root.append(box);
      root.append(text(gateX + 12, gateY + 22, "⛔ merge gate", "wf-gate-title"));
      root.append(
        text(
          gateX + 12,
          gateY + 40,
          gate.require === "threshold"
            ? `${gate.threshold ?? "?"} of ${gate.reviewers.length} must PASS`
            : `all ${gate.reviewers.length} must PASS`,
          "wf-gate-sub"
        )
      );
    }

    // Double-click on empty canvas → a block, THERE. (rev-15 minor: `createBlock(at)` took a
    // point no caller ever passed, and its comment promised a gesture that did not exist. It
    // does now — it is the first thing anyone tries on a canvas, and it was one line to honour.)
    root.addEventListener("dblclick", (ev) => {
      const pt = this.canvasPoint(ev as unknown as PointerEvent, root);
      if (hitTestNodes(this.nodeRects(), pt)) return; // double-clicking a node is not "add here"
      void this.createBlock(pt);
    });
    root.addEventListener("pointerdown", (ev) => this.onCanvasDown(ev, root));
    root.addEventListener("pointermove", (ev) => this.onCanvasMove(ev, root));
    root.addEventListener("pointerup", (ev) => this.onCanvasUp(ev, root));
    root.addEventListener("pointercancel", () => {
      this.dragging = null;
      this.connecting = null;
      this.renderGraph();
    });

    const scroll = el("div", "wf-graph-scroll");
    scroll.append(root as unknown as HTMLElement);
    this.graphPane.replaceChildren(bar, scroll);
  }

  /** Where a gesture begins: on a node's PORT (draw an edge), on a node (move it, select it),
   *  on an edge (select it), or on nothing (deselect). */
  private onCanvasDown(e: PointerEvent, root: SVGElement): void {
    if (e.button !== 0 || this.syntaxBroken()) return;
    const pt = this.canvasPoint(e, root);
    const rects = this.nodeRects();
    const key = hitTestNodes(rects, pt);

    if (key?.startsWith("b:")) {
      const index = Number(key.slice(2));
      const block = this.analysis.workflow.blocks[index];
      const rect = rects.get(key)!;
      const port = outPort(rect);

      if (Math.hypot(pt.x - port.x, pt.y - port.y) <= PORT_HIT && block?.id) {
        // An edge is a pair of IDS, so a block with no id cannot be an endpoint. Offering the
        // gesture would only manufacture the dangling reference the validator then complains
        // about — the file would be describing a mistake the canvas talked you into.
        this.connecting = { from: block.id, at: pt };
        capturePointer(root, e);
        this.renderGraph();
        return;
      }

      this.dragging = {
        key,
        id: block?.id ?? "",
        grab: { x: pt.x - rect.x, y: pt.y - rect.y },
        at: { x: rect.x, y: rect.y },
      };
      capturePointer(root, e);
      // THE #880 GESTURE. This handler always did the selecting; what it never did was bring the
      // editor into view, because the editor was behind a tab and only the gate box remembered
      // to switch to it. There is no tab now and no second thing to remember: `selectItem`
      // refreshes the roster, the inspector and the canvas together, so the block's editor
      // appears beside the node under the pointer.
      this.selectItem({ kind: "block", index });
      return;
    }

    // Not a node. An edge, then? THIS is where the pure hit-test earns its keep: an edge is a
    // 1.5px line and nobody can hit that with a mouse — the tolerance is what makes it
    // clickable at all, and it is arithmetic, so it is tested rather than eyeballed.
    const drawn = this.drawnEdges(rects);
    const hit = hitTestEdges(
      drawn.map((d) => d.geom),
      pt
    );
    this.selectItem(
      hit !== null
        ? { kind: "edge", from: drawn[hit]!.edge.from, to: drawn[hit]!.edge.to }
        : { kind: "workflow" }
    );
  }

  private onCanvasMove(e: PointerEvent, root: SVGElement): void {
    if (!this.dragging && !this.connecting) return;
    const pt = this.canvasPoint(e, root);
    if (this.dragging) {
      this.dragging.at = { x: pt.x - this.dragging.grab.x, y: pt.y - this.dragging.grab.y };
    }
    if (this.connecting) this.connecting.at = pt;
    this.renderGraph();
  }

  private onCanvasUp(e: PointerEvent, root: SVGElement): void {
    const pt = this.canvasPoint(e, root);

    if (this.dragging) {
      const { id, at } = this.dragging;
      this.dragging = null;
      if (id) {
        // A drag writes the LAYOUT file and nothing else. The workflow is not re-serialized, the
        // dirty flag does not move, and your teammate's `git pull` does not show a change to the
        // logic because you nudged a box (§4 — the thing Dify, ComfyUI and Langflow all get
        // wrong by embedding x/y in the semantic file).
        const moved = withPosition(this.layout, id, { x: Math.max(0, at.x), y: Math.max(0, at.y) });
        if (!layoutEquals(moved, this.layout)) {
          this.layout = moved;
          void this.saveLayout();
        }
      }
      this.renderGraph();
      return;
    }

    if (this.connecting) {
      const from = this.connecting.from;
      this.connecting = null;
      const key = hitTestNodes(this.nodeRects(), pt);
      if (key?.startsWith("b:")) {
        const to = this.analysis.workflow.blocks[Number(key.slice(2))]?.id ?? "";
        // Refused BEFORE the edge exists, with the reason. A canvas that lets you complete the
        // gesture and only then tells you the edge was invalid has wasted the gesture and left
        // you to undo it.
        const err = connectionError(this.analysis.workflow, from, to);
        if (err) showToast(err, "info");
        else this.mutate((next) => Object.assign(next, connectBlocks(next, from, to)));
      }
      this.renderGraph();
    }
  }

  /** Erase one edge. No confirm: an edge is one gesture to redraw, and a dialog for something
   *  that cheap is a dialog people learn to click through. A BLOCK is different — it carries a
   *  prompt, a model, a seat on the gate — and deleting one still asks. */
  private eraseEdge(from: string, to: string): void {
    // No selection tidy-up here: `mutate` re-renders the inspector, and an edge the workflow no
    // longer declares is exactly the case `inspectorTarget` falls back on — so the selection
    // lands on the workflow's own settings, once, by the rule rather than by a second check
    // that had to stay in step with it.
    this.mutate((next) => Object.assign(next, disconnectBlocks(next, from, to)));
  }

  /** Delete whatever is selected — the keyboard half of the canvas. A canvas you can only
   *  operate with a mouse is a canvas that is tiring to use. */
  private deleteSelection(): void {
    if (this.selection.kind === "edge") {
      this.eraseEdge(this.selection.from, this.selection.to);
      return;
    }
    if (this.selection.kind === "block") {
      const block = this.analysis.workflow.blocks[this.selection.index];
      if (block) void this.deleteBlock(block, this.selection.index);
    }
  }
}

/** How close to a node's out-port a press must land to mean "draw an edge" rather than "move
 *  the node". Generous — the port is a 5px dot, and the two gestures start in the same place. */
const PORT_HIT = 12;

/** Take pointer capture, BEST EFFORT — never letting it abort the gesture it belongs to.
 *
 *  `setPointerCapture` throws (`NotFoundError`) for a pointer id the browser doesn't consider
 *  active, and it is called from the handler that also SELECTS the block. An exception here
 *  would therefore skip the selection and re-create, exactly, the dead click #880 exists to fix
 *  — a click that changes nothing the human can see. That trade is never worth taking, because
 *  the capture is close to decorative anyway: the very next thing every caller does is
 *  `renderGraph()`, which replaces the SVG root the capture was taken on, so the capture is
 *  released a line later regardless and the drag continues on the new root's own listeners. */
function capturePointer(root: SVGElement, e: PointerEvent): void {
  try {
    root.setPointerCapture(e.pointerId);
  } catch {
    // See above: the gesture is worth more than the capture.
  }
}

// ---------- SVG helpers ----------

function arrowMarker(id: string, color: string): SVGElement {
  const m = svg("marker");
  m.setAttribute("id", id);
  m.setAttribute("viewBox", "0 0 10 10");
  m.setAttribute("refX", "9");
  m.setAttribute("refY", "5");
  m.setAttribute("markerWidth", "6");
  m.setAttribute("markerHeight", "6");
  m.setAttribute("orient", "auto-start-reverse");
  const p = svg("path");
  p.setAttribute("d", "M 0 0 L 10 5 L 0 10 z");
  p.setAttribute("fill", color);
  m.append(p);
  return m;
}

function text(x: number, y: number, s: string, cls: string): SVGElement {
  const t = svg("text");
  t.setAttribute("x", String(x));
  t.setAttribute("y", String(y));
  t.setAttribute("class", cls);
  t.textContent = s;
  return t;
}

/** Clip a label to the node box. Cheaper and steadier than measuring: the box is a fixed
 *  width, so a fixed budget is the honest bound. */
const clip = (s: string, max: number): string => (s.length > max ? s.slice(0, max - 1) + "…" : s);

/** One block, as a draggable, connectable node. */
function nodeGroup(r: Rect, n: GraphNode, selected: boolean): SVGElement {
  const bad = !n.known || !isWorkflowCli(n.block.cli);
  const g = svg("g");
  g.setAttribute("class", `wf-node-g${selected ? " selected" : ""}`);

  const box = svg("rect");
  box.setAttribute("x", String(r.x));
  box.setAttribute("y", String(r.y));
  box.setAttribute("width", String(r.w));
  box.setAttribute("height", String(r.h));
  box.setAttribute("rx", "8");
  box.setAttribute("class", `wf-node wf-node-${isBlockKind(n.block.kind) ? n.block.kind : "unknown"}`);
  g.append(box);
  g.append(text(r.x + 12, r.y + 21, clip(n.block.name || n.block.id || "(no id)", 20), "wf-node-title"));
  g.append(
    text(
      r.x + 12,
      r.y + 38,
      clip(`${bad ? "⚠ " : ""}${n.block.kind || "?"} · ${n.block.cli || "?"}`, 22),
      "wf-node-sub"
    )
  );

  // The ports. The OUT port is the handle you drag an edge from, so it is drawn — a gesture
  // nobody can see is a gesture nobody performs. The IN port is drawn too, smaller, because an
  // arrow that arrives somewhere unmarked looks like it is pointing at the box rather than
  // connecting to it. An id-less block gets no out-port at all: it cannot be an edge's endpoint
  // (an edge is a pair of ids), and offering the handle would be offering a broken promise.
  if (n.block.id) {
    const out = svg("circle");
    const p = outPort(r);
    out.setAttribute("cx", String(p.x));
    out.setAttribute("cy", String(p.y));
    out.setAttribute("r", "5");
    out.setAttribute("class", "wf-port wf-port-out");
    g.append(out);
  }
  const inp = svg("circle");
  const ip = inPort(r);
  inp.setAttribute("cx", String(ip.x));
  inp.setAttribute("cy", String(ip.y));
  inp.setAttribute("r", "3");
  inp.setAttribute("class", "wf-port wf-port-in");
  g.append(inp);
  return g;
}

/** A name an edge mentions that no block answers to. Dashed, unmovable, unconnectable — it is
 *  not a block, it is the ABSENCE of one, and it disappears the moment the file stops
 *  mentioning it. */
function ghostGroup(r: Rect, id: string): SVGElement {
  const g = svg("g");
  g.setAttribute("class", "wf-node-g");
  const box = svg("rect");
  box.setAttribute("x", String(r.x));
  box.setAttribute("y", String(r.y));
  box.setAttribute("width", String(r.w));
  box.setAttribute("height", String(r.h));
  box.setAttribute("rx", "8");
  box.setAttribute("class", "wf-node wf-node-ghost");
  g.append(box);
  g.append(text(r.x + 12, r.y + 21, clip(id, 20), "wf-node-title"));
  g.append(text(r.x + 12, r.y + 38, "no such block", "wf-node-sub"));
  return g;
}

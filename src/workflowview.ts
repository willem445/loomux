// The WORKFLOW pane (#222): `.loomux/workflow.yml`, made configurable.
//
// Three surfaces over ONE model, and the model is the FILE (the Kestra pattern — a form
// edit rewrites the YAML under the hood; the YAML is never a stale export of some hidden
// canvas state):
//
//   1. the ROSTER + property form — an interactive block list and a form per block. Every
//      edit writes the YAML immediately.
//   2. the raw YAML — a text editor over the same buffer. Typing here re-reads the model.
//   3. the GRAPH — READ-ONLY (#222 Q6, adopted). It draws the declared happy path
//      (ADVISORY edges: the orchestrator still schedules) and the merge gate (ENFORCED:
//      the `gh` shim refuses a merge until the named reviewers' verdicts are PASS), and
//      the two are drawn differently BECAUSE they mean different things. It cannot edit
//      the file — GitLab's CI "Visualize" tab, not an editable canvas. OpenAI's
//      Agent-Builder-as-source-of-truth shipped in Oct 2025 and is already being shut
//      down; the file wins that argument.
//
// All the thinking lives in the pure `workflowmodel.ts` (parse / serialize / validate /
// derive), which is where the tests are. This file is DOM: rendering, focus, dialogs, and
// the read/write path through the hash-guarded `ft*` file commands.
//
// The one rule the sync has to obey: while the YAML does not PARSE, the form is disabled.
// A form edit serializes the model back over the buffer, and serializing a model we only
// half-understood would silently destroy the broken text the human is in the middle of
// fixing. So a syntax error disables the form and says why; every other kind of breakage
// (an unknown kind, a dangling edge) still renders — as a stub, with a finding — because
// a block you cannot see is a block you cannot repair.

import {
  analyzeWorkflow,
  parseWorkflow,
  serializeWorkflow,
  serializeWorkflowPreserving,
  formatWorkflowText,
  scaffoldWorkflowText,
  removeBlockAt,
  newBlock,
  connectBlocks,
  disconnectBlocks,
  connectionError,
  isValidBlockId,
  isBlockKind,
  isWorkflowCli,
  hasErrors,
  BLOCK_KINDS,
  WORKFLOW_CLIS,
  GATE_REQUIRES,
  WORKFLOW_FILE,
  type Workflow,
  type WorkflowBlock,
  type WorkflowAnalysis,
  type Finding,
  type GraphNode,
} from "./workflowmodel";
import {
  LAYOUT_FILE,
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
import { ftReadFile, ftWriteFile, ftListDir, errorCode, errorMessage } from "./fileapi";
import { fmNewFolder, fmNewFile, fmErrorCode } from "./filemgr";
import {
  paneSurface,
  createAllowed,
  savePlan,
  layoutPruneIds,
  rewriteImpact,
  rewriteImpactMessage,
  type LayoutWrite,
} from "./workflowpane";
import { appVersion } from "./pty";
import { closeDecision, discardEdits, type ConflictChoice } from "./dirtystate";
import { showToast } from "./toast";
import { modal, promptModal } from "./modal";

/** What the hosting pane provides. Only one host today (the workflow PANE — a workflow
 *  builder is a station you keep open beside an agent, never a glance-and-dismiss
 *  overlay), but the shape mirrors `FileEditHost` so the pane wires it the same way. */
export interface WorkflowHost {
  /** The repo/folder the workflow file lives under (the pane's root). */
  getRoot(): string | null;
  /** Root-relative path of the workflow file. Defaults to `.loomux/workflow.yml`. */
  getFile?(): string;
  /** Never called in embedded mode — the pane's own ✕ closes it (and asks first). */
  onClose(): void;
  /** This view IS a pane's content: no ✕, no Esc-to-close. Same fork as FileEditView. */
  embedded?: boolean;
}

type Tab = "form" | "yaml" | "graph";

/** Which entry of the roster the property form is showing. The workflow's own settings
 *  and the gate are rows in the same list as the blocks, because they are edited the same
 *  way and a second place to click would be a second place to look.
 *
 *  A block is addressed by its INDEX in the roster, not by its id — deliberately. The id
 *  is the identity of a *valid* block, but this pane's whole contract is that a file it
 *  cannot fully understand still opens and is still repairable, and the blocks that need
 *  repairing are exactly the ones whose id is missing or duplicated. Keying the form by id
 *  would make two id-less stubs indistinguishable, and a duplicate pair unfixable — the
 *  form would edit whichever came first, forever. The index addresses a ROW, which is what
 *  the human is actually pointing at. */
type Selection =
  | { kind: "workflow" }
  | { kind: "block"; index: number }
  | { kind: "gate" }
  /** An EDGE selected on the canvas (v2). Held by the pair of ids it joins, not by its index in
   *  the edge list: the canonical formatter re-groups edges on every save, so an index would
   *  point at a different edge the moment anything else changed. */
  | { kind: "edge"; from: string; to: string };

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
  /** Node positions (`.loomux/workflow.layout.json`). NOT part of the workflow: a drag changes
   *  this and nothing else, and it is never serialized into the semantic file (§4). */
  private layout: WorkflowLayout = emptyLayout();
  /** The layout as last written, so a drag that ends where it began writes nothing. */
  private savedLayout: WorkflowLayout = emptyLayout();

  private analysis: WorkflowAnalysis;
  private selection: Selection = { kind: "workflow" };
  private tab: Tab = "form";
  private disposed = false;
  /** This build's version, for `authored_with:` on a workflow this pane CREATES. Empty
   *  until the async lookup lands (and if it never does — the key is simply not written,
   *  which beats writing `authored_with: unknown`). */
  private appVersion = "";

  // Header
  private pathLabel: HTMLElement;
  private dirtyDot: HTMLElement;
  private saveBtn: HTMLButtonElement;
  private statusEl: HTMLElement;

  // Body
  private rosterEl: HTMLElement;
  private tabBar: HTMLElement;
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
   *  `ci/flow.yml` that says `.loomux/workflow.yml` is telling the human about a file they are
   *  not looking at — which, on the error surface, means naming the wrong file as unreadable. */
  private starterBtn: HTMLButtonElement;
  private startPathEl: HTMLElement;
  private errorTitleEl: HTMLElement;

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

    const spacer = el("span", "wf-spacer");
    head.append(this.pathLabel, this.dirtyDot, this.statusEl, spacer, formatBtn, reloadBtn, this.saveBtn);
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
        "pass before a merge. Committed, so everyone who clones the repo gets it. Loomux reads " +
        "it only when Advanced orchestrator is ticked."
    );
    const starterBtn = document.createElement("button");
    this.starterBtn = starterBtn;
    starterBtn.className = "wf-btn wf-btn-primary";
    starterBtn.textContent = "Create workflow";
    starterBtn.title = "Scaffold a commented .loomux/workflow.yml — today's pipeline, ready to edit";
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

    // ---- panel (right): tabs over form / yaml / graph ----
    this.tabBar = el("div", "wf-tabs");
    for (const [key, label] of [
      ["form", "Blocks"],
      ["yaml", "YAML"],
      ["graph", "Graph"],
    ] as [Tab, string][]) {
      const b = document.createElement("button");
      b.className = "wf-tab";
      b.textContent = label;
      b.dataset.tab = key;
      b.addEventListener("click", () => this.setTab(key));
      this.tabBar.append(b);
    }

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
      this.renderRoster();
      this.renderFindings();
      this.renderGraph();
      this.updateDirty();
    });
    this.yamlPane.append(this.yamlArea);
    this.graphPane = el("div", "wf-graph");

    const panel = el("div", "wf-panel");
    panel.append(this.tabBar, this.formPane, this.yamlPane, this.graphPane);

    this.bodyEl = el("div", "wf-body");
    this.bodyEl.append(this.rosterEl, panel);

    this.findingsEl = el("div", "wf-findings");

    // All FIVE surfaces. `errorEl` was built and never appended (rev-15 F1), so the state
    // added to fix the UTF-16 bug rendered as a blank pane — the fix's own headline case was
    // the one thing that didn't work. `render()` only toggles `hidden`; a surface that is not
    // in the document has nothing to un-hide.
    this.el.append(head, this.errorEl, this.emptyEl, this.bodyEl, this.findingsEl);

    // Ctrl+S saves from anywhere in the pane — including from inside the textarea, where
    // the browser would otherwise do nothing at all.
    this.el.addEventListener("keydown", (e) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        void this.save();
        return;
      }
      // Delete removes what the CANVAS has selected — and only on the canvas. Anywhere else in
      // the pane, Delete is the key that deletes a character, and a Delete that erased a block
      // while you were editing a prompt would be the most expensive keystroke in the app.
      if ((e.key === "Delete" || e.key === "Backspace") && this.tab === "graph") {
        const inField = (e.target as HTMLElement | null)?.closest?.("input, textarea, select");
        if (inField) return;
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
    this.rel = this.host.getFile?.() || WORKFLOW_FILE;
    this.pathLabel.textContent = this.rel;
    this.pathLabel.title = this.root ? `${this.root} · ${this.rel}` : this.rel;
    void this.load();
  }

  hide(): void {
    this.el.hidden = true;
  }

  dispose(): void {
    this.disposed = true;
    this.el.remove();
  }

  focus(): void {
    (this.tab === "yaml" ? this.yamlArea : this.el).focus();
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
            ? `The file is there, but it isn't valid UTF-8 text — so loomux can't read it, and neither can the backend. A workflow written from PowerShell with \`>\` or \`Out-File\` is UTF-16; re-save it as UTF-8 (\`Set-Content -Encoding utf8NoBOM\`) and it will open.`
            : `${errorMessage(err)}`;
    }
    await this.loadLayout();
    this.reanalyze();
    this.render();
  }

  /** The canvas positions. A layout that is missing or corrupt is simply COMPUTED instead —
   *  never a finding, never a dialog, never a reason not to open the workflow. Nothing in that
   *  file is anyone's work; it is a picture we can redraw. */
  private async loadLayout(): Promise<void> {
    if (!this.root) return;
    try {
      const fr = await ftReadFile(this.root, LAYOUT_FILE);
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
      await this.ensureLoomuxDir();
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

  /** Make sure `.loomux/` exists before writing into it.
   *
   *  THE OTHER HALF OF v2 BUG 1, and it made the pane's headline feature a lie: `ft_write_file`
   *  writes atomically (temp file + rename) and does NOT create parent directories, so in a
   *  repo with no `.loomux/` — i.e. EVERY repo that has never had a workflow, which is exactly
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
  private async ensureLoomuxDir(): Promise<void> {
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
      await this.ensureLoomuxDir();
      await ftWriteFile(this.root, LAYOUT_FILE, serializeLayout(next), null);
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
    this.setText(scaffoldWorkflowText(this.appVersion));
    this.render();
    await this.save();
    // Land them in the canvas, on the thing they just made. The empty state's job was to get
    // out of the way; leaving them on a form with nothing selected would be a second empty
    // state wearing a different hat.
    this.setTab("graph");
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
    this.analysis = analyzeWorkflow(this.text);
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
   *  would destroy the broken text the human is trying to fix. */
  private syntaxBroken(): boolean {
    return this.analysis.findings.some((f) => f.code === "yaml-syntax" || f.code === "not-a-mapping");
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
    // Both surfaces name the file this pane is actually open on, not the default one.
    this.errorTitleEl.textContent = `Can't read ${this.rel}`;
    this.startPathEl.textContent = this.rel;
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
    this.renderRoster();
    this.renderForm();
    this.renderFindings();
    this.renderGraph();
    this.applyTab();
  }

  private setTab(tab: Tab): void {
    this.tab = tab;
    // Coming back to the form from the text: the model may have changed under it.
    if (tab === "form") this.renderForm();
    this.applyTab();
    if (tab === "yaml") this.yamlArea.focus();
  }

  private applyTab(): void {
    for (const b of Array.from(this.tabBar.children) as HTMLElement[]) {
      b.classList.toggle("active", b.dataset.tab === this.tab);
    }
    this.formPane.hidden = this.tab !== "form";
    this.yamlPane.hidden = this.tab !== "yaml";
    this.graphPane.hidden = this.tab !== "graph";
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
      r.addEventListener("click", () => {
        this.selection = sel;
        this.setTab("form");
        this.renderRoster();
      });
      return r;
    };

    rows.push(el("div", "wf-roster-head", "Workflow"));
    rows.push(row({ kind: "workflow" }, w.name || "(unnamed)", `version ${w.version}`, false));

    rows.push(el("div", "wf-roster-head", "Blocks"));
    w.blocks.forEach((b, i) => {
      const bad = this.blockFindings(b).some((f) => f.severity === "error");
      rows.push(
        row(
          { kind: "block", index: i },
          b.name || b.id || "(no id)",
          `${b.kind || "?"} · ${b.cli || "?"}`,
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

    this.rosterEl.replaceChildren(...rows);
  }

  /** The findings about ONE block row. A finding names a block by ID, because that is what
   *  a human reads — so an id-LESS stub takes the id-less findings ("a block has no id"),
   *  and where there are two such stubs they each show it. That is not a compromise: the
   *  finding is the same finding, and it is true of both. */
  private blockFindings(b: WorkflowBlock): Finding[] {
    return this.analysis.findings.filter((f) => f.blockId === (b.id || ""));
  }

  private renderForm(): void {
    if (this.syntaxBroken()) {
      const warn = el(
        "div",
        "wf-blocked",
        "The YAML doesn't parse, so the form is disabled — editing it here would rewrite the text you're fixing. " +
          "Open the YAML tab, fix the error below, and the form comes back."
      );
      this.formPane.replaceChildren(warn);
      return;
    }
    const w = this.analysis.workflow;
    if (this.selection.kind === "block") {
      const index = this.selection.index;
      const block = w.blocks[index];
      if (!block) {
        // The block the form was on is gone — deleted here, or deleted by an edit in the
        // YAML tab. Fall back rather than render a form over nothing.
        this.selection = { kind: "workflow" };
        this.renderForm();
        return;
      }
      this.formPane.replaceChildren(this.blockForm(w, block, index));
      return;
    }
    if (this.selection.kind === "edge") {
      const { from, to } = this.selection;
      // An edge that no longer exists (erased here, or in the YAML tab) is not an edge to show
      // a panel for.
      if (!w.edges.some((e) => e.from === from && e.to === to)) {
        this.selection = { kind: "workflow" };
        this.renderForm();
        return;
      }
      this.formPane.replaceChildren(this.edgeForm(from, to));
      return;
    }
    this.formPane.replaceChildren(this.selection.kind === "gate" ? this.gateForm(w) : this.workflowForm(w));
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

  private workflowForm(w: Workflow): HTMLElement {
    const box = el("div", "wf-fields");
    box.append(el("h3", "wf-form-title", "Workflow"));
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
    box.append(this.field("Schema version", version, "Set by loomux; a newer version needs a newer build."));
    box.append(
      el(
        "p",
        "wf-note",
        "Edges are ADVISORY — they declare the intended path; the orchestrator still decides when to spawn what. " +
          "The merge gate is ENFORCED: loomux refuses `gh pr merge` until every reviewer it names has recorded a PASS."
      )
    );
    return box;
  }

  private blockForm(w: Workflow, b: WorkflowBlock, index: number): HTMLElement {
    const box = el("div", "wf-fields");
    box.append(el("h3", "wf-form-title", b.name || b.id || `Block ${index + 1}`));

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

    box.append(
      this.field("Agent CLI", this.select(WORKFLOW_CLIS, b.cli, (v) => edit((t) => (t.cli = v))))
    );

    box.append(
      this.field(
        "Model",
        this.textInput(b.model, (v) => edit((t) => (t.model = v), false), "(the CLI's default)"),
        "e.g. opus, sonnet, auto. Blank leaves the CLI's default."
      )
    );

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
        "none = loomux's built-in role instructions. prompt = inline (compiled to the CLI's native inline agent). " +
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
    box.append(el("h3", "wf-form-title", `${from} → ${to}`));
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
    box.append(el("h3", "wf-form-title", "Merge gate"));
    box.append(
      el(
        "p",
        "wf-note",
        "ENFORCED, not advised: loomux refuses `gh pr merge` (via the PATH shim an agent cannot get around) " +
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
                reviewers: next.blocks.filter((b) => b.kind === "reviewer").map((b) => b.id),
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
      const n = document.createElement("input");
      n.className = "wf-input";
      n.type = "number";
      n.min = "1";
      n.value = String(gate.threshold ?? 1);
      n.addEventListener("input", () =>
        this.mutate((next) => {
          next.gates.merge!.threshold = Number(n.value) || 1;
        }, false)
      );
      box.append(this.field("Threshold", n));
    }

    const reviewers = el("div", "wf-checks");
    const reviewerBlocks = w.blocks.filter((b) => b.kind === "reviewer" && b.id);
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
      line.append(cb, el("span", "wf-check-label wf-bad", `${id} — not a reviewer block`));
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
        "Comma-separated extra conditions, enforced by the backend (#197)."
      )
    );
    return box;
  }

  /** Apply an edit to the model and write it straight back into the YAML.
   *
   *  `rerenderForm` is false for the free-text controls: re-rendering the form on every
   *  keystroke would rebuild the very input the human is typing into and drop the caret at
   *  its end. Structural edits (a kind change, an edge toggle, a persona switch) DO
   *  re-render, because they change which controls exist. */
  private mutate(edit: (w: Workflow) => void, rerenderForm = true): void {
    const next: Workflow = structuredClone(this.analysis.workflow);
    edit(next);
    this.commit(next);
    this.renderRoster();
    this.renderFindings();
    this.renderGraph();
    this.updateDirty();
    if (rerenderForm) this.renderForm();
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
    this.selection = { kind: "block", index };
    this.renderRoster();
    this.renderForm();
    this.renderGraph();
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
      const where = f.line ? `line ${f.line}` : f.blockId || "";
      if (where) r.append(el("span", "wf-finding-where", where));
      r.append(el("span", "wf-finding-msg", f.message));
      // Click a finding, land on the thing it is about — the whole value of a pre-run
      // validation pass is that it tells you WHERE.
      r.addEventListener("click", () => {
        if (f.line) {
          this.setTab("yaml");
          this.focusLine(f.line);
          return;
        }
        // A finding names a block by id; the form is keyed by ROW. Land on the first row
        // that answers to that id — which for a duplicate pair is the first of the two,
        // and the duplication is reported on both, so the human sees the pair either way.
        const index = this.analysis.workflow.blocks.findIndex((b) => b.id === f.blockId);
        if (index < 0) return;
        this.selection = { kind: "block", index };
        this.setTab("form");
        this.renderRoster();
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
    defs.append(arrowMarker("wf-arrow", "#6b7394"), arrowMarker("wf-arrow-gate", "#e0af68"));
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
        this.selection = { kind: "gate" };
        this.setTab("form");
        this.renderRoster();
        this.renderGraph();
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
        root.setPointerCapture(e.pointerId);
        this.renderGraph();
        return;
      }

      this.selection = { kind: "block", index };
      this.dragging = {
        key,
        id: block?.id ?? "",
        grab: { x: pt.x - rect.x, y: pt.y - rect.y },
        at: { x: rect.x, y: rect.y },
      };
      root.setPointerCapture(e.pointerId);
      this.renderRoster();
      this.renderForm();
      this.renderGraph();
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
    this.selection =
      hit !== null
        ? { kind: "edge", from: drawn[hit]!.edge.from, to: drawn[hit]!.edge.to }
        : { kind: "workflow" };
    this.renderRoster();
    this.renderForm();
    this.renderGraph();
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
    this.mutate((next) => Object.assign(next, disconnectBlocks(next, from, to)));
    if (this.selection.kind === "edge" && this.selection.from === from && this.selection.to === to) {
      this.selection = { kind: "workflow" };
      this.renderForm();
    }
    this.renderGraph();
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

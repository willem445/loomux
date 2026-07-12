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
  serializeWorkflow,
  starterWorkflow,
  removeBlockAt,
  nextBlockId,
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
import { ftReadFile, ftWriteFile, errorCode, errorMessage } from "./fileapi";
import { closeDecision, discardEdits, type ConflictChoice } from "./dirtystate";
import { showToast } from "./toast";
import { modal } from "./modal";

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
type Selection = { kind: "workflow" } | { kind: "block"; index: number } | { kind: "gate" };

function el(tag: string, cls: string, text?: string): HTMLElement {
  const e = document.createElement(tag);
  e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

const svg = (tag: string): SVGElement => document.createElementNS("http://www.w3.org/2000/svg", tag);

// Graph geometry. Fixed, not measured: a layered draw with known box sizes is
// deterministic — it cannot flash wrong on first paint or depend on a font that hasn't
// loaded, and there is nothing here a ResizeObserver would tell us that we don't know.
const NODE_W = 168;
const NODE_H = 52;
const COL_GAP = 72;
const ROW_GAP = 22;
const PAD = 16;

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

  private analysis: WorkflowAnalysis;
  private selection: Selection = { kind: "workflow" };
  private tab: Tab = "form";
  private disposed = false;

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
  private bodyEl: HTMLElement;

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
    formatBtn.addEventListener("click", () => this.format());

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

    // ---- empty state (no workflow file yet) ----
    this.emptyEl = el("div", "wf-empty");
    const emptyTitle = el("div", "wf-empty-title", "No workflow in this repo yet");
    const emptyBody = el(
      "div",
      "wf-empty-body",
      `A workflow declares the agent blocks a run may use, the path between them, and the ` +
        `gate that must pass before a merge. It lives in ${WORKFLOW_FILE} — committed, so it ` +
        `is shared with everyone who clones the repo.`
    );
    const starterBtn = document.createElement("button");
    starterBtn.className = "wf-btn wf-btn-primary";
    starterBtn.textContent = "Create a starter workflow";
    starterBtn.addEventListener("click", () => {
      this.setText(serializeWorkflow(starterWorkflow()));
      this.render();
      showToast("Starter workflow created — Save (Ctrl+S) writes it to disk.", "info");
    });
    this.emptyEl.append(emptyTitle, emptyBody, starterBtn);
    this.emptyEl.hidden = true;

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

    this.el.append(head, this.emptyEl, this.bodyEl, this.findingsEl);

    // Ctrl+S saves from anywhere in the pane — including from inside the textarea, where
    // the browser would otherwise do nothing at all.
    this.el.addEventListener("keydown", (e) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        void this.save();
      }
    });
  }

  // ---------- lifecycle ----------

  /** Load the file and render. Called by the pane once the view is in the document. */
  show(): void {
    this.el.hidden = false;
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

  private async load(): Promise<void> {
    if (!this.root) {
      this.setText("");
      this.render();
      return;
    }
    try {
      const fr = await ftReadFile(this.root, this.rel);
      if (this.disposed) return;
      this.exists = true;
      this.savedHash = fr.hash;
      this.savedText = fr.content;
      this.text = fr.content;
    } catch (err) {
      if (this.disposed) return;
      const code = errorCode(err);
      if (code !== "not-found") {
        // A file that exists but can't be read (binary, too large, unreadable) is a fact
        // worth saying out loud — silently showing the "no workflow yet" empty state would
        // invite the human to overwrite a file they can't see.
        showToast(`Cannot open ${this.rel}: ${errorMessage(err)}`);
      }
      this.exists = false;
      this.savedHash = "";
      this.savedText = "";
      this.text = "";
    }
    this.reanalyze();
    this.render();
  }

  private async reload(): Promise<void> {
    if (this.dirty && !(await this.canDiscard())) return;
    await this.load();
  }

  private async save(): Promise<void> {
    if (!this.root || !this.dirty) return;
    // Saving a file whose YAML doesn't parse is allowed on purpose: it is text, the human
    // may be mid-edit, and a half-finished workflow on disk is recoverable while a lost
    // one is not. The findings strip is what says it isn't runnable yet.
    try {
      const res = await ftWriteFile(this.root, this.rel, this.text, this.exists ? this.savedHash : null);
      this.savedText = this.text;
      this.savedHash = res.hash;
      this.exists = true;
      this.updateDirty();
      showToast(`Saved ${this.rel}`, "info");
    } catch (err) {
      if (errorCode(err) === "conflict") await this.resolveConflict();
      else showToast(`Save failed: ${errorMessage(err)}`);
    }
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
   *  — it IS a file edit, immediately, in canonical form. */
  private commit(w: Workflow): void {
    this.setText(serializeWorkflow(w));
  }

  private reanalyze(): void {
    this.analysis = analyzeWorkflow(this.text);
  }

  private format(): void {
    if (this.syntaxBroken()) {
      showToast("Fix the YAML syntax first — formatting a file we can't read would rewrite it wrong.");
      return;
    }
    this.commit(this.analysis.workflow);
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

  private render(): void {
    const empty = !this.exists && !this.text.trim();
    this.emptyEl.hidden = !empty;
    this.bodyEl.hidden = empty;
    this.findingsEl.hidden = empty;
    this.yamlArea.value = this.text;
    this.updateDirty();
    if (empty) {
      this.statusEl.textContent = "";
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
    add.addEventListener("click", () => this.addBlock());
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

  private addBlock(): void {
    const w = this.analysis.workflow;
    const id = nextBlockId(w, "block");
    const index = w.blocks.length;
    this.mutate((next) => {
      next.blocks.push({ id, name: "New block", kind: "reviewer", cli: "claude", model: "" });
    });
    this.selection = { kind: "block", index };
    this.setTab("form");
    this.renderRoster();
    this.renderForm();
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

  // ---------- the read-only graph ----------

  private renderGraph(): void {
    const g = this.analysis.graph;
    if (!g.nodes.length) {
      this.graphPane.replaceChildren(el("div", "wf-hint", "No blocks to draw yet."));
      return;
    }

    // GHOST nodes: an edge naming a block that doesn't exist still gets drawn, into a
    // dashed placeholder. Silently omitting it would make the graph disagree with the file
    // — and the graph is supposed to be how you SEE the file.
    const known = new Set(g.nodes.map((n) => n.block.id));
    const ghosts = [...new Set(g.edges.flatMap((e) => [e.from, e.to]).filter((id) => id && !known.has(id)))];

    const layers = g.layers.map((ids) => [...ids]);
    if (ghosts.length) layers.push(ghosts);

    const pos = new Map<string, { x: number; y: number }>();
    layers.forEach((ids, col) => {
      ids.forEach((id, row) => {
        pos.set(id, { x: PAD + col * (NODE_W + COL_GAP), y: PAD + row * (NODE_H + ROW_GAP) });
      });
    });

    const gate = g.gates[0];
    const gateX = PAD + layers.length * (NODE_W + COL_GAP);
    const gateRows = gate ? Math.max(1, gate.reviewers.length) : 0;
    const gateY = PAD;
    const gateH = gate ? Math.max(NODE_H, gateRows * 22 + 30) : 0;

    // Without a gate the drawing ends at the last column's right edge (gateX is one full
    // column-plus-gap past it), so the trailing gap comes back off.
    const width = gate ? gateX + NODE_W + PAD : gateX - COL_GAP + PAD;
    const height =
      PAD * 2 +
      Math.max(
        ...layers.map((ids) => ids.length * (NODE_H + ROW_GAP) - ROW_GAP),
        gate ? gateH : 0
      );

    const root = svg("svg");
    root.setAttribute("class", "wf-graph-svg");
    root.setAttribute("width", String(width));
    root.setAttribute("height", String(height));
    root.setAttribute("viewBox", `0 0 ${width} ${height}`);

    const defs = svg("defs");
    defs.append(arrowMarker("wf-arrow", "#6b7394"), arrowMarker("wf-arrow-gate", "#e0af68"));
    root.append(defs);

    // ADVISORY edges: solid, plain arrow. They say what the workflow INTENDS.
    for (const e of g.edges) {
      const a = pos.get(e.from);
      const b = pos.get(e.to);
      if (!a || !b) continue;
      const path = svg("path");
      const x1 = a.x + NODE_W;
      const y1 = a.y + NODE_H / 2;
      const x2 = b.x;
      const y2 = b.y + NODE_H / 2;
      const mid = (x1 + x2) / 2;
      path.setAttribute("d", `M ${x1} ${y1} C ${mid} ${y1}, ${mid} ${y2}, ${x2} ${y2}`);
      path.setAttribute("class", e.resolved ? "wf-edge" : "wf-edge wf-edge-broken");
      path.setAttribute("marker-end", "url(#wf-arrow)");
      root.append(path);
    }

    for (const n of g.nodes) node(root, pos.get(n.block.id)!, n);
    for (const id of ghosts) ghostNode(root, pos.get(id)!, id);

    // The ENFORCED gate: a different shape (dashed, amber) and a dashed connector from
    // every reviewer it names. Advisory and enforced must not look alike — the whole point
    // of the distinction is that one of them can actually stop a merge.
    if (gate) {
      for (const rid of gate.reviewers) {
        const a = pos.get(rid);
        if (!a) continue;
        const line = svg("path");
        const x1 = a.x + NODE_W;
        const y1 = a.y + NODE_H / 2;
        const x2 = gateX;
        const y2 = gateY + gateH / 2;
        const mid = (x1 + x2) / 2;
        line.setAttribute("d", `M ${x1} ${y1} C ${mid} ${y1}, ${mid} ${y2}, ${x2} ${y2}`);
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
      root.append(box);
      root.append(text(gateX + 12, gateY + 22, "⛔ merge gate", "wf-gate-title"));
      const detail =
        gate.require === "threshold"
          ? `${gate.threshold ?? "?"} of ${gate.reviewers.length} must PASS`
          : `all ${gate.reviewers.length} must PASS`;
      root.append(text(gateX + 12, gateY + 40, detail, "wf-gate-sub"));
    }

    const legend = el("div", "wf-legend");
    legend.append(
      el("span", "wf-legend-item wf-legend-edge", "— advisory edge (the declared path)"),
      el("span", "wf-legend-item wf-legend-gate", "-- enforced gate (blocks the merge)")
    );
    const scroll = el("div", "wf-graph-scroll");
    scroll.append(root as unknown as HTMLElement);
    this.graphPane.replaceChildren(scroll, legend);
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

function node(root: SVGElement, at: { x: number; y: number }, n: GraphNode): void {
  const bad = !n.known || !isWorkflowCli(n.block.cli);
  const box = svg("rect");
  box.setAttribute("x", String(at.x));
  box.setAttribute("y", String(at.y));
  box.setAttribute("width", String(NODE_W));
  box.setAttribute("height", String(NODE_H));
  box.setAttribute("rx", "8");
  box.setAttribute("class", `wf-node wf-node-${isBlockKind(n.block.kind) ? n.block.kind : "unknown"}`);
  root.append(box);
  root.append(text(at.x + 12, at.y + 21, clip(n.block.name || n.block.id, 20), "wf-node-title"));
  root.append(
    text(
      at.x + 12,
      at.y + 38,
      clip(`${bad ? "⚠ " : ""}${n.block.kind || "?"} · ${n.block.cli || "?"}`, 22),
      "wf-node-sub"
    )
  );
}

function ghostNode(root: SVGElement, at: { x: number; y: number }, id: string): void {
  const box = svg("rect");
  box.setAttribute("x", String(at.x));
  box.setAttribute("y", String(at.y));
  box.setAttribute("width", String(NODE_W));
  box.setAttribute("height", String(NODE_H));
  box.setAttribute("rx", "8");
  box.setAttribute("class", "wf-node wf-node-ghost");
  root.append(box);
  root.append(text(at.x + 12, at.y + 21, clip(id, 20), "wf-node-title"));
  root.append(text(at.x + 12, at.y + 38, "no such block", "wf-node-sub"));
}

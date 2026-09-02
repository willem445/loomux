// The model dropdown, shared by every surface that pins a model (#935).
//
// DOM wiring only — every decision it makes lives in `modelcatalog.ts`
// (`pickerSelection`, the merge) where a test can reach it. It was a private
// class inside `launcher.ts`; the workflow pane's block editor needs the same
// control, and the second copy of a control is the second place a fix has to be
// remembered.
//
// A plain `<datalist>` does not work here and that is why this class exists:
// browsers filter a datalist's suggestions by the input's current text, so a
// pre-filled default hides every other option — a "dropdown" that shows one entry
// until you delete what is in the box.
//
// The `custom…` escape is not a nicety either. A dropdown that could only offer
// ids loomux already knew would be a NARROWER field than the free text it
// replaces: Bedrock inference profiles, gateway deployment names and any model
// newer than this build all arrive as ids no curated list or probe carries. The
// picker fails open, exactly like `selectorknobs.ts` does on an id it cannot
// place.

import { INHERIT_MODEL_LABEL, detectedModelOptionLabel, modelSummaryLine } from "./modelnames.ts";
import { CUSTOM_OPTION, pickerSelection, type ModelDetail } from "./modelcatalog.ts";

export interface ModelPickerOptions {
  /** Class for the `<select>`. Defaults to the launcher's dialog styling; the
   *  workflow pane passes its own (`wf-input`), which is the whole reason this
   *  is a parameter — a shared control that hardcoded one host's class would be
   *  unstyled in the other. */
  selectClass?: string;
  /** Class for the custom-id `<input>`. */
  inputClass?: string;
  /** Placeholder for the custom-id input. */
  placeholder?: string;
  /** What the EMPTY id's row reads, when the option list carries one. Defaults to
   *  the launcher's `INHERIT_MODEL_LABEL` — "the model your own CLI config
   *  selects", which is what an empty `--model` means for a pane loomux launches.
   *
   *  A parameter because the same empty id means something else in a workflow
   *  file: `model_of` (workflow.rs) resolves a block's missing `model:` to
   *  `default_model(cli, kind)` — `sonnet`/`opus` on claude, `auto` on copilot,
   *  `pro` on gemini — and only on opencode is it genuinely "send nothing". One
   *  label for both hosts would state a vendor outcome that is false on three of
   *  the four CLIs the block editor offers. */
  blankLabel?: string;
  /** What the CLI itself reported about a model id (#993). Returning `null` for
   *  everything — the default — is exactly the picker every caller had before
   *  detection existed.
   *
   *  A lookup rather than a snapshot because the host owns the catalog and the
   *  answer changes underneath the control: the picker asks at paint time and
   *  never caches, so the host re-renders instead of the picker going stale. */
  detailFor?: (id: string) => ModelDetail | null;
}

export class ModelPicker {
  readonly root: HTMLElement;
  private sel: HTMLSelectElement;
  private custom: HTMLInputElement;
  /** Fired whenever the effective model changes (#687): the context-window knob
   *  is only available on models whose `[1m]` form the vendor documents, so the
   *  knob row has to re-derive when this moves — including when a custom id is
   *  typed, which is the case a plain `change` listener on the select misses. */
  onChange: (() => void) | null = null;
  private readonly blankLabel: string;
  private readonly detailFor: (id: string) => ModelDetail | null;
  /** The reported-facts line under the control. Absent from the DOM until it
   *  has something to be — an empty line reserves height in every picker whose
   *  CLI reported nothing. (#993, and the constraint 1 rule — this is chrome
   *  inside the form, never a PTY resize.)
   *
   *  It is the only thing detection puts on this control now. #1020 removed the
   *  `detect` button that used to sit beside it: detection happens
   *  automatically at startup (`src-tauri/src/modelwire.rs`), so there is
   *  nothing left for a human to ask for, and a button that re-asked would be
   *  an affordance for a spawn this picker can no longer make. */
  private summary: HTMLElement;
  private cli = "";
  /** The options the select currently carries, so `set value` (the repo host
   *  #2010) can re-derive the selection the same way `setOptions` does. */
  private options: readonly string[] = [];

  constructor(opts: ModelPickerOptions = {}) {
    this.blankLabel = opts.blankLabel ?? INHERIT_MODEL_LABEL;
    this.detailFor = opts.detailFor ?? (() => null);
    this.root = document.createElement("div");
    this.root.className = "model-picker";
    this.sel = document.createElement("select");
    this.sel.className = opts.selectClass ?? "dlg-select";
    this.custom = document.createElement("input");
    this.custom.className = opts.inputClass ?? "dlg-input";
    this.custom.placeholder = opts.placeholder ?? "model id…";
    this.custom.spellcheck = false;
    this.custom.hidden = true;
    this.sel.addEventListener("change", () => {
      this.custom.hidden = this.sel.value !== CUSTOM_OPTION;
      this.rehomeInitialFocus();
      if (!this.custom.hidden) this.custom.focus();
      this.paintSummary();
      this.onChange?.();
    });
    this.custom.addEventListener("input", () => {
      this.paintSummary();
      this.onChange?.();
    });
    this.summary = document.createElement("div");
    this.summary.className = "model-picker-summary";
    this.summary.hidden = true;
    this.root.append(this.sel, this.custom, this.summary);
  }

  /** Re-render the reported-facts line for whatever is selected right now.
   *  Empty means the line is hidden rather than blank — a surface with nothing
   *  to say says nothing, the same rule `contextWindowLabel` follows. */
  private paintSummary(): void {
    const line = modelSummaryLine(this.cli, this.value, this.detailFor(this.value));
    this.summary.textContent = line;
    this.summary.hidden = line === "";
  }

  /** Rebuild the options, keeping the current choice when still valid. `cli` is
   *  which CLI's vocabulary the ids belong to — an alias means what the CLI that
   *  documents it says it means, and nothing on any other CLI (#687). */
  setOptions(models: readonly string[], fallback: string, cli = ""): void {
    this.options = models;
    const state = pickerSelection(models, this.value || fallback);
    this.sel.replaceChildren(
      ...models.map((m) => {
        const o = document.createElement("option");
        o.value = m;
        // The VALUE stays the raw id — it is what `--model` receives, `/` and
        // all (#722: opencode's ids are `provider_id/model_id`). Only the label
        // is prettified, and only where the name says something the id doesn't
        // (modelnames.ts). The empty id is a real entry — "send no --model" —
        // and gets the one label that isn't derived from an id, which is the
        // host's to word (`blankLabel`) because the two hosts' empty ids resolve
        // differently.
        // A name the human's own install printed outranks both the prettifier
        // and this repo's quoted alias table (#993) — it is what their CLI's
        // own `/model` picker shows them.
        o.textContent = m.trim() === "" ? this.blankLabel : detectedModelOptionLabel(cli, m, this.detailFor(m));
        return o;
      })
    );
    const custom = document.createElement("option");
    custom.value = CUSTOM_OPTION;
    custom.textContent = "custom…";
    this.sel.appendChild(custom);
    this.sel.value = state.selected;
    if (state.showCustom) this.custom.value = state.custom;
    this.custom.hidden = !state.showCustom;
    // A rebuild flips branches exactly like the two host-facing sites do, so
    // it re-homes the marker the same way. The launcher seeds via setOptions
    // before stamping the marker (launcher.ts:917→923), where this is a
    // no-op — that ordering is a host's choice, not this class's guarantee,
    // and the module header's rule ("the second copy of a control is the
    // second place a fix has to be remembered") is why the method cannot
    // assume it (#2108).
    this.rehomeInitialFocus();
    this.cli = cli;
    this.paintSummary();
  }

  /** Whether the human is typing an id into the `custom…` box RIGHT NOW.
   *
   *  The one question a host has to ask before rebuilding this control from a
   *  reply that landed asynchronously: `setOptions` re-runs `pickerSelection`,
   *  and a half-typed id resolves to the DROPDOWN branch — which hides the input
   *  under the caret and sends the rest of the keystrokes nowhere.
   *
   *  Deliberately narrower than "focus is somewhere in this picker" (#997
   *  review). The hazard is the text input specifically, and only while it is
   *  visible: a human editing a custom id must not have it yanked away, but one
   *  whose focus is merely on the `<select>` loses nothing to a rebuild that
   *  re-selects the same value. Under #1020 the reply that triggers the rebuild
   *  arrives on its own schedule rather than after a click, which widens the
   *  window this guards rather than closing it. */
  get editingCustom(): boolean {
    return !this.custom.hidden && document.activeElement === this.custom;
  }

  /** Rebuild this control now, or as soon as doing so stops being destructive.
   *
   *  The mid-type hazard has two halves and the first cut only handled one
   *  (#997 review NB-3). Refusing to rebuild while the human is typing is
   *  right; *dropping* the rebuild is not, because nothing schedules another
   *  one. On the launcher that was permanent — `applyRoleModels` is otherwise
   *  reachable only from the role's CLI `change` listener and the seed pass, so
   *  a detection that landed mid-type never reached that role's dropdown again
   *  for the life of the dialog, and the human saw the models they were told
   *  about never appear.
   *
   *  So the work is deferred to the input's next `blur`, which is the moment the
   *  hazard ends. `once` per deferral: two replies landing mid-type queue two
   *  rebuilds, which is harmless because a rebuild is idempotent, and cheaper to
   *  reason about than a shared pending slot that has to decide which wins.
   *
   *  Owned here rather than by each host because the knowledge is this module's:
   *  which element is the hazard, and which event ends it. Both surfaces ask the
   *  same question through the same seam. */
  runWhenNotEditing(rebuild: () => void): void {
    if (!this.editingCustom) {
      rebuild();
      return;
    }
    this.custom.addEventListener("blur", () => rebuild(), { once: true });
  }

  get value(): string {
    if (!this.sel.options.length) return "";
    return this.sel.value === CUSTOM_OPTION ? this.custom.value.trim() : this.sel.value;
  }

  /** The two elements a host composes into its own field layout. The repo field
   *  (#2010) re-styles and re-points them per kind — the path input carries the
   *  kind's placeholder and the pane's initial-focus marker (which follows the
   *  VISIBLE half, since the picker hides one of the two by design) — without
   *  the launcher reaching into private state. Read-mostly seams: hosts must
   *  not rebuild these elements; the picker owns their structure. */
  get select(): HTMLSelectElement {
    return this.sel;
  }
  get input(): HTMLInputElement {
    return this.custom;
  }

  /** Set the value from the host — the Browse… pick, whose dialog result is not
   *  a keystroke. Same re-derivation `setOptions` runs: a value that matches an
   *  option marks that option, anything else opens the custom branch carrying
   *  it (an unknown path is normal). Fires nothing: programmatic writes have
   *  never delivered input events, and the caller does its own follow-up work
   *  (the launcher re-derives the pane name and roster after a Browse… pick). */
  set value(v: string) {
    const state = pickerSelection(this.options, v);
    this.sel.value = state.selected;
    if (state.showCustom) this.custom.value = state.custom;
    // The dropdown branch hides the input, so the previously typed path would
    // sit there invisibly and resurface on the next custom… pick — clear it
    // the way a human clearing the field would (#2108).
    else this.custom.value = "";
    this.custom.hidden = !state.showCustom;
    this.rehomeInitialFocus();
    this.paintSummary();
  }

  /** Keep a host's `data-initial-focus` marker on the VISIBLE half (rev-std
   *  round 1, finding 2 on #2010; predicate corrected in round 4, B1). The
   *  launcher stamps it on whichever half shows at construction, and a branch
   *  flip afterwards — a Browse… pick that lands on a recent, or the human
   *  picking one in the dropdown — would otherwise strand it on the now-hidden
   *  element, where `focus()` is a no-op and the pane's focusWelcome() falls to
   *  the marker's DOM-order first match. That stranded select is a
   *  value-changing control: one arrow key fires `change`, hides the input and
   *  silently replaces a half-typed path with a recent directory. The marker
   *  therefore follows the visible half in BOTH directions — the predicate is
   *  the same one `focus()` uses below, and when both halves are showing the
   *  free-text input wins, because that is the half a human is typing into.
   *  Touched only when found on the picker's own two elements: a marker this
   *  picker did not stamp is never moved. */
  private rehomeInitialFocus(): void {
    const MARKER = "data-initial-focus";
    const marked = this.custom.hasAttribute(MARKER)
      ? this.custom
      : this.sel.hasAttribute(MARKER)
        ? this.sel
        : null;
    if (!marked) return;
    const visible = this.custom.hidden ? this.sel : this.custom;
    if (marked === visible) return;
    marked.removeAttribute(MARKER);
    visible.setAttribute(MARKER, "");
  }

  /** Focus whichever half is showing. The validation-error paths bounce the
   *  human to the field that caused the problem — but on the dropdown branch
   *  the free-text input is hidden, and focus() on a hidden element lands
   *  nowhere, so the recents select takes it instead. */
  focus(): void {
    (this.custom.hidden ? this.sel : this.custom).focus();
  }
}

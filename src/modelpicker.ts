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
  /** What the CLI itself reported about a model id, when a human has asked
   *  (#993). Returning `null` for everything — the default — is exactly the
   *  picker every caller had before detection existed.
   *
   *  A lookup rather than a snapshot because the host owns the catalog and the
   *  answer changes underneath the control: the picker asks at paint time and
   *  never caches, so the host re-renders instead of the picker going stale. */
  detailFor?: (id: string) => ModelDetail | null;
  /** Runs the CLI's own list-models request, resolving when the answer (or the
   *  failure) is in hand. Supplying it is what puts the "detect" affordance on
   *  the control; omitting it leaves the picker with no way to spawn anything,
   *  which is the point — see `src-tauri/src/modelwire.rs` for why that must be
   *  a human gesture rather than a paint. */
  onDetect?: () => Promise<void>;
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
  private readonly onDetect: (() => Promise<void>) | null;
  /** The reported-facts line under the control. Absent from the DOM until it
   *  has something to be — an empty line reserves height in every picker that
   *  has never been detected. (#993, and the constraint 1 rule — this is chrome
   *  inside the form, never a PTY resize.)
   *
   *  The detect button is deliberately NOT held as a field: nothing outside its
   *  own click handler reads it, and the handler closes over it. */
  private summary: HTMLElement;
  private cli = "";

  constructor(opts: ModelPickerOptions = {}) {
    this.blankLabel = opts.blankLabel ?? INHERIT_MODEL_LABEL;
    this.detailFor = opts.detailFor ?? (() => null);
    this.onDetect = opts.onDetect ?? null;
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
    if (this.onDetect) this.root.append(this.buildDetectButton());
  }

  /** The one gesture that may spawn an agent CLI. It disables itself for the
   *  duration of the ask, which is not cosmetic: a second click while the first
   *  is in flight would be a second spawn, and this control is the only thing
   *  standing between a human's finger and that. */
  private buildDetectButton(): HTMLButtonElement {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "model-picker-detect";
    btn.textContent = "detect";
    btn.title = "Ask this CLI which models it offers on this machine, and what each one supports.";
    btn.addEventListener("click", () => {
      const ask = this.onDetect;
      if (!ask || btn.disabled) return;
      btn.disabled = true;
      btn.textContent = "asking…";
      void ask().finally(() => {
        btn.disabled = false;
        btn.textContent = "detect";
        this.paintSummary();
      });
    });
    return btn;
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
   *  review). A detection is started by clicking the button *inside* this
   *  control, so the broad test is true on every detect path and would suppress
   *  the very rebuild the click asked for. The hazard is the text input
   *  specifically, and only while it is visible; the seconds an ask can be in
   *  flight are long enough for a human to click into it and start typing. */
  get editingCustom(): boolean {
    return !this.custom.hidden && document.activeElement === this.custom;
  }

  get value(): string {
    if (!this.sel.options.length) return "";
    return this.sel.value === CUSTOM_OPTION ? this.custom.value.trim() : this.sel.value;
  }
}

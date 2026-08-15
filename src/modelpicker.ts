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

import { INHERIT_MODEL_LABEL, modelOptionLabel } from "./modelnames.ts";
import { CUSTOM_OPTION, pickerSelection } from "./modelcatalog.ts";

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

  constructor(opts: ModelPickerOptions = {}) {
    this.blankLabel = opts.blankLabel ?? INHERIT_MODEL_LABEL;
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
      this.onChange?.();
    });
    this.custom.addEventListener("input", () => this.onChange?.());
    this.root.append(this.sel, this.custom);
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
        o.textContent = m.trim() === "" ? this.blankLabel : modelOptionLabel(cli, m);
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
  }

  get value(): string {
    if (!this.sel.options.length) return "";
    return this.sel.value === CUSTOM_OPTION ? this.custom.value.trim() : this.sel.value;
  }
}

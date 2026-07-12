// A minimal confirm/choice modal over the shared `.agent-dialog` / `.dlg-*` kit
// (the same look as editorConfigDialog). Lifted out of fileedit.ts when the file
// manager (#214) needed the identical dialog — one copy, two callers, rather than
// a second implementation that would drift.

export interface ModalButton<T> {
  label: string;
  value: T;
  kind?: "danger" | "primary";
}

export interface ModalSpec<T> {
  title: string;
  body: string;
  buttons: ModalButton<T>[];
  onKey?: (key: string) => void;
  /** Render the body as monospace, wrapping, and selectable — for a hash digest, where
   *  128 hex characters have to be readable AND selectable, not laid out as prose. */
  bodyMono?: boolean;
  /** An itemized list under the body (#219): the quit confirm has to ENUMERATE the
   *  unsaved buffers, and a run-on paragraph of file paths is unreadable at exactly the
   *  moment the human must read it. A real list, so each path wraps as its own item and
   *  a long one scrolls instead of pushing the buttons off-screen. */
  bodyLines?: string[];
}

/** Show a modal and resolve with the chosen button's value. The builder receives
 *  `resolve` so a key handler (Escape) can settle it without a button. Settling is
 *  one-shot: a click and a trailing Escape can't both fire. */
export function modal<T>(build: (resolve: (v: T) => void) => ModalSpec<T>): Promise<T> {
  return new Promise<T>((resolve) => {
    let settled = false;
    const done = (v: T) => {
      if (settled) return;
      settled = true;
      overlay.remove();
      resolve(v);
    };
    const spec = build(done);

    const el = (tag: string, cls: string, text?: string): HTMLElement => {
      const e = document.createElement(tag);
      e.className = cls;
      if (text !== undefined) e.textContent = text;
      return e;
    };

    const overlay = el("div", "launcher-overlay visible");
    const dlg = el("div", "agent-dialog");
    const body = el("div", spec.bodyMono ? "dlg-hint dlg-mono" : "dlg-hint", spec.body);
    dlg.append(el("h2", "", spec.title), body);
    if (spec.bodyLines?.length) {
      const list = el("ul", "dlg-list");
      for (const line of spec.bodyLines) list.appendChild(el("li", "", line));
      dlg.appendChild(list);
    }
    const actions = el("div", "dlg-actions");
    for (const b of spec.buttons) {
      const cls = b.kind === "danger" ? " danger" : b.kind === "primary" ? " primary" : "";
      const btn = el("button", `dlg-btn${cls}`, b.label);
      btn.addEventListener("click", () => done(b.value));
      actions.appendChild(btn);
    }
    dlg.appendChild(actions);
    overlay.appendChild(dlg);
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay && spec.onKey) spec.onKey("Escape");
    });
    overlay.addEventListener("keydown", (e) => {
      e.stopPropagation();
      spec.onKey?.(e.key);
    });
    document.body.appendChild(overlay);
    (dlg.querySelector(".dlg-btn:last-child") as HTMLElement | null)?.focus();
  });
}

/** Ask for one line of text, validated as it is typed. Resolves to the value, or null if the
 *  human cancelled.
 *
 *  Added for the workflow canvas (#222 v2), which must ASK for a block's id rather than mint
 *  one: an id is immutable and human-meaningful (§4), and a canvas that generates
 *  `node_1720794829558` — Dify's actual behaviour — makes every edge in the file unreadable.
 *  `validate` runs on every keystroke and on submit, so a duplicate or malformed id is refused
 *  in the dialog, where the human is still typing it, rather than becoming a finding they have
 *  to go and understand afterwards.
 *
 *  Same overlay kit as `modal`, deliberately: one dialog look, one Escape behaviour, one
 *  place to fix them. */
export function promptModal(spec: {
  title: string;
  body: string;
  label: string;
  placeholder?: string;
  initial?: string;
  affirm: string;
  /** Return an error to REFUSE the value, or null to allow it. */
  validate?: (value: string) => string | null;
}): Promise<string | null> {
  return new Promise<string | null>((resolve) => {
    let settled = false;
    const done = (v: string | null) => {
      if (settled) return;
      settled = true;
      overlay.remove();
      resolve(v);
    };

    const el = (tag: string, cls: string, text?: string): HTMLElement => {
      const e = document.createElement(tag);
      e.className = cls;
      if (text !== undefined) e.textContent = text;
      return e;
    };

    const overlay = el("div", "launcher-overlay visible");
    const dlg = el("div", "agent-dialog");
    const input = document.createElement("input");
    input.className = "dlg-input";
    input.type = "text";
    input.value = spec.initial ?? "";
    if (spec.placeholder) input.placeholder = spec.placeholder;
    // `.dlg-error` is shown by the `visible` CLASS, not by the `hidden` attribute — the rule is
    // `display: none` until then, so an attribute toggle would leave it invisible forever.
    const error = el("div", "dlg-error");

    const submit = () => {
      const value = input.value.trim();
      const err = spec.validate?.(value) ?? null;
      if (err) {
        error.textContent = err;
        error.classList.add("visible");
        input.focus();
        return;
      }
      done(value);
    };

    input.addEventListener("input", () => {
      // Clear the complaint as soon as they start fixing it — an error that persists while you
      // type is an error you learn to ignore.
      error.classList.remove("visible");
    });
    input.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Enter") submit();
      if (e.key === "Escape") done(null);
    });

    const actions = el("div", "dlg-actions");
    const cancel = el("button", "dlg-btn", "Cancel");
    cancel.addEventListener("click", () => done(null));
    const ok = el("button", "dlg-btn primary", spec.affirm);
    ok.addEventListener("click", submit);
    actions.append(cancel, ok);

    dlg.append(
      el("h2", "", spec.title),
      el("div", "dlg-hint", spec.body),
      el("div", "dlg-label", spec.label),
      input,
      error,
      actions
    );
    overlay.appendChild(dlg);
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) done(null);
    });
    overlay.addEventListener("keydown", (e) => e.stopPropagation());
    document.body.appendChild(overlay);
    input.focus();
    input.select();
  });
}

/** A yes/no confirmation. `danger` styles the affirmative button red (destructive). */
export function confirmModal(
  title: string,
  body: string,
  affirm: string,
  danger = false
): Promise<boolean> {
  return modal<boolean>((resolve) => ({
    title,
    body,
    buttons: [
      { label: "Cancel", value: false },
      { label: affirm, value: true, kind: danger ? "danger" : "primary" },
    ],
    onKey: (k) => (k === "Escape" ? resolve(false) : undefined),
  }));
}

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

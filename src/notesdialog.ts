// The Notes overlay (#2116): add / list / delete the human's notes about one
// harness session.
//
// CONSTRAINT 1. It is an OVERLAY — `.launcher-overlay` over the whole window,
// on the same `.agent-dialog` / `.dlg-*` kit `modal.ts` uses, so it has the
// same look and the same Escape behaviour. It is never an in-flow element and
// never resizes a pane: nothing here touches the grid, and a ConPTY resize
// would repaint scrollback the human is reading.
//
// NOT BUILT ON `modal()` ITSELF. That is a button-confirm: a title, a body and
// a row of buttons. This needs a live list with a per-row delete, a textarea,
// and a re-render every time the store changes underneath it. It reuses the
// KIT rather than the function, which is the same call `promptModal` in that
// file already made.
//
// ONE DIALOG, TWO ENTRY POINTS. The pane header opens it against the pane, and
// the sessions list opens it against a recorded session — live or dead. A note
// is the human's record ABOUT a session; whether the session can still be
// resumed is the harness's concern, not the note's, so a dead session's notes
// are read/write like any other.
//
// THE UNSUBMITTED DRAFT LIVES IN THIS VIEW, NEVER IN ITS DOM. The list
// re-renders on every `store.onChange` — a note added on another surface, a
// re-key landing, a rename — and a textarea rebuilt from a literal would eat
// what the human was half-way through typing. So the draft is held in a `Map`
// keyed by target that the textarea is a VIEW of: seeded from the map on open,
// written to the map on every `input`, and pruned when it goes pristine. That
// is CLAUDE.md's in-list-editor rule applied to the one editor this dialog has.

import { confirmModal } from "./modal.ts";
import {
  MAX_NOTE_LEN,
  NoteDrafts,
  noteDraftIsPristine,
  noteWriteFeedback,
  notesEmptyState,
  orderedNotes,
  type NoteTarget,
  type NoteWriteOutcome,
  type SessionNote,
} from "./notesmodel.ts";
import type { SessionLogStore } from "./sessionlog.ts";

/** The session id a target names, or `null` for a pane whose id is not known
 *  yet — which is what `notesEmptyState` needs to tell the two situations
 *  apart. */
function sessionIdOf(target: NoteTarget): string | null {
  return "sessionId" in target ? target.sessionId : null;
}

/** The unsubmitted drafts, module-level and not per-open, so closing the dialog
 *  and reopening it on the same target hands the human back what they were
 *  typing. The book itself lives in `notesmodel.ts`, where it is DOM-free and
 *  unit-tested; this module owns only the wiring. */
const drafts = new NoteDrafts();

export interface NotesDialogSpec {
  /** Which session (or not-yet-identified pane) these notes belong to. */
  target: NoteTarget;
  /** What the header calls it — the pane name, or the session's row title. */
  title: string;
  store: SessionLogStore;
  /** Wall clock, injected so a caller can pin it. Defaults to `Date.now`. */
  now?: () => number;
}

/** Open the notes overlay. Resolves when it closes, so the caller can hand
 *  focus back to the pane (`pane.focus()`, as `openInEditor` does). */
export function openNotes(spec: NotesDialogSpec): Promise<void> {
  const now = spec.now ?? (() => Date.now());
  const sessionId = sessionIdOf(spec.target);

  return new Promise<void>((resolve) => {
    let settled = false;
    let unsubscribe: (() => void) | null = null;
    const close = () => {
      if (settled) return;
      settled = true;
      unsubscribe?.();
      overlay.remove();
      document.removeEventListener("keydown", onDocKey, true);
      resolve();
    };

    const el = (tag: string, cls: string, text?: string): HTMLElement => {
      const e = document.createElement(tag);
      e.className = cls;
      if (text !== undefined) e.textContent = text;
      return e;
    };

    const overlay = el("div", "launcher-overlay visible");
    const dlg = el("div", "agent-dialog notes-dialog");

    const head = el("h2", "", "Notes");
    const subtitle = el("div", "dlg-hint notes-subtitle", spec.title);
    subtitle.title = spec.title;

    const list = el("div", "notes-list");

    const input = document.createElement("textarea");
    input.className = "dlg-input notes-input";
    input.rows = 3;
    input.maxLength = MAX_NOTE_LEN;
    input.placeholder = "Add a note about this session…";
    // Seeded from the VIEW's draft, never from a literal: the human may have
    // been typing when they last closed this.
    input.value = drafts.get(spec.target);

    // `.dlg-error` is shown by the `visible` CLASS, not by the `hidden`
    // attribute — the rule is `display: none` until then, so an attribute
    // toggle would leave it invisible forever (`promptModal` carries the same
    // note, and the same trap).
    const error = el("div", "dlg-error");

    const counter = el("div", "notes-counter");
    const actions = el("div", "dlg-actions");
    const closeBtn = el("button", "dlg-btn", "Close");
    const addBtn = el("button", "dlg-btn primary", "Add note") as HTMLButtonElement;
    actions.append(counter, closeBtn, addBtn);

    /** Reflect the draft: the counter, and whether there is anything to add.
     *  Runs on every keystroke, so a disabled button is one the human can SEE
     *  is disabled rather than one that refuses after the click. */
    const reflectDraft = (): void => {
      const text = input.value;
      drafts.set(spec.target, text);
      addBtn.disabled = noteDraftIsPristine(text);
      // Only ever shown near the cap — a character counter on an empty box is
      // noise, and this is a note, not a form field with a quota.
      const left = MAX_NOTE_LEN - text.length;
      const near = left <= 200;
      counter.textContent = near ? `${left} left` : "";
      counter.classList.toggle("visible", near);
    };

    /** React to what a write actually DID.
     *
     *  Ignoring this is how a note is lost outright: `declined-unread` means
     *  the store recorded nothing anywhere, and the box has already been
     *  cleared, so the human's text would be gone with nothing said. The two
     *  failure shapes need different handling — `notesmodel.ts` owns which is
     *  which, so the wording and the give-it-back decision are testable
     *  without a DOM. */
    const reportWrite = (outcome: NoteWriteOutcome, attempted: string): void => {
      const { message, restoreDraft } = noteWriteFeedback(outcome);
      if (restoreDraft && noteDraftIsPristine(input.value)) {
        // Only if the human has not started typing something else in the
        // meantime — their newer text outranks the one we are handing back.
        input.value = attempted;
        reflectDraft();
      }
      error.textContent = message ?? "";
      error.classList.toggle("visible", message !== null);
    };

    const renderList = (): void => {
      const notes: SessionNote[] =
        sessionId === null
          ? spec.store.pendingFor(paneKeyOf(spec.target))
          : (spec.store.get(sessionId)?.notes ?? []);
      list.replaceChildren();
      if (notes.length === 0) {
        // An explicit sentence, never a blank box: the empty state is where
        // the pending residual is disclosed (`notesmodel.ts`).
        list.appendChild(el("div", "notes-empty", notesEmptyState(sessionId)));
        return;
      }
      for (const note of orderedNotes(notes)) {
        const row = el("div", "notes-row");
        const text = el("div", "notes-text", note.text);
        const when = el("div", "notes-when", new Date(note.created_ms).toLocaleString());
        const del = el("button", "notes-del", "×");
        del.title = "Delete this note";
        del.addEventListener("click", () => {
          void (async () => {
            const ok = await confirmModal(
              "Delete note?",
              note.text.length > 120 ? `${note.text.slice(0, 120)}…` : note.text,
              "Delete",
              true
            );
            if (!ok) return;
            if (sessionId === null) spec.store.deletePendingNote(paneKeyOf(spec.target), note.id);
            // A delete has no text to hand back, so `attempted` is empty — the
            // point here is the MESSAGE: a `failed` delete leaves the note on
            // disk, and a `declined-unread` one never happened at all.
            else reportWrite(await spec.store.deleteNote(sessionId, note.id, now()), "");
          })();
        });
        row.append(text, when, del);
        list.appendChild(row);
      }
    };

    const submit = (): void => {
      const text = input.value;
      if (noteDraftIsPristine(text)) return;
      // Cleared HERE, on the one path both the button and the Enter chord go
      // through, so the two routes cannot disagree about whether the box was
      // emptied (CLAUDE.md's in-list-editor rule: clearing on one route only is
      // the classic miss).
      input.value = "";
      drafts.clear(spec.target);
      reflectDraft();
      error.classList.remove("visible");
      void spec.store
        .addNote(spec.target, text, now())
        .then((outcome) => reportWrite(outcome, text));
    };

    input.addEventListener("input", reflectDraft);
    input.addEventListener("keydown", (e) => {
      // Never let a keystroke inside the dialog reach the terminal underneath.
      e.stopPropagation();
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        submit();
      }
      if (e.key === "Escape") close();
    });
    addBtn.addEventListener("click", submit);
    closeBtn.addEventListener("click", close);

    dlg.append(head, subtitle, list, input, error, actions);
    overlay.appendChild(dlg);
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) close();
    });
    overlay.addEventListener("keydown", (e) => e.stopPropagation());
    // Escape from anywhere in the dialog, including a focused button, matching
    // the rest of the kit. Capture phase, and removed on close.
    //
    // ONLY WHILE THIS IS THE TOP OVERLAY. The per-row delete opens a
    // `confirmModal`, which appends a SECOND `.launcher-overlay`; a capture
    // handler that fired regardless would close the notes dialog out from under
    // the confirm the human was answering, leaving an orphaned dialog over a
    // dismissed one. Document order is insertion order for appended children,
    // so "am I last" is exactly "am I on top".
    const onDocKey = (e: KeyboardEvent): void => {
      if (e.key !== "Escape" || !overlay.isConnected) return;
      const stack = document.querySelectorAll(".launcher-overlay.visible");
      if (stack[stack.length - 1] !== overlay) return;
      e.stopPropagation();
      close();
    };
    document.addEventListener("keydown", onDocKey, true);

    document.body.appendChild(overlay);
    // The store may not have read the file yet — the list would then honestly
    // show the empty state for a session that has notes. Render now for
    // responsiveness, then again once the read lands.
    renderList();
    reflectDraft();
    void spec.store.ensureLoaded().then(() => {
      if (!settled) renderList();
    });
    unsubscribe = spec.store.onChange(() => {
      if (!settled) renderList();
    });
    input.focus();
  });
}

/** The pane key a not-yet-identified target names. Split out so the two reads
 *  above cannot drift, and so the narrowing is stated once. */
function paneKeyOf(target: NoteTarget): string {
  return "paneKey" in target ? target.paneKey : "";
}

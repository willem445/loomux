// The human's notes about a harness session (#2116) — the pure rules, with no
// DOM and no IO. `sessionlog.ts` owns where a note is STORED; this owns what a
// note may BE and how a list of them reads.
//
// Split out rather than inlined because both ends need the same answers: the
// store validates on the way in, and the dialog (`notesdialog.ts`) has to say
// the same thing about an over-long note BEFORE the store refuses it. One
// module means the two can never disagree about the cap.

/** A note as stored. `unknown` carries per-note keys a FUTURE build wrote that
 *  this one cannot interpret, kept verbatim so a downgrade that deletes a
 *  sibling note does not silently strip them from the survivors. */
export interface SessionNote {
  id: string;
  text: string;
  created_ms: number;
  unknown: Record<string, unknown>;
}

/** Longest note text kept. Generous enough for a paragraph of context — this
 *  is a reminder to a human, not a document — and bounded because the whole
 *  log is one file republished on every write.
 *
 *  Measured in UTF-16 code units (`String.length`), which is what both the
 *  dialog's counter and this check read, so the number the human is shown is
 *  the number that is enforced. */
export const MAX_NOTE_LEN = 2000;

/** What a caller's raw note text becomes, or `null` when it is not a note at
 *  all. Trims first — a textarea submitted with a stray newline is empty, not
 *  a blank note — then truncates rather than refusing, so a paste of something
 *  long keeps its opening instead of vanishing. */
export function normalizeNoteText(raw: string): string | null {
  const trimmed = raw.trim();
  if (!trimmed) return null;
  return trimmed.length > MAX_NOTE_LEN ? trimmed.slice(0, MAX_NOTE_LEN) : trimmed;
}

/** Chronological, oldest first — the order the human wrote them, which is the
 *  order they read as a running log. Never mutates its input; ties keep input
 *  order (`sort` is stable since ES2019), so two notes added in the same
 *  millisecond stay in the order they were added. */
export function orderedNotes(notes: readonly SessionNote[]): SessionNote[] {
  return notes.slice().sort((a, b) => a.created_ms - b.created_ms);
}

/** What the notes list says when it has nothing to show.
 *
 *  Two genuinely different situations, so two sentences. A session orrerix has
 *  an id for will keep what the human writes; a pane whose id is not known yet
 *  will not, until it is learned — and that is a residual worth stating rather
 *  than hiding, because an app restart in between loses those notes
 *  (`doc/design/session-notes.md`). `null` for a session id means "we are not
 *  keyed on a session yet", never "there are no notes". */
export function notesEmptyState(sessionId: string | null): string {
  return sessionId === null
    ? "No notes yet. Notes on this pane live in this window only until orrerix learns its session id — they attach to the session then, and are lost if the app restarts first."
    : "No notes yet for this session.";
}

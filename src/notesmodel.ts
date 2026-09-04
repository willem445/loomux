// The human's notes about a harness session (#2116) — the pure rules, with no
// DOM and no IO. `sessionlog.ts` owns where a note is STORED; this owns what a
// note may BE and how a list of them reads.
//
// Split out rather than inlined because both ends need the same answers: the
// store validates on the way in, and the dialog (`notesdialog.ts`) has to say
// the same thing about an over-long note BEFORE the store refuses it. One
// module means the two can never disagree about the cap.

/** Where a note is being written: a known harness session, or a pane whose
 *  session id orrerix has not learned yet.
 *
 *  It lives here rather than in `sessionlog.ts` because it is a NOTES concept —
 *  the store, the dialog and the draft book all take one, and putting it in the
 *  store would make this module depend on the thing that depends on it. */
export type NoteTarget = { sessionId: string } | { paneKey: string };

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

/** Is this draft untouched — nothing the human would be sorry to lose?
 *
 *  ONE predicate, reading the ONE field the note editor has, and it is the same
 *  question `normalizeNoteText` answers on the way in: a draft is pristine
 *  exactly when it would not become a note. Written as its own function rather
 *  than as `!raw` at each site because there are three of them — seeding the
 *  editor, enabling the Add button, and pruning the draft map — and a
 *  whitespace-only box is pristine to the store while `!raw` calls it dirty
 *  (CLAUDE.md: the renderer's seed and the predicate's default are one question
 *  asked twice).
 *
 *  If the editor ever grows a second field, it is added HERE, not beside a
 *  caller. */
export function noteDraftIsPristine(raw: string): boolean {
  return normalizeNoteText(raw) === null;
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

/** A stable string for a target, so a draft survives a close and reopen and two
 *  targets can never share one. It is a MAP KEY and is never joined onto a
 *  path — the `s:` / `p:` prefixes exist so a session id and a pane key that
 *  happen to spell the same thing cannot collide, not to name a directory. */
export function targetKey(target: NoteTarget): string {
  return "sessionId" in target ? `s:${target.sessionId}` : `p:${target.paneKey}`;
}

/** Unsubmitted note text, per target.
 *
 *  THE EDITOR IS A VIEW OF THIS, NEVER THE OTHER WAY ROUND (CLAUDE.md's
 *  in-list-editor rule). The notes list re-renders on every store change — a
 *  note added on another surface, a re-key landing, a rename — and a textarea
 *  whose value is only read at submit loses whatever the human was half-way
 *  through. So the draft is written on every `input`, the editor is SEEDED from
 *  here on open, and the pristine question is asked by one predicate that reads
 *  every field the editor has.
 *
 *  A pristine draft is DELETED rather than stored as `""`, so `size` is the
 *  number of things the human is actually part-way through, and the book cannot
 *  grow one entry per session ever looked at. */
export class NoteDrafts {
  private drafts = new Map<string, string>();

  /** What the editor opens with. `""` for a target nothing is held for — which
   *  is the same thing as pristine, by construction. */
  get(target: NoteTarget): string {
    return this.drafts.get(targetKey(target)) ?? "";
  }

  /** Record what is in the editor right now. */
  set(target: NoteTarget, text: string): void {
    const key = targetKey(target);
    if (noteDraftIsPristine(text)) this.drafts.delete(key);
    else this.drafts.set(key, text);
  }

  /** Forget a target's draft outright — what a successful submit does. */
  clear(target: NoteTarget): void {
    this.drafts.delete(targetKey(target));
  }

  /** How many targets have something unsubmitted. */
  get size(): number {
    return this.drafts.size;
  }
}

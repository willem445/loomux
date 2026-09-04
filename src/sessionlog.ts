// `sessionlog.json` (#2116) — orrerix's own sidecar record of the harness
// sessions the human has touched: what the pane was called, which CLI ran it,
// where it ran, and the notes the human wrote about it.
//
// Pure encode/decode + validation, DOM-free and unit-tested, modelled on
// `boardprefs.ts` (#1270) line for line. `uistate.rs` stores the blob
// atomically as `sessionlog.json` and never parses it beyond "is it JSON at
// all"; this module owns the schema.
//
// ---------------------------------------------------------------------------
// What it is, and what it is not
// ---------------------------------------------------------------------------
//
// Each agent CLI already keeps its own session log — Claude Code's transcripts,
// Copilot's, OpenCode's store — and the sessions browser lists sessions by
// scanning them (`doc/design/session-index.md`). Those files are the harness's
// and orrerix NEVER writes them. This is the other half: the things orrerix
// knows and the harness does not — the name the human gave the pane, and what
// they wrote about the session.
//
// So losing this file loses notes and recorded pane names, and nothing else.
// The sessions list still enumerates every session from the harness stores; the
// rows just fall back to their transcript titles, which is the pre-#2116 list.
//
// It is a sibling of `settings.json` rather than a key inside it for the reason
// #887 gave for `sshprofiles.json` and #1270 for `boardprefs.json`: a
// multi-entry keyed structure with its own lifecycle does not belong in a flat
// bag of app-wide scalars. It is NOT in a group directory either — a basic
// (non-orchestration) pane has no group, and a note would then have two homes.
//
// The session id is a MAP KEY here and never a path — no `.join`, so hard
// constraint 6's surface is untouched by this file and by the two commands
// behind it.

import {
  normalizeNoteText,
  orderedNotes,
  type NoteTarget,
  type NoteWriteOutcome,
  type SessionNote,
} from "./notesmodel.ts";

export {
  MAX_NOTE_LEN,
  type NoteTarget,
  type NoteWriteOutcome,
  type SessionNote,
} from "./notesmodel.ts";

/** Schema version of the persisted blob. Bumped only when an existing key
 *  changes MEANING — a new per-record or per-note key is preserved verbatim by
 *  older builds (see `unknown` below), so adding one needs no bump and no
 *  migration. */
export const SESSION_LOG_VERSION = 1;

/** How many session records to keep. Keyed by session id, so without a cap this
 *  file grows forever: an orchestration group mints a session per delegate and
 *  a new one on every rejoin (`sessionfilter.ts`), so a machine that has run a
 *  few fleets would accumulate thousands of records for panes nobody named and
 *  nobody wrote about.
 *
 *  Five hundred is far past any plausible working set and still a small file.
 *  The cost of falling off the end is documented and deliberately asymmetric:
 *  see `evictionRank` — an unnoted record is only a remembered pane NAME (the
 *  row falls back to its transcript title, which is the pre-#2116 list), while
 *  a noted record is something the human WROTE and is recoverable from nowhere
 *  else. */
export const MAX_SESSIONS = 500;

/** One session's sidecar record. */
export interface SessionRecord {
  /** The agent CLI that owns the session, read off the pane's launch line and
   *  never branched on (#722/#841) — a fourth CLI is recorded correctly on
   *  arrival rather than joining whichever name sits in an else-branch. */
  cli: string;
  /** The name the human gave the pane, at the time of the last update. */
  pane_name: string;
  /** The pane's working directory — what makes a row identifiable when several
   *  sessions share a title. */
  cwd: string;
  created_ms: number;
  /** When this record last CHANGED. The eviction key, and deliberately not
   *  "when it was last seen": a boot that re-records twenty unchanged panes
   *  must not reshuffle the eviction order of the ones the human wrote on. */
  updated_ms: number;
  notes: SessionNote[];
  /** Per-record keys a FUTURE build wrote that this one cannot interpret, kept
   *  verbatim. Without this, opening an older build once would silently delete
   *  whatever a newer one had recorded. */
  unknown: Record<string, unknown>;
}

/** The decoded file: the records, plus whatever else was at the top level.
 *
 *  `unknownTop` is the same forward-compat promise as `SessionRecord.unknown`,
 *  one level up, and it is a field rather than a second return value so that
 *  `encodeSessionLog(decodeSessionLog(raw))` is a total round trip a test can
 *  assert in one line. */
export interface SessionLogData {
  /** A `Map` and not a plain object so a session id can never collide with
   *  `Object.prototype` (`toString` is a legal-looking key), and so iteration
   *  order is insertion order. */
  sessions: Map<string, SessionRecord>;
  unknownTop: Record<string, unknown>;
}

/** The identity fields a caller may record about a session. Deliberately not
 *  the whole `SessionRecord`: `notes` are added through `addNote`, and the two
 *  timestamps are the store's to stamp. */
export type SessionIdentity = Pick<SessionRecord, "cli" | "pane_name" | "cwd">;

/** An empty log — first run, an unreadable file, or a blob that is not a log. */
export function emptySessionLog(): SessionLogData {
  return { sessions: new Map(), unknownTop: {} };
}

/** Eviction rank: **0 for a record carrying notes, 1 for one that carries
 *  none.** Sorted ascending, that puts every noted record ahead of every
 *  unnoted one, so the cap sheds remembered pane names before it sheds a single
 *  thing the human wrote.
 *
 *  This is the whole of the two-tier rule, in one place, so the encoder and any
 *  test read the same function rather than two copies of a comparator. */
export function evictionRank(rec: SessionRecord): 0 | 1 {
  return rec.notes.length > 0 ? 0 : 1;
}

/** Serialize for `saveSessionLog`, keeping at most `MAX_SESSIONS` records:
 *  noted records first (`evictionRank`), then most-recently-updated first.
 *
 *  The eviction happens HERE, at the write, rather than at the read: a build
 *  that only ever loaded would let the file grow unbounded on disk however
 *  small the in-memory map was, and this is the one function every write goes
 *  through. Ties keep insertion order (`sort` is stable since ES2019). */
export function encodeSessionLog(data: SessionLogData): string {
  const kept = [...data.sessions.entries()]
    .sort((a, b) => evictionRank(a[1]) - evictionRank(b[1]) || b[1].updated_ms - a[1].updated_ms)
    .slice(0, MAX_SESSIONS);
  // `Object.create(null)`, not `{}` (#1270 review N3, the same hazard here). A
  // session id is caller-shaped text off a harness store, so `__proto__` is a
  // reachable key: assigning it on an ordinary object literal reaches the
  // setter on `Object.prototype` and creates no own property, so that one
  // record would be silently dropped at every save with no error anywhere.
  const sessions: Record<string, unknown> = Object.create(null);
  for (const [id, rec] of kept) {
    sessions[id] = {
      // Unknown keys first, so a future key can never shadow one this build
      // owns — a newer build writing `{"notes": "nonsense"}` into the unknown
      // bag must not be able to overwrite the validated `notes`.
      ...rec.unknown,
      cli: rec.cli,
      pane_name: rec.pane_name,
      cwd: rec.cwd,
      created_ms: rec.created_ms,
      updated_ms: rec.updated_ms,
      notes: rec.notes.map((n) => ({
        ...n.unknown,
        id: n.id,
        text: n.text,
        created_ms: n.created_ms,
      })),
    };
  }
  return JSON.stringify({ ...data.unknownTop, v: SESSION_LOG_VERSION, sessions }, null, 2);
}

/** Parse the persisted blob. Tolerant in exactly the way every other schema
 *  module here is: anything malformed at the top level yields an EMPTY log (the
 *  rows then fall back to their transcript titles), and a malformed individual
 *  record or field is dropped rather than invalidating the file — one hand-edit
 *  mistake must not cost every other session its notes.
 *
 *  A record whose `notes` is malformed decodes to an empty list rather than
 *  dropping the record: the pane name is still worth having, and dropping the
 *  record would take the `unknown` passthrough with it.
 *
 *  A future version number is read anyway rather than refused: every field is
 *  validated per-key regardless, so the worst a newer file can do is contribute
 *  keys this build ignores and preserves. */
export function decodeSessionLog(raw: string | null): SessionLogData {
  const out = emptySessionLog();
  if (!raw) return out;
  let v: unknown;
  try {
    v = JSON.parse(raw);
  } catch {
    return out;
  }
  if (!v || typeof v !== "object" || Array.isArray(v)) return out;
  const { v: _version, sessions, ...unknownTop } = v as Record<string, unknown>;
  out.unknownTop = unknownTop;
  if (!sessions || typeof sessions !== "object" || Array.isArray(sessions)) return out;
  for (const [id, entry] of Object.entries(sessions as Record<string, unknown>)) {
    if (!id || !entry || typeof entry !== "object" || Array.isArray(entry)) continue;
    const { cli, pane_name, cwd, created_ms, updated_ms, notes, ...unknown } = entry as Record<
      string,
      unknown
    >;
    out.sessions.set(id, {
      cli: str(cli),
      pane_name: str(pane_name),
      cwd: str(cwd),
      created_ms: num(created_ms),
      updated_ms: num(updated_ms),
      notes: decodeNotes(notes),
      unknown,
    });
  }
  return out;
}

function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

function num(v: unknown): number {
  return typeof v === "number" && Number.isFinite(v) ? v : 0;
}

/** Notes off a decoded record. A note with no usable id or no text is dropped —
 *  it can neither be rendered nor deleted, so keeping it would put an
 *  undeletable blank row in the human's list. */
function decodeNotes(v: unknown): SessionNote[] {
  if (!Array.isArray(v)) return [];
  const out: SessionNote[] = [];
  for (const entry of v) {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) continue;
    const { id, text, created_ms, ...unknown } = entry as Record<string, unknown>;
    if (typeof id !== "string" || !id) continue;
    if (typeof text !== "string" || !text) continue;
    out.push({ id, text, created_ms: num(created_ms), unknown });
  }
  return out;
}

/** A deep copy of a record, so a caller's edit cannot skip the store and never
 *  be saved (the `readGroupView` rule in `boardprefs.ts`). */
function cloneRecord(rec: SessionRecord): SessionRecord {
  return {
    cli: rec.cli,
    pane_name: rec.pane_name,
    cwd: rec.cwd,
    created_ms: rec.created_ms,
    updated_ms: rec.updated_ms,
    notes: rec.notes.map((n) => ({ ...n, unknown: { ...n.unknown } })),
    unknown: { ...rec.unknown },
  };
}

// ---------------------------------------------------------------------------
// The one ordering rule the schema cannot express (CLAUDE.md's multi-tenant
// whole-file store rule; #1270 review B1 is where it was first written down)
// ---------------------------------------------------------------------------

/** The two IPC calls the store needs plus its id source, injected so the
 *  ordering below is exercisable without a backend (`src/pty.ts` supplies the
 *  real pair). */
export interface SessionLogIo {
  load: () => Promise<string | null>;
  save: (contents: string) => Promise<void>;
  /** Mints a note id. Defaults to `crypto.randomUUID()` — minted in the
   *  WEBVIEW, so hard constraint 2 (no getrandom in the Rust binary) does not
   *  apply to it. Injected so tests are deterministic. */
  newId?: () => string;
}

/** What a mutation did.
 *
 *  `declined-unread` is the interesting one: the store refused to publish
 *  because it has never successfully read the file, so it does not know what it
 *  would be overwriting. `unchanged` means the call was a no-op (a re-record of
 *  identical identity, a delete of a note that is not there) and nothing was
 *  written — distinct from `saved` so a caller, and a test, can tell "nothing to
 *  do" from "done". `pending` means the note was held in memory against a pane
 *  with no session id yet. */
export type SessionLogWrite = NoteWriteOutcome;

/** Reads and writes the whole `sessionlog.json` blob, holding the invariant
 *  that makes one shared file safe for many sessions.
 *
 *  **A save publishes the WHOLE blob, so it must never run against a store
 *  nobody has read.** The store starts with an empty map and fills it when
 *  `load` resolves; a write that beats that — a human typing a note within the
 *  first moments of a cold start — would serialize the empty map as the entire
 *  file and silently destroy up to `MAX_SESSIONS` other sessions' notes. No
 *  error anywhere, because every individual step succeeded.
 *
 *  So every write awaits the read first, and a read that FAILED declines the
 *  write outright rather than treating "I could not look" as "there was nothing
 *  there". The failure is not latched: the next write retries the read, so one
 *  transient IPC rejection does not disable persistence for the session.
 *
 *  Every mutator also merges passthrough (`unknown`, `unknownTop`) off the
 *  record it just re-read rather than off anything a caller supplied — a caller
 *  cannot lose what it never carries. */
export class SessionLogStore {
  private data: SessionLogData = emptySessionLog();
  /** The blob has been read back at least once — including "the file is not
   *  there", which is a complete answer (an empty log), not a gap. */
  private isLoaded = false;
  /** The read in flight, shared by every concurrent caller so a burst of
   *  gestures cannot start several. Cleared on failure so a later call retries. */
  private reading: Promise<boolean> | null = null;
  /** Notes written against a pane whose session id is not known yet, keyed by
   *  `Pane.key`. In memory only and never persisted: there is nothing durable
   *  to key them to (see `rekey`, and the residual in
   *  `doc/design/session-notes.md`). */
  private pending = new Map<string, SessionNote[]>();
  private listeners = new Set<() => void>();
  /** Explicit fields, not constructor parameter properties: node's strip-only
   *  TypeScript (what `npm test` runs) rejects those outright, so the terser
   *  form would make this module untestable in this repo. */
  private readonly io: SessionLogIo;
  private readonly newId: () => string;

  constructor(io: SessionLogIo) {
    this.io = io;
    this.newId = io.newId ?? (() => crypto.randomUUID());
  }

  /** Whether the store now reflects the file. Never throws. */
  ensureLoaded(): Promise<boolean> {
    if (this.isLoaded) return Promise.resolve(true);
    if (!this.reading) {
      this.reading = this.io.load().then(
        (raw) => {
          this.data = decodeSessionLog(raw);
          this.isLoaded = true;
          this.reading = null;
          return true;
        },
        () => {
          this.reading = null;
          return false;
        }
      );
    }
    return this.reading;
  }

  /** True once the file has been read back at least once. A caller rendering a
   *  list uses this to tell "no notes" from "not read yet". */
  get loaded(): boolean {
    return this.isLoaded;
  }

  /** This session's record, or `null` when there is none — or when the file has
   *  not been read yet, which a caller must NOT collapse into "no notes"
   *  (`loaded` is what separates the two). A fresh copy the caller may
   *  mutate. */
  get(sessionId: string): SessionRecord | null {
    const found = this.data.sessions.get(sessionId);
    return found ? cloneRecord(found) : null;
  }

  /** Every record — what the sessions list joins against. Copies, for the same
   *  reason `get` does. */
  all(): Map<string, SessionRecord> {
    return new Map([...this.data.sessions.entries()].map(([id, r]) => [id, cloneRecord(r)]));
  }

  /** How many notes this session carries. Zero for an unknown session and for
   *  an unread store alike — a count is a number the UI puts on a chip, and
   *  there is no honest number to show for a file nobody has read. */
  notesCount(sessionId: string): number {
    return this.data.sessions.get(sessionId)?.notes.length ?? 0;
  }

  /** The recorded pane name, or `undefined` for an unknown session and for an
   *  unread store alike — the same honesty caveat `notesCount` carries, and
   *  `paneNameLine` renders no line for either.
   *
   *  A SCALAR READ, DELIBERATELY NOT `get(...)?.pane_name` (#2319 review round
   *  1). `get` returns `cloneRecord`, which allocates a fresh object per NOTE
   *  on the record; the sessions list reads this one string per row, on every
   *  render, and `render()` now runs on every store change rather than only on
   *  a refresh. Notes per record are uncapped, so that would be O(total notes
   *  across shown rows) short-lived objects per gesture to read a string that
   *  is immutable anyway. This is `notesCount`'s sibling in every respect. */
  paneName(sessionId: string): string | undefined {
    return this.data.sessions.get(sessionId)?.pane_name;
  }

  /** Notes held in memory against a pane with no session id yet, in the order
   *  they were written. Copies. */
  pendingFor(paneKey: string): SessionNote[] {
    return (this.pending.get(paneKey) ?? []).map((n) => ({ ...n, unknown: { ...n.unknown } }));
  }

  /** Subscribe to any change this store makes — the sessions list and the
   *  notes chips re-render off it. Returns an unsubscribe. */
  onChange(cb: () => void): () => void {
    this.listeners.add(cb);
    return () => {
      this.listeners.delete(cb);
    };
  }

  /** Notify subscribers. **A throwing listener must not cost the human a
   *  note** (#2116 review premortem 1): `emit` runs inside `publish`, before
   *  the save, so an exception escaping here would reject the whole write on a
   *  path the caller's `.then` cannot see — the note in memory, nothing on
   *  disk, and no message. A subscriber that throws has a bug of its own; it is
   *  isolated and reported, and every other subscriber still runs. */
  private emit(): void {
    for (const cb of [...this.listeners]) {
      try {
        cb();
      } catch (e) {
        console.error("sessionlog: an onChange listener threw", e);
      }
    }
  }

  /** Upsert this session's identity.
   *
   *  `updated_ms` is stamped only when a field actually CHANGED, and an
   *  unchanged re-record does not write at all: a boot that re-records every
   *  restored pane must not rewrite the file, and must not reshuffle the
   *  eviction order of records the human wrote on. */
  async record(
    sessionId: string,
    identity: SessionIdentity,
    nowMs: number
  ): Promise<SessionLogWrite> {
    if (!sessionId) return "unchanged";
    if (!(await this.ensureLoaded())) return "declined-unread";
    const existing = this.data.sessions.get(sessionId);
    if (
      existing &&
      existing.cli === identity.cli &&
      existing.pane_name === identity.pane_name &&
      existing.cwd === identity.cwd
    ) {
      return "unchanged";
    }
    this.data.sessions.set(sessionId, {
      cli: identity.cli,
      pane_name: identity.pane_name,
      cwd: identity.cwd,
      created_ms: existing?.created_ms || nowMs,
      updated_ms: nowMs,
      notes: existing?.notes ?? [],
      unknown: existing?.unknown ?? {},
    });
    return this.publish();
  }

  /** Add a note. Against a `sessionId` it is durable; against a `paneKey` it is
   *  held in memory until `rekey` attaches it to a learned session id.
   *
   *  Refuses empty (after trimming) and truncates at `MAX_NOTE_LEN`
   *  (`notesmodel.ts` owns both rules, so the dialog can say the same thing
   *  before the store is asked). */
  async addNote(target: NoteTarget, text: string, nowMs: number): Promise<SessionLogWrite> {
    const clean = normalizeNoteText(text);
    if (clean === null) return "unchanged";
    const note: SessionNote = { id: this.newId(), text: clean, created_ms: nowMs, unknown: {} };
    if ("paneKey" in target) {
      const held = this.pending.get(target.paneKey) ?? [];
      this.pending.set(target.paneKey, [...held, note]);
      this.emit();
      return "pending";
    }
    if (!(await this.ensureLoaded())) return "declined-unread";
    const rec = this.data.sessions.get(target.sessionId);
    this.data.sessions.set(target.sessionId, {
      cli: rec?.cli ?? "",
      pane_name: rec?.pane_name ?? "",
      cwd: rec?.cwd ?? "",
      created_ms: rec?.created_ms || nowMs,
      updated_ms: nowMs,
      notes: [...(rec?.notes ?? []), note],
      unknown: rec?.unknown ?? {},
    });
    return this.publish();
  }

  /** Delete one note by id. Deleting the last note KEEPS the record: the pane
   *  name is still worth having, and dropping it would take the `unknown`
   *  passthrough with it. */
  async deleteNote(sessionId: string, noteId: string, nowMs: number): Promise<SessionLogWrite> {
    if (!(await this.ensureLoaded())) return "declined-unread";
    const rec = this.data.sessions.get(sessionId);
    if (!rec) return "unchanged";
    const notes = rec.notes.filter((n) => n.id !== noteId);
    if (notes.length === rec.notes.length) return "unchanged";
    this.data.sessions.set(sessionId, { ...rec, notes, updated_ms: nowMs });
    return this.publish();
  }

  /** Delete a pending note, before there is anything durable to key it to. */
  deletePendingNote(paneKey: string, noteId: string): SessionLogWrite {
    const held = this.pending.get(paneKey);
    if (!held) return "unchanged";
    const kept = held.filter((n) => n.id !== noteId);
    if (kept.length === held.length) return "unchanged";
    if (kept.length === 0) this.pending.delete(paneKey);
    else this.pending.set(paneKey, kept);
    this.emit();
    return "pending";
  }

  /** Attach a pane's pending notes to the session id orrerix has just learned
   *  (`Pane.adoptSessionId` — the single choke point every late-learned id
   *  passes through, `doc/design/session-id-learning.md`).
   *
   *  Appends onto whatever the record already carries — a RESUMED session
   *  already has notes, and they must not be lost or reordered — and writes
   *  ONCE. Clears the pending list only after the write is accepted, so a
   *  `declined-unread` leaves the notes where a later attempt can still find
   *  them; a second `rekey` for the same pane is then a no-op with nothing
   *  pending, which is what makes the call safe from a site that may fire more
   *  than once. */
  async rekey(paneKey: string, sessionId: string, nowMs: number): Promise<SessionLogWrite> {
    const held = this.pending.get(paneKey) ?? [];
    if (held.length === 0) return "unchanged";
    if (!sessionId) return "unchanged";
    if (!(await this.ensureLoaded())) return "declined-unread";
    const rec = this.data.sessions.get(sessionId);
    this.data.sessions.set(sessionId, {
      cli: rec?.cli ?? "",
      pane_name: rec?.pane_name ?? "",
      cwd: rec?.cwd ?? "",
      created_ms: rec?.created_ms || nowMs,
      updated_ms: nowMs,
      notes: orderedNotes([...(rec?.notes ?? []), ...held]),
      unknown: rec?.unknown ?? {},
    });
    this.pending.delete(paneKey);
    return this.publish();
  }

  /** Serialize and save the whole blob. Only ever reached from a mutator that
   *  has already awaited `ensureLoaded`. */
  private async publish(): Promise<SessionLogWrite> {
    this.emit();
    try {
      await this.io.save(encodeSessionLog(this.data));
      return "saved";
    } catch {
      // Best-effort, the `persistTabs` contract: the in-memory store keeps the
      // newer value, so the next gesture re-offers it.
      return "failed";
    }
  }
}

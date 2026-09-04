// Unit tests for orrerix's own sessions log (#2116) — src/sessionlog.ts.
// Run with `npm test`.
//
// The intent under test is the one CLAUDE.md states for a multi-tenant
// whole-file store: a save publishes the WHOLE blob, so it must never run
// against a store nobody has read, and "I could not look" is not "there was
// nothing there".
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  MAX_SESSIONS,
  SESSION_LOG_VERSION,
  SessionLogStore,
  decodeSessionLog,
  emptySessionLog,
  encodeSessionLog,
  evictionRank,
  type SessionLogData,
  type SessionLogIo,
  type SessionRecord,
} from "../src/sessionlog.ts";

const rec = (over: Partial<SessionRecord> = {}): SessionRecord => ({
  cli: "claude",
  pane_name: "worker",
  cwd: "C:/Projects/loomux",
  created_ms: 1000,
  updated_ms: 1000,
  notes: [],
  unknown: {},
  ...over,
});

const dataWith = (entries: [string, SessionRecord][]): SessionLogData => ({
  sessions: new Map(entries),
  unknownTop: {},
});

// ---------------------------------------------------------------------------
// Schema: round trip, passthrough, tolerance
// ---------------------------------------------------------------------------

test("a record survives an encode/decode round trip", () => {
  const before = dataWith([
    [
      "sess-1",
      rec({
        notes: [{ id: "n1", text: "waiting on CI", created_ms: 1200, unknown: {} }],
        updated_ms: 1200,
      }),
    ],
  ]);
  const after = decodeSessionLog(encodeSessionLog(before));
  assert.deepEqual(after.sessions.get("sess-1"), before.sessions.get("sess-1"));
});

test("the encoded blob carries the schema version", () => {
  const parsed = JSON.parse(encodeSessionLog(emptySessionLog()));
  assert.equal(parsed.v, SESSION_LOG_VERSION);
});

test("an unknown TOP-LEVEL key written by a newer build survives a round trip", () => {
  const raw = JSON.stringify({
    v: 1,
    futureTopLevel: { anything: true },
    sessions: { "sess-1": { cli: "claude", pane_name: "w", cwd: "/x", notes: [] } },
  });
  const parsed = JSON.parse(encodeSessionLog(decodeSessionLog(raw)));
  assert.deepEqual(parsed.futureTopLevel, { anything: true });
});

test("an unknown PER-RECORD key written by a newer build survives a round trip", () => {
  const raw = JSON.stringify({
    v: 1,
    sessions: {
      "sess-1": { cli: "claude", pane_name: "w", cwd: "/x", notes: [], futureField: [1, 2] },
    },
  });
  const parsed = JSON.parse(encodeSessionLog(decodeSessionLog(raw)));
  assert.deepEqual(parsed.sessions["sess-1"].futureField, [1, 2]);
});

test("an unknown key cannot shadow one this build owns", () => {
  // The unknown bag is spread FIRST, so a newer build writing junk under a
  // known name cannot overwrite the validated value.
  const data = dataWith([["sess-1", rec({ unknown: { cli: "NONSENSE", notes: "NONSENSE" } })]]);
  const parsed = JSON.parse(encodeSessionLog(data));
  assert.equal(parsed.sessions["sess-1"].cli, "claude");
  assert.deepEqual(parsed.sessions["sess-1"].notes, []);
});

test("a session id of exactly __proto__ round-trips instead of vanishing", () => {
  // `Object.create(null)` in the encoder. On an ordinary object literal
  // `sessions["__proto__"] = …` reaches the setter on `Object.prototype` and
  // creates no own property, so this one record would be dropped at every save
  // with no error anywhere. A session id is caller-shaped text off a harness
  // store, so this key is reachable.
  const data = dataWith([["__proto__", rec({ pane_name: "odd but legal" })]]);
  const parsed = JSON.parse(encodeSessionLog(data));
  assert.equal(
    Object.prototype.hasOwnProperty.call(parsed.sessions, "__proto__"),
    true,
    "the record must be an OWN property of the encoded map"
  );
  const back = decodeSessionLog(encodeSessionLog(data));
  assert.equal(back.sessions.get("__proto__")?.pane_name, "odd but legal");
});

test("a record whose notes are malformed decodes to an empty list, keeping the record", () => {
  const raw = JSON.stringify({
    v: 1,
    sessions: { "sess-1": { cli: "claude", pane_name: "kept", cwd: "/x", notes: "not-an-array" } },
  });
  const back = decodeSessionLog(raw);
  assert.equal(back.sessions.get("sess-1")?.pane_name, "kept");
  assert.deepEqual(back.sessions.get("sess-1")?.notes, []);
});

test("a note with no id or no text is dropped, and its siblings are not", () => {
  const raw = JSON.stringify({
    v: 1,
    sessions: {
      "sess-1": {
        cli: "claude",
        pane_name: "w",
        cwd: "/x",
        notes: [
          { id: "good", text: "keep me", created_ms: 5 },
          { id: "", text: "no id" },
          { id: "no-text", text: "" },
          "not an object",
        ],
      },
    },
  });
  const notes = decodeSessionLog(raw).sessions.get("sess-1")?.notes ?? [];
  assert.deepEqual(
    notes.map((n) => n.id),
    ["good"]
  );
});

test("a blob that is not a log at all decodes to an empty log rather than throwing", () => {
  for (const raw of [null, "", "not json", "[1,2,3]", '"a string"', "{}"]) {
    assert.equal(decodeSessionLog(raw).sessions.size, 0, `raw: ${String(raw)}`);
  }
});

// ---------------------------------------------------------------------------
// The cap, and its two tiers
// ---------------------------------------------------------------------------

test("a noted record outlives MAX_SESSIONS newer records that carry no notes", () => {
  // The asymmetry the cap exists to hold: an unnoted record is a remembered
  // pane NAME (the row falls back to its transcript title), a noted one is
  // something the human wrote and cannot be recovered from anywhere.
  const entries: [string, SessionRecord][] = [
    ["noted", rec({ updated_ms: 1, notes: [{ id: "n", text: "keep", created_ms: 1, unknown: {} }] })],
  ];
  for (let i = 0; i < MAX_SESSIONS + 20; i++) {
    entries.push([`plain-${i}`, rec({ updated_ms: 10_000 + i })]);
  }
  const parsed = JSON.parse(encodeSessionLog(dataWith(entries)));
  const keys = Object.keys(parsed.sessions);
  assert.equal(keys.length, MAX_SESSIONS);
  assert.ok(keys.includes("noted"), "the one noted record must survive every newer unnoted one");
});

test("within a tier the cap keeps the most recently updated", () => {
  const entries: [string, SessionRecord][] = [];
  for (let i = 0; i < MAX_SESSIONS + 2; i++) entries.push([`s-${i}`, rec({ updated_ms: i })]);
  const keys = Object.keys(JSON.parse(encodeSessionLog(dataWith(entries))).sessions);
  assert.equal(keys.length, MAX_SESSIONS);
  assert.ok(!keys.includes("s-0"), "the oldest must be the one evicted");
  assert.ok(keys.includes(`s-${MAX_SESSIONS + 1}`), "the newest must be kept");
});

test("the eviction rank is the whole of the two-tier rule", () => {
  assert.equal(evictionRank(rec({ notes: [{ id: "n", text: "t", created_ms: 0, unknown: {} }] })), 0);
  assert.equal(evictionRank(rec()), 1);
});

// ---------------------------------------------------------------------------
// The store: the read-before-write invariant
// ---------------------------------------------------------------------------

/** A fake backend. `load` resolves with whatever `raw` holds unless `failLoad`
 *  is set; `save` records every published blob. */
class FakeIo implements SessionLogIo {
  raw: string | null = null;
  failLoad = false;
  failSave = false;
  saved: string[] = [];
  loads = 0;
  private pendingLoad: ((v: string | null) => void) | null = null;
  private pendingReject: ((e: unknown) => void) | null = null;
  /** When true, `load` parks until `settleLoad()` — how a write is made to
   *  arrive BEFORE the read has resolved, deterministically. */
  manual = false;
  private ids = 0;

  load = (): Promise<string | null> => {
    this.loads += 1;
    if (this.manual) {
      return new Promise<string | null>((resolve, reject) => {
        this.pendingLoad = resolve;
        this.pendingReject = reject;
      });
    }
    return this.failLoad ? Promise.reject(new Error("load failed")) : Promise.resolve(this.raw);
  };

  save = (contents: string): Promise<void> => {
    if (this.failSave) return Promise.reject(new Error("save failed"));
    this.saved.push(contents);
    return Promise.resolve();
  };

  newId = (): string => `note-${++this.ids}`;

  settleLoad(): void {
    const resolve = this.pendingLoad;
    this.pendingLoad = null;
    this.manual = false;
    resolve?.(this.raw);
  }

  rejectLoad(): void {
    const reject = this.pendingReject;
    this.pendingReject = null;
    this.manual = false;
    reject?.(new Error("load failed"));
  }

  /** The last blob published, decoded. */
  last(): SessionLogData {
    return decodeSessionLog(this.saved[this.saved.length - 1] ?? null);
  }
}

test("a write that beats the load waits for it, and never publishes an unread store", async () => {
  // THE defect this store exists to prevent: an empty in-memory map serialized
  // as the whole file, silently destroying every other session's notes. Every
  // individual step succeeds, so nothing anywhere reports it.
  const io = new FakeIo();
  io.raw = JSON.stringify({
    v: 1,
    sessions: {
      other: { cli: "copilot", pane_name: "someone else", cwd: "/y", notes: [{ id: "x", text: "theirs", created_ms: 1 }] },
    },
  });
  io.manual = true;
  const store = new SessionLogStore(io);
  const write = store.addNote({ sessionId: "mine" }, "mine", 500);
  assert.equal(io.saved.length, 0, "nothing may be published before the read resolves");
  io.settleLoad();
  assert.equal(await write, "saved");
  const published = io.last();
  assert.equal(published.sessions.get("other")?.notes.length, 1, "the other tenant's note survived");
  assert.equal(published.sessions.get("mine")?.notes.length, 1);
});

test("a failed read DECLINES the write rather than treating it as an empty file", async () => {
  const io = new FakeIo();
  io.failLoad = true;
  const store = new SessionLogStore(io);
  assert.equal(await store.addNote({ sessionId: "s" }, "note", 1), "declined-unread");
  assert.equal(io.saved.length, 0, "a declined write must publish nothing at all");
});

test("a failed read is NOT latched — the next write retries it", async () => {
  const io = new FakeIo();
  io.failLoad = true;
  const store = new SessionLogStore(io);
  assert.equal(await store.addNote({ sessionId: "s" }, "first", 1), "declined-unread");
  io.failLoad = false;
  io.raw = null;
  assert.equal(await store.addNote({ sessionId: "s" }, "second", 2), "saved");
  assert.equal(io.loads, 2, "the second write must have re-read the file");
});

test("concurrent first writes share ONE read", async () => {
  const io = new FakeIo();
  const store = new SessionLogStore(io);
  await Promise.all([
    store.addNote({ sessionId: "a" }, "one", 1),
    store.addNote({ sessionId: "b" }, "two", 2),
  ]);
  assert.equal(io.loads, 1);
});

test("a failed SAVE keeps the newer value in memory so the next gesture re-offers it", async () => {
  const io = new FakeIo();
  const store = new SessionLogStore(io);
  io.failSave = true;
  assert.equal(await store.addNote({ sessionId: "s" }, "lost to disk", 1), "failed");
  assert.equal(store.notesCount("s"), 1, "the in-memory store keeps it");
  io.failSave = false;
  assert.equal(await store.addNote({ sessionId: "s" }, "second", 2), "saved");
  assert.equal(io.last().sessions.get("s")?.notes.length, 2, "both notes reach disk");
});

// ---------------------------------------------------------------------------
// record / addNote / deleteNote
// ---------------------------------------------------------------------------

test("re-recording identical identity writes nothing at all", async () => {
  // A boot that re-records twenty restored panes must not rewrite the file,
  // and must not reshuffle the eviction order of records the human wrote on.
  const io = new FakeIo();
  const store = new SessionLogStore(io);
  const identity = { cli: "claude", pane_name: "w-1", cwd: "/x" };
  assert.equal(await store.record("s", identity, 100), "saved");
  assert.equal(await store.record("s", identity, 900), "unchanged");
  assert.equal(io.saved.length, 1);
  assert.equal(store.get("s")?.updated_ms, 100, "updated_ms must not move on a no-op re-record");
});

test("a rename re-records and stamps updated_ms, keeping created_ms and the notes", async () => {
  const io = new FakeIo();
  const store = new SessionLogStore(io);
  await store.record("s", { cli: "claude", pane_name: "old", cwd: "/x" }, 100);
  await store.addNote({ sessionId: "s" }, "keep me", 150);
  assert.equal(await store.record("s", { cli: "claude", pane_name: "new", cwd: "/x" }, 900), "saved");
  const back = io.last().sessions.get("s");
  assert.equal(back?.pane_name, "new");
  assert.equal(back?.created_ms, 100);
  assert.equal(back?.updated_ms, 900);
  assert.equal(back?.notes.length, 1, "a rename must not drop the notes");
});

test("deleting the last note keeps the record and its pane name", async () => {
  const io = new FakeIo();
  const store = new SessionLogStore(io);
  await store.record("s", { cli: "claude", pane_name: "named", cwd: "/x" }, 1);
  await store.addNote({ sessionId: "s" }, "only note", 2);
  const only = store.get("s")!.notes[0].id;
  assert.equal(await store.deleteNote("s", only, 3), "saved");
  assert.equal(store.get("s")?.pane_name, "named");
  assert.equal(store.notesCount("s"), 0);
});

test("an unknown future field on a note survives the delete of a SIBLING note", async () => {
  const io = new FakeIo();
  io.raw = JSON.stringify({
    v: 1,
    sessions: {
      s: {
        cli: "claude",
        pane_name: "w",
        cwd: "/x",
        notes: [
          { id: "doomed", text: "delete me", created_ms: 1 },
          { id: "survivor", text: "keep me", created_ms: 2, pinnedByFutureBuild: true },
        ],
      },
    },
  });
  const store = new SessionLogStore(io);
  assert.equal(await store.deleteNote("s", "doomed", 9), "saved");
  const parsed = JSON.parse(io.saved[io.saved.length - 1]!);
  assert.equal(parsed.sessions.s.notes.length, 1);
  assert.equal(parsed.sessions.s.notes[0].pinnedByFutureBuild, true);
});

test("deleting a note that is not there changes nothing and writes nothing", async () => {
  const io = new FakeIo();
  const store = new SessionLogStore(io);
  await store.record("s", { cli: "claude", pane_name: "w", cwd: "/x" }, 1);
  const saves = io.saved.length;
  assert.equal(await store.deleteNote("s", "no-such-note", 2), "unchanged");
  assert.equal(io.saved.length, saves);
});

test("an empty note is refused before anything is written", async () => {
  const io = new FakeIo();
  const store = new SessionLogStore(io);
  assert.equal(await store.addNote({ sessionId: "s" }, "   \n ", 1), "unchanged");
  assert.equal(io.saved.length, 0);
  assert.equal(io.loads, 0, "a refused note must not even read the file");
});

test("get and all hand out copies, so a caller's edit cannot skip the store", async () => {
  const io = new FakeIo();
  const store = new SessionLogStore(io);
  await store.addNote({ sessionId: "s" }, "original", 1);
  const copy = store.get("s")!;
  copy.notes[0].text = "tampered";
  copy.pane_name = "tampered";
  assert.equal(store.get("s")?.notes[0].text, "original");
  const all = store.all();
  all.get("s")!.pane_name = "tampered too";
  assert.equal(store.get("s")?.pane_name, "");
});

// ---------------------------------------------------------------------------
// Pending notes and the re-key (doc/design/session-id-learning.md)
// ---------------------------------------------------------------------------

test("a note on a pane with no session id yet is held in memory, not published", async () => {
  const io = new FakeIo();
  const store = new SessionLogStore(io);
  assert.equal(await store.addNote({ paneKey: "pane-3" }, "before the id", 1), "pending");
  assert.equal(io.saved.length, 0);
  assert.equal(store.pendingFor("pane-3").length, 1);
});

test("rekey moves pending notes onto the learned session id, exactly once", async () => {
  const io = new FakeIo();
  const store = new SessionLogStore(io);
  await store.addNote({ paneKey: "pane-3" }, "first", 1);
  await store.addNote({ paneKey: "pane-3" }, "second", 2);
  assert.equal(await store.rekey("pane-3", "sess-9", 10), "saved");
  assert.deepEqual(
    io.last().sessions.get("sess-9")?.notes.map((n) => n.text),
    ["first", "second"],
    "in the order they were written"
  );
  assert.equal(store.pendingFor("pane-3").length, 0);
  const saves = io.saved.length;
  assert.equal(await store.rekey("pane-3", "sess-9", 20), "unchanged", "a second rekey is a no-op");
  assert.equal(io.saved.length, saves, "and writes nothing");
});

test("rekey APPENDS onto a resumed session's existing notes rather than replacing them", async () => {
  const io = new FakeIo();
  io.raw = JSON.stringify({
    v: 1,
    sessions: {
      "sess-9": {
        cli: "claude",
        pane_name: "from last time",
        cwd: "/x",
        notes: [{ id: "old", text: "written last session", created_ms: 1 }],
      },
    },
  });
  const store = new SessionLogStore(io);
  await store.addNote({ paneKey: "pane-3" }, "written this session", 500);
  assert.equal(await store.rekey("pane-3", "sess-9", 600), "saved");
  assert.deepEqual(
    io.last().sessions.get("sess-9")?.notes.map((n) => n.text),
    ["written last session", "written this session"]
  );
  assert.equal(
    io.last().sessions.get("sess-9")?.pane_name,
    "from last time",
    "the re-key must not blank the identity fields it did not set"
  );
});

test("a rekey whose read failed keeps the notes pending for a later attempt", async () => {
  // "I could not look" must not cost the human the notes: declining and
  // KEEPING them is what makes the next attempt able to succeed.
  const io = new FakeIo();
  const store = new SessionLogStore(io);
  await store.addNote({ paneKey: "pane-3" }, "fragile", 1);
  io.failLoad = true;
  assert.equal(await store.rekey("pane-3", "sess-9", 2), "declined-unread");
  assert.equal(store.pendingFor("pane-3").length, 1, "still pending, not lost");
  io.failLoad = false;
  assert.equal(await store.rekey("pane-3", "sess-9", 3), "saved");
  assert.equal(io.last().sessions.get("sess-9")?.notes.length, 1);
});

test("a pending note can be deleted before there is anything to key it to", async () => {
  const io = new FakeIo();
  const store = new SessionLogStore(io);
  await store.addNote({ paneKey: "pane-3" }, "typo", 1);
  const id = store.pendingFor("pane-3")[0].id;
  assert.equal(store.deletePendingNote("pane-3", id), "pending");
  assert.equal(store.pendingFor("pane-3").length, 0);
  assert.equal(await store.rekey("pane-3", "sess-9", 2), "unchanged");
});

test("two panes' pending notes do not mix", async () => {
  const io = new FakeIo();
  const store = new SessionLogStore(io);
  await store.addNote({ paneKey: "pane-1" }, "mine", 1);
  await store.addNote({ paneKey: "pane-2" }, "theirs", 2);
  await store.rekey("pane-1", "sess-a", 3);
  assert.deepEqual(
    io.last().sessions.get("sess-a")?.notes.map((n) => n.text),
    ["mine"]
  );
  assert.equal(store.pendingFor("pane-2").length, 1);
});

test("a note added AFTER a rekey lands on the session, not back in pending", async () => {
  // #2116 review B1, at the store level: this is what the dialog's live target
  // must produce. The sequence is a copilot pane — note written while the id is
  // unknown, id learned, then a SECOND note. Before the fix the dialog kept
  // aiming at the pane key, so this second note was filed pending against a
  // pane `rekey` will never fire for again (`adoptSessionId` refuses a second
  // adoption): on no disk, in no record, gone on restart, silently.
  const io = new FakeIo();
  const store = new SessionLogStore(io);
  await store.addNote({ paneKey: "pane-3" }, "note A", 1);
  assert.equal(await store.rekey("pane-3", "sess-9", 2), "saved");
  assert.equal(store.pendingFor("pane-3").length, 0, "rekey empties the pending list");

  // What a LIVE target does: re-read, see the id, write to the session.
  assert.equal(await store.addNote({ sessionId: "sess-9" }, "note B", 3), "saved");
  assert.deepEqual(
    io.last().sessions.get("sess-9")?.notes.map((n) => n.text),
    ["note A", "note B"],
    "both notes are on disk, in order"
  );

  // And the negative control — what a FROZEN target did — so this test cannot
  // pass by the store having changed shape. The note goes nowhere durable.
  const saves = io.saved.length;
  assert.equal(await store.addNote({ paneKey: "pane-3" }, "note C", 4), "pending");
  assert.equal(io.saved.length, saves, "a stale pane-keyed write publishes nothing");
  assert.equal(
    io.last().sessions.get("sess-9")?.notes.length,
    2,
    "and never reaches the session record"
  );
});

test("an onChange listener that throws cannot cost the human the note", async () => {
  // #2116 review premortem 1. `emit()` runs inside `publish()`, BEFORE the
  // save, so an exception escaping it would reject the whole write on a path
  // the caller's `.then` cannot see: note in memory, nothing on disk, no
  // message. A subscriber with a bug is isolated; every other subscriber and
  // the save itself still run.
  const io = new FakeIo();
  const store = new SessionLogStore(io);
  let secondRan = 0;
  store.onChange(() => {
    throw new Error("a buggy subscriber");
  });
  store.onChange(() => {
    secondRan += 1;
  });
  assert.equal(await store.addNote({ sessionId: "s" }, "survives", 1), "saved");
  assert.equal(io.last().sessions.get("s")?.notes.length, 1, "the note reached disk");
  assert.ok(secondRan > 0, "the listener after the throwing one still ran");
});

// ---------------------------------------------------------------------------
// onChange
// ---------------------------------------------------------------------------

test("every mutation notifies subscribers, and unsubscribing stops it", async () => {
  const io = new FakeIo();
  const store = new SessionLogStore(io);
  let fired = 0;
  const off = store.onChange(() => {
    fired += 1;
  });
  await store.addNote({ sessionId: "s" }, "durable", 1);
  await store.addNote({ paneKey: "p" }, "pending", 2);
  assert.equal(fired, 2, "a pending note changes what the dialog shows, so it fires too");
  off();
  await store.addNote({ sessionId: "s" }, "after", 3);
  assert.equal(fired, 2);
});

test("loaded tells a caller apart from a store with genuinely nothing in it", async () => {
  const io = new FakeIo();
  const store = new SessionLogStore(io);
  assert.equal(store.loaded, false);
  assert.equal(store.notesCount("s"), 0, "an unread store reports no notes…");
  await store.ensureLoaded();
  assert.equal(store.loaded, true, "…and `loaded` is what separates that from an empty file");
});

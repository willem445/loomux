// Pure, DOM-free core of the per-pty event routers (#1301). The Tauri wiring
// lives in pty.ts (`ensureOutputRouter`/`attachOutput`/`detachOutput` and the
// git-watch pair); everything that DECIDES what is kept, dropped or released
// is here so it is unit-testable under `node --test` — the same split as
// panethrottle.ts, which owns the *rate* half of the same subsystem.
//
// What this is for
// ----------------
// One backend event stream (`pty-output`, `git-changed`) carries every pane's
// traffic, demultiplexed frontend-side by pty id. That means a MODULE-LEVEL
// map from pty id to a handler closure, and a handler closure for a pane
// captures the pane — its `Terminal`, its xterm buffer (`scrollback` lines ×
// cols × 12 B, so tens of MB for a pane an agent has been printing into for
// hours), its DOM subtree and every view hanging off it. An entry nobody
// removes is therefore not a leaked number. It is a leaked pane, and a
// many-hour orchestration session that spawns and kills ~40 of them leaks
// them by the hundred-MB (#1301).
//
// Two things went wrong under the old flat `Map<number, Handler>`, and this
// module exists to make both structurally impossible rather than remembered:
//
//   1. ATTACHING TWICE. A pane that respawns in place (`Pane.respawnFresh` —
//      #194 BUG-1's dead-`--resume` backstop, #887 S4's ssh reconnect, and
//      #407's promotion) gets a NEW pty id and attaches under it, while its
//      OLD id's entry stays. The pane is then permanently reachable from the
//      router even after `Pane.dispose()`, because dispose only knows the
//      CURRENT id. Fixed by keying on the OWNER as well: one live attachment
//      per owner, so attaching an owner to a new id releases whatever it held
//      before. Nothing has to remember to detach first, which is the only
//      kind of fix that survives the next call site.
//
//   2. HOLDING FOREVER. Output that arrives for an id with no handler is held
//      so a pane can never lose its process's first bytes to a listen/spawn
//      race. That buffer had no bound and no expiry, and — because `release`
//      only deleted the queue rather than retiring the id — a byte arriving
//      one tick after a pane was torn down opened a fresh one that nothing
//      would ever collect. Bytes for an id that will never attach (a pane
//      whose creation timed out, which is #1301's own presenting symptom, or
//      one disposed while its kill was still in flight) accumulated for the
//      life of the process.
//
// Why an id can be retired for good: pty ids are minted by
// `AtomicU32::fetch_add` (`src-tauri/src/pty.rs`, `spawn_pty`), so they are
// monotonic and never reused within a run. An id this router has released has
// no second life, and dropping its late bytes cannot cost a future pane
// anything.
//
// Only the OUTPUT router holds anything. A `git-changed` event is a signal,
// not a payload — a missed one costs a refresh, never data — so the git-watch
// router uses `attach`/`handler`/`release` and never calls `hold`.

/** A pane's identity as far as the routers are concerned: the `Pane` object
 *  itself at the call site, anything with reference identity in a test.
 *  Deliberately not a pty id — the whole point of rule 1 above is that the
 *  owner outlives any one of its pty ids. */
export type RouteOwner = object;

/** Per-id ceiling on output held for a handler that has not attached yet.
 *
 *  The buffer exists to bridge `spawn_pty` resolving and `attachOutput` being
 *  called, which is one `await` — a shell banner and a prompt, kilobytes.
 *  256 KiB is orders above that, so a healthy attach never approaches THIS cap.
 *  It bites only where there IS no attach coming, which is the case that used
 *  to be unbounded.
 *
 *  Scope, because "lossless startup" is easy to over-claim: this cap alone does
 *  not weaken it. `MAX_PREATTACH_IDS` does, and says so — see its own doc.
 *
 *  Overflow sheds from the FRONT, keeping the tail: a terminal's screen is
 *  determined by its most recent bytes, so if some of a 256 KiB backlog has
 *  to go, the honest thing to lose is the oldest of it. */
export const MAX_PREATTACH_BYTES = 256 * 1024;

/** Per-id ceiling on how many CHUNKS may be held, independent of their size.
 *
 *  The third cap INV-8(a) asks for, and it is not redundant with the byte cap:
 *  the byte cap bounds the PAYLOAD, while each held chunk also costs a
 *  `{chunk, bytes}` wrapper plus a `Uint8Array` header whose size has nothing
 *  to do with how many bytes it carries. Measured on this repo's Node
 *  (`node --expose-gc`, 200k entries of `{chunk: new Uint8Array(1), bytes: 1}`,
 *  heapUsed delta across forced GCs, three runs): **274 B per entry**. So a
 *  drip of 1-byte writes fills 256 KiB only after ~262k chunks and sheds
 *  nothing on the way, for ~72 MB of wrapper heap against a cap that says
 *  256 KiB.
 *
 *  Reaching that needs a pty nobody attaches writing single bytes for over an
 *  hour: the backend coalesces at `PTY_EMIT_MIN_INTERVAL_MS` = 16 ms
 *  (`src-tauri/src/ptyout.rs`), so ~62.5 chunks/s, so ~70 minutes. Extreme —
 *  which is why it is a cap and not a crisis — but the invariant this PR
 *  authors says "bytes AND entries, each with the number stated", and a bound
 *  that a real input shape can walk around is not a bound.
 *
 *  512 rather than a rounder number, for a stated property: 512 × 274 B ≈
 *  137 KiB of wrapper stays UNDER the 256 KiB payload cap, so the unit the
 *  class is named for remains the dominant term. It is also ~8 s of maximally
 *  coalesced drip, three orders above the one-`await` bridge this buffer
 *  actually exists for. Per-id worst case ≈ 256 KiB + 137 KiB ≈ 393 KiB. */
export const MAX_PREATTACH_ENTRIES = 512;

/** Ceiling on how many distinct unattached ids may hold a buffer at once.
 *  Bounds the shape the per-id cap alone does not: a session that keeps
 *  minting ids nobody attaches — a spawn path failing in a loop, which is
 *  exactly #1301's incident. Eviction is oldest-id-first, the one least likely
 *  to still be waiting for an attach.
 *
 *  THIS is the cap that makes lossless startup conditional, and the condition
 *  is worth stating plainly rather than leaving as an argument: eviction drops
 *  an id's ENTIRE held buffer, so a pane that later does attach past 64
 *  concurrent unattached ids loses the startup output this buffer exists to
 *  preserve. "Oldest first" is a choice about WHICH id loses it, not a reason
 *  none does. The condition is recorded in `doc/design/performance.md` INV-8(a)
 *  beside the guarantee itself, because a guarantee whose exception lives only
 *  in the module that takes it is a guarantee nobody reads correctly.
 *
 *  Reaching 64 means 64 spawns in a row went unattached, which is a frontend
 *  already failing the way #1301 failed. Router-wide worst case: 64 × 393 KiB
 *  ≈ 24.6 MiB held (16 MiB of it payload — see `MAX_PREATTACH_ENTRIES` for
 *  where the rest comes from). */
export const MAX_PREATTACH_IDS = 64;

/** How many released ids are remembered as retired, so their trailing bytes
 *  are dropped on arrival instead of opening a fresh buffer.
 *
 *  A ring, not a set that grows: this is the fast path for the common case
 *  (bytes racing a kill that is already in flight), not the bound itself. An
 *  id that ages out of the ring falls back to the pre-attach buffer above,
 *  which is capped by the two constants above — so the bound holds either
 *  way, and the two mechanisms are independent rather than one covering for
 *  the other. */
export const MAX_RETIRED_IDS = 256;

/** What the router did with one payload that had no handler waiting. */
export type HoldResult =
  /** Held for a handler that has not attached yet. `shed` counts older chunks
   *  discarded to keep this id inside `MAX_PREATTACH_BYTES` AND
   *  `MAX_PREATTACH_ENTRIES`; 0 in every healthy attach. */
  | { kind: "hold"; shed: number }
  /** Not held. `retired`: the id was released and can never attach again.
   *  `oversize`: one chunk alone exceeded the per-id ceiling, so holding it
   *  could not respect the cap even with everything else shed. */
  | { kind: "drop"; reason: "retired" | "oversize" };

/** One live attachment. */
interface Attachment<H> {
  id: number;
  owner: RouteOwner;
  handler: H;
}

/** One held chunk with the byte count the cap is measured in. Stored
 *  alongside rather than recomputed, so shedding cannot disagree with
 *  admission about how big anything was. */
interface Held {
  chunk: unknown;
  bytes: number;
}

/** Owner-keyed demultiplexer for a per-pty backend event stream. `H` is the
 *  handler type: `(data: Uint8Array) => void` for output, `() => void` for
 *  the git watch. */
export class PtyRouter<H> {
  private readonly byId = new Map<number, Attachment<H>>();
  /** Strong, and deliberately not a `WeakMap` despite being keyed by a pane —
   *  the case INV-8(a)'s own rule would otherwise flag. It cannot outlive
   *  `byId`: every write to one writes the other, `release` deletes both sides,
   *  and a takeover in `attach` deletes the loser's entry here while
   *  overwriting `byId`. So this map retains nothing `byId` is not already
   *  retaining, and `byId` is what teardown is defined against. */
  private readonly byOwner = new Map<RouteOwner, Attachment<H>>();
  /** Insertion-ordered (Map iteration order) so eviction is oldest-id-first. */
  private readonly pending = new Map<number, { chunks: Held[]; bytes: number }>();
  /** Released ids, newest last. Bounded by MAX_RETIRED_IDS. */
  private readonly retiredRing: number[] = [];
  private readonly retiredSet = new Set<number>();

  /** Bind `owner`'s handler to `id`.
   *
   *  RELEASES the owner's previous id first, if it had one. That is rule 1 in
   *  the module doc and the reason this class exists: a pane that respawns in
   *  place cannot leave its old attachment behind, because there is nowhere
   *  for it to be left. An id already attached to a DIFFERENT owner is taken
   *  over, leaving that owner holding nothing, rather than silently
   *  duplicated — two owners on one id is not a state any caller can want, so
   *  it is resolved here instead of tolerated. */
  attach(owner: RouteOwner, id: number, handler: H): void {
    const previous = this.byOwner.get(owner);
    if (previous && previous.id !== id) this.release(previous.id);
    const taken = this.byId.get(id);
    if (taken && taken.owner !== owner) this.byOwner.delete(taken.owner);
    // Re-attaching a released id is a caller error, not a supported gesture
    // (ids are never reused — see the module doc), but if it happens the id
    // must stop counting as retired or nothing would ever reach it again.
    this.unretire(id);
    const attachment: Attachment<H> = { id, owner, handler };
    this.byId.set(id, attachment);
    this.byOwner.set(owner, attachment);
  }

  /** The handler bound to `id`, or undefined. The dispatch decision: a caller
   *  that gets one delivers to it, and a caller that does not offers the
   *  payload to `hold` rather than deciding for itself what to do with it. */
  handler(id: number): H | undefined {
    return this.byId.get(id)?.handler;
  }

  /** Offer a payload for an id with no handler attached. Returns what
   *  happened — the whole not-attached policy, so a test drives the real
   *  decision rather than a paraphrase of it.
   *
   *  Callers ask `handler` first and only reach here when it answered
   *  undefined, so an attached id never arrives; holding for one would be
   *  buffering nobody ever drains. Not defended against, because the one
   *  caller (pty.ts's `pty-output` listener) is where the check lives and a
   *  second copy of it here would be a second thing to keep in step. */
  hold(id: number, chunk: unknown, bytes: number): HoldResult {
    if (this.retiredSet.has(id)) return { kind: "drop", reason: "retired" };
    if (bytes > MAX_PREATTACH_BYTES) return { kind: "drop", reason: "oversize" };
    let held = this.pending.get(id);
    if (!held) {
      // A brand-new id needs a slot. Evict the OLDEST waiting id rather than
      // refusing the newest: the newest is the one a pane is most likely to
      // be about to attach to.
      if (this.pending.size >= MAX_PREATTACH_IDS) {
        const oldest = this.pending.keys().next();
        if (!oldest.done) this.pending.delete(oldest.value);
      }
      held = { chunks: [], bytes: 0 };
      this.pending.set(id, held);
    }
    held.chunks.push({ chunk, bytes });
    held.bytes += bytes;
    // Shed from the front until it fits BOTH caps. Either can bind first and
    // which one does is a property of the traffic, not of the pane: large
    // chunks hit the byte cap, a drip of small ones hits the entry cap (see
    // `MAX_PREATTACH_ENTRIES` for why the second is not implied by the first).
    // The chunk that just arrived is never shed — the loop stops at one — since
    // a cap that dropped the NEWEST bytes would leave the terminal replaying a
    // stale screen forever.
    let shed = 0;
    while (
      (held.bytes > MAX_PREATTACH_BYTES || held.chunks.length > MAX_PREATTACH_ENTRIES) &&
      held.chunks.length > 1
    ) {
      const dropped = held.chunks.shift();
      if (!dropped) break;
      held.bytes -= dropped.bytes;
      shed++;
    }
    return { kind: "hold", shed };
  }

  /** Everything held for `id` while it had no handler, oldest first, and
   *  clear it. Call right after `attach` to preserve the lossless-startup
   *  guarantee. */
  takeHeld<T>(id: number): T[] {
    const held = this.pending.get(id);
    if (!held) return [];
    this.pending.delete(id);
    return held.chunks.map((h) => h.chunk as T);
  }

  /** Release `id`: drop its handler, its owner binding and anything held for
   *  it, and RETIRE it so trailing bytes are dropped rather than opening a
   *  fresh buffer. Idempotent. */
  release(id: number): void {
    const attachment = this.byId.get(id);
    if (attachment) {
      this.byId.delete(id);
      // Only if the owner still points HERE: a pane that has already
      // re-attached under a newer id must keep that newer binding.
      if (this.byOwner.get(attachment.owner)?.id === id) {
        this.byOwner.delete(attachment.owner);
      }
    }
    this.pending.delete(id);
    this.retire(id);
  }

  /** Release whatever `owner` currently holds, if anything, and answer which
   *  id that was (null if it held none). The teardown call a caller can
   *  always make correctly — it needs no record of which id the owner ended
   *  up on, which is precisely what `Pane.dispose` did not have. */
  releaseOwner(owner: RouteOwner): number | null {
    const attachment = this.byOwner.get(owner);
    if (!attachment) return null;
    this.release(attachment.id);
    return attachment.id;
  }

  /** How many live attachments the router holds. The number that used to grow
   *  without bound (rule 1), so it is the one a test watches. */
  attachedCount(): number {
    return this.byId.size;
  }

  /** Bytes currently held for `id`. For tests and for the bound's own
   *  assertion; nothing in the app reads it. */
  heldBytes(id: number): number {
    return this.pending.get(id)?.bytes ?? 0;
  }

  /** How many CHUNKS are held for `id` — the unit `MAX_PREATTACH_ENTRIES`
   *  bounds, which `heldBytes` cannot see. Tests and the bound's own assertion
   *  only; nothing in the app reads it. */
  heldEntries(id: number): number {
    return this.pending.get(id)?.chunks.length ?? 0;
  }

  /** How many ids are holding a pre-attach buffer. */
  heldIds(): number {
    return this.pending.size;
  }

  /** True while `id` is remembered as released. */
  isRetired(id: number): boolean {
    return this.retiredSet.has(id);
  }

  private retire(id: number): void {
    if (this.retiredSet.has(id)) return;
    this.retiredSet.add(id);
    this.retiredRing.push(id);
    while (this.retiredRing.length > MAX_RETIRED_IDS) {
      const evicted = this.retiredRing.shift();
      if (evicted !== undefined) this.retiredSet.delete(evicted);
    }
  }

  private unretire(id: number): void {
    if (!this.retiredSet.delete(id)) return;
    const at = this.retiredRing.indexOf(id);
    if (at >= 0) this.retiredRing.splice(at, 1);
  }
}

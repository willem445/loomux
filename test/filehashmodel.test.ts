// The pure hashing logic of the file-manager pane (#214) — filehashmodel.ts.
// Pins the auto-hash policy (a directory render must never cost a gigabyte of disk
// reads), the cache key (a STALE hash is worse than no hash — it looks authoritative),
// and the fact that directories and symlinks are never hashed.
//
// The digests themselves are computed in Rust and checked against published FIPS/CRC
// vectors in tests/filehash.rs — this file is about the policy around them.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  planListingHashes,
  rememberDigest,
  hashCacheKey,
  shortDigest,
  algoLabel,
  HASH_ALGOS,
  AUTO_HASH_MAX_BYTES,
  COLUMN_ALGO,
  type HashableEntry,
  type HashCache,
  type HashCell,
} from "../src/filehashmodel.ts";

const file = (name: string, over: Partial<HashableEntry> = {}): HashableEntry => ({
  name,
  is_dir: false,
  is_symlink: false,
  size: 100,
  modified_ms: 1_700_000_000_000,
  ...over,
});
const folder = (name: string) => file(name, { is_dir: true, size: 0 });

const DIGEST = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

const plan = (entries: HashableEntry[], dir = "", cache: HashCache = new Map(), max?: number) =>
  planListingHashes(entries, dir, cache, max ?? AUTO_HASH_MAX_BYTES);

// ---------- what gets hashed, and what doesn't ----------

test("ordinary files are queued for hashing; directories and symlinks never are", () => {
  // A directory has nothing to hash. A symlink must never be FOLLOWED — hashing one
  // would silently digest its target, which can sit outside the pane's root. Both get a
  // blank cell rather than a spurious job.
  const { cells, toHash } = plan([
    file("a.txt"),
    folder("src"),
    file("link", { is_symlink: true }),
  ]);
  assert.deepEqual(toHash, ["a.txt"]);
  assert.deepEqual(cells.get("src"), { kind: "none" });
  assert.deepEqual(cells.get("link"), { kind: "none" });
  assert.deepEqual(cells.get("a.txt"), { kind: "pending" });
});

test("rels are built against the directory being listed, not bare names", () => {
  const { toHash, cells } = plan([file("deep.txt")], "src/design");
  assert.deepEqual(toHash, ["src/design/deep.txt"]);
  assert.ok(cells.has("src/design/deep.txt"));
});

test("a file over the auto threshold becomes CLICK-TO-HASH, not a silent gigabyte read", () => {
  // The whole point of the threshold: opening a folder of ISOs must not spin the disk
  // for minutes filling a column nobody asked for. The hash stays one click away.
  const { cells, toHash } = plan([
    file("small.bin", { size: AUTO_HASH_MAX_BYTES }),
    file("huge.iso", { size: AUTO_HASH_MAX_BYTES + 1 }),
  ]);
  assert.deepEqual(cells.get("small.bin"), { kind: "pending" }, "at the limit: still automatic");
  assert.deepEqual(cells.get("huge.iso"), { kind: "on-demand" }, "over it: the user decides");
  assert.deepEqual(toHash, ["small.bin"], "the big one is NOT queued");
});

test("the threshold is a parameter, so the policy is testable rather than baked in", () => {
  const { toHash } = plan([file("a", { size: 10 }), file("b", { size: 1000 })], "", new Map(), 100);
  assert.deepEqual(toHash, ["a"]);
});

// ---------- the cache ----------

test("a cache hit is served without queueing any work at all", () => {
  const cache: HashCache = new Map([[hashCacheKey("a.txt", 100, 1_700_000_000_000), DIGEST]]);
  const { cells, toHash } = plan([file("a.txt")], "", cache);
  assert.deepEqual(toHash, [], "nothing to do — we already know this file's digest");
  assert.deepEqual(cells.get("a.txt"), {
    kind: "done",
    full: DIGEST,
    short: DIGEST.slice(0, 12),
  });
});

test("a changed MTIME invalidates the cache — the file was edited", () => {
  const cache: HashCache = new Map([[hashCacheKey("a.txt", 100, 1_700_000_000_000), DIGEST]]);
  const { toHash, cells } = plan([file("a.txt", { modified_ms: 1_700_000_009_999 })], "", cache);
  assert.deepEqual(toHash, ["a.txt"], "re-hash it");
  assert.deepEqual(cells.get("a.txt"), { kind: "pending" });
});

test("a changed SIZE invalidates the cache too — and that is not redundant", () => {
  // Size AND mtime, deliberately. A same-size edit landing inside the filesystem's mtime
  // granularity would otherwise serve a stale digest — and a stale hash is worse than no
  // hash, because it looks authoritative. Cheap insurance against a rare, silent lie.
  const cache: HashCache = new Map([[hashCacheKey("a.txt", 100, 1_700_000_000_000), DIGEST]]);
  const { toHash } = plan([file("a.txt", { size: 101 })], "", cache);
  assert.deepEqual(toHash, ["a.txt"]);
});

test("rememberDigest keys the digest to the entry it was computed from", () => {
  const cache: HashCache = new Map();
  const entries = [file("a.txt", { size: 42, modified_ms: 999 })];
  assert.equal(rememberDigest(cache, "a.txt", entries, "", DIGEST), true);
  assert.equal(cache.get(hashCacheKey("a.txt", 42, 999)), DIGEST);

  // And that key is exactly what a re-plan looks up — the round trip closes.
  const { toHash, cells } = plan(entries, "", cache);
  assert.deepEqual(toHash, []);
  assert.equal((cells.get("a.txt") as Extract<HashCell, { kind: "done" }>).full, DIGEST);
});

test("a digest for an entry that is no longer in the listing is DROPPED, not cached", () => {
  // The result arrived after the user navigated away (or the file was deleted). We can no
  // longer observe its size/mtime, so any key we invented would be one nobody looks up —
  // and might collide with a future file of the same name.
  const cache: HashCache = new Map();
  assert.equal(rememberDigest(cache, "gone.txt", [file("other.txt")], "", DIGEST), false);
  assert.equal(cache.size, 0);
});

test("rememberDigest handles a nested directory's rel", () => {
  const cache: HashCache = new Map();
  const entries = [file("deep.txt", { size: 7, modified_ms: 5 })];
  assert.equal(rememberDigest(cache, "src/deep.txt", entries, "src", DIGEST), true);
  assert.equal(cache.get(hashCacheKey("src/deep.txt", 7, 5)), DIGEST);
});

// ---------- presentation ----------

test("the column shows a short prefix; the full digest is never lost", () => {
  assert.equal(shortDigest(DIGEST), "ba7816bf8f01");
  assert.equal(shortDigest(DIGEST).length, 12);
  // The cell keeps the whole thing — the short form is display only.
  const cache: HashCache = new Map([[hashCacheKey("a", 100, 1_700_000_000_000), DIGEST]]);
  const cell = plan([file("a")], "", cache).cells.get("a") as Extract<HashCell, { kind: "done" }>;
  assert.equal(cell.full, DIGEST);
});

test("the CRC variants are NAMED, because a bare CRC-16 is ambiguous", () => {
  // There are dozens of CRC-16s. A user comparing our checksum against another tool's
  // has to know which one they're looking at, or they'll conclude the file is corrupt.
  assert.equal(algoLabel("crc32"), "CRC-32 (ISO-HDLC)");
  assert.equal(algoLabel("crc16"), "CRC-16 (ARC)");
  assert.equal(algoLabel("crc8"), "CRC-8 (SMBUS)");
  assert.equal(algoLabel("sha256"), "SHA-256");
});

test("the submenu offers exactly the six algorithms the backend parses", () => {
  assert.deepEqual(
    HASH_ALGOS.map((a) => a.algo),
    ["sha256", "sha512", "sha1", "crc32", "crc16", "crc8"]
  );
  assert.equal(COLUMN_ALGO, "sha256", "the listing column is SHA-256");
});

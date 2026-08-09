// Unit tests for the group view's lock-resource chrome (#858): the row text a
// human reads beside the panes, and the tone that decides whether it is worth
// looking at. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  lockRows,
  lockSummary,
  minutesLeft,
  span,
  URGENT_MINUTES,
  type LockResourceLike,
} from "../src/locklines.ts";

const MIN = 60_000;
const NOW = 1_000 * MIN;

function resource(over: Partial<LockResourceLike> = {}): LockResourceLike {
  return {
    name: "build",
    slots: 1,
    max_hold_minutes: 45,
    holders: [],
    queue: [],
    ...over,
  };
}

// ---------- minutesLeft / span ----------

test("minutesLeft rounds a sub-minute remainder UP, so a live hold never reads 0", () => {
  assert.equal(minutesLeft(NOW + 1_000, NOW), 1);
  assert.equal(minutesLeft(NOW + 61_000, NOW), 2);
});

test("minutesLeft is 0 only once the deadline has actually passed", () => {
  assert.equal(minutesLeft(NOW, NOW), 0);
  assert.equal(minutesLeft(NOW - MIN, NOW), 0);
});

test("span reads seconds under a minute and hours past one", () => {
  assert.equal(span(45_000), "45s");
  assert.equal(span(12 * MIN), "12m");
  assert.equal(span(125 * MIN), "2h 5m");
});

// ---------- lockRows ----------

test("a free resource says so instead of showing an empty holder list", () => {
  const [row] = lockRows([resource()], NOW);
  assert.equal(row.text, "build 0/1 · free");
  assert.equal(row.tone, "idle");
  assert.match(row.detail, /nobody holds or wants this/);
});

test("a held resource names the holder, its note, and how long it has had it", () => {
  const [row] = lockRows(
    [
      resource({
        holders: [
          { agent: "w-3", note: "cargo test", acquired_ms: NOW - 12 * MIN, expires_ms: NOW + 33 * MIN },
        ],
      }),
    ],
    NOW
  );
  assert.equal(row.text, "build 1/1 · w-3 (cargo test) 12m");
  assert.equal(row.tone, "held");
});

test("a queue is what makes the row worth noticing — waiting outranks held", () => {
  const [row] = lockRows(
    [
      resource({
        holders: [{ agent: "w-3", note: "", acquired_ms: NOW - MIN, expires_ms: NOW + 44 * MIN }],
        queue: [
          { agent: "w-4", note: "", queued_ms: NOW - 30_000, expires_ms: NOW + 59 * MIN },
          { agent: "w-5", note: "", queued_ms: NOW - 10_000, expires_ms: NOW + 59 * MIN },
        ],
      }),
    ],
    NOW
  );
  assert.equal(row.text, "build 1/1 · w-3 1m · 2 waiting");
  assert.equal(row.tone, "waiting");
});

test("an imminent reclaim outranks a queue — it changes the state on its own", () => {
  const [row] = lockRows(
    [
      resource({
        holders: [
          {
            agent: "w-3",
            note: "",
            acquired_ms: NOW - 41 * MIN,
            expires_ms: NOW + URGENT_MINUTES * MIN,
          },
        ],
        queue: [{ agent: "w-4", note: "", queued_ms: NOW, expires_ms: NOW + 59 * MIN }],
      }),
    ],
    NOW
  );
  assert.equal(row.tone, "urgent");
});

test("the detail lists the queue IN ORDER with each waiter's own give-up clock", () => {
  const [row] = lockRows(
    [
      resource({
        holders: [{ agent: "w-3", note: "", acquired_ms: NOW - MIN, expires_ms: NOW + 44 * MIN }],
        queue: [
          { agent: "w-4", note: "docs build", queued_ms: NOW - 5 * MIN, expires_ms: NOW + 55 * MIN },
          { agent: "w-5", note: "", queued_ms: NOW - MIN, expires_ms: NOW + 9 * MIN },
        ],
      }),
    ],
    NOW
  );
  const lines = row.detail.split("\n");
  assert.match(lines[1], /^holding: w-3 1m — reclaimed in 44 min$/);
  assert.match(lines[2], /^#1 in queue: w-4 \(docs build\) — waiting 5m, gives up in 55 min$/);
  assert.match(lines[3], /^#2 in queue: w-5 — waiting 1m, gives up in 9 min$/);
});

test("a multi-slot resource shows how many of its slots are taken", () => {
  const [row] = lockRows(
    [
      resource({
        name: "gpu",
        slots: 2,
        holders: [
          { agent: "w-1", note: "", acquired_ms: NOW - MIN, expires_ms: NOW + 44 * MIN },
          { agent: "w-2", note: "", acquired_ms: NOW - 2 * MIN, expires_ms: NOW + 43 * MIN },
        ],
      }),
    ],
    NOW
  );
  assert.equal(row.text, "gpu 2/2 · w-1 1m, w-2 2m");
});

// ---------- lockSummary ----------

test("lockSummary is empty for a repo that declares nothing, so the row is hidden", () => {
  assert.equal(lockSummary([]), "");
});

test("lockSummary counts queued agents across every resource, not just one", () => {
  const line = lockSummary([
    resource({
      holders: [{ agent: "w-1", note: "", acquired_ms: NOW, expires_ms: NOW + MIN }],
      queue: [{ agent: "w-2", note: "", queued_ms: NOW, expires_ms: NOW + MIN }],
    }),
    resource({
      name: "gpu",
      holders: [{ agent: "w-3", note: "", acquired_ms: NOW, expires_ms: NOW + MIN }],
      queue: [{ agent: "w-4", note: "", queued_ms: NOW, expires_ms: NOW + MIN }],
    }),
  ]);
  assert.equal(line, "locks: 2 held across 2 resources, 2 agents queued");
});

test("lockSummary drops the queue clause entirely when nobody is waiting", () => {
  assert.equal(lockSummary([resource()]), "locks: 0 held across 1 resource");
});

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { SerialQueue, QUEUE_WAIT_LIMIT_MS, WAITED_TOO_LONG } from "../src/gitqueue.ts";

/** A job that resolves only when its returned `finish` is called, and records
 *  when it started and stopped so overlap is observable rather than inferred. */
function controllable(log: string[], name: string) {
  let release!: (v?: unknown) => void;
  const gate = new Promise((r) => (release = r));
  const work = async () => {
    log.push(`+${name}`);
    await gate;
    log.push(`-${name}`);
    return name;
  };
  return { work, finish: () => release() };
}

test("a second job does not start until the first has settled", async () => {
  // The property the freeze used to provide for free: two `git` spawns against
  // one worktree never overlap, so they never contend on index.lock. A `+b`
  // appearing before `-a` is exactly the "stage then commit raced and one lost"
  // regression #726 would otherwise introduce.
  const log: string[] = [];
  const q = new SerialQueue();
  const a = controllable(log, "a");
  const b = controllable(log, "b");

  const pa = q.run(a.work);
  const pb = q.run(b.work);
  // Give the microtask queue every chance to start `b` early if it can.
  await Promise.resolve();
  await Promise.resolve();
  assert.deepEqual(log, ["+a"], "the second job started while the first was in flight");

  a.finish();
  await pa;
  b.finish();
  await pb;
  assert.deepEqual(log, ["+a", "-a", "+b", "-b"]);
});

test("jobs run in the order they were enqueued", async () => {
  // Click order is the ordering that matters: a stage that lands after the
  // commit it was meant to precede changes what got committed.
  const order: number[] = [];
  const q = new SerialQueue();
  const jobs = [30, 0, 10].map((delay, i) =>
    q.run(async () => {
      await new Promise((r) => setTimeout(r, delay));
      order.push(i);
    })
  );
  await Promise.all(jobs);
  assert.deepEqual(order, [0, 1, 2], "a slow first job must still run before a fast second one");
});

test("the caller sees the job's own value and its own rejection", async () => {
  // The queue must be transparent: wrapping a typed binding in it may move the
  // timing but must not change the contract (Promise<T>, same error object).
  const q = new SerialQueue();
  assert.equal(await q.run(async () => "worktree-path"), "worktree-path");

  const boom = new Error("fatal: Unable to create index.lock");
  await assert.rejects(
    q.run(async () => {
      throw boom;
    }),
    (e: unknown) => e === boom
  );
});

test("a failed job does not wedge the queue", async () => {
  // Every one of these commands can fail routinely — a conflicted rebase, a
  // rejected push. If a rejection poisoned the chain, the first such failure
  // would silently kill every later git action for the life of the window.
  const q = new SerialQueue();
  await assert.rejects(
    q.run(async () => {
      throw new Error("rejected");
    })
  );
  assert.equal(await q.run(async () => "still working"), "still working");
});

test("a job that waits past the limit is abandoned, never run late", async () => {
  // The bounded-suppression rule (#496, #513, #518): `run_git` spawns with no
  // timeout and GIT_TERMINAL_PROMPT=0 does not cover a GUI credential helper,
  // so the head job can genuinely never settle. Without a bound, every later
  // mutating op in the window waits forever with nothing on screen.
  const log: string[] = [];
  const q = new SerialQueue();
  const stuck = controllable(log, "stuck");

  const head = q.run(stuck.work, 10_000);
  const late = q.run(async () => {
    log.push("+late");
    return "ran";
  }, 20);

  // Raced against a watchdog rather than awaited directly, because the whole
  // property is "this settles even though the head job never does". Awaiting an
  // unbounded queue would hang the suite forever instead of failing it — a
  // timeout is not evidence, an assertion is.
  const outcome = await Promise.race([
    late.then(
      () => "ran to completion",
      (e: Error) => (e.message === WAITED_TOO_LONG ? "abandoned" : `other rejection: ${e.message}`)
    ),
    new Promise((r) => setTimeout(() => r("still waiting"), 300)),
  ]);
  assert.equal(
    outcome,
    "abandoned",
    "a job queued behind a head that never settles was not bounded — every later mutating op " +
      "in the window is stuck for the life of the process (#496, #513, #518)"
  );
  // The critical half: abandoning must FAIL the job, never release it to run
  // beside the stuck one — that would be the index.lock race the queue exists
  // to prevent, smuggled past the guard by a timer.
  assert.deepEqual(log, ["+stuck"], "the abandoned job ran anyway");

  stuck.finish();
  await head;
  await new Promise((r) => setTimeout(r, 20));
  assert.deepEqual(log, ["+stuck", "-stuck"], "the abandoned job ran when its turn finally came");
});

test("the wait clock covers the wait only, never the job's own duration", async () => {
  // A legitimately slow push must not be cut off mid-flight — the bound exists
  // for jobs that never START, not for jobs that take a while.
  const q = new SerialQueue();
  const slow = await q.run(async () => {
    await new Promise((r) => setTimeout(r, 60));
    return "finished";
  }, 20);
  assert.equal(slow, "finished");
});

test("a job that gets its turn in time is unaffected by the bound", async () => {
  const q = new SerialQueue();
  const results = await Promise.all([
    q.run(async () => "a", 5_000),
    q.run(async () => "b", 5_000),
    q.run(async () => "c", 5_000),
  ]);
  assert.deepEqual(results, ["a", "b", "c"]);
  assert.ok(QUEUE_WAIT_LIMIT_MS >= 30_000, "the default bound must not be tight enough to trip on a slow-but-normal op");
});

test("busy reports whether a job issued now would have to wait", async () => {
  // Drives the git view's "Waiting for another git operation…" notice on the
  // act() paths, which have no button to spin (gitview.ts act()).
  const log: string[] = [];
  const q = new SerialQueue();
  assert.equal(q.busy, false, "an idle queue must not claim to be busy");

  const job = controllable(log, "j");
  const p = q.run(job.work);
  await Promise.resolve();
  await Promise.resolve();
  assert.equal(q.busy, true, "a job is in flight, so the next one would wait");

  job.finish();
  await p;
  assert.equal(q.busy, false, "the queue drained but still claims to be busy");
});

test("every mutating git binding routes through the queue, and no read does", () => {
  // The queue only holds if no call site can bypass it, so pin the wiring in
  // src/git.ts rather than trusting a hand audit — the same reason git.rs
  // scans its own source for the async/run_blocking pair (#726) and
  // tests/acl_manifest.rs parses generate_handler! out of lib.rs.
  const src = readFileSync(new URL("../src/git.ts", import.meta.url), "utf8");

  const queued: string[] = [];
  const direct: string[] = [];
  for (const chunk of src.split(/^export const /m).slice(1)) {
    const invoked = chunk.match(/invoke\("([a-z_]+)"/);
    if (!invoked) continue;
    (chunk.includes("writes.run(") ? queued : direct).push(invoked[1]);
  }

  // The fifteen that write an index, a working tree, or a ref. `git_fetch` is
  // here because --prune rewrites remote-tracking refs; `git_branches` is not,
  // because it only reads them.
  assert.deepEqual(
    queued.sort(),
    [
      "git_branch_create",
      "git_checkout",
      "git_cherry_pick",
      "git_commit",
      "git_discard",
      "git_fetch",
      "git_merge",
      "git_pull",
      "git_push",
      "git_rebase",
      "git_revert",
      "git_stage",
      "git_tag",
      "git_unstage",
      "git_worktree_add",
    ],
    "a mutating git binding is missing its writes.run() — two of these can now " +
      "genuinely overlap and contend on index.lock (#726)"
  );
  assert.deepEqual(
    direct.sort(),
    [
      "git_branches",
      "git_commit_files",
      "git_diff",
      "git_log",
      "git_repo_root",
      "git_status",
      "git_worktree_list",
    ],
    "a read was put behind the write queue — reads have been concurrent with " +
      "writes since #399 and must not be serialized behind an unbounded fetch"
  );
  // Set equality both ways is also the scan's vacuity guard: a refactor that
  // stopped matching `invoke("…")` yields two empty lists and fails here
  // instead of passing over nothing.
});

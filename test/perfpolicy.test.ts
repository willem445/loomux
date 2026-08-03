// The frontend half of #743's enforcement — **E2** in `doc/design/performance.md`.
//
// THE INVARIANTS. Two of the six in that note are properties of frontend code
// that no unit test of any single module can see:
//
//   INV-3 (§3) — every backend event stream the webview subscribes to is
//   bounded *before* the webview pays for it, because a Tauri emit is a
//   per-event JS compilation on the one thread that also services input and
//   paint (§1). A stream whose rate is set by an external producer is bounded
//   backend-side by P2 (coalesce per frame) or handler-side by P5 (rAF
//   dirty-flag) — §2.
//
//   INV-4 (§3) — every fixed-cadence timer declares itself: its real cadence,
//   and what it does when nobody is looking at the window.
//
// Both are properties of the SET of listeners and timers, not of any one of
// them, so the only honest way to pin them is to enumerate: scan `src/*.ts`
// for the two call shapes and require every hit to appear in a manifest that
// states its rate class and its bound. `src-tauri/tests/perf_dispatch.rs`
// (E1) is the same idea on the command surface — scan plus a test-side
// manifest — and the two are meant to read as one mechanism.
//
// WHY A MANIFEST AND NOT A LINT. The property is not "no unbounded streams
// exist" — several do today, enumerated in #743's census (plan parts 2b) and
// owned by later slices. The property is that **an unbounded one cannot be
// added silently**: a new `listen()` or `setInterval()` fails this test until
// somebody writes down what bounds it, and that sentence is a review-visible
// diff. The debt rows are the census made executable; deleting one is the
// roadmap, adding one has to argue itself.
//
// HOW TODAY'S GAPS ARE RECORDED. §3's vocabulary is
// `gated | component-scoped | argued(reason)` for timers and
// `backend-coalesced | rAF-gated | throttled | argued-none(reason)` for
// streams — there is no "unbounded" value, deliberately. A row that has no
// gate today says so in its `reason` and names the slice that owns closing it
// in `debt`, exactly as E1's `debt` class does for sync commands. So `argued`
// here means "argued in the reason field", which for a debt row is the
// argument that it is known, bounded in blast radius, and owned — not that it
// is fine. The test enforces the difference: a producer-rate stream may not
// declare `argued-none` without an owning issue (below).
//
// WHAT THIS IS NOT. A source scan cannot prove a declared bound actually
// bounds anything at runtime — it checks that the claim exists, is one of the
// legal shapes, and still points at real code (an `rAF-gated` row's cite must
// contain a `requestAnimationFrame`). The residue is carried by review, the
// same stated bound E1 accepts for call chains (§3). It also only reads
// `src/*.ts`: a listener registered from Rust-side generated code, or with an
// event name that is not a literal, is not something it can name — so it
// refuses to pass over one instead, by counting call sites and requiring the
// count to match what it extracted.
//
// The one shape it cannot see at all is a **self-rescheduling `setTimeout`** —
// a `tick()` that ends by scheduling itself is a fixed-cadence poll wearing a
// different call, and nothing here would ask it to declare a cadence. This
// test does not go looking for them (deciding which of `src/`'s ~30
// `setTimeout` uses are one-shots is a reading job, not a regex), so INV-4
// names `setInterval` on purpose and the reviewer rule is the residue: a new
// recurring poll is written as `setInterval` and declared here, or its shape
// is argued in the PR that introduces it.
import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync, readdirSync } from "node:fs";

// ---------- the manifests ----------

/** What sets the event's rate — the thing INV-3 actually cares about. */
type RateClass =
  /** Rate set by an external producer: child output, a filesystem walk, agent
   *  activity. The class INV-3 exists for. */
  | "producer"
  /** A fixed backend tick. The rate is a constant somebody chose. */
  | "cadenced"
  /** At most a few per pane/request lifetime. */
  | "lifecycle"
  /** One per human gesture. */
  | "gesture";

/** How the stream is bounded before the webview pays — `performance.md` §3
 *  INV-3's vocabulary, no additions. */
type Bound = "backend-coalesced" | "rAF-gated" | "throttled" | "argued-none";

interface StreamRow {
  event: string;
  rate: RateClass;
  bound: Bound;
  /** Repo-relative file that implements or argues the bound. Must exist; for
   *  `rAF-gated` it must actually contain the rAF gate. */
  cite: string;
  /** The argument, in a sentence. For a debt row: what the cost is today. */
  reason: string;
  /** Owning issue/slice when this row is a declared gap, else `null`. */
  debt: string | null;
}

/** Every `listen()` in `src/*.ts`, keyed by event name. Seeded verbatim from
 *  the #743 census (plan part 2b, comment 5162018391) — this is today's truth,
 *  not the target state; S5 (`perf/743-stream-bounds`) is what deletes the
 *  debt rows. */
const STREAMS: StreamRow[] = [
  {
    event: "pty-output",
    rate: "producer",
    bound: "backend-coalesced",
    cite: "src-tauri/src/ptyout.rs",
    reason:
      "The app's only per-chunk producer flood, and now its best-covered stream: P2 bounds it " +
      "backend-side to <=1 event per pane per 16 ms with a 64 KiB batch cap and a leading edge " +
      "(#714), and unfocused panes are throttled again handler-side by panethrottle.ts (#720/#733).",
    debt: null,
  },
  {
    event: "pty-exit",
    rate: "lifecycle",
    bound: "argued-none",
    cite: "src/main.ts",
    reason:
      "At most one per pane lifetime, emitted by the waiter thread. The handler's O(panes) scan " +
      "and teardown run once per exit, so there is no rate to bound.",
    debt: null,
  },
  {
    event: "git-changed",
    rate: "cadenced",
    bound: "argued-none",
    cite: "src/gitview.ts",
    reason:
      "The emit itself is the bound: gitwatch polls at 1 s and emits only when the signature " +
      "changed, so <=1/s per watched pane. notifyPrompt is throttled 500 ms on top. The gap: " +
      "refreshDir's dir_info invoke rides every event ungated, and dir_info is still a sync command.",
    debt: "#743 S5 (fold refreshDir into the 500 ms notifyPrompt window); #746 owns dir_info",
  },
  {
    event: "system-metrics",
    rate: "cadenced",
    bound: "argued-none",
    cite: "src/statusbar.ts",
    reason:
      "A fixed 2 s backend sampler (metrics.rs) with a constant per-event cost of four DOM " +
      "writes and no fan-out over panes or tabs. Bounding a 0.5 Hz stream buys nothing.",
    debt: null,
  },
  {
    event: "ft-files",
    rate: "producer",
    bound: "rAF-gated",
    cite: "src/fileexplorer.ts",
    reason:
      "P5, and the frontend precedent the other batch streams are meant to copy: the batch sets " +
      "a dirty flag and schedules one requestAnimationFrame render, so a walk emitting hundreds " +
      "of batches costs one render per frame.",
    debt: null,
  },
  {
    event: "ft-search",
    rate: "producer",
    bound: "rAF-gated",
    cite: "src/fileedit.ts",
    reason:
      "P5 via scheduleRender(): the fold-and-tree-update per batch is deferred to one rAF render " +
      "regardless of how fast the search walk streams.",
    debt: null,
  },
  {
    event: "fm-hash",
    rate: "producer",
    bound: "argued-none",
    cite: "src/fileexplorer.ts",
    reason:
      "Unbounded today: the backend emits one batch per 8 files hashed and paintHashCells does a " +
      "full querySelectorAll over every visible row per batch, so hashing a large tree is " +
      "hundreds of whole-DOM passes on the webview thread.",
    debt: "#743 S5 (copy the ft-files rAF gate onto paintHashCells)",
  },
  {
    event: "fm-delete",
    rate: "lifecycle",
    bound: "argued-none",
    cite: "src/filemgr.ts",
    reason:
      "At most one delete operation is in flight at a time and each event updates one row; the " +
      "rate is a human's delete gesture, not a producer.",
    debt: null,
  },
  {
    event: "orch-spawn-request",
    rate: "lifecycle",
    bound: "argued-none",
    cite: "src/orchestration.ts",
    reason:
      "One per spawn request an orchestrator issues — a lifecycle event, not a stream. The " +
      "handler's pane-opening work is proportional to one agent.",
    debt: null,
  },
  {
    event: "orch-spawn-cancelled",
    rate: "lifecycle",
    bound: "argued-none",
    cite: "src/orchestration.ts",
    reason:
      "One per cancelled spawn. The handler's O(panes) sweep for a zombie pane is acceptable at " +
      "that rate; it cannot be driven faster than spawns are requested.",
    debt: null,
  },
  {
    event: "orch-focus",
    rate: "lifecycle",
    bound: "argued-none",
    cite: "src/orchestration.ts",
    reason:
      "One per focus request (an orchestrator or human action). Tab switch plus pane focus, once.",
    debt: null,
  },
  {
    event: "orch-rename",
    rate: "lifecycle",
    bound: "argued-none",
    cite: "src/orchestration.ts",
    reason:
      "One per rename, which is idempotent and touches a single pane's title. Renames are " +
      "gesture- and lifecycle-driven, never producer-driven.",
    debt: null,
  },
  {
    event: "orch-attention",
    rate: "cadenced",
    bound: "argued-none",
    cite: "src/orchestration.ts",
    reason:
      "A 3 s backend tick that emits unconditionally and carries the FULL attention set with no " +
      "diff, so every tick costs an O(panes x tabs) re-badge even when nothing changed. Bounded " +
      "in rate by the tick, unbounded in per-tick work.",
    debt: "#743 S5 (diff the payload against the previous set before applyAttention)",
  },
  {
    event: "orch-delivery-held",
    rate: "lifecycle",
    bound: "argued-none",
    cite: "src/orchestration.ts",
    reason:
      "One per hold event, touching the one pane that is held. Holds are driven by delivery " +
      "attempts, not by a producer's output rate.",
    debt: null,
  },
  {
    event: "orch-delivery-held-cleared",
    rate: "lifecycle",
    bound: "argued-none",
    cite: "src/orchestration.ts",
    reason:
      "The release half of orch-delivery-held: at most one per hold, and it clears one pane's " +
      "badge.",
    debt: null,
  },
  {
    event: "orch-group-ended",
    rate: "lifecycle",
    bound: "argued-none",
    cite: "src/orchestration.ts",
    reason:
      "Once per group teardown. A group ends far less often than it is polled, and the handler " +
      "runs one sweep.",
    debt: null,
  },
  {
    event: "orch-channel",
    rate: "gesture",
    bound: "argued-none",
    cite: "src/orchestration.ts",
    reason:
      "Emitted per channel mutation, which is human-gesture-bound (connect/disconnect), so the " +
      "handler's O(panes x tabs) scan plus tab-bar refresh is paid at gesture rate. No defensive " +
      "batching: if channels ever become agent-driven this row's rate class changes and so must " +
      "its bound.",
    debt: null,
  },
  {
    event: "orch-tasks-changed",
    rate: "producer",
    bound: "argued-none",
    cite: "src/tasksview.ts",
    reason:
      "Unbounded today: emitted on EVERY write_tasks, which agents drive in bursts, and each " +
      "event is a full board refetch plus re-render in every open TasksView — 10 rapid writes " +
      "with N views open costs 10xN full refetches.",
    debt: "#743 S5 (per-view single-flight + trailing-edge merge, the refreshGate precedent)",
  },
];

/** What the timer does when the window is not being looked at —
 *  `performance.md` §3 INV-4's vocabulary, no additions. */
type VisibilityPolicy =
  /** Explicitly visibility-aware (document.hidden / visibilitychange). */
  | "gated"
  /** Alive only while its component/affordance is open, and cleared on close. */
  | "component-scoped"
  /** Argued in `reason` — including "no gate today, here is the cost and who
   *  owns closing it", in which case `debt` names the owner. */
  | "argued";

interface TimerRow {
  /** `src/<file>.ts@<delay expression as written>` — the cadence is part of
   *  the identity on purpose, so changing 4000 to 1000 is a manifest diff a
   *  reviewer sees rather than a silent 4x. */
  key: string;
  /** The resolved cadence, asserted against the source. */
  cadenceMs: number;
  policy: VisibilityPolicy;
  reason: string;
  debt: string | null;
}

/** Every `setInterval()` in `src/*.ts`. Seeded verbatim from the #743 census
 *  (plan part 2b): as of this file, NOTHING in `src/` reads `document.hidden`
 *  or listens for `visibilitychange`, so no row is `gated` yet. S6
 *  (`perf/743-visibility-polls`) is the slice that changes that; recording the
 *  aspiration here instead of the truth would make this test a decoration. */
const TIMERS: TimerRow[] = [
  {
    key: "src/tabbar.ts@4000",
    cadenceMs: 4000,
    policy: "argued",
    reason:
      "App-lifetime tick: started in the tab strip's constructor and never cleared. Each tick's " +
      "pollStatus() loops EVERY group-bound tab and invokes groupSummary plus groupUsage (the " +
      "transcript-scanning command), so a minimized window still pays it forever. No visibility " +
      "gate today; the strip only re-renders when a value differs, which bounds the paint, not the IPC.",
    debt: "#743 S6 (pollgate.ts: pause or stretch while hidden; decide groupUsage's fate in-slice)",
  },
  {
    key: "src/tabbar.ts@PREVIEW_REFRESH_MS",
    cadenceMs: 700,
    policy: "component-scoped",
    reason:
      "Alive only while a hover preview is open and cleared when it closes, so it cannot run " +
      "against a window nobody is looking at — a hover is by definition a pointer on the strip. " +
      "Rebuilds preview DOM only; no backend invoke per tick.",
    debt: null,
  },
  {
    key: "src/groupview.ts@POLL_MS",
    cadenceMs: 2000,
    policy: "component-scoped",
    reason:
      "Armed by show() and cleared by hide() on every close/eviction path, so it stops with the " +
      "panel. That is component scope, not visibility: a panel left open behind a minimized " +
      "window keeps paying Promise.all of nine invokes plus a full render every 2 s.",
    debt: "#743 S6 (visibility gate on top of the existing component scope)",
  },
  {
    key: "src/auditview.ts@FOLLOW_MS",
    cadenceMs: 1500,
    policy: "component-scoped",
    reason:
      "Opt-in: armed only by the follow toggle, cleared by the toggle and by dispose(), so it " +
      "cannot outlive the view. Once armed it refetches orch_audit and re-renders every tick " +
      "whether or not the window is visible.",
    debt: "#743 S6 (pause the follow poll while hidden, refresh on visible)",
  },
  {
    key: "src/timelineview.ts@FOLLOW_MS",
    cadenceMs: 1500,
    policy: "component-scoped",
    reason:
      "The timeline's follow toggle, same shape as auditview's: armed on toggle, cleared on " +
      "toggle and dispose. Its gh half is separately self-gated to GH_REFRESH_MS (60 s) in " +
      "timelinechrome.ts, so a tick is one orch_audit refetch, not two shell-outs.",
    debt: "#743 S6 (pause the follow poll while hidden, refresh on visible)",
  },
  {
    key: "src/main.ts@20_000",
    cadenceMs: 20000,
    policy: "argued",
    reason:
      "App-lifetime and deliberately ungated, because the tick itself is an in-memory filter: " +
      "the real listSessions scan is gated to RECONCILE_MIN_INTERVAL_MS (>=60 s) inside " +
      "reconcileSessionIds, and the 20 s cadence exists so a pane that starts qualifying " +
      "mid-window is picked up promptly. A hidden window pays a predicate, not IPC.",
    debt: null,
  },
];

// ---------- the scanner ----------

interface Source {
  path: string;
  text: string;
}

interface StreamHit {
  path: string;
  line: number;
  event: string;
}

interface TimerHit {
  path: string;
  line: number;
  /** The delay argument exactly as written (`4000`, `POLL_MS`, `20_000`). */
  delayExpr: string;
  /** Resolved through same-file `const NAME = <literal>`, or `null` if the
   *  scanner could not resolve it — which is a failure, not a skip. */
  cadenceMs: number | null;
}

/** `listen(`, `listen<T>(` — including the multi-line form where the event
 *  name is on the next line (`orchestration.ts`'s longer generics). The
 *  `\b` refuses `unlisten(`. */
const LISTEN_CALL_SRC = "\\blisten\\s*(?:<[\\s\\S]*?>)?\\s*\\(";
/** …with the event name as a literal: capture 1 is the quote, capture 2 the name. */
const LISTEN_NAME_SRC = LISTEN_CALL_SRC + "\\s*([\"'`])([^\"'`]*)\\1";
const INTERVAL_CALL_SRC = "\\bsetInterval\\s*\\(";

function lineOf(text: string, index: number): number {
  let line = 1;
  for (let i = 0; i < index && i < text.length; i++) if (text[i] === "\n") line++;
  return line;
}

function countCalls(text: string, source: string): number {
  const re = new RegExp(source, "g");
  let n = 0;
  while (re.exec(text) !== null) n++;
  return n;
}

/** Every `listen()` with a literal event name. */
export function listenHits(src: Source): StreamHit[] {
  const re = new RegExp(LISTEN_NAME_SRC, "g");
  const hits: StreamHit[] = [];
  let m: RegExpExecArray | null;
  while ((m = re.exec(src.text)) !== null) {
    hits.push({ path: src.path, line: lineOf(src.text, m.index), event: m[2] });
  }
  return hits;
}

/** Top-level argument slices of the call whose `(` is at `open`. `null` when
 *  the scanner loses the thread (an unterminated call, or a `)` inside a
 *  template-literal `${}` — the known blind spot); a `null` fails the timer
 *  test at that site rather than dropping it. */
export function callArgs(text: string, open: number): string[] | null {
  const args: string[] = [];
  let depth = 0;
  let start = open + 1;
  let quote: string | null = null;
  for (let i = open; i < text.length; i++) {
    const c = text[i];
    if (quote !== null) {
      if (c === "\\") i++;
      else if (c === quote) quote = null;
      continue;
    }
    if (c === '"' || c === "'" || c === "`") quote = c;
    else if (c === "(" || c === "[" || c === "{") depth++;
    else if (c === ")" || c === "]" || c === "}") {
      depth--;
      if (depth === 0) {
        args.push(text.slice(start, i));
        return args;
      }
    } else if (c === "," && depth === 1) {
      args.push(text.slice(start, i));
      start = i + 1;
    }
  }
  return null;
}

/** A numeric literal (`700`, `20_000`) or a same-file `const NAME = <literal>`.
 *  Anything else is unresolvable on purpose: a cadence this test cannot read
 *  is a cadence it cannot pin, and it says so instead of guessing. */
export function resolveMs(expr: string, fileText: string): number | null {
  const trimmed = expr.trim();
  if (/^\d[\d_]*$/.test(trimmed)) return Number(trimmed.replace(/_/g, ""));
  if (!/^[A-Za-z_$][\w$]*$/.test(trimmed)) return null;
  const decl = new RegExp(
    String.raw`\bconst\s+` + trimmed + String.raw`\s*(?::\s*number\s*)?=\s*(\d[\d_]*)\b`
  ).exec(fileText);
  return decl === null ? null : Number(decl[1].replace(/_/g, ""));
}

/** Every `setInterval()` site, with its cadence resolved against the file. */
export function intervalHits(src: Source): TimerHit[] {
  const re = new RegExp(INTERVAL_CALL_SRC, "g");
  const hits: TimerHit[] = [];
  let m: RegExpExecArray | null;
  while ((m = re.exec(src.text)) !== null) {
    const open = m.index + m[0].length - 1;
    const args = callArgs(src.text, open);
    const delayExpr = args !== null && args.length >= 2 ? args[1].trim() : "";
    hits.push({
      path: src.path,
      line: lineOf(src.text, m.index),
      delayExpr,
      cadenceMs: delayExpr === "" ? null : resolveMs(delayExpr, src.text),
    });
  }
  return hits;
}

function timerKey(hit: TimerHit): string {
  return `${hit.path}@${hit.delayExpr}`;
}

// ---------- the scanner, pinned on synthetic sources ----------
//
// The manifest tests below are only as good as these two extractors, and an
// extractor that has drifted reports green about code it no longer reads.
// These pin the shapes that exist in `src/` today plus the ones that would
// silently break them.

test("listenHits reads every call shape src/ actually uses", () => {
  const src: Source = {
    path: "src/fake.ts",
    text: [
      `listen("plain", cb);`,
      `listen<Payload>("generic", cb);`,
      `void listen<{ a: string; b: number }>(`,
      `  "multiline-with-semicolons-in-the-generic",`,
      `  ({ payload }) => use(payload)`,
      `);`,
      `unlisten("not-a-listener");`,
    ].join("\n"),
  };
  assert.deepEqual(
    listenHits(src).map((h) => `${h.line}:${h.event}`),
    ["1:plain", "2:generic", "3:multiline-with-semicolons-in-the-generic"]
  );
});

test("a listen() whose event name is not a literal is counted but not captured", () => {
  // The case the vacuity guard exists for: extraction silently returning
  // fewer hits than there are call sites is exactly how a manifest goes
  // stale without anyone noticing.
  const text = `listen(EVENT_NAME, cb);\nlisten("literal", cb);`;
  assert.equal(countCalls(text, LISTEN_CALL_SRC), 2);
  assert.equal(listenHits({ path: "src/fake.ts", text }).length, 1);
});

test("intervalHits resolves both literal and named cadences, and refuses the rest", () => {
  const src: Source = {
    path: "src/fake.ts",
    text: [
      `const POLL_MS = 2000;`,
      `setInterval(() => void this.load(), POLL_MS);`,
      `setInterval(() => { f(","); g(")"); }, 20_000);`,
      `window.setInterval(paint, computeDelay(x));`,
    ].join("\n"),
  };
  assert.deepEqual(
    intervalHits(src).map((h) => `${h.line}:${h.delayExpr}=${String(h.cadenceMs)}`),
    ["2:POLL_MS=2000", "3:20_000=20000", "4:computeDelay(x)=null"],
    "commas and parens inside the callback must not be read as the delay argument"
  );
});

// ---------- the real tree ----------

const SRC_DIR = new URL("../src/", import.meta.url);
const REPO = new URL("../", import.meta.url);

function realSources(): Source[] {
  return readdirSync(SRC_DIR)
    .filter((f) => f.endsWith(".ts"))
    .sort()
    .map((f) => ({ path: `src/${f}`, text: readFileSync(new URL(f, SRC_DIR), "utf8") }));
}

test("every backend event stream src/ listens to is declared in the stream manifest", () => {
  const hits = realSources().flatMap(listenHits);
  const declared = new Set(STREAMS.map((r) => r.event));
  const found = new Set(hits.map((h) => h.event));

  const undeclared = hits.filter((h) => !declared.has(h.event));
  assert.deepEqual(
    undeclared.map((h) => `${h.path}:${h.line} listens for "${h.event}"`),
    [],
    "a new backend event stream reached the webview without declaring what bounds it " +
      "(performance.md §3 INV-3) — add a row to STREAMS with its rate class and bound"
  );

  // The other direction: a row nothing listens for is a claim about code that
  // is gone. Leaving it is how a manifest stops describing the app.
  assert.deepEqual(
    [...declared].filter((e) => !found.has(e)).sort(),
    [],
    "a STREAMS row names an event no src/ file listens for — delete the row (or fix the " +
      "event name); a stale row makes the manifest fiction"
  );
});

test("every setInterval in src/ is declared in the timer manifest, at its real cadence", () => {
  const hits = realSources().flatMap(intervalHits);
  const byKey = new Map(TIMERS.map((r) => [r.key, r]));

  const unresolved = hits.filter((h) => h.cadenceMs === null);
  assert.deepEqual(
    unresolved.map((h) => `${h.path}:${h.line} delay=${h.delayExpr || "(none)"}`),
    [],
    "a timer's cadence is not a literal or a same-file `const NAME = <literal>`, so this test " +
      "cannot pin it — make the cadence a named constant (INV-4 is 'declare the cadence')"
  );

  const undeclared = hits.filter((h) => !byKey.has(timerKey(h)));
  assert.deepEqual(
    undeclared.map((h) => `${h.path}:${h.line} setInterval(..., ${h.delayExpr})`),
    [],
    "a new fixed-cadence timer appeared without declaring its cadence and visibility policy " +
      "(performance.md §3 INV-4) — add a row to TIMERS"
  );

  const keys = hits.map(timerKey);
  assert.equal(
    new Set(keys).size,
    keys.length,
    `two timers in one file share a delay expression, so the manifest cannot tell them apart: ` +
      `${keys.join(", ")} — give one of them its own named cadence constant`
  );

  assert.deepEqual(
    TIMERS.map((r) => r.key).filter((k) => !keys.includes(k)).sort(),
    [],
    "a TIMERS row names a setInterval that no longer exists at that cadence — delete the row, " +
      "or update it if the cadence changed (the cadence is part of the key on purpose)"
  );

  // The cadence claim itself, checked against the source rather than trusted.
  for (const hit of hits) {
    const row = byKey.get(timerKey(hit));
    assert.ok(row);
    assert.equal(
      hit.cadenceMs,
      row.cadenceMs,
      `${hit.path}:${hit.line} runs every ${String(hit.cadenceMs)} ms but its manifest row ` +
        `claims ${row.cadenceMs} ms`
    );
  }
});

test("the scan still sees the code it is supposed to read", () => {
  // Anti-vacuity, both scanners, and the only thing standing between this file
  // and a test that passes because its regexes stopped matching anything. A
  // `listen()` wrapper, a renamed helper, or a timer moved behind a utility
  // would each make the scan quietly read an empty tree while the manifests
  // still look authoritative.
  const sources = realSources();
  assert.ok(sources.length > 0, "the src/ scan found no TypeScript files at all");

  const events = new Set(sources.flatMap(listenHits).map((h) => h.event));
  assert.ok(
    events.has("pty-output"),
    "the scan cannot see the pty-output listener — that is the app's only per-chunk producer " +
      "stream (performance.md §2 P2), so a scan that misses it is reading nothing worth reading"
  );

  const timerKeys = sources.flatMap(intervalHits).map(timerKey);
  assert.ok(
    timerKeys.some((k) => k.startsWith("src/tabbar.ts@")),
    "the scan cannot see the tab strip's poll interval — the app-lifetime timer INV-4 was " +
      "written for; its absence means the setInterval scanner has drifted, not that it was removed"
  );

  // And the counting half: every call site must have produced a hit. This is
  // what catches an event name that is not a literal, or a call shape the
  // extractor's regex does not cover — cases where the scan would otherwise
  // under-report and the manifest would pass over a real listener.
  for (const src of sources) {
    assert.equal(
      listenHits(src).length,
      countCalls(src.text, LISTEN_CALL_SRC),
      `${src.path}: a listen() call site produced no event name. If the name is not a string ` +
        `literal it needs a manifest decision (INV-3), and if this is prose in a comment, reword it`
    );
    assert.equal(
      intervalHits(src).length,
      countCalls(src.text, INTERVAL_CALL_SRC),
      `${src.path}: a setInterval() call site was not extracted — same rule as above (INV-4)`
    );
  }
});

test("every declared bound and policy carries an argument that still points at real code", () => {
  const RATES: RateClass[] = ["producer", "cadenced", "lifecycle", "gesture"];
  const BOUNDS: Bound[] = ["backend-coalesced", "rAF-gated", "throttled", "argued-none"];
  const POLICIES: VisibilityPolicy[] = ["gated", "component-scoped", "argued"];
  const ISSUE = /#\d+/;

  for (const row of STREAMS) {
    const at = `STREAMS["${row.event}"]`;
    assert.ok(RATES.includes(row.rate), `${at}: unknown rate class "${row.rate}"`);
    assert.ok(
      BOUNDS.includes(row.bound),
      `${at}: "${row.bound}" is not one of performance.md §3 INV-3's bounds — a new kind of ` +
        `bound is a design-note change first`
    );
    assert.ok(
      existsSync(new URL(row.cite, REPO)),
      `${at}: cite "${row.cite}" does not exist — an argument that points at a deleted file is ` +
        `not an argument`
    );
    assert.ok(
      row.reason.length >= 60,
      `${at}: the reason has to say what bounds the stream (or what it costs unbounded), in a sentence`
    );
    if (row.bound === "rAF-gated") {
      const text = readFileSync(new URL(row.cite, REPO), "utf8");
      assert.match(
        text,
        /requestAnimationFrame\s*\(/,
        `${at}: claims the P5 rAF gate but ${row.cite} contains no requestAnimationFrame — the ` +
          `gate was removed or moved, and the claim went stale with it`
      );
    }
    if (row.bound === "throttled") {
      const text = readFileSync(new URL(row.cite, REPO), "utf8");
      assert.match(
        text,
        /throttle/i,
        `${at}: claims a throttle but ${row.cite} has no throttle in it`
      );
    }
    // INV-3's teeth: producer-rate is the class the invariant exists for, so
    // one may be left unbounded only as declared, owned debt. Everything else
    // may argue its rate away in prose.
    if (row.rate === "producer" && row.bound === "argued-none") {
      assert.match(
        row.debt ?? "",
        ISSUE,
        `${at}: a producer-rate stream with no bound must name the issue that owns bounding it ` +
          `(performance.md §3 INV-3) — "argued-none" is not a place to park one quietly`
      );
    }
    if (row.debt !== null) {
      assert.match(row.debt, ISSUE, `${at}: a debt row must name its owning issue`);
    }
  }

  for (const row of TIMERS) {
    const at = `TIMERS["${row.key}"]`;
    assert.ok(
      POLICIES.includes(row.policy),
      `${at}: "${row.policy}" is not one of performance.md §3 INV-4's visibility policies`
    );
    assert.ok(
      row.reason.length >= 60,
      `${at}: a timer that is not visibility-gated owes a sentence saying what it costs while hidden`
    );
    if (row.debt !== null) {
      assert.match(row.debt, ISSUE, `${at}: a debt row must name its owning issue`);
    }
  }
});

test("a timer's `gated` claim and the file's actual gate agree, in both directions", () => {
  // The one row-level claim that is cheap to verify end to end, checked as an
  // equivalence so it does work in BOTH states of the world. Today nothing in
  // `src/` consults `document.hidden` or `visibilitychange` (#743's census,
  // grep-confirmed), so it is the second direction that runs: no row may claim
  // `gated`. When S6 lands `pollgate.ts` the first direction takes over and the
  // rows it wires must be upgraded from `argued`/`component-scoped` to `gated`
  // in the same PR — which is the point of recording today's truth here rather
  // than the aspiration.
  const GATE = /document\.hidden|visibilitychange|pollgate/;
  for (const row of TIMERS) {
    const file = row.key.split("@")[0];
    const gated = GATE.test(readFileSync(new URL(file, REPO), "utf8"));
    if (row.policy === "gated") {
      assert.ok(
        gated,
        `${row.key}: declares a visibility gate, but ${file} never consults document.hidden, ` +
          `listens for visibilitychange, or goes through the poll gate`
      );
    } else {
      assert.ok(
        !gated,
        `${file} now has a visibility gate but ${row.key} is still declared "${row.policy}" — ` +
          `upgrade the row to "gated" (or say in its reason why THIS timer is deliberately ` +
          `outside the gate its own file uses)`
      );
    }
  }
});

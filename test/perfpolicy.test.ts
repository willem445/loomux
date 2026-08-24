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
// same stated bound E1 accepts for call chains (§3). It reads every `.ts`
// under `src/` at any depth, but a listener registered from Rust-side
// generated code, or with an event name that is not a literal, is not
// something it can name — so it refuses to pass over one instead, by counting
// call sites and requiring the count to match what it extracted.
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

/** Every `listen()` in `src/*.ts`, keyed by event name. Seeded from the #743
 *  census (plan part 2b, comment 5162018391) and kept at today's truth, never
 *  at the target state. Its four debt rows were the census's four unbounded
 *  streams; S5 bounded them, so each now names the mechanism that does it and
 *  a `debt` here means a gap that is still open. */
const STREAMS: StreamRow[] = [
  {
    event: "pty-output",
    rate: "producer",
    bound: "backend-coalesced",
    cite: "src-tauri/src/ptyout.rs",
    reason:
      "The app's only per-chunk producer flood, and now its best-covered stream: P2 bounds it " +
      "backend-side to <=1 event per pane per 16 ms with a 64 KiB batch cap and a leading edge " +
      "(#714), and unfocused panes are throttled again handler-side by panethrottle.ts (#720/#733). " +
      "That handler-side throttle is OFF while the document is hidden (#813): its saving is " +
      "render passes, and a hidden page schedules none (RenderDebouncer is rAF-driven), while " +
      "the deferral's own setTimeout is clamped by the hidden page and sits in front of xterm's " +
      "query auto-replies — the one path out of the terminal that is not display.",
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
    bound: "throttled",
    cite: "src/refreshthrottle.ts",
    reason:
      "The emit itself is a first bound: gitwatch polls at 1 s and emits only when the signature " +
      "changed, so <=1/s per watched pane. Both halves of the pane's reaction — the git view's " +
      "refresh and the header's dir_info read, which used to ride every event ungated (#743 S5) " +
      "— then run the same leading-edge REPO_SIGNAL_WINDOW_MS (500 ms) policy, each in its own " +
      "window (the view's advances only while it is visible), so each is <=1 pass per window. " +
      "dir_info's own sync dispatch is E1's row to close, not this one's.",
    debt: null,
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
    event: "models-detected",
    rate: "lifecycle",
    bound: "argued-none",
    cite: "src-tauri/src/modelwire.rs",
    reason:
      "The startup model sweep (#1020), which runs ONCE per app run and emits at most one event " +
      "per CLI that has a PROTOCOLS row — one today (claude), four if every SUPPORTED_CLIS entry " +
      "ever gained one. There is no rate to bound: the producer is a single sequential pass that " +
      "then exits. The per-event cost is bounded on the other side too — `acceptReport` drops a " +
      "report that changed nothing before any listener is called, so an empty sweep result " +
      "repaints nothing at all.",
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
    bound: "rAF-gated",
    cite: "src/framegate.ts",
    reason:
      "P5, the same gate ft-files uses, as a module: the backend emits one batch per 8 files " +
      "hashed and each paint is a querySelectorAll over every visible row, so the batches set a " +
      "dirty flag and one requestAnimationFrame repaints the column at most once per frame " +
      "(#743 S5). The run's final batch paints straight through — nothing left to coalesce.",
    debt: null,
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
    cite: "src/attentiongate.ts",
    reason:
      "A 3 s backend tick, so the rate is a constant; what needed bounding was the per-tick work, " +
      "an O(panes x tabs) re-badge paid whether or not anything moved. The handler now compares " +
      "the payload AND the pane population against the last applied pass and returns before " +
      "touching a pane when neither changed (#743 S5). Not one of INV-3's three named mechanisms " +
      "— a diff is not a coalescer — so it is argued here rather than mislabelled as one.",
    debt: null,
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
    event: "orch-queue-depth",
    rate: "cadenced",
    bound: "argued-none",
    cite: "src-tauri/src/orchestration/mod.rs",
    reason:
      "The delivery-queue depth badge (#814), pushed from the same 3 s attention tick as " +
      "orch-attention because the age it shows must keep growing and the frontend has no clock " +
      "(adding one would be a TIMERS row, not a free lunch). Bounded backend-side by a skip, not " +
      "by a coalescer: queue_depth_push emits nothing when the set is identical to the last one " +
      "pushed, and the wait it carries is coarsened to what the badge can render differently " +
      "(coarsen_waiting_ms — 1 s under a minute, 1 min above), so an app with nothing queued emits " +
      "zero events and a pane stuck for an hour costs ~1/min rather than 1/tick. The skip is itself " +
      "bounded (INV-6): a non-empty set is re-pushed every QUEUE_DEPTH_REPUSH_MS (30 s) because the " +
      "suppression's signal is a memory of an emit, not an acknowledgement — a reloaded webview, a " +
      "lost emit, or a pane restored/spawned after the last push would otherwise wear no badge on " +
      "exactly the stalled pane whose reading never changes, so that window is also this stream's " +
      "worst-case latency for a newly-appeared pane. Not " +
      "one of INV-3's three named mechanisms — a skip is not a coalescer — so it is argued here, " +
      "the same way orch-attention's diff is.",
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
    event: "orch-questions-changed",
    rate: "producer",
    bound: "throttled",
    cite: "src/refreshgate.ts",
    reason:
      "Emitted on every questions.json write — an ask, an answer, a withdraw, a prune — so " +
      "its rate is set by the orchestrator, not by a clock. Both the NEEDS-YOU panel and the " +
      "task board (#1091 slice G's board-marker chip) refresh through their OWN " +
      "CoalescingRefresh (#743 S5): single-flight with a trailing-edge merge, so a burst costs " +
      "each open view the refetch already in flight plus exactly one more, and the trailing " +
      "run reads the final registry. Each view already shares that same gate with its own " +
      "orch-tasks-changed listener (below), so a simultaneous burst on both streams still " +
      "coalesces to one refresh per view, not two. Both views are ALSO visibility-gated as of " +
      "#1318 (src/wakegate.ts): a closed board or panel drops the wake outright rather than " +
      "coalescing it, and show() refreshes unconditionally so nothing is lost by dropping it.",
    debt: null,
  },
  {
    event: "orch-needs-you-changed",
    rate: "producer",
    bound: "throttled",
    cite: "src/refreshgate.ts",
    reason:
      "Emitted on every needs-you.json write — a raise, a resolve, a withdraw, the board hook, " +
      "the one-shot migration (#1151) — so its rate is set by agents and the board, not by a " +
      "clock. The NEEDS-YOU panel is the only listener and it refreshes through the SAME " +
      "CoalescingRefresh (#743 S5) its orch-questions-changed and orch-tasks-changed listeners " +
      "already share: single-flight with a trailing-edge merge, so a burst costs the refetch " +
      "already in flight plus exactly one more and the trailing run reads the final registry. " +
      "Sharing the gate is what bounds the worst case here, which is one BOARD write: the " +
      "demo-gate hook lives inside upsert_task, so a status transition emits this AND " +
      "orch-tasks-changed, and an ungated third listener would have doubled a board burst's " +
      "cost for this panel rather than added to it. Clear-completed is deliberately not on this " +
      "stream at all — it writes only the watermark marker, emits nothing, and the panel applies " +
      "the stamp the command returns. As of #1318 the panel is visibility-gated too " +
      "(src/wakegate.ts): all three of its streams drop their wake outright while it is off " +
      "screen, and show() refreshes unconditionally so nothing is lost by dropping them.",
    debt: null,
  },
  {
    event: "orch-tasks-changed",
    rate: "producer",
    bound: "throttled",
    cite: "src/refreshgate.ts",
    reason:
      "Emitted on EVERY write_tasks, which agents drive in bursts. Each open board refreshes " +
      "through CoalescingRefresh: single-flight with a trailing-edge merge, so a burst of N " +
      "writes costs the refetch already in flight plus exactly one more, per view, and the " +
      "trailing one reads the final board so nothing is lost (#743 S5). Self-clocking — its " +
      "window is the duration of a refresh, so a slower backend coalesces harder. " +
      "WHICH boards pay that at all is the second bound (#1318, src/wakegate.ts): 'open' meant " +
      "'ever opened' until the board and the NEEDS-YOU panel got the hide hook every woken view " +
      "owes, so an off-screen board now costs one boolean instead of a refetch plus a rebuild " +
      "that is super-linear in the board. show() refreshes unconditionally, so nothing is lost.",
    debt: null,
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

/** Every `setInterval()` in `src/*.ts`. Seeded from the #743 census (plan part
 *  2b), which found that NOTHING in `src/` had ever read `document.hidden` —
 *  every poll here was component-scoped at best and none of them knew whether
 *  the window was on screen. S6 wired the five that drive IPC or rendering
 *  through `src/pollgate.ts`, so `gated` rows are now the majority and the
 *  gate's own recheck ticker is a row like any other. Kept at today's truth,
 *  never at the target state. */
const TIMERS: TimerRow[] = [
  {
    key: "src/tabbar.ts@4000",
    cadenceMs: 4000,
    policy: "gated",
    reason:
      "App-lifetime tick: armed in the tab strip's constructor and never cleared, and each tick's " +
      "pollStatus() loops EVERY group-bound tab invoking groupSummary plus groupUsage — the " +
      "largest standing IPC cost in the app. The PollGate stops the interval outright while the " +
      "window is hidden (not an early return inside the tick) and runs one catch-up poll on the " +
      "way back (#743 S6). Cadence tiers by tab position were considered and rejected in-slice: " +
      "a background tab's badge is what the strip is FOR, and S4 already made groupUsage share " +
      "one ~1 s backend snapshot across callers.",
    debt: null,
  },
  {
    key: "src/tabbar.ts@PREVIEW_REFRESH_MS",
    cadenceMs: 700,
    policy: "gated",
    reason:
      "Alive only while a hover preview is open, and now gated on top of that. Component scope " +
      "looked sufficient — a hover is a pointer on the strip — but nothing synthesizes a " +
      "mouseleave when a window is minimized, so a preview open at that moment kept " +
      "re-serializing up to eight panes every 700 ms. No IPC per tick; the cost is webview-thread " +
      "render work, which is exactly what INV-4 asks a hidden window not to pay.",
    debt: null,
  },
  {
    key: "src/groupview.ts@POLL_MS",
    cadenceMs: 2000,
    policy: "gated",
    reason:
      "Armed by show() and cleared by hide() on every close/eviction path, and gated on window " +
      "visibility inside that scope (#743 S6) — the two are different questions, and a panel left " +
      "open behind a minimized window used to keep paying Promise.all of nine invokes plus a full " +
      "render every 2 s. The gate's arm keeps the defensive clear-before-arm show() used to do.",
    debt: null,
  },
  {
    key: "src/auditview.ts@FOLLOW_MS",
    cadenceMs: 1500,
    policy: "gated",
    reason:
      "Opt-in: armed only by the follow toggle, cleared by the toggle, by a close/eviction " +
      "(AuditView.hide, #1318) and by dispose(), so it outlives neither the view nor the panel " +
      "being on screen — and gated within that, so an armed follow behind a hidden window " +
      "refetches nothing and re-renders nothing until the window is back, then catches up once " +
      "(#743 S6). Until #1318 the close was the missing one: PollGate pauses this behind a hidden " +
      "WINDOW, and nothing stopped it behind a closed PANEL.",
    debt: null,
  },
  {
    key: "src/timelineview.ts@FOLLOW_MS",
    cadenceMs: 1500,
    policy: "gated",
    reason:
      "The timeline's follow toggle, same shape and same gate as auditview's: armed on toggle, " +
      "cleared on toggle, on close (hide()) and on dispose, suppressed while the window is " +
      "hidden (#743 S6) — the view that had the close half right first. Its gh " +
      "half is separately self-gated to GH_REFRESH_MS (60 s) in timelinechrome.ts, so a tick is " +
      "one orch_audit refetch, not two shell-outs.",
    debt: null,
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
  {
    key: "src/pollgate.ts@HIDDEN_RECHECK_MS",
    cadenceMs: 5000,
    policy: "gated",
    reason:
      "The gate's own release path, and the only timer here that runs ONLY while the window is " +
      "hidden: a suppressed poll re-reads document.visibilityState every 5 s instead of trusting " +
      "the visibilitychange event it was suppressed by, so a lost or never-delivered event cannot " +
      "wedge a panel permanently (performance.md §2 P4; the standing rule that a suppression on a " +
      "fallible signal owes an independent release). One boolean read per wake, no IPC, no paint " +
      "— a hidden window still makes zero data polls, which is the point of the slice.",
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

/** Every file under `dir` with `ext`, **recursively**, as paths relative to
 *  `dir` with `/` separators.
 *
 *  The normalization is not cosmetic: `readdirSync(..., {recursive: true})`
 *  yields `perfhole\probe.ts` on Windows and `perfhole/probe.ts` elsewhere, so
 *  without it a nested file's `path` — the thing failure messages print and
 *  `cite` values are compared against — would be platform-dependent. The
 *  normalized form also resolves back to a readable URL on both. */
function walk(dir: URL, ext: string): string[] {
  return readdirSync(dir, { recursive: true })
    .map((entry) => String(entry).replace(/\\/g, "/"))
    .filter((f) => f.endsWith(ext))
    .sort();
}

/** The scan's input: every `.ts` under `src/`, at any depth.
 *
 *  Depth matters more than it looks. A flat read of `src/` is how this test
 *  fails SILENTLY — a listener in a `src/<subdir>/` nobody added yet is not a
 *  hit the manifest is missing, it is a file the scan never opens, and the
 *  suite stays green with an undeclared stream in the tree (found in review on
 *  this PR, reproduced with a planted probe). That is the one failure mode the
 *  header's whole argument is against, so the walk descends. */
function realSources(): Source[] {
  return walk(SRC_DIR, ".ts").map((f) => ({
    path: `src/${f}`,
    text: readFileSync(new URL(f, SRC_DIR), "utf8"),
  }));
}

test("the walk descends into subdirectories, on this platform", () => {
  // The specimen for the recursion itself. `src/` is flat today, so nothing in
  // the manifest tests would notice if the walk quietly stopped descending —
  // which is exactly the state this file shipped in. Pinning it needs a tree
  // that HAS subdirectories, and the repo already has one: `src-tauri/src/`.
  // Walking it for `.rs` proves descent and separator normalization together,
  // without planting a file in `src/` or writing to a temp directory.
  const rust = walk(new URL("src-tauri/src/", REPO), ".rs");
  const nested = rust.filter((f) => f.includes("/"));
  assert.ok(
    nested.includes("orchestration/mod.rs"),
    `the walk did not descend: it found ${rust.length} .rs files but ${nested.length} below the ` +
      `top level, and not orchestration/mod.rs. A non-recursive walk makes a listener in a new ` +
      `src/ subdirectory invisible rather than undeclared — silence, not a failure`
  );
  assert.ok(
    nested.every((f) => !f.includes("\\")),
    `the walk leaked a platform separator into a path: ${nested.find((f) => f.includes("\\"))} — ` +
      `paths are normalized to "/" so failure messages and cites read the same everywhere`
  );
});

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

/** The `listen(` shapes in `src/transport.ts` that are DECLARATIONS of the
 *  primitive rather than subscriptions (#905): the interface member, the local
 *  implementation's property, and the exported forwarder. Each takes an
 *  `event: string` parameter, so none can produce a literal, and none registers
 *  anything by existing.
 *
 *  Pinned as a COUNT rather than skipping the file, so a fourth `listen(` in the
 *  seam fails here and has to be argued rather than inherited. The seam does not
 *  blind the scan: every real subscription still reads `listen("event-name", cb)`
 *  in its own module, with its own literal, and is extracted exactly as before —
 *  #905 moved which module imports Tauri, not what this scanner can see. */
const SEAM_LISTEN_DECLARATIONS: Record<string, number> = { "src/transport.ts": 3 };

test("the scan still sees the code it is supposed to read", () => {
  // Anti-vacuity, both scanners, and the only thing standing between this file
  // and a test that passes because its regexes stopped matching anything. A
  // renamed helper, a timer moved behind a utility, or a wrapper that HID the
  // event name (unlike the #905 seam, which passes it straight through at every
  // call site) would each make the scan quietly read an empty tree while the
  // manifests still look authoritative.
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
      listenHits(src).length + (SEAM_LISTEN_DECLARATIONS[src.path] ?? 0),
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

const RATES: RateClass[] = ["producer", "cadenced", "lifecycle", "gesture"];
const BOUNDS: Bound[] = ["backend-coalesced", "rAF-gated", "throttled", "argued-none"];
const POLICIES: VisibilityPolicy[] = ["gated", "component-scoped", "argued"];
const ISSUE = /#\d+/;

/** Reads a repo-relative file, or `null` if it does not exist. Injected so the
 *  row rules can be exercised against synthetic rows — see the unit tests
 *  below, which is how the `throttled` branch stays live code while no shipped
 *  stream is bound that way. */
type ReadText = (path: string) => string | null;

const readRepoText: ReadText = (path) => {
  const url = new URL(path, REPO);
  return existsSync(url) ? readFileSync(url, "utf8") : null;
};

/** Everything wrong with one stream row, as sentences. Empty = the row's
 *  argument exists, is a legal shape, and still points at real code. */
function streamRowProblems(row: StreamRow, readText: ReadText): string[] {
  const at = `STREAMS["${row.event}"]`;
  const problems: string[] = [];
  if (!RATES.includes(row.rate)) problems.push(`${at}: unknown rate class "${row.rate}"`);
  if (!BOUNDS.includes(row.bound)) {
    problems.push(
      `${at}: "${row.bound}" is not one of performance.md §3 INV-3's bounds — a new kind of ` +
        `bound is a design-note change first`
    );
  }
  const cited = readText(row.cite);
  if (cited === null) {
    problems.push(
      `${at}: cite "${row.cite}" does not exist — an argument that points at a deleted file is ` +
        `not an argument`
    );
  }
  if (row.reason.length < 60) {
    problems.push(
      `${at}: the reason has to say what bounds the stream (or what it costs unbounded), in a sentence`
    );
  }
  // A bound that names a mechanism must still find that mechanism where it
  // says it lives — a claim whose cite no longer implements it is the failure
  // mode a manifest has that a lint does not.
  if (cited !== null && row.bound === "rAF-gated" && !/requestAnimationFrame\s*\(/.test(cited)) {
    problems.push(
      `${at}: claims the P5 rAF gate but ${row.cite} contains no requestAnimationFrame — the ` +
        `gate was removed or moved, and the claim went stale with it`
    );
  }
  if (cited !== null && row.bound === "throttled" && !/throttle/i.test(cited)) {
    problems.push(`${at}: claims a throttle but ${row.cite} has no throttle in it`);
  }
  // INV-3's teeth: producer-rate is the class the invariant exists for, so
  // one may be left unbounded only as declared, owned debt. Everything else
  // may argue its rate away in prose.
  if (row.rate === "producer" && row.bound === "argued-none" && !ISSUE.test(row.debt ?? "")) {
    problems.push(
      `${at}: a producer-rate stream with no bound must name the issue that owns bounding it ` +
        `(performance.md §3 INV-3) — "argued-none" is not a place to park one quietly`
    );
  }
  if (row.debt !== null && !ISSUE.test(row.debt)) {
    problems.push(`${at}: a debt row must name its owning issue`);
  }
  return problems;
}

/** Everything wrong with one timer row. */
function timerRowProblems(row: TimerRow): string[] {
  const at = `TIMERS["${row.key}"]`;
  const problems: string[] = [];
  if (!POLICIES.includes(row.policy)) {
    problems.push(`${at}: "${row.policy}" is not one of performance.md §3 INV-4's visibility policies`);
  }
  if (row.reason.length < 60) {
    problems.push(
      `${at}: a timer that is not visibility-gated owes a sentence saying what it costs while hidden`
    );
  }
  if (row.debt !== null && !ISSUE.test(row.debt)) {
    problems.push(`${at}: a debt row must name its owning issue`);
  }
  return problems;
}

test("every declared bound and policy carries an argument that still points at real code", () => {
  assert.deepEqual(
    STREAMS.flatMap((r) => streamRowProblems(r, readRepoText)),
    []
  );
  assert.deepEqual(
    TIMERS.flatMap(timerRowProblems),
    []
  );
});

test("neither manifest can hold two rows for the same subject", () => {
  // The manifest side of a guard the scan side already has. A duplicate would
  // not fail either set comparison — `new Map(TIMERS.map(...))` silently keeps
  // the last row, and STREAMS is compared as a Set — so two contradictory
  // rows for one event could both sit here, and whichever the code happened to
  // read would be "the" declared bound.
  const events = STREAMS.map((r) => r.event);
  assert.deepEqual(
    events.filter((e, i) => events.indexOf(e) !== i),
    [],
    "STREAMS declares the same event twice — delete one; two rows for one stream means the " +
      "manifest asserts two different bounds and the reader picks"
  );
  const keys = TIMERS.map((r) => r.key);
  assert.deepEqual(
    keys.filter((k, i) => keys.indexOf(k) !== i),
    [],
    "TIMERS declares the same timer twice — the Map lookup keeps only the last, so the other " +
      "row's cadence and policy are never checked against anything"
  );
});

test("the row rules fire on every shape they are written for, including `throttled`", () => {
  // Synthetic rows, so every branch of the row rules stays live whatever the
  // shipped manifest happens to contain — a branch no row exercises is untested
  // code in a test file, and the day a row first needs it is the wrong day to
  // find it broken.
  const files: Record<string, string> = {
    "src/with-raf.ts": "requestAnimationFrame(() => paint());",
    "src/with-throttle.ts": "// leading-edge throttle window\nconst throttleMs = 100;",
    "src/plain.ts": "export const nothing = 1;",
  };
  const read: ReadText = (p) => files[p] ?? null;
  const ok: StreamRow = {
    event: "fake",
    rate: "lifecycle",
    bound: "argued-none",
    cite: "src/plain.ts",
    reason: "x".repeat(60),
    debt: null,
  };
  const only = (row: StreamRow): string => {
    const problems = streamRowProblems(row, read);
    assert.equal(problems.length, 1, `expected exactly one problem, got: ${problems.join(" | ")}`);
    return problems[0];
  };

  assert.deepEqual(streamRowProblems(ok, read), [], "a well-formed row has no problems");
  assert.deepEqual(
    streamRowProblems({ ...ok, bound: "throttled", cite: "src/with-throttle.ts" }, read),
    [],
    "a throttled row citing a file that HAS a throttle passes"
  );
  assert.match(
    only({ ...ok, bound: "throttled", cite: "src/plain.ts" }),
    /claims a throttle but src\/plain\.ts has no throttle in it/
  );
  assert.deepEqual(
    streamRowProblems({ ...ok, bound: "rAF-gated", cite: "src/with-raf.ts" }, read),
    [],
    "an rAF-gated row citing a file that HAS the gate passes"
  );
  assert.match(only({ ...ok, bound: "rAF-gated", cite: "src/plain.ts" }), /no requestAnimationFrame/);
  assert.match(only({ ...ok, cite: "src/gone.ts" }), /does not exist/);
  assert.match(only({ ...ok, reason: "too short" }), /in a sentence/);
  assert.match(only({ ...ok, rate: "producer" }), /must name the issue that owns bounding it/);
  assert.match(only({ ...ok, debt: "S6, no issue number" }), /must name its owning issue/);
  // ...and the same for the timer rules.
  const timer: TimerRow = {
    key: "src/fake.ts@1000",
    cadenceMs: 1000,
    policy: "argued",
    reason: "x".repeat(60),
    debt: null,
  };
  assert.deepEqual(timerRowProblems(timer), []);
  assert.match(timerRowProblems({ ...timer, reason: "short" })[0], /what it costs while hidden/);
  assert.match(timerRowProblems({ ...timer, debt: "S6" })[0], /must name its owning issue/);
});

test("a timer's `gated` claim and the file's actual gate agree, in both directions", () => {
  // The one row-level claim that is cheap to verify end to end, checked as an
  // equivalence so it does work in BOTH states of the world. Both directions
  // now run: S6 wired five timers through `pollgate.ts`, so those rows must
  // claim `gated` and their files must show it, while `src/main.ts`'s ungated
  // 20 s tick is held to the other direction — it may not quietly acquire a
  // gate while still declaring itself `argued`.
  //
  // The check is per FILE, not per timer, which is deliberate and has teeth: a
  // file that adopts the gate for one of its timers cannot leave a second one
  // outside it and say nothing. That is what pulled the tab strip's 700 ms
  // hover-preview timer into S6 — the row had to become true or the wiring had
  // to. There is deliberately NO prose escape: a `reason` saying why one timer
  // sits outside its file's gate is not read here and must not be written as if
  // it were, because a check a sentence can satisfy is not an equivalence. If a
  // file ever genuinely needs one gated and one ungated timer, that is an
  // enforcement change — extend this assertion under #767 (E2 hardening), with
  // the argument written down, rather than arguing past it in a row.
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
          `upgrade the row to "gated", or take the timer out of the gate. This is a per-file ` +
          `equivalence and nothing in the row's prose relaxes it: one timer inside its file's ` +
          `gate and another outside it is an enforcement change, so it goes to #767, not into a ` +
          `reason field`
      );
    }
  }
});

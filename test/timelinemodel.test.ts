// Unit tests for the progress timeline's pure event model (src/timelinemodel.ts).
// These pin the EXTRACTION TABLE — one audit action in, exactly one event kind
// with the right fields out — plus the four honesty properties the view is
// built on: an undatable event is parked and counted, an unknown action lands
// in a default-off lane and is tallied, a capped read says so, and a merge
// reported by both sources collapses to one dot without ever swallowing a
// merge only one source saw. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  AUDIT_VIEW_LIMIT,
  CATEGORY_ORDER,
  DEFAULT_CATEGORIES,
  DEFAULT_WINDOW_ID,
  MIN_WINDOW_MS,
  categoryOf,
  coverageNotes,
  eventKey,
  extractTimeline,
  filterTimeline,
  resolveWindow,
  retainExpandedEvents,
  windowPreset,
  type AuditEntryLike,
  type GhActivityLike,
  type GhIssueActivityLike,
  type GhPrActivityLike,
  type TimelineEvent,
} from "../src/timelinemodel.ts";

const T0 = Date.UTC(2026, 6, 1, 12, 0, 0); // a fixed instant; no Date.now() here
const MIN = 60_000;
const HOUR = 3_600_000;

function entry(action: string, detail: unknown, over: Partial<AuditEntryLike> = {}): AuditEntryLike {
  return { ts_ms: T0, actor: "loomux", action, detail, ...over };
}

function only(events: TimelineEvent[], kind: string): TimelineEvent {
  const hits = events.filter((e) => e.kind === kind);
  assert.equal(hits.length, 1, `expected exactly one ${kind} event, got ${hits.length}`);
  return hits[0];
}

function ghActivity(over: Partial<GhActivityLike> = {}): GhActivityLike {
  return { issues: [], prs: [], limit: 100, issues_truncated: false, prs_truncated: false, ...over };
}

function issue(over: Partial<GhIssueActivityLike> = {}): GhIssueActivityLike {
  return {
    number: 608,
    title: "Workflow visualization pane",
    state: "OPEN",
    created_at: "2026-07-01T09:00:00Z",
    closed_at: null,
    updated_at: "2026-07-01T12:00:00Z",
    url: "https://example.invalid/608",
    ...over,
  };
}

function pr(over: Partial<GhPrActivityLike> = {}): GhPrActivityLike {
  return {
    number: 644,
    title: "gh_activity",
    state: "OPEN",
    created_at: "2026-07-01T10:00:00Z",
    closed_at: null,
    merged_at: null,
    updated_at: "2026-07-01T12:00:00Z",
    url: "https://example.invalid/644",
    head_ref: "feat/608-viz-slice-a",
    ...over,
  };
}

// --- the extraction table --------------------------------------------------

test("group lifecycle actions become one group event each", () => {
  const { events } = extractTimeline([
    entry("group-create", { repo: "C:/Projects/loomux", max_agents: 6 }),
    entry("group-pause", {}, { ts_ms: T0 + 1 }),
    entry("group-resume", {}, { ts_ms: T0 + 2 }),
    entry("group-end", {}, { ts_ms: T0 + 3 }),
  ]);
  assert.equal(events.length, 4);
  assert.deepEqual(
    events.map((e) => e.kind),
    ["group", "group", "group", "group"]
  );
  assert.match(events[0].label, /group created/);
  assert.match(events[0].label, /C:\/Projects\/loomux/);
  assert.match(events[1].label, /paused/);
  assert.equal(categoryOf(events[0].kind), "group");
});

test("agent-spawn carries the display name and role; the id is the fallback", () => {
  const { events } = extractTimeline([
    entry("agent-spawn", { agent: "w-142", name: "w: 608-B viz", role: "worker", task: "slice B" }),
    entry("agent-spawn", { agent: "orch-1", role: "orchestrator" }, { ts_ms: T0 + 1 }),
  ]);
  assert.equal(events[0].agent, "w: 608-B viz");
  assert.match(events[0].label, /w: 608-B viz spawned \(worker\)/);
  assert.equal(events[1].agent, "orch-1", "no display name — fall back to the agent id");
});

test("agent-exit, agent-kill and idle-kill all land in the agent-exit lane", () => {
  const { events } = extractTimeline([
    entry("agent-exit", { agent: "w-1", exit_code: 0 }),
    entry("agent-kill", { agent: "w-2", initiator: "human" }, { ts_ms: T0 + 1 }),
    entry("idle-kill", { agent: "w-3", name: "w: idle", idle_minutes: 45 }, { ts_ms: T0 + 2 }),
  ]);
  assert.deepEqual(
    events.map((e) => e.kind),
    ["agent-exit", "agent-exit", "agent-exit"]
  );
  assert.match(events[0].label, /w-1 exited \(code 0\)/);
  assert.match(events[1].label, /killed by human/);
  assert.match(events[2].label, /reaped — idle 45m/);
  assert.equal(events[2].agent, "w: idle");
});

test("every prompt is one delivery event, labelled sender -> recipient", () => {
  const { events } = extractTimeline([
    entry("prompt", { to: "w-142", text: "You are a worker\nsecond line" }, { actor: "loomux" }),
    entry("prompt", { to: "w-142", text: "review findings on #644" }, { actor: "orch-1", ts_ms: T0 + 1 }),
  ]);
  assert.deepEqual(
    events.map((e) => e.kind),
    ["delivery", "delivery"]
  );
  // The audit detail is {to, text} and nothing else, so a kickoff and a
  // mid-stream delivery are the same shape — the model must not claim to tell
  // them apart. The sender IS in the data, and that is what the label says.
  assert.equal(events[0].label, "loomux → w-142: You are a worker");
  assert.equal(events[1].label, "orch-1 → w-142: review findings on #644");
  assert.equal(events[0].agent, "w-142", "the recipient is who the event is about");
});

test("a report tool-call becomes a report event; every other tool-call is ops", () => {
  const { events, unmapped } = extractTimeline([
    entry(
      "tool-call",
      { tool: "report", args: { outcome: "done", ref: "#644", detail_url: "https://x.invalid" } },
      { actor: "w-142" }
    ),
    entry("tool-call", { tool: "list_agents", args: {} }, { actor: "orch-1", ts_ms: T0 + 1 }),
  ]);
  const rep = only(events, "report");
  assert.equal(rep.agent, "w-142");
  assert.match(rep.label, /w-142 reports done \(#644\)/);
  assert.equal(only(events, "ops").label.startsWith("tool-call"), true);
  assert.equal(unmapped["tool-call"], 1, "the ops-lane tally names what landed there");
});

test("a legacy report (status/summary) is read too", () => {
  const { events } = extractTimeline([
    entry("tool-call", { tool: "report", args: { status: "blocked", summary: "needs a call" } }, { actor: "w-9" }),
  ]);
  assert.match(only(events, "report").label, /w-9 reports blocked/);
});

test("task-upsert becomes a task-status event carrying its issue and PR", () => {
  const { events } = extractTimeline([
    entry("task-upsert", {
      id: "t-3",
      title: "Slice B of the viz pane",
      status: "in_progress",
      issue: "608",
      pr: "#645",
      assignee: "w-142",
    }),
    entry("task-delete", { id: "t-3" }, { ts_ms: T0 + 1 }),
  ]);
  assert.equal(events[0].kind, "task-status");
  assert.equal(events[0].issue, "608");
  assert.equal(events[0].pr, "645", "a '#645' reference normalizes to the bare number");
  assert.equal(events[0].agent, "w-142");
  assert.match(events[0].label, /t-3 → in_progress: Slice B of the viz pane/);
  assert.match(events[1].label, /task t-3 deleted/);
});

test("review-verdict becomes a verdict event on its PR", () => {
  const { events } = extractTimeline([
    entry("review-verdict", { pr: 644, verdict: "request_changes", block: "reviewer" }, { actor: "rev-1" }),
  ]);
  const v = only(events, "verdict");
  assert.equal(v.pr, "644", "review_verdict writes pr as a NUMBER");
  assert.equal(v.agent, "rev-1");
  assert.match(v.label, /review request_changes on #644/);
});

test("merge-gate events say the gate PERMITTED a merge, not that one happened", () => {
  // The shim writes `pr` as a string, and it audits its own decision before
  // exec'ing the real gh — which can still fail. Only gh's mergedAt proves a
  // merge, so the label must not claim one.
  const { events } = extractTimeline([
    entry("merge-gate-allowed", { base: "main", pr: "640" }, { actor: "gh-shim" }),
    entry("merge-gate-blocked", { reason: "gate-closed", base: "main", pr: "641" }, { actor: "gh-shim", ts_ms: T0 + 1 }),
  ]);
  assert.deepEqual(
    events.map((e) => e.kind),
    ["merge", "merge"]
  );
  assert.equal(events[0].pr, "640");
  assert.match(events[0].label, /merge of #640 allowed \(autonomous\)/);
  assert.doesNotMatch(events[0].label, /merged/, "the gate did not observe a merge");
  assert.match(events[1].label, /merge of #641 refused \(gate-closed\)/);
});

test("release-gate events land in the gates lane with their tag", () => {
  const { events } = extractTimeline([
    entry("release-gate-blocked", { tag: "v1.1.0", action: "push" }, { actor: "git-shim" }),
  ]);
  const r = only(events, "release");
  assert.equal(categoryOf(r.kind), "gates");
  assert.match(r.label, /release v1\.1\.0 refused/);
});

test("intake-signal is its own kind, not folded into ops or group lifecycle", () => {
  const { events, unmapped } = extractTimeline([entry("intake-signal", { summary: "2 issues labeled agent-ready" })]);
  const i = only(events, "intake");
  assert.match(i.label, /intake: 2 issues labeled agent-ready/);
  assert.equal(categoryOf("intake"), "group", "it shares the group lane but keeps its own label");
  assert.deepEqual(unmapped, {}, "a mapped action is never counted as unmapped");
});

// --- the honesty properties ------------------------------------------------

test("an unknown action is plotted in the default-off ops lane and tallied", () => {
  const { events, unmapped } = extractTimeline([
    entry("some-action-from-a-newer-loomux", { a: 1 }),
    entry("some-action-from-a-newer-loomux", { a: 2 }, { ts_ms: T0 + 1 }),
    entry("watch-register", { target: "pr_checks", id: "w1" }, { ts_ms: T0 + 2 }),
  ]);
  assert.equal(events.length, 3, "nothing is dropped");
  assert.equal(events.every((e) => e.kind === "ops"), true);
  assert.equal(unmapped["some-action-from-a-newer-loomux"], 2);
  assert.equal(unmapped["watch-register"], 1);
  assert.match(events[0].label, /some-action-from-a-newer-loomux: \{"a":1\}/);

  // ...and it is invisible until the human turns the ops chip on.
  const range = { startMs: T0 - HOUR, endMs: T0 + HOUR };
  assert.equal(filterTimeline(events, range, DEFAULT_CATEGORIES).length, 0);
  assert.equal(filterTimeline(events, range, CATEGORY_ORDER).length, 3);
});

test("ts_ms 0 is parked as undatable, never plotted at 1970", () => {
  const { events, undatable, auditOldestMs } = extractTimeline([
    entry("merge-gate-allowed", { pr: "640" }, { ts_ms: 0, actor: "gh-shim" }), // the shim's `date` fallback
    entry("merge-gate-allowed", { pr: "641" }, { actor: "gh-shim" }),
  ]);
  assert.equal(undatable, 1);
  assert.equal(events.length, 1);
  assert.equal(events[0].pr, "641");
  assert.equal(auditOldestMs, T0, "an undatable entry does not drag the coverage floor to 1970");
});

test("a row that is not an audit entry is counted, not thrown on", () => {
  const { events, malformed } = extractTimeline([
    null,
    "not an entry",
    { action: "group-end" }, // no ts_ms / actor
    { ts_ms: "later", actor: "loomux", action: "group-end" },
    entry("group-end", {}),
  ]);
  assert.equal(malformed, 4);
  assert.equal(events.length, 1);
});

test("a capped audit read reports itself as at the cap", () => {
  const rows = Array.from({ length: AUDIT_VIEW_LIMIT }, (_, i) =>
    entry("group-resume", {}, { ts_ms: T0 + i })
  );
  const x = extractTimeline(rows);
  assert.equal(x.auditAtLimit, true);
  assert.equal(x.auditCount, AUDIT_VIEW_LIMIT);
  const notes = coverageNotes(x, { startMs: T0, endMs: T0 + AUDIT_VIEW_LIMIT });
  assert.equal(notes.some((n) => n.id === "audit-cap"), true);
  assert.equal(extractTimeline(rows.slice(1)).auditAtLimit, false);
});

test("a window reaching past the loaded log says where coverage starts", () => {
  const x = extractTimeline([entry("group-create", { repo: "r" })]);
  const inside = coverageNotes(x, { startMs: T0, endMs: T0 + HOUR });
  assert.equal(
    inside.some((n) => n.id === "audit-coverage"),
    false,
    "a window starting exactly at the oldest entry covers everything loaded"
  );
  const outside = coverageNotes(x, { startMs: T0 - 12 * HOUR, endMs: T0 + HOUR });
  const note = outside.find((n) => n.id === "audit-coverage");
  assert.ok(note, "the window predates the oldest entry loaded");
  assert.match(note.text, /Audit coverage starts 2026-07-01T12:00:00Z/);
  assert.match(note.text, /not "nothing happened"/, "empty space must not read as a quiet period");
});

test("gh caps, undatable events and malformed rows each get their own note", () => {
  const x = extractTimeline(
    [entry("group-create", { repo: "r" }, { ts_ms: 0 }), 7],
    ghActivity({ issues: [issue()], prs: [pr()], issues_truncated: true, prs_truncated: true })
  );
  const ids = coverageNotes(x, { startMs: T0 - HOUR, endMs: T0 + HOUR }).map((n) => n.id);
  assert.deepEqual(ids.sort(), ["gh-cap", "malformed", "undatable"]);
  const gh = coverageNotes(x, { startMs: T0, endMs: T0 + HOUR }).find((n) => n.id === "gh-cap");
  assert.match(gh.text, /issues and PRs capped at the 100 most recently active/);
});

test("no coverage notes when nothing is missing", () => {
  const x = extractTimeline([entry("group-create", { repo: "r" })], ghActivity({ issues: [issue()] }));
  assert.deepEqual(coverageNotes(x, { startMs: T0, endMs: T0 + MIN }), []);
});

// --- gh extraction ---------------------------------------------------------

test("issues and PRs become opened/closed/merged events", () => {
  const { events, undatable } = extractTimeline(
    [],
    ghActivity({
      issues: [
        issue({ number: 1, created_at: "2026-07-01T09:00:00Z" }),
        issue({ number: 2, state: "CLOSED", created_at: "2026-07-01T08:00:00Z", closed_at: "2026-07-01T11:00:00Z" }),
      ],
      prs: [
        pr({ number: 10, created_at: "2026-07-01T10:00:00Z" }),
        pr({ number: 11, state: "MERGED", closed_at: "2026-07-01T11:30:00Z", merged_at: "2026-07-01T11:30:00Z" }),
        pr({ number: 12, state: "CLOSED", closed_at: "2026-07-01T11:45:00Z", merged_at: null }),
      ],
    })
  );
  const kinds = events.map((e) => `${e.kind}:${e.issue ?? e.pr}`);
  assert.deepEqual(kinds.sort(), [
    "issue-closed:2",
    "issue-opened:1",
    "issue-opened:2",
    "pr-closed:12",
    "pr-merged:11",
    "pr-opened:10",
    "pr-opened:11",
    "pr-opened:12",
  ]);
  assert.equal(undatable, 0, "an open issue's null closed_at is a state, not a missing timestamp");
  // A merged PR carries BOTH closed_at and merged_at; keying off closed_at
  // would render every abandoned PR as a merge.
  assert.equal(events.filter((e) => e.kind === "pr-closed").length, 1);
});

test("a gh timestamp that will not parse is undatable, not 1970", () => {
  const { events, undatable } = extractTimeline(
    [],
    ghActivity({ issues: [issue({ created_at: "" })], prs: [pr({ created_at: "not-a-date" })] })
  );
  assert.equal(undatable, 2);
  assert.equal(events.length, 0);
  assert.equal(events.every((e) => e.ts_ms > 0), true);
});

test("events come back in ascending time order across both sources", () => {
  const { events } = extractTimeline(
    [entry("group-create", { repo: "r" }, { ts_ms: Date.UTC(2026, 6, 1, 10, 0, 0) })],
    ghActivity({ issues: [issue({ created_at: "2026-07-01T09:00:00Z" })] })
  );
  assert.deepEqual(
    events.map((e) => e.kind),
    ["issue-opened", "group"]
  );
});

// --- merge dedupe ----------------------------------------------------------

test("a merge both sources saw collapses onto gh's instant, keeping the gate detail", () => {
  const mergedAt = "2026-07-01T11:30:00Z";
  const { events, mergesDeduped } = extractTimeline(
    [entry("merge-gate-granted", { pr: "644", base: "main" }, { actor: "gh-shim", ts_ms: T0 - MIN })],
    ghActivity({ prs: [pr({ number: 644, state: "MERGED", closed_at: mergedAt, merged_at: mergedAt })] })
  );
  assert.equal(mergesDeduped, 1);
  assert.equal(events.filter((e) => e.kind === "merge").length, 0, "the gate event was folded in");
  const merged = only(events, "pr-merged");
  assert.equal(merged.ts_ms, Date.parse(mergedAt), "gh has the authoritative instant");
  assert.equal(merged.source, "audit+gh");
  const detail = merged.detail as { gh: GhPrActivityLike; gate: unknown[] };
  assert.equal(detail.gh.number, 644, "the gh row survives");
  assert.equal(detail.gate.length, 1, "so does the gate's own record");
  assert.deepEqual(detail.gate[0], { pr: "644", base: "main" });
});

test("an audit-only merge survives the dedupe", () => {
  // The PR is older than gh's 100-row window, or the gate allowed a merge gh
  // never completed. Dropping it would hide exactly what this view is for.
  const { events, mergesDeduped } = extractTimeline(
    [entry("merge-gate-allowed", { pr: "42", base: "main" }, { actor: "gh-shim" })],
    ghActivity({ prs: [pr({ number: 644, state: "MERGED", merged_at: "2026-07-01T11:30:00Z" })] })
  );
  assert.equal(mergesDeduped, 0);
  const kept = only(events, "merge");
  assert.equal(kept.pr, "42");
  assert.equal(kept.source, "audit");
});

test("a REFUSED merge is never collapsed into a later merge of the same PR", () => {
  // The gate blocked at 11:00 and the PR merged at 11:30 after a human grant:
  // two real events, an hour apart in the story of that PR.
  const { events, mergesDeduped } = extractTimeline(
    [
      entry("merge-gate-blocked", { pr: "644", reason: "gate-closed" }, { actor: "gh-shim", ts_ms: T0 }),
      entry("merge-gate-granted", { pr: "644" }, { actor: "gh-shim", ts_ms: T0 + 30 * MIN }),
    ],
    ghActivity({ prs: [pr({ number: 644, state: "MERGED", merged_at: "2026-07-01T11:30:00Z" })] })
  );
  assert.equal(mergesDeduped, 1, "only the permitting event is the same event as the merge");
  const refusal = only(events, "merge");
  assert.match(refusal.label, /refused/);
  assert.equal(refusal.ts_ms, T0);
});

test("two gate events for one merged PR both fold into it", () => {
  const { events, mergesDeduped } = extractTimeline(
    [
      entry("merge-gate-granted", { pr: "644", attempt: 1 }, { actor: "gh-shim", ts_ms: T0 }),
      entry("merge-gate-granted", { pr: "644", attempt: 2 }, { actor: "gh-shim", ts_ms: T0 + MIN }),
    ],
    ghActivity({ prs: [pr({ number: 644, state: "MERGED", merged_at: "2026-07-01T11:30:00Z" })] })
  );
  assert.equal(mergesDeduped, 2);
  const detail = only(events, "pr-merged").detail as { gh: GhPrActivityLike; gate: unknown[] };
  assert.equal(detail.gate.length, 2);
  assert.equal(detail.gh.number, 644);
});

// --- windowing -------------------------------------------------------------

test("the default window is 12 hours back from now", () => {
  const w = windowPreset(DEFAULT_WINDOW_ID);
  assert.equal(w.ms, 12 * HOUR);
  const range = resolveWindow(DEFAULT_WINDOW_ID, T0, []);
  assert.equal(range.endMs, T0);
  assert.equal(range.startMs, T0 - 12 * HOUR);
});

test("an unknown preset id falls back to the default instead of blanking the view", () => {
  // A persisted value from another build must not produce an empty chart.
  assert.equal(windowPreset("42y").id, DEFAULT_WINDOW_ID);
  assert.deepEqual(resolveWindow("42y", T0, []), resolveWindow(DEFAULT_WINDOW_ID, T0, []));
});

test("window edges are inclusive on both sides", () => {
  const { events } = extractTimeline([
    entry("group-create", { repo: "old" }, { ts_ms: T0 - 12 * HOUR - 1 }),
    entry("group-pause", {}, { ts_ms: T0 - 12 * HOUR }),
    entry("group-resume", {}, { ts_ms: T0 }),
  ]);
  const range = resolveWindow("12h", T0, events);
  const shown = filterTimeline(events, range, DEFAULT_CATEGORIES);
  assert.deepEqual(
    shown.map((e) => e.ts_ms),
    [T0 - 12 * HOUR, T0],
    "one millisecond older than the window is out; exactly on the edge is in"
  );
});

test("'all' spans from the oldest event to now", () => {
  const { events } = extractTimeline([entry("group-create", { repo: "r" }, { ts_ms: T0 - 40 * HOUR })]);
  const range = resolveWindow("all", T0, events);
  assert.equal(range.startMs, T0 - 40 * HOUR);
  assert.equal(range.endMs, T0);
  assert.equal(filterTimeline(events, range, DEFAULT_CATEGORIES).length, 1);
});

test("'all' with no events still yields a usable axis", () => {
  const range = resolveWindow("all", T0, []);
  assert.ok(range.endMs - range.startMs >= MIN_WINDOW_MS, "never a zero-span axis");
});

test("an event stamped in the future extends the window rather than falling off it", () => {
  // A clock-skewed agent's report must stay visible; silently clipping it is
  // the same class of defect as plotting an undatable event at 1970.
  const { events } = extractTimeline([entry("group-resume", {}, { ts_ms: T0 + 2 * HOUR })]);
  const range = resolveWindow("1h", T0, events);
  assert.equal(range.endMs, T0 + 2 * HOUR);
  assert.equal(filterTimeline(events, range, DEFAULT_CATEGORIES).length, 1);
});

test("a window with no span is widened to something a scale can divide by", () => {
  const range = resolveWindow("all", T0, [
    { ts_ms: T0, kind: "group", label: "a", source: "audit", detail: null },
    { ts_ms: T0, kind: "group", label: "b", source: "audit", detail: null },
  ]);
  assert.equal(range.endMs - range.startMs, MIN_WINDOW_MS);
});

test("category filtering is by category, not by kind", () => {
  const { events } = extractTimeline([
    entry("agent-spawn", { agent: "w-1", role: "worker" }),
    entry("prompt", { to: "w-1", text: "go" }, { ts_ms: T0 + 1 }),
    entry("review-verdict", { pr: 1, verdict: "pass" }, { ts_ms: T0 + 2 }),
  ]);
  const range = { startMs: T0 - MIN, endMs: T0 + MIN };
  assert.deepEqual(
    filterTimeline(events, range, ["work"]).map((e) => e.kind),
    ["delivery"]
  );
  assert.deepEqual(
    filterTimeline(events, range, ["agents", "gates"]).map((e) => e.kind),
    ["agent-spawn", "verdict"]
  );
  assert.deepEqual(filterTimeline(events, range, []), []);
});

test("every kind the extractor can emit has a category", () => {
  // Guards the pairing: a new kind added without a lane would otherwise
  // silently filter out of every chip.
  const kinds = [
    "group",
    "intake",
    "agent-spawn",
    "agent-exit",
    "delivery",
    "report",
    "task-status",
    "verdict",
    "merge",
    "release",
    "issue-opened",
    "issue-closed",
    "pr-opened",
    "pr-merged",
    "pr-closed",
    "ops",
  ] as const;
  for (const k of kinds) {
    assert.ok(CATEGORY_ORDER.includes(categoryOf(k)), `${k} has no lane`);
  }
  assert.equal(DEFAULT_CATEGORIES.includes("ops"), false, "ops is off by default");
});

// --- eventKey / retainExpandedEvents: the detail-panel expand prune (#1316) ---
//
// Before this, TimelineView.expanded was cleared only on a dot click, never
// pruned against what the latest poll actually loaded — a row expanded while
// its cluster stayed selected could sit stranded once its event aged out of
// orch_audit's AUDIT_VIEW_LIMIT window.

function ev(over: Partial<TimelineEvent> = {}): TimelineEvent {
  return { ts_ms: T0, kind: "ops", label: "x", source: "audit", detail: {}, ...over };
}

test("eventKey is keyed by the event, not by array position", () => {
  const a = ev({ label: "one" });
  const b = ev({ label: "two" });
  assert.notEqual(eventKey(a), eventKey(b));
  assert.equal(eventKey(a), eventKey(ev({ label: "one" })), "same event content -> same key");
});

test("retainExpandedEvents keeps only keys that still name a loaded event", () => {
  const a = ev({ label: "aged-out" });
  const b = ev({ label: "still-loaded" });
  const live = retainExpandedEvents([eventKey(a), eventKey(b)], [b]);
  assert.deepEqual([...live], [eventKey(b)]);
});

test("retainExpandedEvents on an empty event list (or empty expand set) keeps nothing", () => {
  const a = ev();
  assert.equal(retainExpandedEvents([eventKey(a)], []).size, 0);
  assert.equal(retainExpandedEvents([], [a]).size, 0);
});

test("retainExpandedEvents returns a fresh set, not the input", () => {
  const a = ev();
  const expanded = new Set([eventKey(a)]);
  const live = retainExpandedEvents(expanded, [a]);
  assert.notEqual(live, expanded);
});

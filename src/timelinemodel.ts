// DOM-free event model for the progress timeline (#608, Slice B): audit
// entries + gh issue/PR activity in, one sorted `TimelineEvent[]` out, plus the
// counters that let the view state its own coverage honestly.
//
// Pure and self-contained by convention — no imports at all, not even a type
// import. Every node:test-covered module here (layout.ts, embedsplit.ts,
// overlaysize.ts, taskboard.ts) is written this way: tsc's build forbids the
// explicit `.ts` extension an intra-src import needs for `node --test` to
// resolve it directly (TS5097, see embedsplit.ts), so a bare specifier would
// work for one runner and not the other. The `*Like` input interfaces below
// therefore MIRROR `AuditEntry` (auditsummary.ts) and `GhActivity`
// (issues.ts) structurally rather than importing them; TypeScript's structural
// typing means the real values pass straight in, and the mirrors are the only
// place a wire-shape change has to be re-checked.
//
// Why the frontend and not Rust: extraction is presentation-shaped, the same
// ≤5000-entry payload already crosses IPC for the audit view, and the repo's
// convention is to keep testable logic in DOM-free TS. See
// doc/design/progress-timeline.md.
//
// The one rule that shapes everything here: **nothing is silently dropped.**
// An entry either becomes a plotted event, becomes an `ops` event (a real
// event in a default-OFF lane), is parked as undatable, or is counted as
// malformed. A chart that looks complete when it isn't is the defect class
// this repo has been burned by (.loomux/lessons.md, "no silent caps").

/** One orchestration audit record. Mirrors `AuditEntry` in auditsummary.ts,
 *  which is what `orch_audit` serves. Fields are typed as they arrive over
 *  IPC — i.e. trusted no further than JSON: `extractTimeline` takes
 *  `unknown[]` and validates each row into this shape. */
export interface AuditEntryLike {
  ts_ms: number;
  actor: string;
  action: string;
  detail: unknown;
}

/** One issue's lifecycle timestamps. Mirrors `GhIssueActivity` (issues.ts,
 *  Slice A). ISO-8601 strings stay strings over the wire; parsing is this
 *  module's job. `created_at` can be empty when gh omitted it — that is an
 *  undatable event, never 1970. */
export interface GhIssueActivityLike {
  number: number;
  title: string;
  state: string;
  created_at: string;
  closed_at: string | null;
  updated_at: string;
  url: string;
}

/** One PR's lifecycle timestamps. Mirrors `GhPrActivity` (issues.ts). GitHub
 *  closes a PR when it merges it, so a merged PR carries BOTH `closed_at` and
 *  `merged_at`: "merged" keys off `merged_at` only, or every closed-unmerged
 *  PR renders as a merge that never happened. */
export interface GhPrActivityLike {
  number: number;
  title: string;
  state: string;
  created_at: string;
  closed_at: string | null;
  merged_at: string | null;
  updated_at: string;
  url: string;
  head_ref: string;
}

/** Issue + PR activity for one repo, carrying its own coverage boundary.
 *  Mirrors `GhActivity` (issues.ts): `limit` crosses the wire so the rendered
 *  coverage note cannot drift from the query that produced the data. */
export interface GhActivityLike {
  issues: readonly GhIssueActivityLike[];
  prs: readonly GhPrActivityLike[];
  limit: number;
  issues_truncated: boolean;
  prs_truncated: boolean;
}

/** What a plotted dot IS. One kind per distinguishable real-world event.
 *
 *  Three refinements to the kind list the plan sketched, each additive and
 *  argued in doc/design/progress-timeline.md:
 *  - `delivery` rather than `kickoff`: the `prompt` audit detail is only
 *    `{to, text}`, so nothing in the data separates a first kickoff from a
 *    mid-stream delivery. Naming the kind for what the data actually is beats
 *    a byte-shape guess at the text (lessons.md: an open set of shapes is
 *    always one shape behind).
 *  - `intake` added: `intake-signal` is real work arriving and has no honest
 *    home in the sketch's set — folding it into `group` would mislabel it and
 *    folding it into `ops` would default-hide it.
 *  - `pr-closed` added: gh returns closed-unmerged PRs, and without this kind
 *    such a PR opens on the chart and never resolves. */
export type TimelineEventKind =
  | "group"
  | "intake"
  | "agent-spawn"
  | "agent-exit"
  | "delivery"
  | "report"
  | "task-status"
  | "verdict"
  | "merge"
  | "release"
  | "issue-opened"
  | "issue-closed"
  | "pr-opened"
  | "pr-merged"
  | "pr-closed"
  | "ops";

/** A lane on the chart and a toggle chip in the header. Coarser than the kind
 *  so the header stays readable: kinds label a dot, categories filter. */
export type TimelineCategory = "group" | "agents" | "work" | "gates" | "github" | "ops";

/** Lane order, top to bottom. Group lifecycle frames the session, agents and
 *  their work sit in the middle, gates and GitHub outcomes at the bottom
 *  where the "what came of it" story is. */
export const CATEGORY_ORDER: readonly TimelineCategory[] = [
  "group",
  "agents",
  "work",
  "gates",
  "github",
  "ops",
];

const KIND_CATEGORY: Record<TimelineEventKind, TimelineCategory> = {
  group: "group",
  intake: "group",
  "agent-spawn": "agents",
  "agent-exit": "agents",
  delivery: "work",
  report: "work",
  "task-status": "work",
  verdict: "gates",
  merge: "gates",
  release: "gates",
  "issue-opened": "github",
  "issue-closed": "github",
  "pr-opened": "github",
  "pr-merged": "github",
  "pr-closed": "github",
  ops: "ops",
};

export function categoryOf(kind: TimelineEventKind): TimelineCategory {
  return KIND_CATEGORY[kind];
}

/** Categories a fresh view opens with: everything except `ops`. `ops` is the
 *  high-volume plumbing (deliveries' internal queue states, compaction, watch
 *  registrations, every non-report MCP tool-call) — present and one chip away,
 *  never plotted by default. */
export const DEFAULT_CATEGORIES: readonly TimelineCategory[] = CATEGORY_ORDER.filter(
  (c) => c !== "ops"
);

/** One point on the timeline. `detail` is the raw source record, kept so the
 *  click-to-expand body can show it without a second lookup. */
export interface TimelineEvent {
  ts_ms: number;
  kind: TimelineEventKind;
  /** One-line, summarize()-style. Never empty. */
  label: string;
  /** The agent this is about, when there is one (spawn/exit target, delivery
   *  recipient, report sender). */
  agent?: string;
  /** Issue number as a bare string ("608"), when the source names one. */
  issue?: string;
  /** PR number as a bare string ("644"). Normalized: the gh shim writes `pr`
   *  as a STRING and review_verdict writes it as a NUMBER. */
  pr?: string;
  /** `audit+gh` marks an event both sources reported — a merge, where gh has
   *  the authoritative instant and the audit has the gate's own record. */
  source: "audit" | "gh" | "audit+gh";
  detail: unknown;
}

/** The `orch_audit` cap, mirrored from `AUDIT_VIEW_LIMIT` in
 *  src-tauri/src/orchestration/mod.rs. Duplicated rather than plumbed: the
 *  command sends no truncation flag (`audit_log_windowed` keeps that for
 *  derivations), so a full-looking list is the only signal the frontend has —
 *  and it is deliberately reported as "at the cap", not as "truncated",
 *  because a log holding exactly 5000 entries is indistinguishable from one
 *  that was cut. */
export const AUDIT_VIEW_LIMIT = 5000;

/** Everything `extractTimeline` learned, including what it could NOT plot. */
export interface TimelineExtraction {
  /** Ascending by `ts_ms`, ties in source order (audit before gh). */
  events: TimelineEvent[];
  /** Entries with no usable instant: the shims' `ts_ms: 0` fallback (`date`
   *  unavailable) and gh timestamps that would not parse. Parked and counted —
   *  plotting them at 1970 would put a fake dot on the axis. */
  undatable: number;
  /** Rows that were not audit entries at all (not an object, no string
   *  `action`, non-numeric `ts_ms`). */
  malformed: number;
  /** Audit action -> count, for every action with no plotted kind of its own.
   *  These became `ops` events; the tally is what lets the view name them
   *  instead of leaving a silent lane. */
  unmapped: Record<string, number>;
  /** Oldest datable audit event — the floor of what the log actually loaded. */
  auditOldestMs: number | null;
  /** Rows handed in. */
  auditCount: number;
  /** `auditCount >= AUDIT_VIEW_LIMIT` — history older than `auditOldestMs`
   *  may exist and was not loaded. */
  auditAtLimit: boolean;
  ghLimit: number | null;
  ghIssuesTruncated: boolean;
  ghPrsTruncated: boolean;
  /** How many audit merge events were collapsed into a gh merge. */
  mergesDeduped: number;
}

const HOUR_MS = 3_600_000;

/** Time-window choices. 12h is the default: long enough to cover a working
 *  session, short enough that the default view is not dominated by history. */
export interface WindowPreset {
  id: string;
  label: string;
  /** Span back from now, or null for "everything loaded". */
  ms: number | null;
}

export const WINDOW_PRESETS: readonly WindowPreset[] = [
  { id: "1h", label: "1h", ms: HOUR_MS },
  { id: "6h", label: "6h", ms: 6 * HOUR_MS },
  { id: "12h", label: "12h", ms: 12 * HOUR_MS },
  { id: "24h", label: "24h", ms: 24 * HOUR_MS },
  { id: "72h", label: "72h", ms: 72 * HOUR_MS },
  { id: "all", label: "All", ms: null },
];

export const DEFAULT_WINDOW_ID = "12h";

/** Narrowest span the axis is ever laid out over. A window whose events all
 *  share one instant would otherwise have zero span, which no scale can
 *  divide by. */
export const MIN_WINDOW_MS = 60_000;

export interface TimelineRange {
  startMs: number;
  endMs: number;
}

// ---------------------------------------------------------------------------
// Small JSON helpers. Deliberate duplicates of auditsummary.ts's `asObject` /
// `str` — same no-intra-src-import rule as the type mirrors above.
// ---------------------------------------------------------------------------

function asObject(v: unknown): Record<string, unknown> | null {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
}

function str(v: unknown): string | undefined {
  return typeof v === "string" ? v : undefined;
}

/** First line of a possibly-long text, capped. Keeps a pasted kickoff from
 *  becoming the whole label. */
function firstLine(s: string, max = 120): string {
  const line = s.split("\n", 1)[0].trim();
  return line.length > max ? line.slice(0, max) + "…" : line;
}

/** An issue/PR reference as a bare number string, whatever shape it arrived
 *  in: the gh shim writes `"pr":"642"` (a string, from `$num`), the MCP
 *  verdict path writes `"pr": 642` (a number), and agents write refs as
 *  `"#642"`. Returns undefined for anything with no digits, so a garbled
 *  reference never becomes a dot keyed to the wrong PR. */
function refNumber(v: unknown): string | undefined {
  if (typeof v === "number") return Number.isFinite(v) && v > 0 ? String(Math.trunc(v)) : undefined;
  const s = str(v);
  if (!s) return undefined;
  const m = /^#?(\d+)$/.exec(s.trim());
  return m ? m[1] : undefined;
}

/** An ISO-8601 instant as epoch ms, or null when absent/unparseable. Empty
 *  strings arrive when gh omitted the field; `Date.parse` would give NaN and
 *  a naive `+new Date()` would give a 1970 dot. */
function parseIso(v: string | null | undefined): number | null {
  if (typeof v !== "string" || v.trim() === "") return null;
  const ms = Date.parse(v);
  return Number.isFinite(ms) ? ms : null;
}

/** Compact one-line rendering of an unknown detail blob, for the `ops` lane
 *  where there is no per-action sentence. */
function compactDetail(detail: unknown): string {
  let json: string;
  try {
    json = JSON.stringify(detail ?? {});
  } catch {
    return ""; // cyclic/unserializable — the action name alone carries the row
  }
  if (json === undefined || json === "{}" || json === "null") return "";
  return json.length > 120 ? json.slice(0, 120) + "…" : json;
}

// ---------------------------------------------------------------------------
// Audit extraction
// ---------------------------------------------------------------------------

/** An audit merge event, plus whether the gate PERMITTED the merge. Only a
 *  permitted one is the same real-world event as a gh merge; a refusal
 *  (`merge-gate-blocked`) for the same PR happened at a different moment and
 *  must survive the dedupe on its own. */
interface AuditMergeMark {
  event: TimelineEvent;
  permitted: boolean;
}

const MERGE_PERMITTED = new Set([
  "merge-gate-allowed",
  "merge-gate-granted",
  "merge-gate-dangerous",
]);
const MERGE_REFUSED = new Set([
  "merge-gate-blocked",
  "merge-gate-unsatisfiable",
  "merge-gate-workflow-blocked",
]);

function mergeLabel(action: string, pr: string | undefined, d: Record<string, unknown>): string {
  const at = pr ? `#${pr}` : "a PR";
  switch (action) {
    // The gate records PERMISSION, not the merge itself: the shim exec's the
    // real `gh` right after and that can still fail. Only a gh `mergedAt`
    // proves a merge happened, which is exactly what the dedupe below uses.
    case "merge-gate-allowed":
      return `merge of ${at} allowed (autonomous)`;
    case "merge-gate-granted":
      return `merge of ${at} allowed (human grant)`;
    case "merge-gate-dangerous":
      return `merge of ${at} allowed (dangerous mode)`;
    case "merge-gate-blocked":
      return `merge of ${at} refused (${str(d.reason) ?? "gate closed"})`;
    case "merge-gate-unsatisfiable":
      return `merge of ${at} refused — gate unsatisfiable`;
    default:
      return `merge of ${at} refused — workflow gate`;
  }
}

function releaseLabel(action: string, d: Record<string, unknown>): string {
  const tag = str(d.tag) ?? "a release";
  switch (action) {
    case "release-gate-allowed":
      return `release ${tag} allowed (autonomous)`;
    case "release-gate-granted":
      return `release ${tag} allowed (human grant)`;
    case "release-gate-dangerous":
      return `release ${tag} allowed (dangerous mode)`;
    case "release-gate-blocked":
      return `release ${tag} refused`;
    default:
      return `release gate restored (${tag})`;
  }
}

const RELEASE_ACTIONS = new Set([
  "release-gate-allowed",
  "release-gate-granted",
  "release-gate-dangerous",
  "release-gate-blocked",
  "release-gate-restored",
]);

/** Map one audit entry to its event. Returns null only for `ops` handling by
 *  the caller (so the caller can keep the per-action tally in one place). */
function mapAuditEntry(
  e: AuditEntryLike,
  ts: number
): { event: TimelineEvent; mergeMark?: boolean } | null {
  const d = asObject(e.detail) ?? {};
  const base = { ts_ms: ts, source: "audit" as const, detail: e.detail };

  switch (e.action) {
    case "group-create":
      return { event: { ...base, kind: "group", label: `group created${str(d.repo) ? ` — ${str(d.repo)}` : ""}` } };
    case "group-resume":
      return { event: { ...base, kind: "group", label: "group resumed" } };
    case "group-pause":
      return { event: { ...base, kind: "group", label: "group paused" } };
    case "group-end":
      return { event: { ...base, kind: "group", label: "group ended" } };

    case "intake-signal":
      return {
        event: { ...base, kind: "intake", label: `intake: ${firstLine(str(d.summary) ?? "new work")}` },
      };

    case "agent-spawn": {
      // `name` is the human-facing pane label ("w: 608-B viz frontend");
      // `agent` is the id ("w-142"). Prefer the name, fall back to the id.
      const agent = str(d.name) ?? str(d.agent);
      const role = str(d.role);
      return {
        event: {
          ...base,
          kind: "agent-spawn",
          agent,
          label: `${agent ?? "an agent"} spawned${role ? ` (${role})` : ""}`,
        },
      };
    }
    case "agent-exit": {
      const agent = str(d.agent);
      const code = d.exit_code;
      return {
        event: {
          ...base,
          kind: "agent-exit",
          agent,
          label: `${agent ?? "an agent"} exited${code === null || code === undefined ? "" : ` (code ${code})`}`,
        },
      };
    }
    case "agent-kill": {
      const agent = str(d.agent);
      return {
        event: {
          ...base,
          kind: "agent-exit",
          agent,
          label: `${agent ?? "an agent"} killed${str(d.initiator) ? ` by ${str(d.initiator)}` : ""}`,
        },
      };
    }
    case "idle-kill": {
      const agent = str(d.name) ?? str(d.agent);
      return {
        event: {
          ...base,
          kind: "agent-exit",
          agent,
          label: `${agent ?? "an agent"} reaped — idle ${d.idle_minutes ?? "?"}m`,
        },
      };
    }

    // Every `prompt` is one lane. The detail is `{to, text}` and nothing else,
    // so a kickoff and a mid-stream delivery are indistinguishable here; the
    // actor (sender) is the useful distinction the data DOES carry, and it
    // goes in the label.
    case "prompt": {
      const to = str(d.to);
      return {
        event: {
          ...base,
          kind: "delivery",
          agent: to,
          label: `${e.actor} → ${to ?? "?"}: ${firstLine(str(d.text) ?? "")}`,
        },
      };
    }

    case "tool-call": {
      const tool = str(d.tool);
      if (tool !== "report") return null; // every other tool-call is ops
      const args = asObject(d.args) ?? {};
      // Structured shape first (`outcome`/`ref`), legacy shape second
      // (`status`/`summary`) — report() still accepts both.
      const outcome = str(args.outcome) ?? str(args.status) ?? "reported";
      const ref = str(args.ref);
      return {
        event: {
          ...base,
          kind: "report",
          agent: e.actor,
          label: `${e.actor} reports ${outcome}${ref ? ` (${ref})` : ""}`,
        },
      };
    }

    case "task-upsert": {
      const id = str(d.id) ?? "?";
      const status = str(d.status);
      const title = str(d.title);
      return {
        event: {
          ...base,
          kind: "task-status",
          issue: refNumber(d.issue),
          pr: refNumber(d.pr),
          agent: str(d.assignee),
          label: `${id}${status ? ` → ${status}` : ""}${title ? `: ${firstLine(title, 60)}` : ""}`,
        },
      };
    }
    case "task-delete":
      return { event: { ...base, kind: "task-status", label: `task ${str(d.id) ?? "?"} deleted` } };

    case "review-verdict": {
      const pr = refNumber(d.pr);
      return {
        event: {
          ...base,
          kind: "verdict",
          pr,
          agent: e.actor,
          label: `review ${str(d.verdict) ?? "?"} on ${pr ? `#${pr}` : "a PR"}`,
        },
      };
    }
    default:
      break;
  }

  if (MERGE_PERMITTED.has(e.action) || MERGE_REFUSED.has(e.action)) {
    const pr = refNumber(d.pr);
    return {
      event: { ...base, kind: "merge", pr, label: mergeLabel(e.action, pr, d) },
      mergeMark: MERGE_PERMITTED.has(e.action),
    };
  }
  if (RELEASE_ACTIONS.has(e.action)) {
    return { event: { ...base, kind: "release", label: releaseLabel(e.action, d) } };
  }
  return null;
}

/** Is this row shaped like an audit entry at all? */
function asAuditEntry(row: unknown): AuditEntryLike | null {
  const o = asObject(row);
  if (!o) return null;
  if (typeof o.action !== "string" || o.action === "") return null;
  if (typeof o.actor !== "string") return null;
  if (typeof o.ts_ms !== "number" || !Number.isFinite(o.ts_ms)) return null;
  return o as unknown as AuditEntryLike;
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/** Build the timeline's events from the two sources.
 *
 *  `audit` is taken as `unknown[]` on purpose: it arrives from IPC as JSON and
 *  a row that isn't an audit entry must be counted, not thrown on — one bad
 *  line in `audit.jsonl` (a spliced write, an older loomux's shape) cannot be
 *  allowed to blank the whole view. */
export function extractTimeline(
  audit: readonly unknown[],
  gh?: GhActivityLike | null
): TimelineExtraction {
  const events: TimelineEvent[] = [];
  const auditMerges: AuditMergeMark[] = [];
  const unmapped: Record<string, number> = {};
  let undatable = 0;
  let malformed = 0;
  let auditOldestMs: number | null = null;

  for (const row of audit) {
    const e = asAuditEntry(row);
    if (!e) {
      malformed++;
      continue;
    }
    // The shims' fallback when `date` is unavailable is a literal 0 (see
    // loomux_audit in mod.rs). Park it: a 1970 dot would silently stretch the
    // axis across 56 years and read as real data.
    if (e.ts_ms <= 0) {
      undatable++;
      continue;
    }
    if (auditOldestMs === null || e.ts_ms < auditOldestMs) auditOldestMs = e.ts_ms;

    const mapped = mapAuditEntry(e, e.ts_ms);
    if (!mapped) {
      unmapped[e.action] = (unmapped[e.action] ?? 0) + 1;
      const compact = compactDetail(e.detail);
      events.push({
        ts_ms: e.ts_ms,
        kind: "ops",
        label: compact ? `${e.action}: ${compact}` : e.action,
        agent: e.actor,
        source: "audit",
        detail: e.detail,
      });
      continue;
    }
    if (mapped.mergeMark !== undefined) {
      auditMerges.push({ event: mapped.event, permitted: mapped.mergeMark });
      continue;
    }
    events.push(mapped.event);
  }

  // ---- gh ----------------------------------------------------------------
  const ghMergedPrs = new Map<string, TimelineEvent>();
  if (gh) {
    for (const issue of gh.issues) {
      const num = refNumber(issue.number);
      const opened = parseIso(issue.created_at);
      if (opened === null) undatable++;
      else
        events.push({
          ts_ms: opened,
          kind: "issue-opened",
          issue: num,
          source: "gh",
          label: `#${issue.number} opened: ${firstLine(issue.title, 80)}`,
          detail: issue,
        });
      const closed = parseIso(issue.closed_at);
      if (closed !== null)
        events.push({
          ts_ms: closed,
          kind: "issue-closed",
          issue: num,
          source: "gh",
          label: `#${issue.number} closed: ${firstLine(issue.title, 80)}`,
          detail: issue,
        });
      // A null `closed_at` is an OPEN issue, not a missing timestamp — not
      // undatable, nothing to count.
    }
    for (const pr of gh.prs) {
      const num = refNumber(pr.number);
      const opened = parseIso(pr.created_at);
      if (opened === null) undatable++;
      else
        events.push({
          ts_ms: opened,
          kind: "pr-opened",
          pr: num,
          source: "gh",
          label: `PR #${pr.number} opened: ${firstLine(pr.title, 80)}`,
          detail: pr,
        });
      const merged = parseIso(pr.merged_at);
      if (merged !== null) {
        const ev: TimelineEvent = {
          ts_ms: merged,
          kind: "pr-merged",
          pr: num,
          source: "gh",
          label: `PR #${pr.number} merged: ${firstLine(pr.title, 80)}`,
          detail: pr,
        };
        events.push(ev);
        if (num) ghMergedPrs.set(num, ev);
        continue; // a merged PR's closed_at is the same event as its merge
      }
      const closed = parseIso(pr.closed_at);
      if (closed !== null)
        events.push({
          ts_ms: closed,
          kind: "pr-closed",
          pr: num,
          source: "gh",
          label: `PR #${pr.number} closed unmerged: ${firstLine(pr.title, 80)}`,
          detail: pr,
        });
    }
  }

  // ---- dedupe merges -----------------------------------------------------
  // A merge loomux's gate permitted AND gh confirms is one event reported
  // twice. Collapse onto the gh row: gh has the authoritative instant (the
  // gate fires before the merge and can be followed by a failure), the audit
  // has the gate's own record, and both details survive on the kept event.
  // An audit merge with no gh counterpart SURVIVES UNCHANGED — that is a
  // merge past gh's 100-row limit, or one the gate allowed and gh never
  // completed, and dropping it would hide exactly the case this view exists
  // to show.
  let mergesDeduped = 0;
  for (const m of auditMerges) {
    const gh0 = m.permitted && m.event.pr ? ghMergedPrs.get(m.event.pr) : undefined;
    if (!gh0) {
      events.push(m.event);
      continue;
    }
    mergesDeduped++;
    gh0.source = "audit+gh";
    const prior = asObject(gh0.detail);
    const gate = Array.isArray((prior as { gate?: unknown })?.gate)
      ? ((prior as { gate: unknown[] }).gate as unknown[])
      : [];
    gh0.detail = { gh: prior && "gh" in prior ? prior.gh : gh0.detail, gate: [...gate, m.event.detail] };
  }

  events.sort((a, b) => a.ts_ms - b.ts_ms);

  return {
    events,
    undatable,
    malformed,
    unmapped,
    auditOldestMs,
    auditCount: audit.length,
    auditAtLimit: audit.length >= AUDIT_VIEW_LIMIT,
    ghLimit: gh ? gh.limit : null,
    ghIssuesTruncated: gh ? gh.issues_truncated : false,
    ghPrsTruncated: gh ? gh.prs_truncated : false,
    mergesDeduped,
  };
}

// ---------------------------------------------------------------------------
// Windowing & filtering
// ---------------------------------------------------------------------------

export function windowPreset(id: string): WindowPreset {
  // An unknown id is a persisted value from another build, not a bug worth a
  // blank view — fall back to the default rather than throwing.
  return (
    WINDOW_PRESETS.find((p) => p.id === id) ??
    WINDOW_PRESETS.find((p) => p.id === DEFAULT_WINDOW_ID)!
  );
}

/** The axis bounds for a preset.
 *
 *  `endMs` is `now`, EXTENDED to the newest event when one is stamped in the
 *  future — a clock-skewed agent's report must not fall off the right edge
 *  without a trace. "All" starts at the oldest event (or one hour back when
 *  there are none, so the axis is never degenerate), and every window is
 *  widened to `MIN_WINDOW_MS` if it would otherwise have no span. */
export function resolveWindow(
  presetId: string,
  nowMs: number,
  events: readonly TimelineEvent[]
): TimelineRange {
  const preset = windowPreset(presetId);
  let oldest: number | null = null;
  let newest: number | null = null;
  for (const e of events) {
    if (oldest === null || e.ts_ms < oldest) oldest = e.ts_ms;
    if (newest === null || e.ts_ms > newest) newest = e.ts_ms;
  }
  let endMs = newest !== null && newest > nowMs ? newest : nowMs;
  let startMs = preset.ms === null ? (oldest ?? nowMs - HOUR_MS) : nowMs - preset.ms;
  if (startMs > endMs) startMs = endMs - MIN_WINDOW_MS; // future-only data
  if (endMs - startMs < MIN_WINDOW_MS) endMs = startMs + MIN_WINDOW_MS;
  return { startMs, endMs };
}

/** Events inside the window whose category is enabled, in time order. Both
 *  bounds are inclusive: an event exactly at the window edge is IN, because
 *  the alternative is a dot that vanishes as the window slides by a
 *  millisecond. */
export function filterTimeline(
  events: readonly TimelineEvent[],
  range: TimelineRange,
  categories: Iterable<TimelineCategory>
): TimelineEvent[] {
  const on = new Set(categories);
  return events.filter(
    (e) => e.ts_ms >= range.startMs && e.ts_ms <= range.endMs && on.has(categoryOf(e.kind))
  );
}

// ---------------------------------------------------------------------------
// Coverage honesty
// ---------------------------------------------------------------------------

/** One sentence the view must render when it applies. `id` is stable for
 *  styling/testing; `text` is the sentence. */
export interface CoverageNote {
  id: string;
  text: string;
}

/** ISO-8601 UTC, seconds precision. Deliberately not localized: this module is
 *  locale- and DST-free by design (the view may re-render the same instant in
 *  local time next to it). */
function isoUtc(ms: number): string {
  return new Date(ms).toISOString().replace(/\.\d{3}Z$/, "Z");
}

/** What this chart is NOT showing. Every note here exists because the
 *  alternative is a view that looks complete and isn't. */
export function coverageNotes(x: TimelineExtraction, range: TimelineRange): CoverageNote[] {
  const notes: CoverageNote[] = [];
  if (x.auditAtLimit) {
    notes.push({
      id: "audit-cap",
      text: `Audit log loaded at its ${AUDIT_VIEW_LIMIT}-entry cap — anything older than ${
        x.auditOldestMs === null ? "the oldest entry shown" : isoUtc(x.auditOldestMs)
      } is not included.`,
    });
  }
  // Stated as a coverage FLOOR, not as loss. A window wider than the group's
  // own lifetime is the ordinary case, and "data is missing" would be a false
  // alarm there; equally, the frontend cannot tell a group that simply started
  // recently from one whose oldest generation was rotated away
  // (AUDIT_ROTATE_BYTES keeps one generation). "Coverage starts here" is true
  // in both, and is the sentence that stops a reader inferring a quiet period
  // from empty space at the left edge.
  if (x.auditOldestMs !== null && range.startMs < x.auditOldestMs) {
    notes.push({
      id: "audit-coverage",
      text: `Audit coverage starts ${isoUtc(x.auditOldestMs)} — the window reaches further back, and the empty space before that is "not recorded", not "nothing happened".`,
    });
  }
  if (x.ghIssuesTruncated || x.ghPrsTruncated) {
    const which =
      x.ghIssuesTruncated && x.ghPrsTruncated
        ? "issues and PRs"
        : x.ghIssuesTruncated
          ? "issues"
          : "PRs";
    notes.push({
      id: "gh-cap",
      text: `GitHub ${which} capped at the ${x.ghLimit ?? "?"} most recently active — older activity is not shown.`,
    });
  }
  if (x.undatable > 0) {
    notes.push({
      id: "undatable",
      text: `${x.undatable} event${x.undatable === 1 ? "" : "s"} carried no usable timestamp and ${
        x.undatable === 1 ? "is" : "are"
      } not plotted.`,
    });
  }
  if (x.malformed > 0) {
    notes.push({
      id: "malformed",
      text: `${x.malformed} audit row${x.malformed === 1 ? "" : "s"} could not be read and ${
        x.malformed === 1 ? "was" : "were"
      } skipped.`,
    });
  }
  const unmappedTotal = Object.values(x.unmapped).reduce((a, b) => a + b, 0);
  if (unmappedTotal > 0) {
    const kinds = Object.keys(x.unmapped).length;
    notes.push({
      id: "ops-lane",
      text: `${unmappedTotal} event${unmappedTotal === 1 ? "" : "s"} across ${kinds} action${
        kinds === 1 ? "" : "s"
      } have no timeline category of their own and are plotted in "ops" (off by default).`,
    });
  }
  return notes;
}

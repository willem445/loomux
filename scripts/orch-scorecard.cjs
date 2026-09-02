#!/usr/bin/env node
'use strict';
// orch-scorecard — per-PR orchestration cost from logs that already exist (#2011 B1).
//
// WHAT THIS ANSWERS. For one pull request: how much orchestrator *attention* the
// review loop cost (wakes, by what woke it), how many *tokens* it cost on each side
// of the fleet, and the orchestrator's SHARE of those tokens. The point is a
// before/after number for the engine-owned review driver (#1778) that can be
// produced identically for a PR routed by hand, a PR driven by beta4, and a PR
// driven by beta5 — the driver writes `rd-*` rows, but every other counter here
// reads rows that predate it.
//
// WHY A SCRIPT AND NOT PRODUCT CODE. This slice adds no Rust. Every number below is
// a projection over files the app already writes: the group's `audit.jsonl`,
// `usage.json`, `agents.json`, and the orchestrator CLI's own transcript. Nothing
// here is a new row, a new field, or a new gate.
//
// RETIREMENT. `doc/design/orchestration-evals.md` carries the clause: this file is
// DELETED in #2011 S3, once an engine module (`crates/loomux-engine`, exposed as the
// `group_metrics` MCP tool) reproduces this output byte-for-byte on the fixture
// corpus. Until then it is the only reader, and the design note is its spec.
//
// PRIVACY. The orchestrator transcript is read for `timestamp` and `message.usage`
// ONLY. No transcript text is parsed, stored, printed, or checked in — the tests run
// against a synthetic six-line transcript written by hand.
//
// Dependency-free CJS (the root package.json is `"type": "module"`, so a `.js` file
// here would be ESM and `require` a ReferenceError). Node's own JSON and `readline`
// are all this needs; the transcript is ~230 MB, so it is streamed, never slurped.

const fs = require('node:fs');
const readline = require('node:readline');

// The S5 rule (#1778): a notice arriving up to ten minutes after the loop's last row
// still belongs to that loop — the orchestrator reads a merge/CI notice after the
// driver has stopped writing.
const DEFAULT_TAIL_MIN = 10;

// ---------------------------------------------------------------------------
// Wake classification — the plan's part-1 prefix classes, written as shapes.
//
// A wake is a `prompt` audit row whose `detail.to` is an orchestrator pane. Its
// class is decided by the LEADING shape of `detail.text`, first match wins, and the
// order below is the tie-break: `reports approved` is a reviewer report rather than
// a generic delegate report, and `recorded verdict` is the verdict echo rather than
// a system notice. `other` is a text with no `[orrerix]` prefix — in practice a
// human typing into the orchestrator pane.
// ---------------------------------------------------------------------------

const WAKE_KINDS = [
  'delegate-progress',
  'delegate-done',
  'reviewer-report',
  'delegate-blocked',
  'verdict-notice',
  'system-notice',
  'other',
];

const WAKE_SHAPES = [
  ['delegate-progress', /^\[orrerix\] \S+ reports progress\b/],
  ['delegate-done', /^\[orrerix\] \S+ reports done\b/],
  ['reviewer-report', /^\[orrerix\] \S+ reports (?:approved|request_changes)\b/],
  ['delegate-blocked', /^\[orrerix\] (?:\S+ reports blocked\b|message from \S)/],
  ['verdict-notice', /^\[orrerix\] \S+ \([^)]+\) recorded verdict\b/],
];

function classifyWake(text) {
  const t = typeof text === 'string' ? text : '';
  for (const [kind, re] of WAKE_SHAPES) if (re.test(t)) return kind;
  return /^\[orrerix\]/.test(t) ? 'system-notice' : 'other';
}

function emptyWakeCounts() {
  const out = {};
  for (const k of WAKE_KINDS) out[k] = 0;
  return out;
}

// ---------------------------------------------------------------------------
// "Does this row name PR N?"
//
// Structural first: `detail.pr === N` is what every `rd-*` and `review-verdict` row
// carries. Otherwise a text join on the `#N` token, with a trailing-digit guard so
// `#175` cannot match inside `#1751`. Issues and PRs share one number space on
// GitHub, so `#N` is unambiguous once N is known to be a PR.
// ---------------------------------------------------------------------------

function prTokenRe(pr) {
  return new RegExp('#' + pr + '(?![0-9])');
}

function rowNamesPr(row, pr, re) {
  const detail = row && row.detail;
  if (!detail || typeof detail !== 'object') return false;
  if (detail.pr === pr) return true;
  return (re || prTokenRe(pr)).test(JSON.stringify(detail));
}

// ---------------------------------------------------------------------------
// Windows.
//
// TWO windows per PR, because they answer different questions and the plan asks for
// both:
//
//   loop  — first `rd-*` row carrying `detail.pr === N` to the last one. This is the
//           #1778 S5 window; `span_h` and the driver counters are measured on it and
//           reproduce that table exactly. A hand-routed PR has no loop window.
//   pr    — first audit row NAMING the PR, to the PR's merge (from `--pr-meta`);
//           falling back to the last `rd-*` row, then to the last naming row. The
//           last-naming-row fallback is a poor end — a PR stays cited in the log for
//           days after it merges — and coverage says when it was used.
//
// Both take the +10 min tail for membership tests. `span_h` is the untailed span, so
// it is the honest duration of the thing measured.
// ---------------------------------------------------------------------------

function computeWindows(rows, pr, mergedMs, tailMs) {
  const re = prTokenRe(pr);
  let namedFirst = null;
  let namedLast = null;
  let rdFirst = null;
  let rdLast = null;
  for (const row of rows) {
    if (!rowNamesPr(row, pr, re)) continue;
    if (namedFirst === null) namedFirst = row.ts_ms;
    namedLast = row.ts_ms;
    if (typeof row.action === 'string' && row.action.startsWith('rd-') && row.detail.pr === pr) {
      if (rdFirst === null) rdFirst = row.ts_ms;
      rdLast = row.ts_ms;
    }
  }
  const loop = rdFirst === null ? null : {
    start_ms: rdFirst,
    end_ms: rdLast,
    tail_ms: tailMs,
    span_h: round2((rdLast - rdFirst) / 3600000),
  };
  if (namedFirst === null) return { pr: null, loop };
  let endMs = mergedMs;
  let endSource = 'merged_at';
  if (endMs === null || endMs === undefined) {
    if (rdLast !== null) { endMs = rdLast; endSource = 'last-rd-row'; }
    else { endMs = namedLast; endSource = 'last-naming-row'; }
  }
  return {
    pr: {
      start_ms: namedFirst,
      end_ms: endMs,
      end_source: endSource,
      tail_ms: tailMs,
      span_h: round2((endMs - namedFirst) / 3600000),
    },
    loop,
  };
}

function inWindow(ts, win) {
  if (!win || typeof ts !== 'number') return false;
  return ts >= win.start_ms && ts <= win.end_ms + win.tail_ms;
}

function round2(n) { return Math.round(n * 100) / 100; }

// ---------------------------------------------------------------------------
// Transcript usage.
//
// A Claude Code transcript writes an `assistant` line per content block, so ONE API
// response appears two or more times with the SAME `message.id` and the SAME usage
// object. Summing the lines double-counts: on the orchestrator's own transcript
// 30,842 assistant lines carry only 15,087 distinct message ids. Dedup by
// `message.id`, keeping the occurrence with the largest `output_tokens` — three
// pairs in that file differ only because an earlier line was written mid-stream.
//
// A line with no `message.id` cannot be deduped and is counted once, reported in
// coverage as `usage_rows_without_id` (the fixture carries one, so that path is not
// a claim with no test behind it).
// ---------------------------------------------------------------------------

function usageOf(entry) {
  const u = entry && entry.message && entry.message.usage;
  if (!u || typeof u !== 'object') return null;
  return {
    input: num(u.input_tokens),
    cache_read: num(u.cache_read_input_tokens),
    cache_creation: num(u.cache_creation_input_tokens),
    output: num(u.output_tokens),
  };
}

function num(v) { return typeof v === 'number' && Number.isFinite(v) ? v : 0; }

function emptyTokens() {
  return { input: 0, cache_read: 0, cache_creation: 0, output: 0, total: 0, turns: 0 };
}

function addTokens(acc, u) {
  acc.input += u.input;
  acc.cache_read += u.cache_read;
  acc.cache_creation += u.cache_creation;
  acc.output += u.output;
  acc.total += u.input + u.cache_read + u.cache_creation + u.output;
  acc.turns += 1;
  return acc;
}

function scaleTokens(t, factor) {
  return {
    input: Math.round(t.input * factor),
    cache_read: Math.round(t.cache_read * factor),
    cache_creation: Math.round(t.cache_creation * factor),
    output: Math.round(t.output * factor),
    total: Math.round(t.total * factor),
    turns: t.turns,
  };
}

// Collapses transcript lines into one record per API response.
// Exported so a test can pin the dedup rule without a 230 MB file.
function dedupeTranscriptTurns(entries) {
  const byId = new Map();
  const anonymous = [];
  let skipped = 0;
  for (const e of entries) {
    if (!e || e.type !== 'assistant') { skipped += 1; continue; }
    const u = usageOf(e);
    if (!u) { skipped += 1; continue; }
    const ts = e.timestamp ? Date.parse(e.timestamp) : NaN;
    if (!Number.isFinite(ts)) { skipped += 1; continue; }
    const id = e.message && e.message.id;
    const rec = { ts_ms: ts, usage: u };
    if (!id) { anonymous.push(rec); continue; }
    const prev = byId.get(id);
    // Keep the occurrence that saw the most output: a mid-stream line is written
    // before the response finished and under-reports `output_tokens`.
    if (!prev || u.output > prev.usage.output) byId.set(id, rec);
  }
  return {
    turns: [...byId.values(), ...anonymous].sort((a, b) => a.ts_ms - b.ts_ms),
    without_id: anonymous.length,
    non_usage_lines: skipped,
  };
}

// ---------------------------------------------------------------------------
// Agent -> PR attribution.
//
// Two tiers, and which tier carried an agent is reported per agent, because the
// difference is the whole of #2011 B2's scope:
//
//   structural — the row itself carries both the agent and the PR:
//                `rd-lane-spawned` / `rd-handback` (`detail.agent` + `detail.pr`),
//                and `review-verdict` (`actor` + `detail.pr`).
//   text       — an `agent-spawn` row inside the PR's window whose serialized detail
//                names `#N`. This is a heuristic (H2/H3 below): a brief can name a PR
//                it is not working on, and the window bound is what stops a later
//                brief citing an old PR from claiming its tokens.
//
// An agent attributed to k PRs contributes 1/k of its tokens to each, and is listed
// in coverage as `agents_split_across_prs`. Structural wins: once an agent has a
// structural attribution, text rows cannot add another PR to it.
// ---------------------------------------------------------------------------

function attributeAgents(rows, prs, windowsByPr) {
  const structural = new Map(); // agentId -> Set<pr>
  const textual = new Map();
  const add = (map, agent, pr) => {
    if (typeof agent !== 'string' || !agent) return;
    if (!map.has(agent)) map.set(agent, new Set());
    map.get(agent).add(pr);
  };
  const prSet = new Set(prs);
  for (const row of rows) {
    const d = row.detail;
    if (!d || typeof d !== 'object') continue;
    if ((row.action === 'rd-lane-spawned' || row.action === 'rd-handback') && prSet.has(d.pr)) {
      add(structural, d.agent, d.pr);
    } else if (row.action === 'review-verdict' && prSet.has(d.pr)) {
      add(structural, row.actor, d.pr);
    } else if (row.action === 'agent-spawn') {
      const blob = JSON.stringify(d);
      for (const pr of prs) {
        const win = windowsByPr.get(pr);
        if (!win || !win.pr || !inWindow(row.ts_ms, win.pr)) continue;
        if (prTokenRe(pr).test(blob)) add(textual, d.agent, pr);
      }
    }
  }
  const out = new Map(); // agentId -> {prs:Set, tier}
  for (const [agent, set] of structural) out.set(agent, { prs: set, tier: 'structural' });
  for (const [agent, set] of textual) {
    if (out.has(agent)) continue;
    out.set(agent, { prs: set, tier: 'text' });
  }
  return out;
}

// ---------------------------------------------------------------------------
// Per-PR scorecard.
// ---------------------------------------------------------------------------

function scorePr(ctx, pr) {
  const { rows, orchIds, tailMs, transcriptTurns, usageBySession, usageByAgent, sessionAgents, agentsById, attribution, prMeta } = ctx;
  const meta = (prMeta && prMeta[String(pr)]) || {};
  const mergedMs = meta.merged_at ? Date.parse(meta.merged_at) : null;
  const win = computeWindows(rows, pr, Number.isFinite(mergedMs) ? mergedMs : null, tailMs);
  const re = prTokenRe(pr);

  const wakes = emptyWakeCounts();
  let wakesTotal = 0;
  let windowWakes = 0; // every orchestrator wake in the window, this PR's or not
  let loopNotices = 0;
  let loopNoticesAnyPane = 0;
  const verdicts = {};
  let verdictsTotal = 0;
  const driver = {
    drives: 0, lane_spawns: 0, hand_backs: 0, refused: 0, held: 0,
    cancelled: 0, consumed: 0, satisfied: 0, ci_green: 0, resumed: 0, pruned: 0,
  };
  const refusedByReason = {};
  const heldByReason = {};
  const unclassified = {};
  let rowsClassified = 0;
  // A `review-verdict` or `rd-*` row is matched STRUCTURALLY on `detail.pr`, so it is
  // counted wherever it occurs — a re-drive or a late verdict after the merge is still
  // a round the PR cost. That makes those counters wider than the PR window, so the
  // number that fall outside it is reported rather than left for a reader to wonder
  // about. It is 0 on all eleven benchmark PRs.
  let structuralOutsideWindow = 0;

  for (const row of rows) {
    const d = row.detail;
    const isWake = row.action === 'prompt' && d && orchIds.has(d.to);
    if (isWake && inWindow(row.ts_ms, win.pr)) windowWakes += 1;
    if (!rowNamesPr(row, pr, re)) continue;

    // An `[orrerix]`-prefixed prompt naming the PR inside the LOOP window, delivered
    // to ANY pane. This is #1778's S5 "orch notices" column verbatim — it is what
    // that instrument counted, and it is reproduced here so the table stays checkable
    // — but it is NOT a count of orchestrator wakes: 1-2 per PR were the driver's own
    // resume prompts typed into a WORKER pane, which is precisely the traffic the
    // driver moved OFF the orchestrator. `loop_notices` below is the corrected count.
    if (row.action === 'prompt' && inWindow(row.ts_ms, win.loop) && /^\[orrerix\]/.test((d && d.text) || '')) {
      loopNoticesAnyPane += 1;
    }
    if (isWake) {
      if (!inWindow(row.ts_ms, win.pr)) continue;
      wakes[classifyWake(d.text)] += 1;
      wakesTotal += 1;
      rowsClassified += 1;
      if (inWindow(row.ts_ms, win.loop) && /^\[orrerix\]/.test(d.text || '')) loopNotices += 1;
      continue;
    }
    if (row.action === 'review-verdict' && d.pr === pr) {
      if (!inWindow(row.ts_ms, win.pr)) structuralOutsideWindow += 1;
      const block = d.block || 'unknown';
      const verdict = String(d.verdict || 'unknown').toLowerCase();
      verdicts[block] = verdicts[block] || {};
      verdicts[block][verdict] = (verdicts[block][verdict] || 0) + 1;
      verdictsTotal += 1;
      rowsClassified += 1;
      continue;
    }
    if (typeof row.action === 'string' && row.action.startsWith('rd-') && d.pr === pr) {
      if (!inWindow(row.ts_ms, win.pr)) structuralOutsideWindow += 1;
      rowsClassified += 1;
      switch (row.action) {
        case 'rd-started': driver.drives += 1; break;
        case 'rd-lane-spawned': driver.lane_spawns += 1; break;
        case 'rd-handback': driver.hand_backs += 1; break;
        case 'rd-refused':
          driver.refused += 1;
          refusedByReason[d.reason || 'unknown'] = (refusedByReason[d.reason || 'unknown'] || 0) + 1;
          break;
        case 'rd-held':
          driver.held += 1;
          heldByReason[d.reason || 'unknown'] = (heldByReason[d.reason || 'unknown'] || 0) + 1;
          break;
        case 'rd-cancelled': driver.cancelled += 1; break;
        case 'rd-consumed': driver.consumed += 1; break;
        case 'rd-satisfied': driver.satisfied += 1; break;
        case 'rd-ci-green': driver.ci_green += 1; break;
        case 'rd-resumed': driver.resumed += 1; break;
        case 'rd-pruned': driver.pruned += 1; break;
        default: driver[row.action] = (driver[row.action] || 0) + 1; break;
      }
      continue;
    }
    if (inWindow(row.ts_ms, win.pr)) {
      unclassified[row.action] = (unclassified[row.action] || 0) + 1;
    }
  }

  // Orchestrator tokens: every deduped transcript turn stamped inside the PR window.
  // `usage.json` cannot answer this — it is CUMULATIVE per session and carries no
  // time series (doc/design/group-cost-tracking.md).
  const orchTokens = emptyTokens();
  for (const t of transcriptTurns) {
    if (inWindow(t.ts_ms, win.pr)) addTokens(orchTokens, t.usage);
  }
  // The orchestrator serves several PRs at once, so the raw window sum is an UPPER
  // BOUND for one PR. The apportioned figure splits it by this PR's share of the
  // orchestrator wakes in the same window (H5). Both are reported: the raw one is
  // what the brief asks for, the apportioned one is what is comparable when two PRs
  // ran concurrently.
  const wakeShare = windowWakes > 0 ? wakesTotal / windowWakes : (wakesTotal > 0 ? 1 : 0);
  const orchTokensAttributed = scaleTokens(orchTokens, wakeShare);

  const delegateTokens = emptyTokens();
  const delegates = [];
  for (const [agent, att] of attribution) {
    if (!att.prs.has(pr)) continue;
    const info = agentsById.get(agent);
    if (info && info.role === 'orchestrator') continue;
    // Session first (see `indexUsage`), agent id only as a fallback.
    const session = info && typeof info.session === 'string' ? info.session : null;
    let usage = session ? usageBySession.get(session) : undefined;
    let usageKey = usage ? 'session' : null;
    let sessionAgentCount = 1;
    if (usage) {
      sessionAgentCount = (sessionAgents.get(session) || [agent]).length || 1;
    } else {
      usage = usageByAgent.get(agent);
      usageKey = usage ? 'agent_id' : null;
    }
    const prWeight = 1 / att.prs.size;
    const sessionWeight = 1 / sessionAgentCount;
    const weight = prWeight * sessionWeight;
    const t = usage
      ? { input: usage.input, cache_read: usage.cache_read, cache_creation: usage.cache_creation, output: usage.output, total: usage.total }
      : { input: 0, cache_read: 0, cache_creation: 0, output: 0, total: 0 };
    delegateTokens.input += t.input * weight;
    delegateTokens.cache_read += t.cache_read * weight;
    delegateTokens.cache_creation += t.cache_creation * weight;
    delegateTokens.output += t.output * weight;
    delegateTokens.total += t.total * weight;
    delegateTokens.turns += 1;
    delegates.push({
      agent,
      block: info ? info.block : null,
      role: info ? info.role : null,
      tier: att.tier,
      weight: round2(weight),
      pr_weight: round2(prWeight),
      session_weight: round2(sessionWeight),
      shared_with_prs: att.prs.size,
      shared_session_agents: sessionAgentCount,
      // `tokens` is the WHOLE row the agent's session carries; `tokens_credited` is what
      // this PR actually got after both weights. They differ whenever a session was
      // shared or an agent worked more than one PR, and the card shows both so the
      // delegate total is checkable by hand.
      tokens: t.total,
      tokens_credited: Math.round(t.total * weight),
      usage_key: usageKey,
      has_usage_row: Boolean(usage),
    });
  }
  for (const k of ['input', 'cache_read', 'cache_creation', 'output', 'total']) {
    delegateTokens[k] = Math.round(delegateTokens[k]);
  }
  delegates.sort((a, b) => b.tokens - a.tokens || (a.agent < b.agent ? -1 : 1));

  return {
    pr,
    build: meta.build || null,
    issue: meta.issue || null,
    outcome: meta.outcome || null,
    windows: win,
    orchestrator: {
      wakes_total: wakesTotal,
      wakes_by_kind: wakes,
      loop_notices: loopNotices,
      loop_notices_any_pane_s5: loopNoticesAnyPane,
      wake_share: { pr_wakes: wakesTotal, window_wakes: windowWakes, share: round2(wakeShare) },
      tokens_window: orchTokens,
      tokens_attributed: orchTokensAttributed,
    },
    review: { rounds: verdictsTotal, by_block: verdicts },
    driver: { ...driver, refused_by_reason: refusedByReason, held_by_reason: heldByReason },
    delegates: { count: delegates.length, tokens: delegateTokens, agents: delegates },
    share: {
      orchestrator_pct_raw: pct(orchTokens.total, orchTokens.total + delegateTokens.total),
      orchestrator_pct_attributed: pct(orchTokensAttributed.total, orchTokensAttributed.total + delegateTokens.total),
    },
    rows_classified: rowsClassified,
    rows_counted_outside_pr_window: structuralOutsideWindow,
    rows_unclassified_in_window: unclassified,
  };
}

function pct(part, whole) {
  if (!whole) return null;
  return round2((part / whole) * 100);
}

// ---------------------------------------------------------------------------
// Group-wide totals — the plan's part-1 calibration numbers, per audit file.
// ---------------------------------------------------------------------------

function groupTotals(files, orchIds) {
  return files.map((f) => {
    const wakes = emptyWakeCounts();
    let total = 0;
    let typed = 0;
    let tsFirst = null;
    let tsLast = null;
    for (const row of f.rows) {
      if (typeof row.ts_ms === 'number') {
        if (tsFirst === null || row.ts_ms < tsFirst) tsFirst = row.ts_ms;
        if (tsLast === null || row.ts_ms > tsLast) tsLast = row.ts_ms;
      }
      const d = row.detail;
      if (!d || !orchIds.has(d.to)) continue;
      if (row.action === 'prompt') { wakes[classifyWake(d.text)] += 1; total += 1; }
      else if (row.action === 'prompt-typed') typed += 1;
    }
    return {
      path: f.path,
      rows: f.rows.length,
      parse_errors: f.parseErrors,
      ts_first: tsFirst,
      ts_last: tsLast,
      span_h: tsFirst === null ? null : round2((tsLast - tsFirst) / 3600000),
      orchestrator_wakes: total,
      prompt_typed_to_orchestrator: typed,
      wakes_by_kind: wakes,
    };
  });
}

// ---------------------------------------------------------------------------
// Loading.
// ---------------------------------------------------------------------------

function readJsonl(pathname, cutMs) {
  const rows = [];
  let parseErrors = 0;
  const text = fs.readFileSync(pathname, 'utf8');
  for (const line of text.split('\n')) {
    if (!line.trim()) continue;
    let row;
    try { row = JSON.parse(line); } catch { parseErrors += 1; continue; }
    if (cutMs !== null && typeof row.ts_ms === 'number' && row.ts_ms > cutMs) continue;
    rows.push(row);
  }
  return { path: pathname, rows, parseErrors };
}

async function readTranscript(pathname, cutMs) {
  const entries = [];
  const rl = readline.createInterface({
    input: fs.createReadStream(pathname, { encoding: 'utf8' }),
    crlfDelay: Infinity,
  });
  let lines = 0;
  for await (const line of rl) {
    lines += 1;
    if (!line) continue;
    // Cheap prefilter before JSON.parse: the file is ~230 MB and only a quarter of
    // its lines are assistant responses carrying usage.
    if (line.indexOf('"usage"') === -1 || line.indexOf('"assistant"') === -1) continue;
    let entry;
    try { entry = JSON.parse(line); } catch { continue; }
    // Project to the three fields this reader is allowed to see BEFORE retaining
    // anything: the parsed line is dropped here, so no transcript prose is ever held
    // in memory, printed, or written. (It also keeps a 230 MB file's worth of
    // assistant messages from becoming a 30k-element array of full objects.)
    if (entry.type !== 'assistant' || !entry.message || !entry.message.usage) continue;
    entries.push({
      type: 'assistant',
      timestamp: entry.timestamp,
      message: { id: entry.message.id, usage: entry.message.usage },
    });
  }
  const deduped = dedupeTranscriptTurns(entries);
  if (cutMs !== null) deduped.turns = deduped.turns.filter((t) => t.ts_ms <= cutMs);
  return { path: pathname, lines, assistant_usage_lines: entries.length, ...deduped };
}

function indexAgents(agents) {
  const byId = new Map();
  for (const a of agents) {
    if (!a || typeof a.id !== 'string') continue;
    byId.set(a.id, a);
  }
  return byId;
}

// A `usage.json` row is keyed by **CLI SESSION**, not by agent. `group-cost-tracking.md`
// says why — "keyed by CLI session id … a resumed session updates one row instead of
// double-counting, since the transcript is cumulative" — and the consequence is the
// thing to get right here: when a pane's session is carried to a NEW agent id, that one
// row's `agent_id` names only the LAST occupant, and every earlier agent on the session
// has no row of its own at all. Joining on `agent_id` therefore reports an agent that
// demonstrably spent tokens as having spent zero, while crediting its successor with the
// whole lineage. On this group's own store that is not a corner case: 514 of 1356 rows
// sit on a session shared by more than one agent id, carrying 31.2 G of 44.1 G tokens.
//
// So the row is indexed by session and split evenly across the agents that occupied it
// (heuristic H8). The split is a guess about how a shared session's spend divided, but it
// is self-correcting in the common case: a session reused by a worker's own successive
// agent ids has every occupant attributed to the SAME PR, so the halves re-sum to the
// whole row. Where only some occupants are attributed, the PR gets a fraction rather than
// all-or-nothing.
//
// `byAgent` is kept as a FALLBACK for a row whose session key is missing, and which index
// answered is reported per delegate as `usage_key`.
function indexUsage(usage) {
  const bySession = new Map();
  const byAgent = new Map();
  let unusable = 0;
  const blank = () => ({ input: 0, cache_read: 0, cache_creation: 0, output: 0, total: 0, cost_usd: 0, rows: 0 });
  const add = (map, key, u) => {
    const acc = map.get(key) || blank();
    acc.input += num(u.input_tokens);
    acc.cache_read += num(u.cache_read_tokens);
    acc.cache_creation += num(u.cache_creation_tokens);
    acc.output += num(u.output_tokens);
    acc.cost_usd += num(u.cost_usd);
    acc.rows += 1;
    acc.total = acc.input + acc.cache_read + acc.cache_creation + acc.output;
    map.set(key, acc);
  };
  for (const u of usage) {
    if (!u || typeof u !== 'object') { unusable += 1; continue; }
    const hasSession = typeof u.key === 'string' && u.key;
    const hasAgent = typeof u.agent_id === 'string' && u.agent_id;
    if (!hasSession && !hasAgent) { unusable += 1; continue; }
    if (hasSession) add(bySession, u.key, u);
    if (hasAgent) add(byAgent, u.agent_id, u);
  }
  return { bySession, byAgent, unusable };
}

// session id -> every agent id that occupied it, from `agents.json`. Used only to size
// the H8 split, so it counts EVERY agent on the session, attributed or not.
function indexSessionAgents(agents) {
  const bySession = new Map();
  for (const a of agents) {
    if (!a || typeof a.session !== 'string' || !a.session) continue;
    if (!bySession.has(a.session)) bySession.set(a.session, []);
    bySession.get(a.session).push(a.id);
  }
  return bySession;
}

// ---------------------------------------------------------------------------
// Rendering.
// ---------------------------------------------------------------------------

function fmtTokens(n) {
  if (n === null || n === undefined) return '—';
  if (n >= 1e9) return (n / 1e9).toFixed(2) + ' G';
  if (n >= 1e6) return (n / 1e6).toFixed(1) + ' M';
  if (n >= 1e3) return (n / 1e3).toFixed(0) + ' K';
  return String(n);
}

function renderPrTable(cards) {
  const lines = [
    '| PR | build | wakes | of which reports | rounds | lane spawns | hand-backs | `rd-refused` | held | loop span h | loop notices (S5) | orch tokens | delegate tokens | orch share |',
    '|---|---|---|---|---|---|---|---|---|---|---|---|---|---|',
  ];
  for (const c of cards) {
    const w = c.orchestrator.wakes_by_kind;
    const reports = w['delegate-progress'] + w['delegate-done'] + w['reviewer-report'] + w['delegate-blocked'];
    lines.push('| #' + c.pr
      + ' | ' + (c.build || '—')
      + ' | ' + c.orchestrator.wakes_total
      + ' | ' + reports
      + ' | ' + c.review.rounds
      + ' | ' + c.driver.lane_spawns
      + ' | ' + c.driver.hand_backs
      + ' | ' + c.driver.refused
      + ' | ' + c.driver.held
      + ' | ' + (c.windows.loop ? c.windows.loop.span_h.toFixed(2) : '—')
      + ' | ' + (c.windows.loop ? c.orchestrator.loop_notices + ' (' + c.orchestrator.loop_notices_any_pane_s5 + ')' : '—')
      + ' | ' + fmtTokens(c.orchestrator.tokens_window.total)
      + ' | ' + fmtTokens(c.delegates.tokens.total)
      + ' | ' + (c.share.orchestrator_pct_raw === null ? '—' : c.share.orchestrator_pct_raw.toFixed(1) + ' %')
      + ' |');
  }
  return lines.join('\n');
}

function renderGroupTable(files) {
  const lines = [
    '| audit file | rows | span h | orch wakes | progress | done | reviewer | blocked/msg | verdict | system | other | `prompt-typed` |',
    '|---|---|---|---|---|---|---|---|---|---|---|---|',
  ];
  for (const f of files) {
    const w = f.wakes_by_kind;
    lines.push('| `' + f.path.split(/[\\/]/).pop() + '` | ' + f.rows + ' | ' + (f.span_h === null ? '—' : f.span_h.toFixed(1))
      + ' | ' + f.orchestrator_wakes
      + ' | ' + w['delegate-progress'] + ' | ' + w['delegate-done'] + ' | ' + w['reviewer-report']
      + ' | ' + w['delegate-blocked'] + ' | ' + w['verdict-notice'] + ' | ' + w['system-notice'] + ' | ' + w.other
      + ' | ' + f.prompt_typed_to_orchestrator + ' |');
  }
  return lines.join('\n');
}

// The heuristics this reader has to use. This list IS #2011 B2's scope — every row
// is a place where one structural field would replace a guess.
const HEURISTICS = [
  { id: 'H1', what: 'A PR is joined to an audit row by the `#N` token in the serialized `detail` wherever `detail.pr` is absent.', fix: 'A `pr` field on `prompt` / `delivery-queued` rows (plan part 2, A1 / missing row 2).' },
  { id: 'H2', what: 'An agent is joined to a PR by the `#N` token in its `agent-spawn` detail (brief text, name, branch) when no `rd-*` or `review-verdict` row carries both.', fix: 'A `pr` field on `agent-spawn`.' },
  { id: 'H3', what: 'A text-tier attribution counts only when the `agent-spawn` row falls inside the PR window; outside it the same token is ignored.', fix: 'Same as H2 — the window bound exists only because the token is ambiguous.' },
  { id: 'H4', what: "An agent attributed to k PRs contributes 1/k of its lifetime tokens to each — and k counts only the PRs in THIS run's selection, so running one PR alone gives its shared agents full weight. Always run the whole comparison set together.", fix: 'A `block` and a `pr` on `UsageSnapshot` (plan part 2, A6 / missing row 4).' },
  { id: 'H5', what: "Orchestrator tokens in a window cover every PR in flight; the apportioned figure splits them by this PR's share of the orchestrator wakes in the same window.", fix: 'Per-turn PR attribution — nothing structural exists; see "What this cannot say" in the note.' },
  { id: 'H6', what: 'Delegate tokens come from `usage.json`, which is CUMULATIVE per session and cannot be windowed; a delegate that worked on one PR reports its whole life against that PR.', fix: 'A windowed usage series, or accepting the approximation (delegates are spawned per task).' },
  { id: 'H7', what: 'A PR window ends at `merged_at` supplied via `--pr-meta`; without it the end falls back to the last `rd-*` row, then to the last naming row — which is days late, because a merged PR stays cited.', fix: 'A loomux row for a human merge (plan part 2, A4 / #388).' },
  { id: 'H8', what: "A `usage.json` row is keyed by CLI SESSION, and a session carried to a new agent id names only its LAST occupant. The row is therefore split evenly across every agent that occupied that session — a guess about how a shared session's spend divided, self-correcting where the whole lineage is attributed to one PR and a fraction where it is not.", fix: 'An `agent_id` (or `block` + `pr`) on every `UsageSnapshot`, not just the latest — the same missing field as H4.' },
];

// ---------------------------------------------------------------------------
// CLI.
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const opts = {
    audit: [], transcript: [], usage: null, agents: null, prMeta: null,
    prs: [], all: false, tailMin: DEFAULT_TAIL_MIN, format: 'json', cut: null, help: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    const next = () => argv[++i];
    switch (a) {
      case '--audit': opts.audit.push(next()); break;
      case '--transcript': opts.transcript.push(next()); break;
      case '--usage': opts.usage = next(); break;
      case '--agents': opts.agents = next(); break;
      case '--pr-meta': opts.prMeta = next(); break;
      case '--pr': opts.prs.push(Number(next())); break;
      case '--all': opts.all = true; break;
      case '--tail-min': opts.tailMin = Number(next()); break;
      case '--format': opts.format = next(); break;
      case '--cut': opts.cut = /^\d+$/.test(String(argv[i + 1])) ? Number(next()) : Date.parse(next()); break;
      case '--help': case '-h': opts.help = true; break;
      default: throw new Error('unknown argument: ' + a);
    }
  }
  return opts;
}

const USAGE_TEXT = `orch-scorecard — per-PR orchestration cost from existing logs (#2011 B1)

  node scripts/orch-scorecard.cjs --audit <audit.jsonl> [--audit <audit.1.jsonl>]
      --usage <usage.json> --agents <agents.json>
      [--transcript <session.jsonl>]... [--pr-meta <meta.json>]
      (--pr <n> [--pr <n>]... | --all)
      [--tail-min 10] [--cut <ms|iso>] [--format json|table|both]

  --all        every PR that any rd-* row names (the driven set).
  --pr-meta    {"2104": {"merged_at": "...", "build": "beta5", "issue": 2010}} —
               without merged_at a PR window ends at its last rd row (coverage says so).
  --cut        drop audit rows and transcript turns after this instant; reproduces a
               historical measurement on a log that has since grown.
`;

async function main(argv) {
  const opts = parseArgs(argv);
  if (opts.help || opts.audit.length === 0) { process.stdout.write(USAGE_TEXT); return 0; }
  if (!opts.usage || !opts.agents) throw new Error('--usage and --agents are required');

  const cut = Number.isFinite(opts.cut) ? opts.cut : null;
  const tailMs = opts.tailMin * 60000;
  const files = opts.audit.map((p) => readJsonl(p, cut));
  const rows = files.flatMap((f) => f.rows).sort((a, b) => (a.ts_ms || 0) - (b.ts_ms || 0));
  const agents = JSON.parse(fs.readFileSync(opts.agents, 'utf8'));
  const usage = JSON.parse(fs.readFileSync(opts.usage, 'utf8'));
  const prMeta = opts.prMeta ? JSON.parse(fs.readFileSync(opts.prMeta, 'utf8')) : {};

  const agentsById = indexAgents(agents);
  const orchIds = new Set([...agentsById.values()].filter((a) => a.role === 'orchestrator').map((a) => a.id));
  const { bySession: usageBySession, byAgent: usageByAgent, unusable: usageUnusable } = indexUsage(usage);
  const sessionAgents = indexSessionAgents(agents);

  const transcripts = [];
  for (const p of opts.transcript) transcripts.push(await readTranscript(p, cut));
  const transcriptTurns = transcripts.flatMap((t) => t.turns).sort((a, b) => a.ts_ms - b.ts_ms);

  let prs = opts.prs.slice();
  if (opts.all) {
    const seen = new Set(prs);
    for (const r of rows) {
      if (typeof r.action === 'string' && r.action.startsWith('rd-') && r.detail && typeof r.detail.pr === 'number') seen.add(r.detail.pr);
    }
    prs = [...seen];
  }
  prs = [...new Set(prs)].filter((n) => Number.isFinite(n)).sort((a, b) => a - b);
  if (prs.length === 0) throw new Error('no PRs selected: pass --pr <n> or --all');

  const windowsByPr = new Map();
  for (const pr of prs) {
    const meta = prMeta[String(pr)] || {};
    const mergedMs = meta.merged_at ? Date.parse(meta.merged_at) : null;
    windowsByPr.set(pr, computeWindows(rows, pr, Number.isFinite(mergedMs) ? mergedMs : null, tailMs));
  }
  const attribution = attributeAgents(rows, prs, windowsByPr);

  const ctx = { rows, orchIds, tailMs, transcriptTurns, usageBySession, usageByAgent, sessionAgents, agentsById, attribution, prMeta };
  const cards = prs.map((pr) => scorePr(ctx, pr));

  const spawnedInWindow = new Set();
  for (const r of rows) {
    if (r.action !== 'agent-spawn' || !r.detail || !r.detail.agent) continue;
    for (const pr of prs) {
      const w = windowsByPr.get(pr);
      if (w && w.pr && inWindow(r.ts_ms, w.pr)) { spawnedInWindow.add(r.detail.agent); break; }
    }
  }
  const unattributed = [...spawnedInWindow].filter((a) => !attribution.has(a)).sort();
  const split = [...attribution.entries()]
    .filter(([, v]) => v.prs.size > 1)
    .map(([a, v]) => ({ agent: a, prs: [...v.prs].sort((x, y) => x - y), tier: v.tier }));

  const out = {
    generated_ms: Date.now(),
    inputs: {
      audit: opts.audit, usage: opts.usage, agents: opts.agents,
      transcript: opts.transcript, pr_meta: opts.prMeta, tail_min: opts.tailMin, cut_ms: cut,
    },
    group: { files: groupTotals(files, orchIds) },
    prs: cards,
    coverage: {
      audit_rows_read: rows.length,
      audit_parse_errors: files.reduce((n, f) => n + f.parseErrors, 0),
      rows_classified: cards.reduce((n, c) => n + c.rows_classified, 0),
      orchestrator_agent_ids: orchIds.size,
      transcripts: transcripts.map((t) => ({
        path: t.path, lines: t.lines, assistant_usage_lines: t.assistant_usage_lines,
        deduped_turns: t.turns.length, usage_rows_without_id: t.without_id,
      })),
      usage_rows_unusable: usageUnusable,
      usage_sessions_indexed: usageBySession.size,
      usage_sessions_shared_by_more_than_one_agent: [...sessionAgents.values()].filter((v) => v.length > 1).length,
      agents_attributed: attribution.size,
      agents_unattributed_spawned_in_window: unattributed,
      agents_split_across_prs: split,
      heuristics: HEURISTICS,
    },
  };

  if (opts.format === 'json' || opts.format === 'both') process.stdout.write(JSON.stringify(out, null, 2) + '\n');
  if (opts.format === 'table' || opts.format === 'both') {
    process.stdout.write('\n' + renderPrTable(cards) + '\n\n' + renderGroupTable(out.group.files) + '\n');
  }
  return 0;
}

module.exports = {
  WAKE_KINDS, classifyWake, prTokenRe, rowNamesPr, computeWindows, inWindow,
  dedupeTranscriptTurns, attributeAgents, scorePr, groupTotals, indexAgents,
  indexUsage, indexSessionAgents, renderPrTable, renderGroupTable, parseArgs, HEURISTICS,
  DEFAULT_TAIL_MIN, main,
};

if (require.main === module) {
  main(process.argv.slice(2)).then((code) => { process.exitCode = code; }, (err) => {
    process.stderr.write('orch-scorecard: ' + (err && err.message ? err.message : String(err)) + '\n');
    process.exitCode = 1;
  });
}
